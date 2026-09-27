// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse local-cache queries and namespace invalidation in a populated scene.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions};
use layerstack_schemas::{Scene, Time, XformCache};
use std::{hint::black_box, sync::Arc};

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("xform_cache");
    group.sample_size(10);
    for n in [1_000, 10_000, 100_000] {
        let mut store = InMemoryStore::default();
        let xform = store.tokens.intern("Xform");
        let mut layer = Layer::new(LayerId(1));
        let root = store.path("/World");
        layer.insert_prim(root, PrimSpec::def().with_type_name(xform));
        let mut paths = Vec::new();
        for i in 0..n {
            let path = store.path(&format!("/World/P{i}"));
            layer.insert_prim(path, PrimSpec::def().with_type_name(xform));
            paths.push(path);
        }
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        let stage = Stage::compose(&mut store, LayerId(1), options);
        let scene = Scene::new(&stage, &store);
        let mut cache = XformCache::new(Time::Default);
        for &path in &paths {
            black_box(cache.local_to_world(&scene, path));
        }
        let leaf = paths[n - 1];
        group.bench_with_input(BenchmarkId::new("leaf_invalidate_query", n), &n, |b, _| {
            b.iter(|| {
                cache.invalidate(&scene, leaf);
                black_box(cache.local_to_world(&scene, leaf));
            });
        });
        group.bench_with_input(BenchmarkId::new("warm_query", n), &n, |b, _| {
            b.iter(|| black_box(cache.local_to_world(&scene, leaf)));
        });
        group.bench_with_input(BenchmarkId::new("cold_all", n), &n, |b, _| {
            b.iter(|| {
                let mut cache = XformCache::new(Time::Default);
                for &path in &paths {
                    black_box(cache.local_to_world(&scene, path));
                }
                black_box(cache);
            });
        });
    }
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
