// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Eager or retained USDC/USDZ loading probe; textures are not decoded.
//! Usage: `binary_loading file.usdc 5` (or `file.usdz`). Every iteration
//! owns a fresh input buffer, store and stage; filesystem caches remain warm.

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset, Stage,
    StageOptions, TokenInterner,
};
use std::{sync::Arc, time::Instant};
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
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let file = args.get(1).expect("expected binary USD path");
    let repeats: usize = args
        .get(2)
        .map_or(Ok(5), |s| s.parse())
        .expect("expected repetition count");
    let mode = args.get(3).map_or("eager", String::as_str);
    assert!(
        matches!(mode, "eager" | "retained" | "retained-all"),
        "unknown mode"
    );
    for run in 0..repeats {
        let start = Instant::now();
        let data = std::fs::read(file).expect("read USD bytes");
        let read_ms = start.elapsed().as_secs_f64() * 1000.;
        let bytes = data.len();
        let mut store = InMemoryStore::default();
        let start = Instant::now();
        let mut retained = None;
        let layer = if data.starts_with(b"PXR-USDC") && mode != "eager" {
            let result = layerstack_usdc::read_usdc_lazy(
                Arc::from(data.as_slice()),
                LayerId(1),
                &mut store.tokens,
                &mut store.paths,
                &mut NoAssets,
            )
            .expect("retained USDC");
            assert!(
                result.assembled.diagnostics.is_empty(),
                "{:?}",
                result.assembled.diagnostics
            );
            assert!(
                result.assembled.resolved_layers.is_empty(),
                "self-contained layer"
            );
            retained = Some(result.values);
            result.assembled.layer
        } else if data.starts_with(b"PXR-USDC") {
            let result = layerstack_usdc::read_usdc(
                &data,
                LayerId(1),
                &mut store.tokens,
                &mut store.paths,
                &mut NoAssets,
            )
            .expect("import USDC");
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            assert!(result.resolved_layers.is_empty(), "self-contained layer");
            result.layer
        } else {
            let result = layerstack_usdz::read_usdz(
                &data,
                LayerId(1),
                &mut store.tokens,
                &mut store.paths,
                &mut NoAssets,
            )
            .expect("import USDZ");
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            assert!(result.resolved_layers.is_empty(), "self-contained layer");
            result.layer
        };
        store.insert_layer(layer);
        let import_ms = start.elapsed().as_secs_f64() * 1000.;
        drop(data);
        let start = Instant::now();
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let compose_ms = start.elapsed().as_secs_f64() * 1000.;
        assert!(
            stage.composition_errors().is_empty(),
            "{:?}",
            stage.composition_errors()
        );
        let root = store.path("/");
        let prims = stage.traverse(root).count();
        let start = Instant::now();
        if mode == "retained-all" {
            for layer in store.layers.values() {
                for prim in layer.prims.values() {
                    for property in &prim.properties {
                        for value in property.spec.default.iter().chain(
                            property
                                .spec
                                .time_samples
                                .iter()
                                .flat_map(|samples| samples.iter().map(|(_, value)| value)),
                        ) {
                            if let layerstack::Value::TypedArray(array) = value {
                                std::hint::black_box(
                                    array.try_materialize().expect("numeric payload"),
                                );
                            }
                        }
                    }
                }
            }
        }
        let demand_ms = start.elapsed().as_secs_f64() * 1000.;
        let stats = retained
            .as_ref()
            .map(layerstack_usdc::RetainedValues::stats)
            .unwrap_or_default();
        println!(
            "{{\"mode\":\"{mode}\",\"run\":{run},\"bytes\":{bytes},\"prims\":{prims},\"read_ms\":{read_ms:.3},\"import_ms\":{import_ms:.3},\"compose_ms\":{compose_ms:.3},\"demand_ms\":{demand_ms:.3},\"decode_attempts\":{},\"cached_element_bytes\":{}}}",
            stats.decode_attempts, stats.element_bytes
        );
    }
}
