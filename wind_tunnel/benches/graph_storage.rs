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
            for properties in [0, 3] {
                let mut store = InMemoryStore::default();
                let paths: Vec<_> = (0..prims)
                    .map(|i| store.path(&format!("/World/P{i}")))
                    .collect();
                let fields: Vec<_> = (0..properties)
                    .map(|i| store.tokens.intern(format!("transform{i}")))
                    .collect();
                for id in 1..=layers {
                    let mut layer = Layer::new(LayerId(id));
                    for path in &paths {
                        let mut spec = PrimSpec::def();
                        for &field in &fields {
                            spec = spec.with_property(
                                field,
                                layerstack::PropertySpec::typed_attribute(
                                    layerstack::PropertyType::new(
                                        "float3",
                                        false,
                                        layerstack::Value::Vec3f([0.0; 3]),
                                    ),
                                )
                                .with_default(layerstack::Value::Vec3f([0.0; 3])),
                            );
                        }
                        layer.insert_prim(*path, spec);
                    }
                    if id == 1 {
                        *layer.sublayers_mut() = (2..=layers)
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
                let storage = stage.layer_dependency_storage().unwrap();
                assert_eq!(
                    storage.unique_layer_sets, 1,
                    "equivalent contributor sets share one allocation"
                );
                assert_eq!(
                    storage.physical_layer_references,
                    usize::try_from(layers).unwrap(),
                    "physical references scale with distinct contributor sets"
                );
                assert_eq!(
                    storage.logical_layer_references,
                    storage.prims * usize::try_from(layers).unwrap(),
                    "sharing preserves every dependency edge"
                );
                assert_eq!(
                    stage.authored_property_names(paths[0], &store).len(),
                    properties,
                    "every transform property composes"
                );
                let case = if properties == 0 {
                    format!("{layers}layers")
                } else {
                    format!("{layers}layers_{properties}properties")
                };
                group.bench_function(BenchmarkId::new(case, prims), |b| {
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
    }
    group.finish();
}
criterion_group!(benches, graph_storage, layer_dependencies);
criterion_main!(benches);
