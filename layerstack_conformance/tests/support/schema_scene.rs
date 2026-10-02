// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared scene loader for schema behavior fixtures without external assets.
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, LiveStage, PathInterner,
    ResolvedAsset, StageOptions, TokenInterner,
};
use std::sync::Arc;
struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}
pub(crate) fn scene(text: &str) -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let parsed = layerstack_usda::parser::parse(text);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(
        parsed.diagnostics.is_empty() && emitted.diagnostics.is_empty(),
        "{:?} {:?}",
        parsed.diagnostics,
        emitted.diagnostics
    );
    store.insert_layer(emitted.layer);
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    assert!(
        live.stage().composition_errors().is_empty(),
        "{:?}",
        live.stage().composition_errors()
    );
    (store, live)
}
