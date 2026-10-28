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
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}
pub(crate) fn scene(text: &str) -> (InMemoryStore, LiveStage) {
    let store = InMemoryStore::default();
    scene_in(store, text, LayerId(1))
}

/// An empty root layer, `LayerId(1)`, over `weak` as its sublayer,
/// `LayerId(2)`: authoring in the root keeps the weak layer's opinions.
#[allow(
    dead_code,
    reason = "the wind tunnel benches include this module without layered scenes"
)]
pub(crate) fn layered_scene(weak: &str) -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let mut root = layerstack::Layer::new(LayerId(1));
    root.sublayers_mut()
        .push(layerstack::SublayerEntry::new(LayerId(2)));
    store.insert_layer(root);
    let (mut store, _) = scene_in(store, weak, LayerId(2));
    let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    (store, live)
}

fn scene_in(mut store: InMemoryStore, text: &str, id: LayerId) -> (InMemoryStore, LiveStage) {
    let parsed = layerstack_usda::parser::parse(text);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        id,
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    );
    assert!(
        parsed.diagnostics.is_empty() && emitted.diagnostics.is_empty(),
        "{:?} {:?}",
        parsed.diagnostics,
        emitted.diagnostics
    );
    store.insert_layer(emitted.layer);
    let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
    let live = LiveStage::compose(
        &mut store,
        id,
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
