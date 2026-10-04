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
}
