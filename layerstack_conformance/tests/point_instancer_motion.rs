// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse composed motion anchoring and fixed-base shutter samples.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{
    Scene, Time,
    point_instancer::{InstanceTransformOptions, PointInstancerError},
    usd_geom::PointInstancer,
};
const OPTIONS: InstanceTransformOptions = InstanceTransformOptions {
    include_prototype_transform: false,
    apply_mask: false,
};
const BASE: &str = r#"#usda 1.0
(timeCodesPerSecond = 4)
def PointInstancer "Base" {
    int[] protoIndices = [0]
    int64[] ids = [42]
    point3f[] positions.timeSamples = {0: [(0,0,0)], 2: [(200,0,0)], 4: [(4,0,0)]}
    vector3f[] velocities.timeSamples = {0: [(2,0,0)], 2: [(200,0,0)], 4: [(2,0,0)]}
}
"#;
#[test]
fn sparse_defaults_preserve_weaker_motion_grid_and_integrate_composed_values() {
    let source = format!(
        "{BASE}\ndef PointInstancer \"I\" (references = </Base>) {{ point3f[] positions = edit [write (10,0,0) to [0]] vector3f[] velocities = edit [write (8,0,0) to [0]] }}"
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/I");
    let scene = Scene::new(live.stage(), &store);
    let q = PointInstancer::new(&scene, path).unwrap();
    let samples = q
        .compute_instance_transforms_at_times(
            &[Time::at(2.), Time::at(0.), Time::at(2.)],
            Time::at(0.),
            OPTIONS,
        )
        .unwrap();
    assert_eq!(samples[0][0].matrix[3], [14., 0., 0., 1.]);
    assert_eq!(samples[1][0].matrix[3], [10., 0., 0., 1.]);
    assert_eq!(samples[0], samples[2]);
    assert_eq!(samples[0][0].id, 42);
}
#[test]
fn sparse_sample_grids_union_and_mixed_dense_samples_mask_weaker_sources() {
    for (positions, velocities, base, time, expected) in [
        (
            "{0: edit [write (10,0,0) to [0]], 2: edit [write (20,0,0) to [0]]}",
            "{0: edit [write (8,0,0) to [0]], 2: edit [write (4,0,0) to [0]]}",
            2.,
            3.,
            21.,
        ),
        (
            "{0: [(10,0,0)], 4: edit []}",
            "{0: [(8,0,0)], 4: edit []}",
            2.,
            3.,
            16.,
        ),
    ] {
        let source = format!(
            "{BASE}\ndef PointInstancer \"I\" (references = </Base>) {{ point3f[] positions.timeSamples = {positions} vector3f[] velocities.timeSamples = {velocities} }}"
        );
        let (mut store, live) = support::scene(&source);
        let path = store.path("/I");
        let scene = Scene::new(live.stage(), &store);
        let q = PointInstancer::new(&scene, path).unwrap();
        let samples = q
            .compute_instance_transforms(Time::at(time), Time::at(base), OPTIONS)
            .unwrap();
        assert_eq!(samples[0].matrix[3], [expected, 0., 0., 1.]);
    }
}
#[test]
fn misaligned_sparse_grids_interpolate_normally_and_offsets_use_stage_time() {
    let source = format!(
        "{BASE}\ndef PointInstancer \"I\" (references = </Base>) {{ point3f[] positions.timeSamples = {{1: edit [write (10,0,0) to [0]], 3: edit [write (20,0,0) to [0]]}} }}\ndef PointInstancer \"Offset\" (references = </Base> (offset = 10; scale = -2)) {{ vector3f[] velocities = edit [write (8,0,0) to [0]] }}"
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/I");
    let offset = store.path("/Offset");
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        PointInstancer::new(&scene, path)
            .unwrap()
            .compute_instance_transforms(Time::at(2.5), Time::at(2.), OPTIONS)
            .unwrap()[0]
            .matrix[3],
        [15., 0., 0., 1.]
    );
    // In stage order the lower source sample is at 2 (local 4), then 10.
    assert_eq!(
        PointInstancer::new(&scene, offset)
            .unwrap()
            .compute_instance_transforms(Time::at(4.), Time::at(3.), OPTIONS)
            .unwrap()[0]
            .matrix[3],
        [8., 0., 0., 1.]
    );
}
#[test]
fn ordered_batches_match_cpp_motion_fixture_and_reject_partial_results() {
    let (mut store, live) =
        support::scene(include_str!("../fixtures/point_instancer_behavior.usda"));
    let path = store.path("/World/Instances");
    let scene = Scene::new(live.stage(), &store);
    let q = PointInstancer::new(&scene, path).unwrap();
    let times = [Time::at(0.), Time::at(2.), Time::at(4.), Time::at(2.)];
    let batch = q
        .compute_instance_transforms_at_times(&times, Time::at(0.), OPTIONS)
        .unwrap();
    for (&time, sample) in times.iter().zip(&batch) {
        assert_eq!(
            *sample,
            q.compute_instance_transforms(time, Time::at(0.), OPTIONS)
                .unwrap()
        );
    }
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/point_instancer_behavior.json")).unwrap();
    for (got, want) in batch[1]
        .iter()
        .zip(oracle["noPrototype"].as_array().unwrap())
    {
        for (actual, row) in got.matrix.iter().zip(want.as_array().unwrap()) {
            for (a, b) in actual.iter().zip(row.as_array().unwrap()) {
                assert!((*a - b.as_f64().unwrap()).abs() < 1e-8);
            }
        }
    }
    assert_eq!(
        q.compute_instance_transforms_at_times(
            &[Time::at(0.), Time::Default],
            Time::at(0.),
            OPTIONS
        ),
        Err(PointInstancerError::InvalidTime)
    );
    assert!(
        q.compute_instance_transforms_at_times(&[], Time::Default, OPTIONS)
            .unwrap()
            .is_empty()
    );
}
