// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Times a skinning bake of a self-contained USDA scene.
//!
//! Run in release mode: `cargo run -p wind_tunnel --release --example
//! skel_bake -- /path/to/scene.usda 5`. `tools/skel_bake_cpp_timing.py`
//! writes the character-sized scene (50000 points, 100 joints, 200 frames)
//! and times OpenUSD's bake of it. Every run reads the file into a fresh
//! store and composes it untimed, then bakes every `SkelRoot` into the root
//! layer with `SkinningBake`; it prints the bake's median and minimum time,
//! and what the last run wrote.

use std::{sync::Arc, time::Instant};

use layerstack::{
    AssetResolveError, AssetResolver, EditTarget, InMemoryStore, LayerId, LiveStage, PathInterner,
    ResolvedAsset, StageOptions, TokenInterner,
};
use layerstack_schemas::{Scene, skel::SkinningBake};

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

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let file = args.get(1).expect("expected a USDA path");
    let runs: usize = args
        .get(2)
        .map_or(Ok(5), |arg| arg.parse())
        .expect("expected an integer run count");
    let threads: usize = args
        .get(3)
        .map_or(Ok(1), |arg| arg.parse())
        .expect("expected an integer thread count");
    assert!(runs > 0, "at least one run is required");
    let source = std::fs::read_to_string(file).expect("read UTF-8 USDA source");
    let mut timings = Vec::with_capacity(runs);
    let mut summary = String::new();
    for _ in 0..runs {
        let mut store = InMemoryStore::default();
        let imported = layerstack_usda::read_usda(
            &source,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        assert!(
            imported.parse_diagnostics.is_empty() && imported.emitted.diagnostics.is_empty(),
            "{:?} {:?}",
            imported.parse_diagnostics,
            imported.emitted.diagnostics
        );
        store.insert_layer(imported.emitted.layer);
        let options = StageOptions {
            schemas: Some(Arc::new(layerstack_schemas::openusd(store.tokens()))),
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let root = store.path("/");
        let bake = SkinningBake::for_subtree(
            &Scene::new(live.stage(), &store),
            root,
            EditTarget::for_layer(LayerId(1)),
        )
        .with_threads(threads);
        let start = Instant::now();
        let report = bake.bake(&mut live, &mut store).expect("bake");
        timings.push(start.elapsed().as_secs_f64() * 1000.0);
        let samples: usize = report.prims.iter().map(|prim| prim.times.len()).sum();
        summary = format!(
            "prims={} samples={samples} problems={}",
            report.prims.len(),
            report.problems.len()
        );
    }
    timings.sort_by(f64::total_cmp);
    println!(
        "layerstack bake {summary} threads={threads} median_ms={:.1} min_ms={:.1}",
        timings[timings.len() / 2],
        timings[0]
    );
}
