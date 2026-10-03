// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Materialized multi-layer scene loading probe for local USDA/USDC assets.
//! Usage: `stage_loading root.usd [repetitions] [audit_directory]`. Each run owns a fresh store
//! and canonical-path resolver; file caches remain warm. Textures are not
//! decoded. Per-layer timings include recursively imported dependencies.
//! Missing layers and importer diagnostics fail the probe. This host adapter
//! handles file-relative arcs, not resolver plugins or package identifiers.
//! With one repetition, an audit directory saves every imported layer as
//! `ID.usdc` and a tab-separated `layers.tsv` mapping IDs to source filenames.
//! Exporting happens after the reported load timings; source files are unchanged.

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, Layer, LayerId, PathInterner, ResolvedAsset,
    Stage, StageOptions, TokenInterner,
};
use std::{collections::BTreeMap, io::Write, path::PathBuf, sync::Arc, time::Instant};

#[derive(Default)]
struct Files {
    ids: BTreeMap<PathBuf, LayerId>,
    locations: BTreeMap<LayerId, Arc<str>>,
    pending: Vec<Layer>,
    bytes: usize,
    errors: Vec<AssetResolveError>,
}

impl Files {
    fn load_asset(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let base =
            anchor
                .and_then(|id| self.locations.get(&id))
                .map_or_else(PathBuf::new, |path| {
                    PathBuf::from(path.as_ref())
                        .parent()
                        .expect("canonical layer has a parent")
                        .to_path_buf()
                });
        let path = base
            .join(asset)
            .canonicalize()
            .map_err(|e| AssetResolveError::LoadError(format!("{asset}: {e}").into()))?;
        let location: Arc<str> = path
            .to_str()
            .ok_or_else(|| AssetResolveError::LoadError("non-UTF-8 asset path".into()))?
            .into();
        if let Some(&id) = self.ids.get(&path) {
            return Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: location,
                layer: None,
            });
        }
        let id = LayerId(u64::try_from(self.ids.len()).expect("layer count fits u64") + 1);
        self.ids.insert(path.clone(), id);
        self.locations.insert(id, location.clone());
        let start = Instant::now();
        let data = std::fs::read(&path)
            .map_err(|e| AssetResolveError::LoadError(format!("{}: {e}", path.display()).into()))?;
        self.bytes += data.len();
        let fail = |message: String| {
            AssetResolveError::LoadError(format!("{}: {message}", path.display()).into())
        };
        let layer = if data.starts_with(b"PXR-USDC") {
            let result = layerstack_usdc::read_usdc(&data, id, tokens, paths, self)
                .map_err(|e| fail(e.to_string()))?;
            if !result.diagnostics.is_empty() {
                return Err(fail(format!("{:?}", result.diagnostics)));
            }
            self.pending.extend(result.resolved_layers);
            result.layer
        } else {
            let source = std::str::from_utf8(&data).map_err(|e| fail(e.to_string()))?;
            let imported = layerstack_usda::read_usda(source, id, tokens, paths, self);
            if !imported.parse_diagnostics.is_empty() || !imported.lower_diagnostics.is_empty() {
                return Err(fail(format!(
                    "{:?} {:?}",
                    imported.parse_diagnostics, imported.lower_diagnostics
                )));
            }
            let result = imported.emitted;
            if !result.diagnostics.is_empty() {
                return Err(fail(format!("{:?}", result.diagnostics)));
            }
            self.pending.extend(result.resolved_layers);
            result.layer
        };
        println!(
            "layer id={} bytes={} inclusive_ms={:.3} file={}",
            id.0,
            data.len(),
            start.elapsed().as_secs_f64() * 1000.0,
            path.display()
        );
        Ok(ResolvedAsset {
            layer_id: id,
            resolved_path: location,
            layer: Some(layer),
        })
    }
}

impl AssetResolver for Files {
    fn resolve(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let result = self.load_asset(asset, anchor, tokens, paths);
        if let Err(error) = &result {
            self.errors.push(error.clone());
        }
        result
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.locations.get(&id).map(AsRef::as_ref)
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let root = args.get(1).expect("expected root USD path");
    let repeats: usize = args
        .get(2)
        .map_or(Ok(3), |s| s.parse())
        .expect("expected repetition count");
    assert!(repeats > 0, "at least one iteration is required");
    let audit = args.get(3).map(PathBuf::from);
    assert!(
        audit.is_none() || repeats == 1,
        "audit mode requires one run"
    );
    for run in 0..repeats {
        let mut store = InMemoryStore::default();
        let mut files = Files::default();
        let start = Instant::now();
        let result = files
            .resolve(root, None, &mut store.tokens, &mut store.paths)
            .expect("load all scene layers without diagnostics");
        assert!(files.errors.is_empty(), "{:?}", files.errors);
        let root = result.layer_id;
        store.insert_layer(result.layer.expect("fresh root layer"));
        for layer in files.pending.drain(..) {
            store.insert_layer(layer);
        }
        let import_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let stage = Stage::compose(&mut store, root, StageOptions::default());
        let compose_ms = start.elapsed().as_secs_f64() * 1000.0;
        assert!(
            stage.composition_errors().is_empty(),
            "{:?}",
            stage.composition_errors()
        );
        let root = store.path("/");
        let prims = stage.traverse(root).count();
        let instances = stage
            .traverse(root)
            .filter(|&p| stage.is_instance(p))
            .count();
        println!(
            "{{\"run\":{run},\"layers\":{},\"bytes\":{},\"prims\":{prims},\"instances\":{instances},\"import_ms\":{import_ms:.3},\"compose_ms\":{compose_ms:.3}}}",
            store.layers.len(),
            files.bytes
        );
        if let Some(directory) = &audit {
            std::fs::create_dir_all(directory).expect("create audit directory");
            let mut manifest =
                std::fs::File::create(directory.join("layers.tsv")).expect("create audit manifest");
            for (id, location) in &files.locations {
                assert!(
                    !location.contains(['\t', '\r', '\n']),
                    "manifest requires one path per line"
                );
                let bytes = layerstack_usdc::writer::save_layer(
                    &store.layers[id],
                    &store.tokens,
                    &store.paths,
                )
                .expect("save imported layer for field audit");
                std::fs::write(directory.join(format!("{}.usdc", id.0)), bytes)
                    .expect("write audit layer");
                writeln!(manifest, "{}\t{location}", id.0).expect("write audit manifest");
            }
        }
    }
}
