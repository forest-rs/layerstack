// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded, stage-owned composed change evidence.

use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec::Vec};

use crate::{Changes, LayerStore, Stage};

/// An independent position in one live stage's composed change history.
#[derive(Clone, Debug)]
pub struct ChangeCursor {
    identity: Arc<()>,
    revision: u64,
}

impl ChangeCursor {
    /// Last composed revision consumed by this observer.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// Why incremental evidence cannot be replayed for an observer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeHistoryError {
    /// More batches have passed than the stage retains. Rebuild derived state.
    Expired,
    /// The cursor belongs to a different live stage.
    DifferentStage,
}

/// A subscription to synchronous composed stage-change notifications.
///
/// Unsubscribe explicitly through `LiveStage::unsubscribe_changes`. Dropping
/// this token alone does not unsubscribe; callbacks live until removed or the
/// stage is dropped.
#[derive(Clone, Debug)]
pub struct ChangeSubscription {
    identity: Arc<()>,
    id: u64,
}

/// A fully updated composed scene and the evidence for this change batch.
#[derive(Clone, Copy)]
pub struct ChangeNotice<'a> {
    /// Stage after the completed update.
    pub stage: &'a Stage,
    /// Source store used to compose the stage.
    pub store: &'a dyn LayerStore,
    /// Monotonically increasing revision of this stage's change stream.
    pub revision: u64,
    /// Composed namespace and property changes.
    pub changes: &'a Changes,
}

impl core::fmt::Debug for ChangeNotice<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChangeNotice")
            .field("revision", &self.revision)
            .field("changes", &self.changes)
            .finish_non_exhaustive()
    }
}

type Callback = Box<dyn FnMut(ChangeNotice<'_>) + Send + Sync>;

#[derive(Default)]
pub(super) struct Journal {
    identity: Arc<()>,
    revision: u64,
    enabled: bool,
    reports: VecDeque<(u64, Changes)>,
    callbacks: Vec<(u64, Callback)>,
    next_subscription: u64,
}

impl core::fmt::Debug for Journal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Journal")
            .field("revision", &self.revision)
            .field("retained_batches", &self.reports.len())
            .field("callbacks", &self.callbacks.len())
            .finish_non_exhaustive()
    }
}

impl Journal {
    pub(super) fn subscribe(
        &mut self,
        callback: impl FnMut(ChangeNotice<'_>) + Send + Sync + 'static,
    ) -> ChangeSubscription {
        let id = self.next_subscription;
        self.next_subscription = id.checked_add(1).expect("subscription identity exhausted");
        self.callbacks.push((id, Box::new(callback)));
        ChangeSubscription {
            identity: self.identity.clone(),
            id,
        }
    }

    pub(super) fn unsubscribe(&mut self, subscription: &ChangeSubscription) -> bool {
        if !Arc::ptr_eq(&self.identity, &subscription.identity) {
            return false;
        }
        if let Some(index) = self
            .callbacks
            .iter()
            .position(|(id, _)| *id == subscription.id)
        {
            let _ = self.callbacks.remove(index);
            true
        } else {
            false
        }
    }

    pub(super) fn cursor(&mut self) -> ChangeCursor {
        self.enabled = true;
        ChangeCursor {
            identity: self.identity.clone(),
            revision: self.revision,
        }
    }

    pub(super) fn publish(&mut self, changes: &Changes, stage: &Stage, store: &dyn LayerStore) {
        if changes.created.is_empty()
            && changes.removed.is_empty()
            && changes.resynced.is_empty()
            && changes.changed_info_only.is_empty()
        {
            return;
        }
        self.revision = self
            .revision
            .checked_add(1)
            .expect("stage revision exhausted");
        if self.enabled {
            if self.reports.len() == 64 {
                self.reports.pop_front();
            }
            self.reports.push_back((self.revision, changes.clone()));
        }
        for (_, callback) in &mut self.callbacks {
            callback(ChangeNotice {
                stage,
                store,
                revision: self.revision,
                changes,
            });
        }
    }

    pub(super) fn read<'a>(
        &'a self,
        cursor: &mut ChangeCursor,
    ) -> Result<impl Iterator<Item = &'a Changes> + use<'a>, ChangeHistoryError> {
        if !Arc::ptr_eq(&self.identity, &cursor.identity) {
            return Err(ChangeHistoryError::DifferentStage);
        }
        let previous = cursor.revision;
        cursor.revision = self.revision;
        if self
            .reports
            .front()
            .is_some_and(|(first, _)| previous < first - 1)
        {
            return Err(ChangeHistoryError::Expired);
        }
        let unread = usize::try_from(self.revision - previous).unwrap_or(usize::MAX);
        let skip = self.reports.len().saturating_sub(unread);
        Ok(self.reports.iter().skip(skip).map(|(_, changes)| changes))
    }
}
