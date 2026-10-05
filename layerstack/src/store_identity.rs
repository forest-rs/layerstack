// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Affinity of the shared token and path domains used by a layer store.

use alloc::sync::Arc;

/// Retained identity of a store's token and path domains.
///
/// Moving a store or interning more names preserves affinity. Replacing either
/// interner changes it. Adapters exposing the same two interners share affinity;
/// their different layer contents must still be checked through composed queries.
/// This is process-local evidence, not a serialized identifier or content hash.
#[derive(Clone)]
pub struct StoreIdentity {
    pub(crate) tokens: Arc<()>,
    pub(crate) paths: Arc<()>,
}
impl PartialEq for StoreIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.tokens, &other.tokens) && Arc::ptr_eq(&self.paths, &other.paths)
    }
}
impl Eq for StoreIdentity {}
impl core::fmt::Debug for StoreIdentity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StoreIdentity").finish_non_exhaustive()
    }
}
#[cfg(test)]
mod tests {
    use crate::{InMemoryStore, LayerStore, PathInterner, TokenInterner};
    #[test]
    fn snapshot_preserves_existing_ids_without_aliasing_future_domains() {
        let mut store = InMemoryStore::default();
        let path = store.path("/Original");
        let mut snapshot = store.snapshot();
        assert_eq!(snapshot.paths.display(path, &snapshot.tokens), "/Original");
        assert_ne!(snapshot.identity(), store.identity());
        let future = store.path("/PublishedOnly");
        let other = snapshot.path("/CandidateOnly");
        assert_eq!(
            future, other,
            "independent domains can allocate the same numeric ID"
        );
        assert_eq!(store.paths.display(future, &store.tokens), "/PublishedOnly");
        assert_eq!(
            snapshot.paths.display(other, &snapshot.tokens),
            "/CandidateOnly"
        );
        assert!(store.tokens.lookup("CandidateOnly").is_none());
    }
    #[test]
    fn affinity_survives_moves_and_interning_but_not_domain_replacement() {
        let mut store = InMemoryStore::default();
        let identity = store.identity();
        store.path("/New");
        let mut moved = store;
        assert_eq!(identity, moved.identity());
        assert_ne!(identity, InMemoryStore::default().identity());
        moved.paths = PathInterner::default();
        assert_ne!(identity, moved.identity());
        let before = moved.identity();
        moved.tokens = TokenInterner::default();
        assert_ne!(before, moved.identity());
    }
    #[test]
    fn retained_stage_refreshes_affinity_when_snapshot_generations_are_unchanged() {
        use crate::{Layer, LayerId, LiveStage, PrimSpec, StageOptions};
        let mut store = InMemoryStore::default();
        let path = store.path("/Root");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(path, PrimSpec::def());
        store.insert_layer(layer);
        let mut stage = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let mut cursor = stage.change_cursor();
        let mut replacement = store.snapshot();
        stage.synchronize(&mut replacement);
        assert_eq!(
            stage.stage().store_identity(),
            Some(&replacement.identity())
        );
        assert!(stage.stage().has_prim(path));
        assert_eq!(stage.changes_since(&mut cursor).unwrap().count(), 1);
        stage.synchronize(&mut replacement);
        assert_eq!(stage.changes_since(&mut cursor).unwrap().count(), 0);
    }
}
