// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Single authored-value edits in increasingly large, nested stages.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertyPath, PropertySpec, PropertyType,
    StageOptions, Value,
    edit::{EditTarget, Transaction},
};

fn bench_live_edits(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_value_edit");
    group.sample_size(10);
    for n in [1_000, 10_000, 100_000] {
        let mut store = InMemoryStore::default();
        let translate = store.tokens.intern("xformOp:translate");
        let rotate = store.tokens.intern("xformOp:rotateXYZ");
        let scale = store.tokens.intern("xformOp:scale");
        let xform = store.tokens.intern("Xform");
        let mut layer = Layer::new(LayerId(1));
        let mut names = vec![String::from("/World")];
        for i in 1..n {
            names.push(format!("{}/P{i}", names[(i - 1) / 10]));
        }
        let mut leaf = store.path("/World");
        for (i, name) in names.iter().enumerate() {
            let path = store.path(name);
            leaf = path;
            let mut rotation = PropertySpec::typed_attribute(PropertyType::new(
                "float3",
                false,
                Value::Vec3f([0.0; 3]),
            ));
            if i % 97 == 0 {
                rotation.time_samples = Some(vec![
                    (0.0, Value::Vec3f([0.0; 3])),
                    (10.0, Value::Vec3f([90.0; 3])),
                ]);
            } else {
                rotation.default = Some(Value::Vec3f([0.0; 3]));
            }
            layer.insert_prim(
                path,
                PrimSpec::def()
                    .with_type_name(xform)
                    .with_property(
                        translate,
                        PropertySpec::typed_attribute(PropertyType::new(
                            "double3",
                            false,
                            Value::Vec3d([0.0; 3]),
                        ))
                        .with_default(Value::Vec3d([0.0; 3])),
                    )
                    .with_property(rotate, rotation)
                    .with_property(
                        scale,
                        PropertySpec::typed_attribute(PropertyType::new(
                            "float3",
                            false,
                            Value::Vec3f([1.0; 3]),
                        ))
                        .with_default(Value::Vec3f([1.0; 3])),
                    ),
            );
        }
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let mut txn = Transaction::new();
        txn.set_default(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(leaf, translate)),
            Value::Vec3d([1.0; 3]),
        );
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                // Alternating exact inverses avoids benchmarking no-op sets.
                let applied = live.apply(&mut store, &txn).unwrap();
                black_box(&applied.recomposed);
                txn = applied.inverse;
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_live_edits);
criterion_main!(benches);
