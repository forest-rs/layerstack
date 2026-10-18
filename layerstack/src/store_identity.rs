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
        self.shares_tokens(other) && self.shares_paths(other)
    }
}
impl Eq for StoreIdentity {}
impl StoreIdentity {
    /// Whether both identities have the same token domain.
    pub(crate) fn shares_tokens(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.tokens, &other.tokens)
    }
    /// Whether both identities have the same path domain.
    pub(crate) fn shares_paths(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.paths, &other.paths)
    }
    /// Says which of `other`'s interners differ from these, for diagnostics.
    pub(crate) fn differing_domains(&self, other: &Self) -> &'static str {
        match (self.shares_tokens(other), self.shares_paths(other)) {
            (false, false) => "token and path interners differ",
            (false, true) => "token interner differs",
            (true, false) => "path interner differs",
            (true, true) => "interners are the same",
        }
    }
}
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
        let store = InMemoryStore::default();
        let path = store.path("/Original");
        let snapshot = store.snapshot();
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
        let store = InMemoryStore::default();
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
    fn apply_rejects_a_foreign_store_and_changes_nothing() {
        use crate::{
            EditError, EditTarget, Layer, LayerId, LiveStage, PrimSpec, Specifier, StageOptions,
            Transaction,
        };
        use alloc::{string::ToString, vec::Vec};
        let scene = || {
            let mut store = InMemoryStore::default();
            let mut layer = Layer::new(LayerId(1));
            layer.insert_prim(store.path("/Rock"), PrimSpec::def());
            store.insert_layer(layer);
            store
        };
        let mut store = scene();
        let mut stage = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let mut cursor = stage.change_cursor();
        let mut foreign = scene();
        let mut txn = Transaction::new();
        let fern = store.path("/Fern");
        txn.create_prim(
            EditTarget::for_layer(LayerId(1)).prim(fern),
            Specifier::Def,
            None,
        );
        let generations = |store: &InMemoryStore| store.layers[&LayerId(1)].generation();
        let (before, foreign_before) = (generations(&store), generations(&foreign));
        let prims: Vec<_> = stage.stage().prim_paths().collect();

        let error = stage.apply(&mut foreign, &txn).unwrap_err();
        assert_eq!(
            error,
            EditError::ForeignStore {
                stage: store.identity(),
                store: foreign.identity(),
            }
        );
        assert_eq!(
            error.to_string(),
            "the store is not the one the stage was composed from: its token and path \
             interners differ; synchronize the stage against it first if it replaces that store"
        );
        assert_eq!(stage.stage().prim_paths().collect::<Vec<_>>(), prims);
        assert_eq!(stage.stage().store_identity(), Some(&store.identity()));
        assert_eq!(
            (generations(&store), generations(&foreign)),
            (before, foreign_before)
        );
        assert_eq!(stage.changes_since(&mut cursor).unwrap().count(), 0);
        assert!(!stage.recomposition_work().full_rebuild);

        // A snapshot published in the store's place is another store until
        // the stage is synchronized against it, which rebinds the stage.
        let mut snapshot = store.snapshot();
        assert!(matches!(
            stage.apply(&mut snapshot, &txn),
            Err(EditError::ForeignStore { .. })
        ));
        stage.synchronize(&mut snapshot);
        assert!(stage.recomposition_work().full_rebuild);
        stage.apply(&mut snapshot, &txn).unwrap();
        assert!(stage.stage().has_prim(fern));
        assert!(matches!(
            stage.apply(&mut store, &txn),
            Err(EditError::ForeignStore { .. })
        ));
    }
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "the store is not the one the stage was composed from")]
    fn viewing_a_stage_through_a_foreign_store_panics_in_debug_builds() {
        use crate::{Layer, LayerId, Stage, StageOptions};
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(LayerId(1)));
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let foreign = InMemoryStore::default();
        let _ = stage.view(&foreign);
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
