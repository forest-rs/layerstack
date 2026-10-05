// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Collision-group policy snapshots, without simulation or collider evaluation.
//!
//! OpenUSD: `UsdPhysicsCollisionGroup::ComputeCollisionGroupTable`.
//! Relationship and metadata composition follow AOUSD Core §12; default
//! traversal follows §11.3.3 and defined, non-abstract prim ancestry.
use crate::{PrimView, Scene, usd::CollectionApi, usd_physics::PhysicsCollisionGroup};
use alloc::{sync::Arc, vec, vec::Vec};
use layerstack::{HashMap, HashSet, PathId, TargetPath};

/// A malformed filtering relationship or unrepresentable table size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollisionGroupError {
    /// A filter names a property or a group absent from default traversal.
    InvalidFilteredGroup {
        /// The group authoring the invalid filter.
        group: PathId,
        /// The invalid relationship target.
        target: TargetPath,
    },
    /// The number of merged pairs cannot fit in addressable storage.
    TooManyGroups,
}
impl core::fmt::Display for CollisionGroupError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid collision groups: {self:?}")
    }
}
impl core::error::Error for CollisionGroupError {}

/// Symmetric collision policy resolved once from a scene. Recompute after
/// group/filter edits; this snapshot does not observe edits automatically.
/// Merged groups share packed rows instead of duplicating every original pair.
/// Unknown group paths or out-of-range indices collide by default, as in USD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollisionGroupTable {
    groups: Vec<PathId>,
    indices: HashMap<PathId, usize>,
    merged: Vec<usize>,
    merged_count: usize,
    enabled: Vec<bool>,
}
impl CollisionGroupTable {
    /// Group roots in default stage traversal order; merged roots stay distinct.
    #[must_use]
    pub fn groups(&self) -> &[PathId] {
        &self.groups
    }
    /// Number of distinct groups after applying authored merge names.
    #[must_use]
    pub fn merged_group_count(&self) -> usize {
        self.merged_count
    }
    /// Stored unordered pairs, including self-pairs, after merging.
    #[must_use]
    pub fn stored_pair_count(&self) -> usize {
        self.enabled.len()
    }
    /// Whether two original group roots may collide. Unknown paths pass.
    #[must_use]
    pub fn is_collision_enabled(&self, a: PathId, b: PathId) -> bool {
        self.indices
            .get(&a)
            .zip(self.indices.get(&b))
            .is_none_or(|(&a, &b)| self.is_collision_enabled_at(a, b))
    }
    /// Whether two entries in `groups` may collide. Out-of-range indices pass.
    #[must_use]
    pub fn is_collision_enabled_at(&self, a: usize, b: usize) -> bool {
        self.merged
            .get(a)
            .zip(self.merged.get(b))
            .is_none_or(|(&a, &b)| self.enabled[slot(a, b, self.merged_count)])
    }
}
fn slot(a: usize, b: usize, count: usize) -> usize {
    let (a, b) = (a.min(b), a.max(b));
    // count*(count+1) was checked before allocation; each product fits.
    a * count - a * (a + 1) / 2 + b
}
impl<'a> PhysicsCollisionGroup<'a> {
    /// The schema's built-in `colliders` collection, including ordinary
    /// collection membership expressions and included/excluded relationships.
    #[must_use]
    pub fn colliders_collection(&self) -> CollectionApi<'a> {
        CollectionApi::from_view(PrimView::new(self.scene(), self.path()), "colliders")
    }
}
/// Resolves authored group filters, inversion and merge names at default time.
/// A filter on either group disables the pair in both directions. Merged
/// groups contribute all their filters, including authored empty merge names
/// (OpenUSD 26.8 behavior). Inversion allows only the listed merged groups.
/// Default traversal excludes abstract/undefined subtrees and instance proxies.
/// Invalid targets return an error without exposing a partial table.
#[doc(alias = "UsdPhysicsCollisionGroup::ComputeCollisionGroupTable")]
#[doc(alias = "ComputeCollisionGroupTable")]
pub fn compute_collision_group_table(
    scene: &Scene<'_>,
) -> Result<CollisionGroupTable, CollisionGroupError> {
    let mut pending: Vec<_> = scene.root().into_iter().collect();
    let mut groups = Vec::new();
    while let Some(path) = pending.pop() {
        let root = scene.parent(path).is_none();
        if !root
            && (!scene.stage().is_defined(path, scene.store())
                || scene.stage().is_abstract(path, scene.store()))
        {
            continue;
        }
        if let Some(group) = PhysicsCollisionGroup::new(scene, path) {
            groups.push(group);
        }
        if !scene.stage().is_instance(path) {
            pending.extend(
                scene
                    .stage()
                    .children_of(path)
                    .unwrap_or_default()
                    .iter()
                    .rev()
                    .copied(),
            );
        }
    }
    let mut names: HashMap<Arc<str>, usize> = HashMap::new();
    let mut merged = Vec::new();
    let mut indices = HashMap::new();
    let mut merged_count = 0;
    let key = scene
        .store()
        .tokens()
        .lookup(PhysicsCollisionGroup::MERGE_GROUP_NAME);
    for (i, group) in groups.iter().enumerate() {
        indices.insert(group.path(), i);
        let authored = key.is_some_and(|key| {
            scene
                .stage()
                .authored_property_names(group.path(), scene.store())
                .contains(&key)
        });
        let name = authored.then(|| group.merge_group_name().unwrap_or_else(|| Arc::from("")));
        let index = if let Some(name) = name {
            *names.entry(name).or_insert_with(|| {
                let i = merged_count;
                merged_count += 1;
                i
            })
        } else {
            let i = merged_count;
            merged_count += 1;
            i
        };
        merged.push(index);
    }
    let pairs = merged_count
        .checked_add(1)
        .and_then(|n| n.checked_mul(merged_count))
        .map(|n| n / 2)
        .ok_or(CollisionGroupError::TooManyGroups)?;
    let mut enabled = vec![true; pairs];
    for (i, group) in groups.iter().enumerate() {
        let mut targets = HashSet::new();
        for target in group.filtered_groups() {
            let index = match target {
                TargetPath::Prim(path) => indices.get(&path).copied(),
                TargetPath::Property(_) => None,
            };
            let Some(index) = index else {
                return Err(CollisionGroupError::InvalidFilteredGroup {
                    group: group.path(),
                    target,
                });
            };
            targets.insert(merged[index]);
        }
        let inverted = group.invert_filtered_groups().unwrap_or(false);
        for other in 0..merged_count {
            if targets.contains(&other) != inverted {
                enabled[slot(merged[i], other, merged_count)] = false;
            }
        }
    }
    Ok(CollisionGroupTable {
        groups: groups.iter().map(|g| g.path()).collect(),
        indices,
        merged,
        merged_count,
        enabled,
    })
}
