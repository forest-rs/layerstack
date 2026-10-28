// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Repeated primary-frustum tests of oriented world bounds, and picking, excluding
//! scene reads. Run `cargo bench -p wind_tunnel --bench camera_frustum`. Each
//! projection uses mixed visible/culled bounds at 1,000 and 10,000 boxes; setup
//! checks equal visible counts. Preparation and queries are measured separately,
//! with 20 samples, 500 ms warmup and a 1 s measurement target. Picking casts the
//! window-center pick ray against 10,000 triangles and oriented bounds, and tests
//! the same triangles against a frustum narrowed to 2% of the window; setup checks
//! the hits are mixed. Results cover CPU predicates; scene loading, camera sampling
//! and renderer work are separate.

use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main,
    measurement::WallTime,
};
use layerstack::{
    InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, Stage, StageOptions, Value,
};
use layerstack_schemas::{
    Scene, Time, XformCache,
    bounds::{BoundingBox, Range3d},
    camera::{ComputedCamera, PreparedFrustum},
    usd_geom::Camera,
};
use std::{hint::black_box, sync::Arc, time::Duration};

fn camera(orthographic: bool) -> ComputedCamera {
    let mut store = InMemoryStore::default();
    let path = store.path("/Camera");
    let schema = store.tokens().intern("Camera");
    let projection = store.tokens().intern("projection");
    let projection_value = store.tokens().intern(if orthographic {
        "orthographic"
    } else {
        "perspective"
    });
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        path,
        PrimSpec::def()
            .with_type_name(schema)
            .with_property(
                projection,
                PropertySpec::attribute().with_default(Value::Token(projection_value)),
            )
            .with_property(
                store.tokens().intern("clippingRange"),
                PropertySpec::attribute().with_default(Value::Vec2f([1., 1000.])),
            ),
    );
    store.insert_layer(layer);
    let schemas =
        layerstack_schemas::registry(&[layerstack_schemas::Domain::UsdGeom], store.tokens());
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(Arc::new(schemas)),
            ..StageOptions::default()
        },
    );
    Camera::new(&Scene::new(&stage, &store), path)
        .unwrap()
        .compute_camera(&mut XformCache::new(Time::Default))
        .unwrap()
}

fn boxes(count: u32) -> Vec<BoundingBox> {
    (0..count)
        .map(|i| {
            let mut matrix = [[0.; 4]; 4];
            matrix[0][0] = 1.;
            matrix[1][1] = 1.;
            matrix[2][2] = 1.;
            matrix[3][3] = 1.;
            matrix[0][1] = if i.is_multiple_of(3) { 0.3 } else { 0. };
            matrix[3][0] = f64::from(i % 31) - 15.;
            matrix[3][1] = f64::from(i / 31 % 31) - 15.;
            matrix[3][2] = -f64::from(i % 90) - 2.;
            BoundingBox {
                range: Range3d {
                    min: [-0.25; 3],
                    max: [0.25; 3],
                },
                matrix,
            }
        })
        .collect()
}

/// Small triangles at the boxes' centers, alternately facing and edge-on to `-Z`,
/// with some straddling the window center.
fn triangles(count: u32) -> Vec<[[f64; 3]; 3]> {
    boxes(count)
        .iter()
        .enumerate()
        .map(|(i, bound)| {
            let [x, y, z] = [bound.matrix[3][0], bound.matrix[3][1], bound.matrix[3][2]];
            let (x, y) = if i % 4 == 0 {
                (x * 0.01, y * 0.01)
            } else {
                (x, y)
            };
            if i % 2 == 0 {
                [
                    [x - 0.5, y - 0.5, z],
                    [x + 0.5, y - 0.5, z],
                    [x, y + 0.5, z],
                ]
            } else {
                [[x - 0.5, y, z], [x + 0.5, y, z], [x, y, z - 0.5]]
            }
        })
        .collect()
}

fn bench_picking(
    group: &mut BenchmarkGroup<'_, WallTime>,
    projection: &str,
    frustum: &PreparedFrustum,
) {
    let count = 10_000;
    let triangles = triangles(count);
    let bounds = boxes(count);
    let ray = frustum.pick_ray([0., 0.]);
    let narrowed = frustum.narrowed([0., 0.], [0.02, 0.02]);
    let ray_hits = triangles
        .iter()
        .filter(|t| ray.intersect_triangle(**t).is_some())
        .count();
    let narrowed_hits = triangles
        .iter()
        .filter(|t| narrowed.intersects_triangle(**t))
        .count();
    assert!(
        ray_hits > 0 && narrowed_hits >= ray_hits && narrowed_hits < triangles.len(),
        "mixed picked and missed triangles"
    );
    assert!(
        bounds.iter().any(|b| ray.intersect_bound(b).is_some()),
        "the pick ray hits some bounds"
    );
    group.throughput(Throughput::Elements(1));
    group.bench_function(format!("{projection}/pick_ray_and_narrow"), |b| {
        b.iter(|| {
            let frustum = black_box(frustum);
            black_box((
                frustum.pick_ray(black_box([0.25, -0.5])),
                frustum.narrowed(black_box([0.25, -0.5]), [0.02, 0.02]),
            ))
        });
    });
    group.throughput(Throughput::Elements(u64::from(count)));
    group.bench_with_input(
        BenchmarkId::new(format!("{projection}/ray_triangles"), count),
        &triangles,
        |b, triangles| {
            b.iter(|| {
                let ray = black_box(&ray);
                black_box(
                    triangles
                        .iter()
                        .filter(|t| ray.intersect_triangle(*black_box(*t)).is_some())
                        .count(),
                )
            });
        },
    );
    group.bench_with_input(
        BenchmarkId::new(format!("{projection}/ray_bounds"), count),
        &bounds,
        |b, bounds| {
            b.iter(|| {
                let ray = black_box(&ray);
                black_box(
                    bounds
                        .iter()
                        .filter(|bound| ray.intersect_bound(black_box(bound)).is_some())
                        .count(),
                )
            });
        },
    );
    group.bench_with_input(
        BenchmarkId::new(format!("{projection}/narrowed_triangles"), count),
        &triangles,
        |b, triangles| {
            b.iter(|| {
                let narrowed = black_box(&narrowed);
                black_box(
                    triangles
                        .iter()
                        .filter(|t| narrowed.intersects_triangle(*black_box(*t)))
                        .count(),
                )
            });
        },
    );
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("camera_frustum");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for orthographic in [false, true] {
        let camera = camera(orthographic);
        let projection = if orthographic {
            "orthographic"
        } else {
            "perspective"
        };
        group.throughput(Throughput::Elements(1));
        group.bench_function(format!("{projection}/prepare"), |b| {
            b.iter(|| black_box(black_box(&camera).prepare_frustum()));
        });
        for count in [1_000, 10_000] {
            let boxes = boxes(count);
            let visible = boxes
                .iter()
                .filter(|bound| camera.intersects_world_bound(bound))
                .count();
            assert!(
                visible > 0 && visible < boxes.len(),
                "mixed visible and culled bounds"
            );
            let frustum = camera.prepare_frustum();
            assert_eq!(
                boxes
                    .iter()
                    .filter(|bound| frustum.intersects_world_bound(bound))
                    .count(),
                visible,
                "prepared and one-shot visible checksums agree"
            );
            group.throughput(Throughput::Elements(u64::from(count)));
            group.bench_with_input(
                BenchmarkId::new(format!("{projection}/one_shot"), count),
                &boxes,
                |b, boxes| {
                    b.iter(|| {
                        black_box(
                            boxes
                                .iter()
                                .filter(|bound| {
                                    black_box(&camera).intersects_world_bound(black_box(bound))
                                })
                                .count(),
                        )
                    });
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("{projection}/prepared_reuse"), count),
                &boxes,
                |b, boxes| {
                    b.iter(|| {
                        black_box(
                            boxes
                                .iter()
                                .filter(|bound| {
                                    black_box(&frustum).intersects_world_bound(black_box(bound))
                                })
                                .count(),
                        )
                    });
                },
            );
        }
        bench_picking(&mut group, projection, &camera.prepare_frustum());
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
