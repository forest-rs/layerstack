// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Total scalar edit cost with increasing callback fan-out, without query caches.
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertyPath, PropertySpec,
    PropertyType, StageOptions, Transaction, Value,
};
use std::hint::black_box;

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("stage_notices");
    group.sample_size(10);
    for count in [1000, 10000] {
        let mut store = InMemoryStore::default();
        let value = store.tokens.intern("value");
        let mut layer = Layer::new(LayerId(1));
        let mut paths = Vec::new();
        for i in 0..count {
            let path = store.path(&format!("/P{i}"));
            paths.push(path);
            layer.insert_prim(
                path,
                PrimSpec::def().with_property(
                    value,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "float",
                        false,
                        Value::Float(0.0),
                    ))
                    .with_default(Value::Float(0.0)),
                ),
            );
        }
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let edits = [0.25, 0.75].map(|v| {
            let mut edit = Transaction::new();
            edit.set_default(
                EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(paths[0], value)),
                Value::Float(v),
            );
            edit
        });
        for listeners in [0, 1, 8, 64] {
            let subscriptions: Vec<_> = (0..listeners)
                .map(|_| {
                    live.subscribe_changes(|notice| {
                        black_box((notice.revision, notice.changes.changed_info_only.len()));
                    })
                })
                .collect();
            group.bench_with_input(
                BenchmarkId::new(format!("edit_with_{listeners}_callbacks"), count),
                &count,
                |b, _| {
                    let mut frame = 0;
                    b.iter(|| {
                        frame ^= 1;
                        black_box(live.apply(&mut store, &edits[frame]).unwrap());
                    });
                },
            );
            for subscription in subscriptions {
                assert!(
                    live.unsubscribe_changes(&subscription),
                    "benchmark owns the subscription"
                );
            }
        }
    }
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
