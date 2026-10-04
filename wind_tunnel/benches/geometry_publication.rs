// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Publication into a populated caller-owned stage: 100 native references plus
//! 100, 10,000 and 100,000 point instances, sharing one asset. A second group
//! varies native references (100 and 10,000) with 100 point instances.
//! Mesh payload: a 64 by 64 grid (4,096 points and 7,938 triangles).
//! Timed work includes validation, transaction planning and live-stage application.
//! Initial publication recreates an absent source in an already populated stage;
//! fixture construction, producer evaluation and alternating-buffer setup are excluded.
//! Change reports are consumed by a retained point-instancer bounds cache on prototype
//! edits. The source buffers and per-instance inputs stay shared on unchanged updates.
#[path = "../../layerstack_examples/examples/support/generated_assets.rs"]
#[allow(
    dead_code,
    reason = "one fixture shared with the runnable example and conformance tests"
)]
mod support;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{EditTarget, Transaction, Value};
use layerstack_schemas::{
    GeneratedMesh, Scene, Time,
    bounds::{BoundsCache, BoundsOptions},
};
use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

fn grid() -> GeneratedMesh {
    let points = (0_u16..64)
        .flat_map(|y| (0_u16..64).map(move |x| [f32::from(x), f32::from(y), 0.]))
        .collect();
    let mut indices = Vec::new();
    for y in 0..63 {
        for x in 0..63 {
            let p = y * 64 + x;
            indices.extend([p, p + 1, p + 65, p, p + 65, p + 64]);
        }
    }
    GeneratedMesh {
        points: Arc::new(points),
        face_vertex_counts: Arc::new(vec![3; indices.len() / 3]),
        face_vertex_indices: Arc::new(indices),
        primvars: Vec::new(),
    }
}

fn publication(c: &mut Criterion) {
    for (name, counts) in [
        ("geometry_publication", &[100, 10_000, 100_000][..]),
        ("native_geometry_publication", &[100, 10_000][..]),
    ] {
        let mut group = c.benchmark_group(name);
        for &count in counts {
            for workload in [
                "initial",
                "unchanged",
                "points",
                "topology",
                "prototype_bounds",
            ] {
                // Build only selected cases; setup remains outside timed iterations.
                group.bench_function(BenchmarkId::new(workload, count), |b| {
                    let mut generated = if name == "geometry_publication" {
                        support::GeneratedScene::with_counts(100, count)
                    } else {
                        support::GeneratedScene::with_counts(count, 100)
                    };
                    let base = grid();
                    generated.geometry = base.clone();
                    generated.publish();
                    let mut alternate = base.clone();
                    if workload == "topology" {
                        let mut indices = base.face_vertex_indices.as_ref().clone();
                        indices.swap(0, 1);
                        alternate.face_vertex_indices = Arc::new(indices);
                    } else {
                        let mut points = base.points.as_ref().clone();
                        points[0][1] = -1.;
                        alternate.points = Arc::new(points);
                    }
                    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
                    cache
                        .world_bound(
                            &Scene::new(generated.live.stage(), &generated.store),
                            generated.scatter,
                        )
                        .unwrap();
                    let mut swap = false;
                    b.iter_custom(|iterations| {
                        let mut elapsed = Duration::ZERO;
                        for _ in 0..iterations {
                            if workload == "initial" {
                                let mut deletion = Transaction::new();
                                deletion.remove_spec(
                                    EditTarget::for_layer(support::ASSET).prim(generated.source),
                                );
                                generated
                                    .live
                                    .apply(&mut generated.store, &deletion)
                                    .unwrap();
                            }
                            generated.geometry = if workload == "unchanged" || workload == "initial"
                            {
                                base.clone()
                            } else {
                                swap = !swap;
                                if swap {
                                    alternate.clone()
                                } else {
                                    base.clone()
                                }
                            };
                            let start = Instant::now();
                            let changes = generated.publish();
                            if workload == "prototype_bounds" {
                                let scene = Scene::new(generated.live.stage(), &generated.store);
                                cache.apply_changes(&scene, &changes);
                                black_box(cache.world_bound(&scene, generated.scatter).unwrap());
                            }
                            elapsed += start.elapsed();
                            black_box(changes);
                        }
                        elapsed
                    });
                });
            }
        }
        group.finish();
    }
}
fn procedural_publication(c: &mut Criterion) {
    let mut group = c.benchmark_group("procedural_publication");
    for count in [100, 10_000, 100_000] {
        for workload in [
            "unchanged",
            "recipe_points",
            "upstream_points",
            "recipe_bounds",
        ] {
            group.bench_function(BenchmarkId::new(workload, count), |b| {
                let mut generated = support::GeneratedScene::with_counts(100, count);
                generated.terrain_generator.evaluator_mut().template = grid();
                generated.asset_generator.evaluator_mut().template = grid();
                generated.evaluate_terrain(Time::Default).unwrap();
                generated.evaluate_asset(Time::Default).unwrap();
                let (layer, property) = if workload == "upstream_points" {
                    (support::TERRAIN, "/Recipes/Terrain.primvars:height")
                } else {
                    (support::ASSET, "/Recipes/Tree.primvars:height")
                };
                let parameter = generated.store.property_path(property);
                let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
                cache
                    .world_bound(
                        &Scene::new(generated.live.stage(), &generated.store),
                        generated.scatter,
                    )
                    .unwrap();
                let mut swap = false;
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        if workload != "unchanged" {
                            swap = !swap;
                            let mut edit = Transaction::new();
                            edit.set_default(
                                EditTarget::for_layer(layer).property(parameter),
                                Value::Float(if swap { 1.25 } else { 1. }),
                            );
                            let applied =
                                generated.live.apply(&mut generated.store, &edit).unwrap();
                            cache.apply_changes(
                                &Scene::new(generated.live.stage(), &generated.store),
                                &applied.changes,
                            );
                            if workload == "upstream_points" {
                                let changes = generated.evaluate_terrain(Time::Default).unwrap();
                                cache.apply_changes(
                                    &Scene::new(generated.live.stage(), &generated.store),
                                    &changes,
                                );
                            }
                        }
                        // Time input checks, application evaluation and validated publication.
                        // Recipe editing and upstream publication are deliberately outside it.
                        let start = Instant::now();
                        let changes = generated.evaluate_asset(Time::Default).unwrap();
                        if workload == "recipe_bounds" {
                            let scene = Scene::new(generated.live.stage(), &generated.store);
                            cache.apply_changes(&scene, &changes);
                            black_box(cache.world_bound(&scene, generated.scatter).unwrap());
                        }
                        elapsed += start.elapsed();
                        black_box(generated.asset_generator.work());
                    }
                    elapsed
                });
            });
        }
    }
    group.finish();
}
criterion_group!(benches, publication, procedural_publication);
criterion_main!(benches);
