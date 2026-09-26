// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDZ loader for conformance testing.
//!
//! Reads `.usdz` packages through `layerstack_usdz::read_usdz` and
//! produces a [`LoadedStage`] ready for composition.

use std::path::Path;

use layerstack::{AssetResolver, InMemoryStore, LayerStore};

use crate::usda_real::{FileResolver, LoadedStage};

/// Loads a USDZ file, producing a [`LoadedStage`] ready for composition.
/// Asset paths that name no member resolve to `.usda` files beside it.
pub fn load_entry_usdz(entry: &Path) -> LoadedStage {
    let data =
        std::fs::read(entry).unwrap_or_else(|e| panic!("failed to read {}: {e}", entry.display()));
    load_usdz(&data, entry.parent().unwrap_or(Path::new(".")))
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", entry.display()))
}

/// Loads a USDZ package from its bytes, as if it lay in `directory`,
/// producing a [`LoadedStage`] ready for composition: the root layer and
/// every layer it loads are in the store. Asset paths that name no member
/// resolve to `.usda` files under `directory`, with IDs from the same
/// resolver as the members'.
///
/// A member is named by its path inside the package, and a file outside
/// it by its path relative to `directory`.
pub fn load_usdz(data: &[u8], directory: &Path) -> Result<LoadedStage, layerstack_usdz::UsdzError> {
    let mut store = InMemoryStore::default();
    let mut resolver = FileResolver::new(directory.to_path_buf());
    let layer_id = resolver.allocate_layer_id().expect("allocates");
    let result = layerstack_usdz::read_usdz(
        data,
        layer_id,
        &mut store.tokens,
        &mut store.paths,
        &mut resolver,
    )?;

    let mut layer_names = resolver.layer_names;
    for (id, path) in result.member_paths {
        layer_names.insert(id, path.to_string());
    }
    let layers = result
        .resolved_layers
        .into_iter()
        .chain(resolver.pending_layers)
        .chain([result.layer]);
    for layer in layers {
        assert!(
            store.layer(layer.id).is_none(),
            "two layers share {:?}",
            layer.id
        );
        store.insert_layer(layer);
    }

    Ok(LoadedStage {
        store,
        root_layer: layer_id,
        layer_names,
        invalid: Vec::new(),
    })
}
