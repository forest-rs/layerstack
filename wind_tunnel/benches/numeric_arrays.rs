// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained numeric array clones and sparse edits at mesh-buffer sizes.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, InMemoryStore, Layer, LayerId, PrimSpec,
    PropertyPath, PropertySpec, PropertyType, Stage, StageOptions, SublayerEntry, Time, Value,
};
use layerstack_schemas::{Scene, usd_geom::Mesh};

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
            let name = store.tokens().intern("points");
            let mesh = store.tokens().intern("Mesh");
            let mut layer = Layer::new(LayerId(1));
            layer.insert_prim(
                prim,
                PrimSpec::def().with_type_name(mesh).with_property(
                    name,
                    PropertySpec::typed_attribute(PropertyType::parse("point3f[]").unwrap())
                        .with_default(points.clone()),
                ),
            );
            store.insert_layer(layer);
            let options = StageOptions {
                schemas: Some(std::sync::Arc::new(layerstack_schemas::openusd(
                    store.tokens(),
                ))),
                ..StageOptions::default()
            };
            let stage = Stage::compose(&mut store, LayerId(1), options);
            let property = PropertyPath::new(prim, name);
            let points = stage.view(&store).attribute(property).unwrap();
            group.bench_function(BenchmarkId::new("resolve_scan", len), |b| {
                b.iter(|| {
                    let value = black_box(points)
                        .authored_value(Time::Default)
                        .unwrap()
                        .value;
                    let array = value.array_ref().unwrap();
                    let points = array
                        .typed()
                        .and_then(layerstack::TypedArray::as_vec3f)
                        .unwrap();
                    let sum: f32 = black_box(points).iter().map(|point| point[0]).sum();
                    black_box(sum)
                });
            });
            let scene = Scene::new(&stage, &store);
            let mesh = Mesh::new(&scene, prim).unwrap();
            group.bench_function(BenchmarkId::new("schema_points", len), |b| {
                b.iter(|| black_box(mesh.points().unwrap()));
            });
            let point_type = PropertyType::parse("point3f[]").unwrap();
            let mut stronger = Layer::new(LayerId(2));
            stronger
                .sublayers_mut()
                .push(SublayerEntry::new(LayerId(1)));
            stronger.insert_prim(
                prim,
                PrimSpec::over().with_property(
                    name,
                    PropertySpec::typed_attribute(point_type.clone()).with_default(
                        Value::typed_array_edit(
                            ArrayEdit {
                                ops: vec![ArrayEditOp::Insert {
                                    src: ArrayEditOperand::Literal(Value::Vec3f([4.0, 5.0, 6.0])),
                                    index: ArrayIndex::End,
                                }],
                            },
                            point_type,
                        ),
                    ),
                ),
            );
            store.insert_layer(stronger);
            let options = StageOptions {
                schemas: Some(std::sync::Arc::new(layerstack_schemas::openusd(
                    store.tokens(),
                ))),
                ..StageOptions::default()
            };
            let sparse = Stage::compose(&mut store, LayerId(2), options);
            let scene = Scene::new(&sparse, &store);
            let mesh = Mesh::new(&scene, prim).unwrap();
            group.bench_function(BenchmarkId::new("schema_sparse_points_default", len), |b| {
                b.iter(|| black_box(mesh.points().unwrap()));
            });
            group.bench_function(BenchmarkId::new("schema_sparse_points_numeric", len), |b| {
                b.iter(|| black_box(mesh.points_at(Time::at(9.0)).unwrap()));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, arrays);
criterion_main!(benches);
