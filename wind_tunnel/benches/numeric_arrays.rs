// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained numeric array clones and sparse edits at mesh-buffer sizes.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, InMemoryStore, Layer, LayerId, PrimSpec,
    PropertyPath, PropertySpec, PropertyType, ResolvedValue, Stage, StageOptions, Value,
};

fn arrays(c: &mut Criterion) {
    let mut group = c.benchmark_group("numeric_arrays");
    for len in [1_000, 1_000_000] {
        let points = Value::TypedArray(layerstack::TypedArray::Vec3f(std::sync::Arc::new(vec![
            [
                1.0, 2.0, 3.0
            ];
            len
        ])));
        group.bench_with_input(BenchmarkId::new("clone", len), &points, |b, value| {
            b.iter(|| black_box(black_box(value).clone()));
        });
        let edit = ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Vec3f([4.0, 5.0, 6.0])),
                index: ArrayIndex::Position(0),
            }],
        };
        group.bench_with_input(
            BenchmarkId::new("sparse_write", len),
            &points,
            |b, value| {
                b.iter(|| {
                    black_box(layerstack::array_edit::apply_to_value(
                        &edit,
                        black_box(value),
                        None,
                    ))
                });
            },
        );
        {
            let mut store = InMemoryStore::default();
            let prim = store.path("/Mesh");
            let name = store.tokens.intern("points");
            let mut layer = Layer::new(LayerId(1));
            layer.insert_prim(
                prim,
                PrimSpec::def().with_property(
                    name,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "point3f",
                        true,
                        Value::Vec3f([0.0; 3]),
                    ))
                    .with_default(points.clone()),
                ),
            );
            store.insert_layer(layer);
            let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
            let property = PropertyPath::new(prim, name);
            group.bench_function(BenchmarkId::new("resolve_scan", len), |b| {
                b.iter(|| {
                    let resolved = stage.resolve_property_path(black_box(property)).unwrap();
                    let ResolvedValue::Scalar(value) = resolved.value else {
                        panic!("points value");
                    };
                    let array = value.array_ref().unwrap();
                    let points = array
                        .typed()
                        .and_then(layerstack::TypedArray::as_vec3f)
                        .unwrap();
                    let sum: f32 = black_box(points).iter().map(|point| point[0]).sum();
                    black_box(sum)
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, arrays);
criterion_main!(benches);
