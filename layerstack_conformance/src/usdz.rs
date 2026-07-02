// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDZ loader for conformance testing.
//!
//! Reads `.usdz` package files through `layerstack_usdz::read_usdz` and
//! produces a [`LoadedStage`] ready for composition.

use std::collections::BTreeMap;
use std::path::Path;

use layerstack::doc::LayerId;
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, InMemoryStore, ResolvedAsset};

use crate::usda_real::LoadedStage;

/// Loads a USDZ file, producing a [`LoadedStage`] ready for composition.
pub fn load_entry_usdz(entry: &Path) -> LoadedStage {
    let mut store = InMemoryStore::default();

    let mut resolver = StubResolver;

    let data =
        std::fs::read(entry).unwrap_or_else(|e| panic!("failed to read {}: {e}", entry.display()));

    let layer_id = LayerId(1);
    let result = layerstack_usdz::read_usdz(
        &data,
        layer_id,
        &mut store.tokens,
        &mut store.paths,
        &mut resolver,
    )
    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", entry.display()));

    // Insert resolved layers.
    for layer in result.resolved_layers {
        store.insert_layer(layer);
    }
    store.insert_layer(result.layer);

    let mut layer_names = BTreeMap::new();
    layer_names.insert(
        layer_id,
        entry
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string(),
    );

    LoadedStage {
        store,
        root_layer: layer_id,
        layer_names,
    }
}

/// Stub resolver that doesn't resolve any assets outside the package.
struct StubResolver;

impl AssetResolver for StubResolver {
    fn resolve(
        &mut self,
        _asset_path: &str,
        _anchor: Option<LayerId>,
        _tokens: &mut TokenInterner,
        _paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}
