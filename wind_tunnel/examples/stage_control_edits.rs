// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Measure a payload toggle beside an unrelated wide scene. Warm resident
//! layers; synchronization only, with no asset I/O. Pass unrelated prim count
//! and repetitions. Baseline comparison uses an explicit structural rebuild.
use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, Reference, StageOptions};
use std::time::Instant;
fn main() {
    let count = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    let runs = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(21);
    let mut store = InMemoryStore::default();
    let target = store.path("/Payload");
    let source = store.path("/Asset");
    let mut root = Layer::new(LayerId(1));
    let mut prim = PrimSpec::def();
    prim.payloads.explicit = Some(vec![Reference::with_asset(
        LayerId(2),
        source,
        "asset.usda",
    )]);
    root.insert_prim(target, prim);
    for i in 0..count {
        root.insert_prim(store.path(&format!("/Unrelated{i}")), PrimSpec::def());
    }
    store.insert_layer(root);
    let mut asset = Layer::new(LayerId(2));
    asset.insert_prim(source, PrimSpec::def());
    for i in 0..32 {
        asset.insert_prim(store.path(&format!("/Asset/Child{i}")), PrimSpec::def());
    }
    store.insert_layer(asset);
    for forced in [true, false] {
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let mut times = Vec::new();
        for i in 0..runs + 2 {
            if i % 2 == 0 {
                live.unload(&store, target);
            } else {
                live.load(&store, target, layerstack::LoadPolicy::WithDescendants);
            }
            if forced {
                live.notify_structural_change();
            }
            let start = Instant::now();
            let changes = live.recompose_changes(&mut store);
            std::hint::black_box(changes);
            if i >= 2 {
                times.push(start.elapsed().as_secs_f64() * 1_000.0);
            }
        }
        times.sort_by(f64::total_cmp);
        println!(
            "forced_full={forced} unrelated={count} runs={runs} median_ms={:.3} min_ms={:.3} max_ms={:.3}",
            times[times.len() / 2],
            times[0],
            times[times.len() - 1]
        );
    }
}
