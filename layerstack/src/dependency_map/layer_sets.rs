// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Canonical immutable contributor sets, scoped to one dependency catalog.
use crate::{LayerId, PathId};
use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::ops::Deref;
use hashbrown::{HashMap, HashSet};

/// Physical sharing of retained prim-to-layer contributor sets.
/// Counts exclude reverse indexes, arc topology, cache keys and allocator overhead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayerDependencyStorage {
    /// Number of prims with recorded layer dependencies.
    pub prims: usize,
    /// Distinct retained contributor-set allocations.
    pub unique_layer_sets: usize,
    /// Layer references counted once per prim.
    pub logical_layer_references: usize,
    /// Layer references counted once per shared allocation.
    pub physical_layer_references: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LayerSets {
    by_prim: HashMap<PathId, Arc<Vec<LayerId>>>,
    // Weak<Vec<_>> keeps only the owner header after the last prim releases a
    // list; unlike Weak<[T]>, it releases the payload allocation immediately.
    // Reclaim dead keys above twice the current prim count (with a small floor).
    interned: HashMap<Vec<LayerId>, Weak<Vec<LayerId>>>,
}
impl PartialEq for LayerSets {
    fn eq(&self, other: &Self) -> bool {
        self.by_prim == other.by_prim
    }
}
impl Deref for LayerSets {
    type Target = HashMap<PathId, Arc<Vec<LayerId>>>;
    fn deref(&self) -> &Self::Target {
        &self.by_prim
    }
}
impl LayerSets {
    /// No sets, with room for `prims` prims'.
    pub(crate) fn with_capacity(prims: usize) -> Self {
        Self {
            by_prim: HashMap::with_capacity(prims),
            interned: HashMap::new(),
        }
    }

    /// Sets the layers of `prim` to `layers`, sorted and each once,
    /// sharing the storage of an equal set.
    pub(crate) fn insert(&mut self, prim: PathId, layers: &[LayerId]) {
        self.set(prim, layers, None);
    }
    fn prune(&mut self) {
        if self.interned.len() > self.by_prim.len().saturating_mul(2).max(16) {
            self.interned.retain(|_, weak| weak.strong_count() != 0);
        }
    }
    fn set(&mut self, prim: PathId, layers: &[LayerId], shared: Option<&Arc<Vec<LayerId>>>) {
        debug_assert!(
            layers.windows(2).all(|pair| pair[0] < pair[1]),
            "canonical sorted unique layers"
        );
        if layers.is_empty() {
            self.by_prim.remove(&prim);
            self.prune();
            return;
        }
        let owner = self
            .interned
            .get(layers)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let owner = shared.cloned().unwrap_or_else(|| Arc::new(layers.to_vec()));
                if let Some(weak) = self.interned.get_mut(layers) {
                    *weak = Arc::downgrade(&owner);
                } else {
                    self.interned
                        .insert(layers.to_vec(), Arc::downgrade(&owner));
                }
                owner
            });
        self.by_prim.insert(prim, owner);
        self.prune();
    }
    pub(crate) fn set_shared(&mut self, prim: PathId, layers: &Arc<Vec<LayerId>>) {
        self.set(prim, layers, Some(layers));
    }
    pub(crate) fn add(&mut self, prim: PathId, layer: LayerId) {
        let current = self
            .by_prim
            .get(&prim)
            .map_or(&[][..], |layers| layers.as_ref());
        let Err(at) = current.binary_search(&layer) else {
            return;
        };
        let mut layers = current.to_vec();
        layers.insert(at, layer);
        self.set(prim, &layers, None);
    }
    pub(crate) fn remove(&mut self, prim: &PathId) -> Option<Arc<Vec<LayerId>>> {
        let old = self.by_prim.remove(prim);
        self.prune();
        old
    }
    pub(crate) fn storage(&self) -> LayerDependencyStorage {
        let mut storage = LayerDependencyStorage {
            prims: self.by_prim.len(),
            ..Default::default()
        };
        let mut seen = HashSet::new();
        for layers in self.by_prim.values() {
            storage.logical_layer_references += layers.len();
            if seen.insert(Arc::as_ptr(layers)) {
                storage.unique_layer_sets += 1;
                storage.physical_layer_references += layers.len();
            }
        }
        storage
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn equal_contributors_share_storage_and_mutation_preserves_old_snapshots() {
        let mut sets = LayerSets::default();
        for i in 0..1000 {
            sets.add(PathId::from_raw(i), LayerId(1));
            sets.add(PathId::from_raw(i), LayerId(2));
        }
        let storage = sets.storage();
        assert_eq!(storage.unique_layer_sets, 1);
        assert_eq!(storage.physical_layer_references, 2);
        assert_eq!(storage.logical_layer_references, 2000);
        let old = sets.clone();
        sets.add(PathId::from_raw(1), LayerId(3));
        assert_eq!(
            old[&PathId::from_raw(1)].as_ref(),
            &[LayerId(1), LayerId(2)]
        );
        assert_eq!(
            sets[&PathId::from_raw(1)].as_ref(),
            &[LayerId(1), LayerId(2), LayerId(3)]
        );
        let borrowed = Arc::new(vec![LayerId(1), LayerId(2)]);
        sets.set_shared(PathId::from_raw(1001), &borrowed);
        assert!(Arc::ptr_eq(
            &sets[&PathId::from_raw(0)],
            &sets[&PathId::from_raw(1001)]
        ));
        assert_eq!(sets.storage().unique_layer_sets, 2);
    }
    #[test]
    fn replacing_one_prims_contributors_does_not_accumulate_dead_cache_keys() {
        let mut sets = LayerSets::default();
        let prim = PathId::from_raw(1);
        for i in 0..10000 {
            sets.set(prim, &[LayerId(i)], None);
            assert!(sets.interned.len() <= 16);
        }
        assert!(sets.interned.len() <= 16);
        assert_eq!(sets[&prim].as_ref(), &[LayerId(9999)]);
        sets.remove(&prim);
        assert_eq!(sets.storage(), LayerDependencyStorage::default());
    }
}
