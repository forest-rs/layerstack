// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point-instance transforms and extents agree with C++.
//!
//! `fixtures/point_instancer_transforms/oracle.json` is what
//! `scripts/point_instancer_transforms_oracle.py` records with OpenUSD's
//! `ComputeInstanceTransformsAtTime` and `ComputeInstanceTransformsAtTimes`:
//! prototype transforms included or excluded, the mask applied or ignored,
//! velocity, acceleration and angular velocity extrapolation, interpolation
//! that carries arrays from sample to sample, and prototype targets that are
//! not prims; and `ComputeExtentAtTime(s)`, with a transform through
//! `UsdGeomBoundable::ComputeExtentFromPlugins`. OpenUSD's bindings return
//! an empty array for a refused computation (and the oracle records `null`
//! for one that raises a coding error), so a refusal here compares as
//! empty.
use crate::support::schema_scene as support;
use layerstack_schemas::{
    Scene, Time,
    bounds::Extent,
    point_instancer::{InstanceTransform, InstanceTransformOptions},
    usd_geom::PointInstancer,
};
use serde::Deserialize;
use std::collections::BTreeMap;

const SCENE: &str = include_str!("../../fixtures/point_instancer_transforms/scene.usda");

type Matrices = Vec<[[f64; 4]; 4]>;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    cases: Vec<Case>,
    batches: Vec<Batch>,
    matrix: [[f64; 4]; 4],
    transformed: Vec<Transformed>,
}
#[derive(Deserialize)]
struct Case {
    case: (String, Option<f64>, Option<f64>),
    transforms: BTreeMap<String, Option<Matrices>>,
    extent: Option<Vec<[f64; 3]>>,
}
#[derive(Deserialize)]
struct Batch {
    batch: (String, Vec<Option<f64>>, Option<f64>),
    transforms: BTreeMap<String, Option<Vec<Matrices>>>,
    extents: Option<Vec<Vec<[f64; 3]>>>,
}
#[derive(Deserialize)]
struct Transformed {
    entry: (String, Option<f64>),
    extent: Option<Vec<[f64; 3]>>,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(include_str!(
        "../../fixtures/point_instancer_transforms/oracle.json"
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

/// A computed extent against OpenUSD's: a refusal (an error here) is an
/// empty array or `null` there; an extent is two `float3` corners.
fn same_extent<E: core::fmt::Debug>(
    actual: Result<Extent, E>,
    expected: Option<&Vec<[f64; 3]>>,
    what: &str,
) {
    match (actual, expected.filter(|corners| !corners.is_empty())) {
        (Err(_), None) => {}
        (Ok(actual), Some(expected)) => {
            assert_eq!(expected.len(), 2, "{what}");
            for (corner, expected) in actual.iter().zip(expected) {
                for (&value, &expected) in corner.iter().zip(expected) {
                    assert!(
                        (f64::from(value) - expected).abs() <= 1e-6 * expected.abs().max(1.0),
                        "{what}: {actual:?} != {expected:?}"
                    );
                }
            }
        }
        (actual, expected) => panic!("{what}: {actual:?} against {expected:?}"),
    }
}

#[test]
fn extents_match_cpp_alone_in_batches_and_transformed() {
    let oracle = oracle();
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    let instancer = |path: &str| PointInstancer::new(&scene, store.path(path)).unwrap();
    for case in &oracle.cases {
        let (path, at, base) = &case.case;
        same_extent(
            instancer(path).compute_extent(time(*at), time(*base), None),
            case.extent.as_ref(),
            &format!("{:?}", case.case),
        );
    }
    for batch in &oracle.batches {
        let (path, times, base) = &batch.batch;
        let times: Vec<Time> = times.iter().copied().map(time).collect();
        let actual = instancer(path).compute_extent_at_times(&times, time(*base), None);
        let expected = batch.extents.as_ref();
        match (actual, expected) {
            (Ok(actual), Some(expected)) if actual.len() == expected.len() => {
                for (i, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
                    same_extent::<()>(
                        Ok(actual),
                        Some(expected),
                        &format!("{:?} {i}", batch.batch),
                    );
                }
            }
            (Err(_), None) => {}
            (Err(_), Some(expected)) if expected.is_empty() && !times.is_empty() => {}
            (actual, expected) => panic!("{:?}: {actual:?} against {expected:?}", batch.batch),
        }
    }
    for entry in &oracle.transformed {
        let (path, at) = &entry.entry;
        same_extent(
            instancer(path).compute_extent(time(*at), time(*at), Some(&oracle.matrix)),
            entry.extent.as_ref(),
            &format!("{:?}", entry.entry),
        );
    }
}
