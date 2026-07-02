// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDC loader for conformance testing.
//!
//! Reads `.usdc` binary files through `layerstack_usdc::read_usdc` and
//! produces a [`LoadedStage`] ready for composition, analogous to
//! [`usda_real::load_entry_usda`](crate::usda_real::load_entry_usda).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use layerstack::doc::{Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, InMemoryStore, ResolvedAsset};

use crate::usda_real::LoadedStage;

/// Loads a USDC file and all of its sublayers/references recursively,
/// producing a [`LoadedStage`] ready for composition.
pub fn load_entry_usdc(entry: &Path) -> LoadedStage {
    let mut store = InMemoryStore::default();
    let root_dir = entry.parent().unwrap_or(Path::new(".")).to_path_buf();

    let mut resolver = UsdcFileResolver {
        root_dir: root_dir.clone(),
        next_layer_id: 1,
        by_path: BTreeMap::new(),
        layer_paths: BTreeMap::new(),
        layer_names: BTreeMap::new(),
        pending_layers: Vec::new(),
    };

    // Load the entry file.
    let root_layer = load_usdc_file(entry, &mut store, &mut resolver);

    // Insert any resolved layers produced during assembly.
    while let Some(layer) = resolver.pending_layers.pop() {
        store.insert_layer(layer);
    }

    LoadedStage {
        store,
        root_layer,
        layer_names: resolver.layer_names,
    }
}

fn load_usdc_file(
    path: &Path,
    store: &mut InMemoryStore,
    resolver: &mut UsdcFileResolver,
) -> LayerId {
    let canonical = path.to_path_buf();

    // Deduplication check.
    if let Some(id) = resolver.by_path.get(&canonical) {
        return *id;
    }

    // Assign a new layer ID.
    let layer_id = LayerId(resolver.next_layer_id);
    resolver.next_layer_id += 1;
    resolver.by_path.insert(canonical.clone(), layer_id);
    resolver.layer_paths.insert(layer_id, canonical.clone());

    // Compute relative name.
    let relative = canonical
        .strip_prefix(&resolver.root_dir)
        .unwrap_or(canonical.as_path())
        .to_string_lossy()
        .replace('\\', "/");
    resolver.layer_names.insert(layer_id, relative);

    // Read binary data.
    let data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

    // Parse through the full USDC pipeline.
    let result = layerstack_usdc::read_usdc(
        &data,
        layer_id,
        &mut store.tokens,
        &mut store.paths,
        resolver,
    )
    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));

    // Insert resolved layers (from sublayer/reference resolution).
    for layer in result.resolved_layers {
        store.insert_layer(layer);
    }

    // Insert the assembled layer.
    store.insert_layer(result.layer);

    layer_id
}

// ── File-based asset resolver for USDC ──────────────────────────────────

struct UsdcFileResolver {
    root_dir: PathBuf,
    next_layer_id: u64,
    by_path: BTreeMap<PathBuf, LayerId>,
    /// Maps layer ID → absolute file path (for resolving relative references).
    layer_paths: BTreeMap<LayerId, PathBuf>,
    layer_names: BTreeMap<LayerId, String>,
    /// Layers produced during asset resolution that need to be inserted
    /// into the store after assembly completes.
    pending_layers: Vec<Layer>,
}

impl AssetResolver for UsdcFileResolver {
    fn resolve(
        &mut self,
        asset_path: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        // Resolve relative paths from the anchor layer's directory (or root).
        let base_dir = anchor
            .and_then(|id| self.layer_paths.get(&id))
            .and_then(|p| p.parent())
            .unwrap_or(&self.root_dir);
        let resolved_path = base_dir.join(asset_path.trim_start_matches("./"));

        // Deduplication.
        if let Some(id) = self.by_path.get(&resolved_path) {
            return Ok(ResolvedAsset {
                layer_id: *id,
                resolved_path: Arc::from(resolved_path.to_string_lossy().as_ref()),
                layer: None,
            });
        }

        let layer_id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        self.by_path.insert(resolved_path.clone(), layer_id);
        self.layer_paths.insert(layer_id, resolved_path.clone());

        // Compute relative name.
        let relative = resolved_path
            .strip_prefix(&self.root_dir)
            .unwrap_or(resolved_path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        self.layer_names.insert(layer_id, relative);

        // Determine file type from extension and parse accordingly.
        let ext = resolved_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        match ext {
            "usdc" => {
                let data = std::fs::read(&resolved_path).map_err(|e| {
                    AssetResolveError::LoadError(Arc::from(format!(
                        "failed to read {}: {e}",
                        resolved_path.display()
                    )))
                })?;
                let result = layerstack_usdc::read_usdc(&data, layer_id, tokens, paths, self)
                    .map_err(|e| {
                        AssetResolveError::LoadError(Arc::from(format!(
                            "failed to parse {}: {e}",
                            resolved_path.display()
                        )))
                    })?;
                for layer in result.resolved_layers {
                    self.pending_layers.push(layer);
                }
                Ok(ResolvedAsset {
                    layer_id,
                    resolved_path: Arc::from(resolved_path.to_string_lossy().as_ref()),
                    layer: Some(result.layer),
                })
            }
            _ => {
                // For non-USDC files (e.g. USDA references), return a stub.
                Err(AssetResolveError::LoadError(Arc::from(format!(
                    "unsupported file format for USDC resolver: {}",
                    resolved_path.display()
                ))))
            }
        }
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.layer_names.get(&id).map(|s| s.as_str())
    }
}
