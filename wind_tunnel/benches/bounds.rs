// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Cached authored-extent queries, and a changed leaf propagated to the root.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, Stage, StageOptions, Value,
};
use layerstack_schemas::{
    Scene, Time,
    bounds::{BoundsCache, BoundsOptions},
};
use std::{hint::black_box, sync::Arc};

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("bounds_cache");
    group.sample_size(10);
    for n in [1_000, 10_000, 100_000] {
        for (shape, fanout) in [("balanced", 10), ("wide", n)] {
            let mut store = InMemoryStore::default();
            let xform = store.tokens.intern("Xform");
            let mesh = store.tokens.intern("Mesh");
            let extent = store.tokens.intern("extent");
            let mut layer = Layer::new(LayerId(1));
            let root = store.path("/World");
            let mut names = vec![String::from("/World")];
            for i in 1..n {
                names.push(format!("{}/P{i}", names[(i - 1) / fanout]));
            }
            let mut leaf = root;
            for (i, name) in names.iter().enumerate() {
                let path = store.path(name);
                leaf = path;
                let prim = if i * fanout + 1 < n {
                    PrimSpec::def().with_type_name(xform)
                } else {
                    PrimSpec::def().with_type_name(mesh).with_property(
                        extent,
                        PropertySpec::attribute().with_default(Value::Array(vec![
                            Value::Vec3f([-1.0; 3]),
                            Value::Vec3f([1.0; 3]),
                        ])),
                    )
                };
                layer.insert_prim(path, prim);
            }
            store.insert_layer(layer);
            let options = StageOptions {
                schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
                ..StageOptions::default()
            };
            let stage = Stage::compose(&mut store, LayerId(1), options);
            let scene = Scene::new(&stage, &store);
            let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
            black_box(cache.world_bound(&scene, root).unwrap());
            group.bench_with_input(
                BenchmarkId::new(format!("{shape}_leaf_invalidate_root_query"), n),
                &n,
                |b, _| {
                    b.iter(|| {
                        cache.invalidate(&scene, leaf);
                        black_box(cache.world_bound(&scene, root).unwrap());
                    });
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("{shape}_first_leaf_edit"), n),
                &n,
                |b, _| {
                    b.iter_batched_ref(
                        || {
                            let mut cache =
                                BoundsCache::new(Time::Default, BoundsOptions::default());
                            black_box(cache.world_bound(&scene, root).unwrap());
                            cache
                        },
                        |cache| {
                            cache.invalidate(&scene, leaf);
                            black_box(cache.world_bound(&scene, root).unwrap());
                        },
                        criterion::BatchSize::PerIteration,
                    );
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("{shape}_warm_root"), n),
                &n,
                |b, _| b.iter(|| black_box(cache.world_bound(&scene, root).unwrap())),
            );
            group.bench_with_input(
                BenchmarkId::new(format!("{shape}_cold_root"), n),
                &n,
                |b, _| {
                    b.iter(|| {
                        let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
                        black_box(cache.world_bound(&scene, root).unwrap());
                    });
                },
            );
        }
    }
    group.finish();
}
fn animation(c: &mut Criterion) {
    let mut group = c.benchmark_group("bounds_animation");
    group.sample_size(10);
    for (name, every) in [("static", 0), ("mixed", 10), ("animated", 1)] {
        let mut store = InMemoryStore::default();
        let xform = store.tokens.intern("Xform");
        let mesh = store.tokens.intern("Mesh");
        let extent = store.tokens.intern("extent");
        let mut layer = Layer::new(LayerId(1));
        let root = store.path("/World");
        layer.insert_prim(root, PrimSpec::def().with_type_name(xform));
        for i in 0..10_000 {
            let parent = store.path(&format!("/World/G{}", i / 16));
            if i % 16 == 0 {
                layer.insert_prim(parent, PrimSpec::def().with_type_name(xform));
            }
            let path = store.path(&format!("/World/G{}/P{i}", i / 16));
            let value =
                |size: f32| Value::Array(vec![Value::Vec3f([-size; 3]), Value::Vec3f([size; 3])]);
            let mut spec = PropertySpec::attribute().with_default(value(1.0));
            if every != 0 && i % every == 0 {
                spec = spec.with_time_samples(vec![(0.0, value(1.0)), (10.0, value(2.0))]);
            }
            layer.insert_prim(
                path,
                PrimSpec::def()
                    .with_type_name(mesh)
                    .with_property(extent, spec),
            );
        }
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        let stage = Stage::compose(&mut store, LayerId(1), options);
        let scene = Scene::new(&stage, &store);
        let mut cache = BoundsCache::new(Time::at(0.0), BoundsOptions::default());
        black_box(cache.world_bound(&scene, root).unwrap());
        let mut frame = 0;
        group.bench_function(name, |b| {
            b.iter(|| {
                frame = (frame + 1) % 20;
                cache.set_time(Time::at(f64::from(frame) * 0.5));
                black_box(cache.world_bound(&scene, root).unwrap());
            });
        });
    }
    group.finish();
}
criterion_group!(benches, bench, animation);
criterion_main!(benches);
