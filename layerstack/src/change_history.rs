// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit budgets and accounting for authored and composed change evidence.

use alloc::collections::VecDeque;
use core::{mem::size_of, ops::Deref};

/// Limits for retained change evidence. A zero limit disables retention.
///
/// Byte accounting includes record headers and owned vector capacities, excluding
/// allocator overhead and spare ring capacity (reported separately in stats).
/// An oversized batch is not retained; it creates a history gap requiring recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeHistoryBudget {
    /// Maximum number of retained batches.
    pub max_batches: usize,
    /// Maximum retained record/vector bytes.
    pub max_retained_bytes: usize,
}
impl Default for ChangeHistoryBudget {
    fn default() -> Self {
        Self {
            max_batches: 64,
            max_retained_bytes: 1024 * 1024,
        }
    }
}

/// Retained evidence memory and cumulative retention work, not whole-stage memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeHistoryStats {
    /// Batches currently available for replay.
    pub retained_batches: usize,
    /// Record headers and their owned vector allocations.
    pub retained_bytes: usize,
    /// Retained bytes plus spare ring storage; excludes allocator overhead.
    pub allocated_bytes: usize,
    /// Batches accepted into retention, including those subsequently evicted.
    pub recorded_batches: u64,
    /// Path/field entries accepted into retention across all batches.
    pub recorded_items: u64,
    /// Previously retained batches removed by budget changes, expiry or clearing.
    pub evicted_batches: u64,
    /// Incoming batches not retained because they exceed a configured limit.
    pub discarded_batches: u64,
}

pub(crate) trait Record {
    fn revision(&self) -> u64;
    fn bytes(&self) -> usize;
    fn items(&self) -> usize;
}

#[derive(Debug)]
pub(crate) struct History<T> {
    entries: VecDeque<T>,
    budget: ChangeHistoryBudget,
    stats: ChangeHistoryStats,
    discarded_through: u64,
}
impl<T: Record + Clone> Clone for History<T> {
    fn clone(&self) -> Self {
        let entries = self.entries.clone();
        // Vec clones can shrink spare capacity; copied byte counters would be
        // incorrect and could leave nonzero accounting after the last eviction.
        let retained_bytes = entries.iter().map(Record::bytes).sum();
        Self {
            entries,
            budget: self.budget,
            stats: ChangeHistoryStats {
                retained_bytes,
                ..self.stats
            },
            discarded_through: self.discarded_through,
        }
    }
}
impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            budget: ChangeHistoryBudget::default(),
            stats: ChangeHistoryStats::default(),
            discarded_through: 0,
        }
    }
}
impl<T> Deref for History<T> {
    type Target = VecDeque<T>;
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}
impl<T: Record> History<T> {
    pub(crate) fn budget(&self) -> ChangeHistoryBudget {
        self.budget
    }
    pub(crate) fn stats(&self) -> ChangeHistoryStats {
        ChangeHistoryStats {
            retained_batches: self.entries.len(),
            allocated_bytes: self.stats.retained_bytes.saturating_add(
                (self.entries.capacity() - self.entries.len()).saturating_mul(size_of::<T>()),
            ),
            ..self.stats
        }
    }
    pub(crate) fn floor(&self) -> u64 {
        self.discarded_through
    }
    pub(crate) fn set_budget(&mut self, budget: ChangeHistoryBudget) {
        self.budget = budget;
        self.trim();
        self.entries.shrink_to_fit();
    }
    pub(crate) fn clear(&mut self) {
        while !self.entries.is_empty() {
            self.evict();
        }
        self.entries.shrink_to_fit();
    }
    fn evict(&mut self) {
        if let Some(record) = self.entries.pop_front() {
            self.discarded_through = self.discarded_through.max(record.revision());
            self.stats.retained_bytes -= record.bytes();
            self.stats.evicted_batches = self.stats.evicted_batches.saturating_add(1);
        }
    }
    fn trim(&mut self) {
        while self.entries.len() > self.budget.max_batches
            || self.stats.retained_bytes > self.budget.max_retained_bytes
        {
            self.evict();
        }
    }
    pub(crate) fn record_with(
        &mut self,
        revision: u64,
        estimated_bytes: usize,
        build: impl FnOnce() -> T,
    ) {
        if self.budget.max_batches == 0 || estimated_bytes > self.budget.max_retained_bytes {
            self.discard(revision);
            return;
        }
        self.record(build());
    }
    pub(crate) fn record(&mut self, record: T) {
        let bytes = record.bytes();
        if self.budget.max_batches == 0 || bytes > self.budget.max_retained_bytes {
            self.discard(record.revision());
            return;
        }
        // Evict before addition so even usize::MAX budgets cannot overflow accounting.
        while self.entries.len() >= self.budget.max_batches
            || self.stats.retained_bytes > self.budget.max_retained_bytes - bytes
        {
            self.evict();
        }
        self.stats.retained_bytes += bytes;
        self.stats.recorded_batches = self.stats.recorded_batches.saturating_add(1);
        self.stats.recorded_items = self
            .stats
            .recorded_items
            .saturating_add(u64::try_from(record.items()).unwrap_or(u64::MAX));
        self.entries.push_back(record);
    }
    fn discard(&mut self, revision: u64) {
        // No observer may skip a missing middle batch: preceding evidence is unusable.
        self.clear();
        self.discarded_through = self.discarded_through.max(revision);
        self.stats.discarded_batches = self.stats.discarded_batches.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use crate::{ChangeHistoryBudget, Layer, LayerId, PathId};
    use alloc::{vec, vec::Vec};
    #[test]
    fn authored_history_budget_preserves_content_and_reports_missing_precision() {
        let mut layer = Layer::new(LayerId(1));
        let path = PathId::from_raw(1);
        layer.record_change(false, Some(vec![path]));
        let bytes = layer.change_history_stats().retained_bytes;
        assert_eq!(layer.changed_paths_since(0), Some(vec![path]));
        layer.set_change_history_budget(ChangeHistoryBudget {
            max_batches: 1,
            max_retained_bytes: bytes,
        });
        layer.record_change(false, Some(vec![path]));
        assert_eq!(layer.changed_paths_since(0), None);
        assert_eq!(layer.changed_paths_since(1), Some(vec![path]));
        assert_eq!(layer.change_history_stats().evicted_batches, 1);
        layer.set_change_history_budget(ChangeHistoryBudget {
            max_batches: 0,
            max_retained_bytes: 0,
        });
        layer.record_change(false, Some(vec![path]));
        assert_eq!(layer.changed_paths_since(2), None);
        assert_eq!(layer.changed_paths_since(3), Some(Vec::new()));
        assert_eq!(layer.change_history_stats().allocated_bytes, 0);
        assert_eq!(layer.change_history_stats().discarded_batches, 1);
    }
    #[test]
    fn cloning_history_reaccounts_vector_capacity_before_eviction() {
        let mut layer = Layer::new(LayerId(1));
        let mut paths = Vec::with_capacity(100);
        paths.push(PathId::from_raw(1));
        layer.record_change(false, Some(paths));
        let mut cloned = layer.clone();
        assert!(
            cloned.change_history_stats().retained_bytes
                < layer.change_history_stats().retained_bytes
        );
        cloned.set_change_history_budget(ChangeHistoryBudget {
            max_batches: 0,
            max_retained_bytes: 0,
        });
        assert_eq!(cloned.change_history_stats().allocated_bytes, 0);
        assert_eq!(cloned.change_history_stats().retained_bytes, 0);
    }
}
