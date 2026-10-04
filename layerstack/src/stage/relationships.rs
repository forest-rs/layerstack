// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AOUSD Core §12.4; OpenUSD `UsdRelationship::GetForwardedTargets`.
use super::*;
use crate::{PropertyKind, TargetPath};
use alloc::vec;

/// Work performed by one retained forwarded-relationship query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelationshipQueryWork {
    /// Target resolutions, including missing or incompatible relationships.
    pub evaluations: u64,
    /// Reads reused from unchanged forwarding dependencies.
    pub cache_hits: u64,
}

/// Retained terminal targets and every prim visited during forwarding.
/// Missing property targets are dependencies too: creation as a relationship
/// must invalidate a previously terminal path. Cycles retain all visited prims.
/// Store-local paths must stay with their original interners. No notices are
/// needed; each read checks immutable prim identities. AOUSD Core §12.4.
#[derive(Clone, Debug)]
pub struct RelationshipQuery {
    path: PropertyPath,
    cached: Option<Option<Vec<TargetPath>>>,
    snapshots: Vec<PrimSnapshot>,
    work: RelationshipQueryWork,
}
impl RelationshipQuery {
    /// Binds a concrete relationship path; resolution happens on first read.
    pub fn new(path: PropertyPath) -> Self {
        Self {
            path,
            cached: None,
            snapshots: Vec::new(),
            work: RelationshipQueryWork::default(),
        }
    }
    /// Whether all visited forwarding prims still have their captured records.
    pub fn is_current(&self, stage: &Stage) -> bool {
        self.cached.is_some() && self.snapshots.iter().all(|s| s.is_current(stage))
    }
    /// Resolves forwarded targets or returns their retained value. `None` means
    /// the root is absent or not a relationship; an empty vector is a relationship
    /// with no terminal targets. Returning a vector copies only the target paths.
    pub fn get(&mut self, stage: &Stage) -> Option<Vec<TargetPath>> {
        if self.is_current(stage) {
            self.work.cache_hits = self.work.cache_hits.saturating_add(1);
        } else {
            let (targets, snapshots) = stage.forwarded_with_snapshots(self.path, true);
            self.snapshots = snapshots;
            self.cached = Some(
                (stage.property_kind(self.path) == Some(PropertyKind::Relationship))
                    .then_some(targets),
            );
            self.work.evaluations = self.work.evaluations.saturating_add(1);
        }
        self.cached
            .as_ref()
            .expect("resolved relationship query")
            .clone()
    }
    /// Drops retained dependencies/targets, preserving cumulative work counters.
    pub fn clear(&mut self) {
        self.cached = None;
        self.snapshots.clear();
    }
    /// Cumulative resolution and reuse work.
    pub fn work(&self) -> RelationshipQueryWork {
        self.work
    }
}
impl Stage {
    /// Composed authored or schema-defined property kind.
    pub fn property_kind(&self, path: PropertyPath) -> Option<PropertyKind> {
        self.property_definition_ref(path.prim_path(), path.property())
            .map(|d| d.kind)
            .or_else(|| {
                self.resolve_property_declaration(path.prim_path(), path.property())
                    .map(|d| d.kind)
            })
    }
    /// Ordered, deduplicated relationship targets with recursive forwarding.
    /// Cycles contribute no terminal target along the cyclic edge. Missing prim
    /// and attribute targets remain terminal paths, matching OpenUSD.
    pub fn forwarded_relationship_targets(&self, root: PropertyPath) -> Vec<TargetPath> {
        self.forwarded_with_snapshots(root, false).0
    }

    fn forwarded_with_snapshots(
        &self,
        root: PropertyPath,
        retain: bool,
    ) -> (Vec<TargetPath>, Vec<PrimSnapshot>) {
        let mut snapshots = if retain {
            vec![self.prim_snapshot(root.prim_path())]
        } else {
            Vec::new()
        };
        if self.property_kind(root) != Some(PropertyKind::Relationship) {
            return (Vec::new(), snapshots);
        }
        let mut pending = alloc::vec![TargetPath::Property(root)];
        let mut seen = HashSet::new();
        let mut unique = HashSet::new();
        let mut targets = Vec::new();
        let mut prims = HashSet::new();
        if retain {
            prims.insert(root.prim_path());
        }
        while let Some(target) = pending.pop() {
            if let TargetPath::Property(path) = target
                && retain
                && prims.insert(path.prim_path())
            {
                snapshots.push(self.prim_snapshot(path.prim_path()));
            }
            if let TargetPath::Property(path) = target
                && self.property_kind(path) == Some(PropertyKind::Relationship)
            {
                if seen.insert(path)
                    && let Some(resolved) = self.resolve_target_list_path(path)
                {
                    pending.extend(resolved.value.into_iter().rev());
                }
            } else if unique.insert(target) {
                targets.push(target);
            }
        }
        (targets, snapshots)
    }
}

#[cfg(test)]
mod tests;
