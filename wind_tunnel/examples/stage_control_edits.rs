// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Measure a payload toggle beside an unrelated wide scene. Warm resident
//! layers; synchronization only, with no asset I/O. Pass unrelated prim count
//! and repetitions. Baseline comparison uses an explicit structural rebuild.
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, Reference, Specifier,
    StageOptions, Transaction,
};
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
    let source_edits = std::env::args().nth(3).as_deref() == Some("source");
    let session = (std::env::args().nth(4).as_deref() == Some("session")).then_some(LayerId(3));
    if let Some(session) = session {
        let mut layer = Layer::new(session);
        layer
            .sublayers
            .push(layerstack::SublayerEntry::new(LayerId(4)));
        store.insert_layer(layer);
        store.insert_layer(Layer::new(LayerId(4)));
    }
    let child = store.path("/Asset/New");
    for forced in [true, false] {
        if source_edits && store.layers[&LayerId(2)].prims.contains_key(&child) {
            let mut remove = Transaction::new();
            remove.remove_spec(EditTarget::for_layer(LayerId(2)).prim(child));
            remove.apply(&mut store).unwrap();
        }
        let mut live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                session_layer: session,
                ..StageOptions::default()
            },
        );
        let mut times = Vec::new();
        for i in 0..runs + 2 {
            if source_edits {
                let mut edit = Transaction::new();
                let address = EditTarget::for_layer(LayerId(2)).prim(child);
                if i % 2 == 0 {
                    edit.create_prim(address, Specifier::Def, None);
                } else {
                    edit.remove_spec(address);
                }
                edit.apply(&mut store).unwrap();
            } else if i % 2 == 0 {
                live.unload(&store, target);
            } else {
                live.load(&store, target, layerstack::LoadPolicy::WithDescendants);
            }
            if forced {
                live.notify_structural_change();
            }
            let start = Instant::now();
            live.notify_changed_layers(&store);
            let changes = live.recompose_changes(&mut store);
            std::hint::black_box(changes);
            if i >= 2 {
                times.push(start.elapsed().as_secs_f64() * 1_000.0);
            }
        }
        times.sort_by(f64::total_cmp);
        println!(
            "session={session:?} source_edits={source_edits} forced_full={forced} unrelated={count} runs={runs} median_ms={:.3} min_ms={:.3} max_ms={:.3}",
            times[times.len() / 2],
            times[0],
            times[times.len() - 1]
        );
        println!("work={:?}", live.recomposition_work());
    }
}
