// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export a self-contained USD layer as USDC and localized USDZ for audits.
//! Usage: `scene_roundtrip input.usda output_directory [layer-only]`.
//! USDC and USDZ input are also accepted; `layer-only` exports the root
//! without collecting media for another package.
//! Texture bytes are copied without decoding. This host adapter interprets
//! Windows path separators explicitly; it does not change the source layer.

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, LayerStore, PathInterner,
    ResolvedAsset, TokenInterner,
};
use layerstack_usdz::localize::{LocalizationError, LocalizationTarget, localize_asset};
use std::{collections::BTreeMap, error::Error, path::PathBuf, sync::Arc, time::Instant};

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
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    let input = PathBuf::from(args.get(1).ok_or("expected input USDA path")?);
    let output = PathBuf::from(args.get(2).ok_or("expected output directory")?);
    std::fs::create_dir_all(&output)?;
    let data = std::fs::read(&input)?;
    let mut store = InMemoryStore::default();
    let layer = if data.starts_with(b"PXR-USDC") {
        let result = layerstack_usdc::read_usdc(
            &data,
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
            &mut NoAssets,
        )?;
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(
            result.resolved_layers.is_empty(),
            "self-contained input layer"
        );
        result.layer
    } else if data.starts_with(b"PK") {
        let result = layerstack_usdz::read_usdz(
            &data,
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
            &mut NoAssets,
        )?;
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(
            result.resolved_layers.is_empty(),
            "self-contained input layer"
        );
        result.layer
    } else {
        let source = std::str::from_utf8(&data)?;
        let imported = layerstack_usda::read_usda(
            source,
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
            &mut NoAssets,
        );
        assert!(
            imported.parse_diagnostics.is_empty(),
            "{:?}",
            imported.parse_diagnostics
        );
        assert!(
            imported.lower_diagnostics.is_empty(),
            "{:?}",
            imported.lower_diagnostics
        );
        let emitted = imported.emitted;
        assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
        assert!(
            emitted.resolved_layers.is_empty(),
            "self-contained input layer"
        );
        emitted.layer
    };
    store.insert_layer(layer);
    drop(data);
    let start = Instant::now();
    let crate_bytes = layerstack_usdc::writer::save_layer(
        &store.layers[&LayerId(1)],
        store.tokens(),
        store.paths(),
    )?;
    std::fs::write(output.join("raw.usdc"), &crate_bytes)?;
    println!(
        "USDC: {} bytes; {:.3}s",
        crate_bytes.len(),
        start.elapsed().as_secs_f64()
    );
    drop(crate_bytes);
    if args.get(3).is_some_and(|arg| arg == "layer-only") {
        return Ok(());
    }
    let mut cache: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
    let parent = input.parent().ok_or("input has no parent")?;
    let start = Instant::now();
    let plan = localize_asset(&store, LayerId(1), "scene.usdc", |dependency| {
        let fail = |reason: Arc<str>| LocalizationError::Resolution {
            dependency: Box::new(dependency.clone()),
            reason,
        };
        if dependency.layer_hint.is_some() {
            return Err(fail(
                "external composition layers are unsupported by this probe".into(),
            ));
        }
        let member = dependency.identifier.replace('\\', "/");
        let data = if let Some(data) = cache.get(&member) {
            data.clone()
        } else {
            let bytes: Arc<[u8]> = std::fs::read(parent.join(&member))
                .map_err(|e| fail(Arc::<str>::from(e.to_string())))?
                .into();
            cache.insert(member.clone(), bytes.clone());
            bytes
        };
        Ok(LocalizationTarget::asset(member, data))
    })?;
    let package = plan.write_usdz(store.tokens(), store.paths())?;
    std::fs::write(output.join("scene.usdz"), &package)?;
    println!(
        "USDZ: {} bytes; {} assets; {:.3}s",
        package.len(),
        plan.assets.len(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
