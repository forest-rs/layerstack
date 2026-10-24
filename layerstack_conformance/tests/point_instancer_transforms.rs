// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point-instance transforms agree with C++ for every option pair.
//!
//! `fixtures/point_instancer_transforms/oracle.json` is what
//! `scripts/point_instancer_transforms_oracle.py` records with OpenUSD's
//! `ComputeInstanceTransformsAtTime` and `ComputeInstanceTransformsAtTimes`:
//! prototype transforms included or excluded, the mask applied or ignored,
//! velocity, acceleration and angular velocity extrapolation, interpolation
//! that carries arrays from sample to sample, and prototype targets that are
//! not prims. OpenUSD's bindings return an empty array for a refused
//! computation, so a refusal here compares as empty.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{
    Scene, Time,
    point_instancer::{InstanceTransform, InstanceTransformOptions},
    usd_geom::PointInstancer,
};
use serde::Deserialize;
use std::collections::BTreeMap;

const SCENE: &str = include_str!("../fixtures/point_instancer_transforms/scene.usda");

type Matrices = Vec<[[f64; 4]; 4]>;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    cases: Vec<Case>,
    batches: Vec<Batch>,
}
#[derive(Deserialize)]
struct Case {
    case: (String, Option<f64>, Option<f64>),
    transforms: BTreeMap<String, Option<Matrices>>,
}
#[derive(Deserialize)]
struct Batch {
    batch: (String, Vec<Option<f64>>, Option<f64>),
    transforms: BTreeMap<String, Option<Vec<Matrices>>>,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(include_str!(
        "../fixtures/point_instancer_transforms/oracle.json"
    ))
    .unwrap();
    assert_eq!(
        oracle.openusd_version,
        layerstack_schemas::OPENUSD_VERSION,
        "the oracle was recorded with this OpenUSD release"
    );
    oracle
}

fn time(code: Option<f64>) -> Time {
    code.map_or(Time::Default, Time::at)
}

fn options(label: &str) -> InstanceTransformOptions {
    let (include, mask) = match label {
        "include_mask" => (true, true),
        "include_all" => (true, false),
        "exclude_mask" => (false, true),
        "exclude_all" => (false, false),
        other => panic!("unknown options {other}"),
    };
    InstanceTransformOptions {
        include_prototype_transform: include,
        apply_mask: mask,
    }
}

fn agree(actual: &[InstanceTransform], expected: &Matrices, what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: instance count");
    for (instance, expected) in actual.iter().zip(expected) {
        for (row, expected) in instance.matrix.iter().zip(expected) {
            for (value, expected) in row.iter().zip(expected) {
                assert!(
                    (value - expected).abs() <= 1e-10 * expected.abs().max(1.0),
                    "{what}: instance {} {:?} != {expected:?}",
                    instance.index,
                    instance.matrix
                );
            }
        }
    }
}

#[test]
fn instance_transforms_match_cpp_for_every_option_pair() {
    let oracle = oracle();
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for case in &oracle.cases {
        let (path, at, base) = &case.case;
        let instancer = PointInstancer::new(&scene, store.path(path)).unwrap();
        for (label, expected) in &case.transforms {
            let expected = expected.as_ref().expect("transforms raise no coding error");
            let actual = instancer
                .compute_instance_transforms(time(*at), time(*base), options(label))
                .unwrap_or_default();
            agree(&actual, expected, &format!("{:?} {label}", case.case));
        }
    }
}

#[test]
fn batches_read_their_base_once_and_carry_interpolated_arrays() {
    let oracle = oracle();
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for batch in &oracle.batches {
        let (path, times, base) = &batch.batch;
        let instancer = PointInstancer::new(&scene, store.path(path)).unwrap();
        let times: Vec<Time> = times.iter().copied().map(time).collect();
        for (label, expected) in &batch.transforms {
            let expected = expected.as_ref().expect("transforms raise no coding error");
            let actual = instancer
                .compute_instance_transforms_at_times(&times, time(*base), options(label))
                .unwrap_or_default();
            assert_eq!(actual.len(), expected.len(), "{:?} {label}", batch.batch);
            for (i, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                agree(
                    actual,
                    expected,
                    &format!("{:?} {label} sample {i}", batch.batch),
                );
            }
        }
    }
}
