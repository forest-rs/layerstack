// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA loader using the production `layerstack_usda` parser and emit pipeline.
//!
//! Routes through the real lexer → CST → AST → emit pipeline.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use layerstack::doc::{Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{
    AssetResolveError, AssetResolver, ExpressionAssetPath, InMemoryStore, ResolvedAsset,
    expression_asset_paths,
};

use layerstack_usda::diagnostic::{Diagnostic, Severity};
use layerstack_usda::emit;
use layerstack_usda::lower;
use layerstack_usda::parser::parse_cst;

#[derive(Debug)]
pub struct LoadedStage {
    pub store: InMemoryStore,
    pub root_layer: LayerId,
    pub layer_names: BTreeMap<LayerId, String>,
    /// Why OpenUSD's text parser rejects the entry layer, which then does
    /// not open (see `layerstack_usda::emit`); empty when it loads. A
    /// sublayer or arc target it rejects is unresolved instead, as OpenUSD
    /// cannot open it either.
    pub invalid: Vec<String>,
}

/// Loads a USDA file and all of its sublayers/references recursively,
/// producing a [`LoadedStage`] ready for composition.
pub fn load_entry_usda(entry: &Path) -> LoadedStage {
    let mut store = InMemoryStore::default();
    let root_dir = entry.parent().unwrap_or(Path::new(".")).to_path_buf();

    let mut resolver = FileResolver {
        root_dir: root_dir.clone(),
        next_layer_id: 1,
        by_path: BTreeMap::new(),
        layer_paths: BTreeMap::new(),
        layer_names: BTreeMap::new(),
        pending_layers: Vec::new(),
    };

    // Load the entry file.
    let (root_layer, invalid) = load_file(entry, &mut store, &mut resolver);

    // Insert any resolved layers produced during emit.
    while let Some(layer) = resolver.pending_layers.pop() {
        store.insert_layer(layer);
    }

    load_expression_assets(&mut store, root_layer, |store, path| {
        let resolved = resolver.resolve(
            &path.asset_path,
            Some(path.anchor),
            &mut store.tokens,
            &mut store.paths,
        );
        while let Some(layer) = resolver.pending_layers.pop() {
            store.insert_layer(layer);
        }
        resolved.ok()
    });

    LoadedStage {
        store,
        root_layer,
        layer_names: resolver.layer_names,
        invalid,
    }
}

/// Loads the layers that asset paths authored as variable expressions
/// evaluate to, with the variables of the layer stacks that reach them
/// ([`expression_asset_paths`]), until nothing new resolves: a loaded layer
/// may author expressions of its own.
///
/// `resolve` resolves one path, anchored to its layer, inserting any
/// layer it loads into the store.
pub(crate) fn load_expression_assets(
    store: &mut InMemoryStore,
    root: LayerId,
    mut resolve: impl FnMut(&mut InMemoryStore, &ExpressionAssetPath) -> Option<ResolvedAsset>,
) {
    let mut attempted = BTreeSet::new();
    loop {
        let mut loaded_any = false;
        for path in expression_asset_paths(store, root) {
            if !attempted.insert(path.clone()) {
                continue;
            }
            if let Some(resolved) = resolve(store, &path) {
                if let Some(layer) = resolved.layer {
                    store.insert_layer(layer);
                }
                store.insert_asset_layer(path.anchor, &path.asset_path, resolved.layer_id);
                loaded_any = true;
            }
        }
        if !loaded_any {
            break;
        }
    }
}

/// Loads only the layer stack structure (sublayers), ignoring prim contents.
///
/// Loads only the layer stack structure (sublayers), ignoring prim contents.
pub fn load_entry_usda_sublayers_only(entry: &Path) -> LoadedStage {
    // For sublayers-only mode, we still run the full pipeline but the effect
    // is the same — sublayer arcs are resolved, prims are populated but tests
    // only check layer stack structure.
    load_entry_usda(entry)
}

/// Loads the layer at `path`, with the reasons it is rejected (see
/// [`LoadedStage::invalid`]).
fn load_file(
    path: &Path,
    store: &mut InMemoryStore,
    resolver: &mut FileResolver,
) -> (LayerId, Vec<String>) {
    let canonical = path.to_path_buf();

    // Deduplication check.
    if let Some(id) = resolver.by_path.get(&canonical) {
        return (*id, Vec::new());
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

    // Read and parse.
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let cst = parse_cst(&source);
    if !cst.diagnostics.is_empty() {
        eprintln!(
            "CST diagnostics for {}: {:?}",
            path.display(),
            cst.diagnostics
        );
    }
    let ast_result = lower::lower(&cst.tree, &source);
    if !ast_result.diagnostics.is_empty() {
        eprintln!(
            "AST diagnostics for {}: {:?}",
            path.display(),
            ast_result.diagnostics
        );
    }

    // Emit.
    let emit_result = emit::emit(
        &ast_result.layer,
        layer_id,
        &mut store.tokens,
        &mut store.paths,
        resolver,
    );
    if !emit_result.diagnostics.is_empty() {
        eprintln!(
            "emit diagnostics for {}: {:?}",
            path.display(),
            emit_result.diagnostics
        );
    }

    // Insert resolved layers (from sublayer/reference resolution).
    for layer in emit_result.resolved_layers {
        store.insert_layer(layer);
    }

    let invalid = if emit_result.rejected {
        rejections(&emit_result.diagnostics)
    } else {
        Vec::new()
    };

    // Insert the emitted layer.
    store.insert_layer(emit_result.layer);

    (layer_id, invalid)
}

/// The messages of the error diagnostics of a rejected layer.
fn rejections(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.clone())
        .collect()
}

// ── File-based asset resolver ───────────────────────────────────────────

struct FileResolver {
    root_dir: PathBuf,
    next_layer_id: u64,
    by_path: BTreeMap<PathBuf, LayerId>,
    /// Maps layer ID → absolute file path (for resolving relative references).
    layer_paths: BTreeMap<LayerId, PathBuf>,
    layer_names: BTreeMap<LayerId, String>,
    /// Layers produced during asset resolution that need to be inserted
    /// into the store after emit completes (since we can't borrow the store
    /// during emit).
    pending_layers: Vec<Layer>,
}

impl AssetResolver for FileResolver {
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

        // A missing file is not found every time it is asked for; it is
        // never given a layer ID.
        if !resolved_path.is_file() {
            return Err(AssetResolveError::NotFound);
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

        // Read and parse.
        let source = std::fs::read_to_string(&resolved_path).map_err(|e| {
            AssetResolveError::LoadError(Arc::from(format!(
                "failed to read {}: {e}",
                resolved_path.display()
            )))
        })?;

        let cst = parse_cst(&source);
        let ast_result = lower::lower(&cst.tree, &source);

        let emit_result = emit::emit(&ast_result.layer, layer_id, tokens, paths, self);
        // OpenUSD cannot open a layer its text parser rejects, so the arc or
        // sublayer that names it does not resolve.
        if emit_result.rejected {
            self.by_path.remove(&resolved_path);
            self.layer_paths.remove(&layer_id);
            self.layer_names.remove(&layer_id);
            return Err(AssetResolveError::LoadError(Arc::from(format!(
                "{} is rejected: {}",
                resolved_path.display(),
                rejections(&emit_result.diagnostics).join("; ")
            ))));
        }

        // Collect resolved sub-layers.
        for layer in emit_result.resolved_layers {
            self.pending_layers.push(layer);
        }

        Ok(ResolvedAsset {
            layer_id,
            resolved_path: Arc::from(resolved_path.to_string_lossy().as_ref()),
            layer: Some(emit_result.layer),
        })
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.layer_names.get(&id).map(|s| s.as_str())
    }
}
