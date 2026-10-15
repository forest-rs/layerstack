// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Graph ownership and dependency retention on sparse referenced hierarchies.
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Reference, Stage, StageOptions};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

fn fixture(arcs: usize, children: usize) -> (InMemoryStore, layerstack::PathId) {
    let mut store = InMemoryStore::default();
    let target = store.path("/Target");
    let mut root = PrimSpec::def();
    let mut layer = Layer::new(LayerId(1));
    for arc in 0..arcs {
        let source = store.path(&format!("/Source{arc}"));
        layer.insert_prim(source, PrimSpec::def());
        root = root.with_reference(Reference::new(LayerId(1), source));
    }
    layer.insert_prim(target, root);
    for child in 0..children {
        let path = store.path(&format!("/Target/C{child}"));
        layer.insert_prim(path, PrimSpec::def());
    }
    store.insert_layer(layer);
    (store, target)
}
fn graph_storage(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_storage");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for arcs in [0, 8, 32] {
        let (mut store, target) = fixture(arcs, 128);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert!(
            stage.composition_errors().is_empty(),
            "fixture composes completely"
        );
        assert_eq!(
            stage.children_of(target).unwrap().len(),
            128,
            "all sparse children are populated"
        );
        let graph = stage.explain_prim_graph(target).unwrap();
        assert_eq!(
            graph.len(),
            arcs + 1,
            "all references contribute graph nodes"
        );
        group.bench_function(BenchmarkId::new("clone", arcs), |b| {
            b.iter_batched(
                || (),
                |()| black_box(black_box(graph).clone()),
                BatchSize::PerIteration,
            );
        });
        for dependencies in [false, true] {
            group.bench_function(
                BenchmarkId::new(
                    if dependencies {
                        "compose_dependencies"
                    } else {
                        "compose"
                    },
                    arcs,
                ),
                |b| {
                    b.iter_custom(|iterations| {
                        let mut elapsed = Duration::ZERO;
                        for _ in 0..iterations {
                            let start = Instant::now();
                            let stage = Stage::compose(
                                &mut store,
                                LayerId(1),
                                StageOptions {
                                    with_dependencies: dependencies,
                                    ..Default::default()
                                },
                            );
                            elapsed += start.elapsed();
                            black_box(&stage);
                            drop(stage);
                        }
                        elapsed
                    });
                },
            );
        }
    }
    group.finish();
}
fn layer_dependencies(c: &mut Criterion) {
    let mut group = c.benchmark_group("layer_dependencies");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for prims in [1024, 4096] {
        for layers in [1_u64, 4] {
            let mut store = InMemoryStore::default();
            let paths: Vec<_> = (0..prims)
                .map(|i| store.path(&format!("/World/P{i}")))
                .collect();
            for id in 1..=layers {
                let mut layer = Layer::new(LayerId(id));
                for path in &paths {
                    layer.insert_prim(*path, PrimSpec::def());
                }
                if id == 1 {
                    layer.sublayers = (2..=layers)
                        .map(|id| layerstack::SublayerEntry::new(LayerId(id)))
                        .collect();
                }
                store.insert_layer(layer);
            }
            let stage = Stage::compose(
                &mut store,
                LayerId(1),
                StageOptions {
                    with_dependencies: true,
                    ..Default::default()
                },
            );
            assert_eq!(
                stage.layers_affecting_prim(paths[0]).len(),
                usize::try_from(layers).unwrap(),
                "every layer contributes dependency evidence"
            );
            group.bench_function(BenchmarkId::new(format!("{layers}layers"), prims), |b| {
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let start = Instant::now();
                        let stage = Stage::compose(
                            &mut store,
                            LayerId(1),
                            StageOptions {
                                with_dependencies: true,
                                ..Default::default()
                            },
                        );
                        elapsed += start.elapsed();
                        black_box(&stage);
                        drop(stage);
                    }
                    elapsed
                });
            });
        }
    }
    group.finish();
}
criterion_group!(benches, graph_storage, layer_dependencies);
criterion_main!(benches);
