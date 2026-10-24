// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Per-frame point-instancer work on a million instances: masks, transforms,
//! motion batches, extents, mask compaction and ID authoring.
//!
//! The fixture is built in memory outside the timers (see [`fixture`]);
//! `tools/point_instancer_cpp_timing.py` builds the same scene and times
//! OpenUSD's `ComputeMaskAtTime`, `ComputeInstanceTransformsAtTime(s)` and
//! `ComputeExtentAtTime` on it for comparison.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    FieldValue, InMemoryStore, Layer, LayerId, ListOp, PathId, PrimSpec, PropertySpec,
    PropertyType, Stage, StageOptions, TargetPath, Value, edit::EditTarget,
};
use layerstack_schemas::{
    Scene, SchemaEdit, Time,
    point_instancer::{InstanceTransform, InstanceTransformOptions},
    usd_geom::{PointInstancer, PointInstancerEdit},
    value::{
        write_float3_array_owned, write_int_array_owned, write_int64_array_owned,
        write_quatf_array_owned,
    },
};
use std::{hint::black_box, num::NonZeroUsize, sync::Arc};

/// The fixture's stage and instancers.
struct Fixture {
    store: InMemoryStore,
    stage: Stage,
    still: PathId,
    moving: PathId,
    culled: PathId,
}

/// Unit quaternion `[i, j, k, r]` about Z by `degrees`.
#[allow(clippy::cast_possible_truncation, reason = "quatf storage")]
fn about_z(degrees: f64) -> [f32; 4] {
    let half = degrees.to_radians() / 2.0;
    [0.0, 0.0, half.sin() as f32, half.cos() as f32]
}

/// `n` instances of two prototypes on a 1000-wide grid, one in 100 IDs
/// invisible and one in 1000 inactive: `/World/Still` holds defaults,
/// `/World/Moving` time samples at 0 and 24 with velocities, accelerations
/// and angular velocities; `/World/Culled` is `/World/Still` with one in ten
/// IDs inactive. Matches `tools/point_instancer_cpp_timing.py`.
#[allow(clippy::cast_precision_loss, reason = "grid coordinates")]
fn fixture(n: usize) -> Fixture {
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(LayerId(1));
    let typed = |name: &str| PropertySpec::typed_attribute(PropertyType::parse(name).unwrap());
    let world = store.path("/World");
    layer.insert_prim(
        world,
        PrimSpec::def().with_type_name(store.tokens.intern("Xform")),
    );
    let protos = [
        store.path("/World/Protos/Box"),
        store.path("/World/Protos/Ball"),
    ];
    layer.insert_prim(store.path("/World/Protos"), PrimSpec::def());
    let size = store.tokens.intern("size");
    let radius = store.tokens.intern("radius");
    layer.insert_prim(
        protos[0],
        PrimSpec::def()
            .with_type_name(store.tokens.intern("Cube"))
            .with_property(size, typed("double").with_default(Value::Double(0.5))),
    );
    layer.insert_prim(
        protos[1],
        PrimSpec::def()
            .with_type_name(store.tokens.intern("Sphere"))
            .with_property(radius, typed("double").with_default(Value::Double(0.25))),
    );
    let count = i64::try_from(n).unwrap();
    let indices: Vec<i32> = (0..n).map(|i| i32::from(i % 2 == 1)).collect();
    let ids: Vec<i64> = (0..count).map(|i| i * 3).collect();
    let invisible: Vec<i64> = (0..count).step_by(100).map(|i| i * 3).collect();
    let inactive: Vec<i64> = (0..count).step_by(1000).map(|i| i * 3 + 3).collect();
    let culled_ids: Vec<i64> = (0..count).step_by(10).map(|i| i * 3 + 3).collect();
    let positions = |lift: f32| -> Vec<[f32; 3]> {
        (0..n)
            .map(|i| [(i % 1000) as f32, (i / 1000) as f32, lift])
            .collect()
    };
    let rotations: Vec<[f32; 4]> = (0..n).map(|i| about_z((i % 360) as f64)).collect();
    let scales: Vec<[f32; 3]> = (0..n)
        .map(|i| [1.0 + (i % 5) as f32 * 0.1, 1.0, 1.0])
        .collect();
    let velocities: Vec<[f32; 3]> = (0..n).map(|i| [0.0, 0.0, (i % 7) as f32]).collect();
    let spins: Vec<[f32; 3]> = (0..n).map(|i| [0.0, 0.0, (i % 90) as f32]).collect();
    let prototypes = store.tokens.intern("prototypes");
    let inactive_ids = store.tokens.intern("inactiveIds");
    let names = [
        "protoIndices",
        "ids",
        "invisibleIds",
        "positions",
        "orientationsf",
        "scales",
        "velocities",
        "accelerations",
        "angularVelocities",
    ]
    .map(|name| store.tokens.intern(name));
    let targets = ListOp::explicit(protos.iter().map(|&p| TargetPath::Prim(p)).collect());
    let mut instancer = |still: bool, inactive: &[i64]| {
        let tokens = &mut store.tokens;
        let sampled = |spec: PropertySpec, a: Value, b: Value| {
            spec.with_time_samples(vec![(0.0, a), (24.0, b)])
        };
        let mut prim = PrimSpec::def()
            .with_type_name(tokens.intern("PointInstancer"))
            .with_field(
                inactive_ids,
                FieldValue::Int64ListOp(Box::new(ListOp::explicit(inactive.to_vec()))),
            )
            .with_property(
                prototypes,
                PropertySpec::relationship().with_targets(targets.clone()),
            )
            .with_property(
                names[0],
                typed("int[]").with_default(write_int_array_owned(indices.clone(), tokens)),
            )
            .with_property(
                names[1],
                typed("int64[]").with_default(write_int64_array_owned(ids.clone(), tokens)),
            )
            .with_property(
                names[2],
                typed("int64[]").with_default(write_int64_array_owned(invisible.clone(), tokens)),
            )
            .with_property(
                names[5],
                typed("float3[]").with_default(write_float3_array_owned(scales.clone(), tokens)),
            );
        if still {
            prim = prim
                .with_property(
                    names[3],
                    typed("point3f[]")
                        .with_default(write_float3_array_owned(positions(0.0), tokens)),
                )
                .with_property(
                    names[4],
                    typed("quatf[]")
                        .with_default(write_quatf_array_owned(rotations.clone(), tokens)),
                );
        } else {
            prim = prim
                .with_property(
                    names[3],
                    sampled(
                        typed("point3f[]"),
                        write_float3_array_owned(positions(0.0), tokens),
                        write_float3_array_owned(positions(1.0), tokens),
                    ),
                )
                .with_property(
                    names[4],
                    sampled(
                        typed("quatf[]"),
                        write_quatf_array_owned(rotations.clone(), tokens),
                        write_quatf_array_owned(rotations.clone(), tokens),
                    ),
                )
                .with_property(
                    names[6],
                    sampled(
                        typed("vector3f[]"),
                        write_float3_array_owned(velocities.clone(), tokens),
                        write_float3_array_owned(velocities.clone(), tokens),
                    ),
                )
                .with_property(
                    names[7],
                    sampled(
                        typed("vector3f[]"),
                        write_float3_array_owned(velocities.clone(), tokens),
                        write_float3_array_owned(velocities.clone(), tokens),
                    ),
                )
                .with_property(
                    names[8],
                    sampled(
                        typed("vector3f[]"),
                        write_float3_array_owned(spins.clone(), tokens),
                        write_float3_array_owned(spins.clone(), tokens),
                    ),
                );
        }
        prim
    };
    let still_prim = instancer(true, &inactive);
    let moving_prim = instancer(false, &inactive);
    let culled_prim = instancer(true, &culled_ids);
    let (still, moving, culled) = (
        store.path("/World/Still"),
        store.path("/World/Moving"),
        store.path("/World/Culled"),
    );
    layer.insert_prim(still, still_prim);
    layer.insert_prim(moving, moving_prim);
    layer.insert_prim(culled, culled_prim);
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut store, LayerId(1), options);
    Fixture {
        store,
        stage,
        still,
        moving,
        culled,
    }
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("point_instancer");
    group.sample_size(10);
    for n in [100_000, 1_000_000] {
        let mut fixture = fixture(n);
        let deactivate: Vec<i64> = (0..10_000).map(|i| i * 7).collect();
        group.bench_with_input(
            BenchmarkId::new("deactivate_and_hide_10k_ids", n),
            &n,
            |b, _| {
                // Collecting edits interns names but changes no layer.
                b.iter(|| {
                    let mut edit = SchemaEdit::new(
                        &fixture.stage,
                        &mut fixture.store,
                        EditTarget::for_layer(LayerId(1)),
                    );
                    let point = PointInstancerEdit::new(&edit, fixture.still)
                        .expect("outside any instance")
                        .unwrap();
                    point.deactivate_ids(&mut edit, &deactivate).unwrap();
                    point.make_invisible(&mut edit, &deactivate).unwrap();
                    black_box(edit.finish())
                });
            },
        );
        let scene = Scene::new(&fixture.stage, &fixture.store);
        let still = PointInstancer::new(&scene, fixture.still).unwrap();
        let moving = PointInstancer::new(&scene, fixture.moving).unwrap();
        let options = InstanceTransformOptions::default();
        group.bench_with_input(BenchmarkId::new("compute_mask", n), &n, |b, _| {
            b.iter(|| black_box(still.try_compute_mask(Time::Default).unwrap()));
        });
        let culled = PointInstancer::new(&scene, fixture.culled).unwrap();
        group.bench_with_input(
            BenchmarkId::new("compute_mask_tenth_inactive", n),
            &n,
            |b, _| {
                b.iter(|| black_box(culled.try_compute_mask(Time::Default).unwrap()));
            },
        );
        let mut output: Vec<InstanceTransform> = Vec::new();
        group.bench_with_input(BenchmarkId::new("transforms_into_reused", n), &n, |b, _| {
            b.iter(|| {
                still
                    .compute_instance_transforms_into(
                        Time::Default,
                        Time::Default,
                        options,
                        &mut output,
                    )
                    .unwrap();
                black_box(output.len())
            });
        });
        group.bench_with_input(BenchmarkId::new("transforms_collected", n), &n, |b, _| {
            b.iter(|| {
                black_box(
                    still
                        .compute_instance_transforms(Time::Default, Time::Default, options)
                        .unwrap(),
                )
            });
        });
        group.bench_with_input(BenchmarkId::new("transforms_streamed", n), &n, |b, _| {
            b.iter(|| {
                let prepared = still
                    .prepare_instance_transforms(Time::Default, Time::Default, options)
                    .unwrap();
                let mut sum = 0.0;
                for instance in prepared.iter() {
                    sum += instance.matrix[3][0];
                }
                black_box(sum)
            });
        });
        let shutter = [Time::at(0.25), Time::at(0.5), Time::at(0.75)];
        group.bench_with_input(BenchmarkId::new("motion_batch_3_samples", n), &n, |b, _| {
            b.iter(|| {
                black_box(
                    moving
                        .compute_instance_transforms_at_times(&shutter, Time::at(0.0), options)
                        .unwrap(),
                )
            });
        });
        group.bench_with_input(BenchmarkId::new("compute_extent", n), &n, |b, _| {
            b.iter(|| {
                black_box(
                    still
                        .compute_extent(Time::Default, Time::Default, None)
                        .unwrap(),
                )
            });
        });
        let mask = still.try_compute_mask(Time::Default).unwrap();
        let values: Vec<[f32; 3]> = vec![[1.0; 3]; n];
        group.bench_with_input(BenchmarkId::new("apply_mask_to_array", n), &n, |b, _| {
            b.iter_batched(
                || values.clone(),
                |mut values| {
                    PointInstancer::apply_mask_to_array(
                        &mask,
                        &mut values,
                        NonZeroUsize::new(1).unwrap(),
                    )
                    .unwrap();
                    black_box(values)
                },
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
