// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sampled camera math against GfCamera/GfFrustum, plus explicit invalid inputs.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    LayerId, PropertyPath, Value,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::{
    Scene, Time, XformCache,
    bounds::{BoundingBox, Range3d},
    camera::{CameraError, ComputedCamera},
    usd_geom::Camera,
};
use serde::Deserialize;
type Matrix = [[f64; 4]; 4];
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    path: String,
    time: Option<f64>,
    interpolation: String,
    transform: Matrix,
    view: Matrix,
    projection: Matrix,
    corners: [[f64; 3]; 8],
    window: [f64; 4],
    clipping_range: [f64; 2],
    aperture: [f32; 2],
    aperture_offset: [f32; 2],
    focal_length: f32,
    f_stop: f32,
    focus_distance: f32,
    clipping_planes: Vec<[f32; 4]>,
    shutter: [f64; 2],
    points: Vec<Point>,
    boxes: Vec<BoxRow>,
}
#[derive(Deserialize)]
struct Point {
    point: [f64; 3],
    inside: bool,
}
#[derive(Deserialize)]
struct BoxRow {
    min: [f64; 3],
    max: [f64; 3],
    matrix: Matrix,
    intersects: bool,
}
fn close(actual: impl IntoIterator<Item = f64>, expected: impl IntoIterator<Item = f64>) {
    let actual: Vec<_> = actual.into_iter().collect();
    let expected: Vec<_> = expected.into_iter().collect();
    assert_eq!(
        actual.len(),
        expected.len(),
        "camera component counts agree with OpenUSD"
    );
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (a - b).abs() < 2e-10 * b.abs().max(1.),
            "element {i}: {a} != {b}"
        );
    }
}
#[test]
fn sampled_cameras_match_openusd_including_conformed_transforms_and_culling() {
    let oracle: Oracle = serde_json::from_str(include_str!("../fixtures/camera.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, live) = support::scene(include_str!("../fixtures/camera.usda"));
    for row in oracle.rows {
        let path = store.path(&row.path);
        let scene = Scene::new(live.stage(), &store);
        let time = row.time.map_or(Time::Default, |t| {
            if row.interpolation == "held" {
                Time::held(t)
            } else {
                Time::at(t)
            }
        });
        let camera = Camera::new(&scene, path)
            .unwrap()
            .compute_camera(&mut XformCache::new(time))
            .unwrap();
        eprintln!("{} {time:?}", row.path);
        close(
            camera.camera_to_world().iter().flatten().copied(),
            row.transform.into_iter().flatten(),
        );
        close(
            camera.view_matrix().iter().flatten().copied(),
            row.view.into_iter().flatten(),
        );
        close(
            camera.projection_matrix().iter().flatten().copied(),
            row.projection.into_iter().flatten(),
        );
        close(
            camera.frustum_corners().into_iter().flatten(),
            row.corners.into_iter().flatten(),
        );
        close(camera.window(), row.window);
        assert_eq!(camera.clipping_range(), row.clipping_range);
        assert_eq!(camera.aperture(), row.aperture);
        assert_eq!(camera.aperture_offset(), row.aperture_offset);
        assert_eq!(camera.focal_length(), row.focal_length);
        assert_eq!(camera.f_stop(), row.f_stop);
        assert_eq!(camera.focus_distance(), row.focus_distance);
        assert_eq!(camera.clipping_planes(), row.clipping_planes);
        assert_eq!(camera.shutter_offsets(), row.shutter);
        assert_eq!(
            camera.shutter_interval(24.).unwrap(),
            row.shutter.map(|v| v + 24.)
        );
        for p in row.points {
            assert_eq!(
                camera.contains_world_point(p.point),
                p.inside,
                "{} {:?}",
                row.path,
                p.point
            );
        }
        for b in row.boxes {
            assert_eq!(
                camera.intersects_world_bound(&BoundingBox {
                    range: Range3d {
                        min: b.min,
                        max: b.max
                    },
                    matrix: b.matrix
                }),
                b.intersects
            );
        }
        assert!(!camera.intersects_world_bound(&BoundingBox::default()));
        for p in camera.frustum_planes() {
            close([p[0] * p[0] + p[1] * p[1] + p[2] * p[2]], [1.]);
        }
    }
}
fn compute(attributes: &str, time: Time) -> Result<ComputedCamera, CameraError> {
    let source = format!("#usda 1.0\ndef Camera \"C\" {{ {attributes} }}");
    let (mut store, live) = support::scene(&source);
    let path = store.path("/C");
    Camera::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .compute_camera(&mut XformCache::new(time))
}
#[test]
fn invalid_parameters_error_instead_of_returning_partial_frusta() {
    for (text, error) in [
        (
            "token projection = \"fisheye\"",
            CameraError::UnsupportedProjection,
        ),
        (
            "float horizontalAperture = 0",
            CameraError::InvalidAttribute("aperture"),
        ),
        (
            "float verticalAperture = nan",
            CameraError::InvalidAttribute("verticalAperture"),
        ),
        (
            "float focalLength = -1",
            CameraError::InvalidAttribute("focalLength"),
        ),
        (
            "float2 clippingRange = (1,1)",
            CameraError::InvalidAttribute("clippingRange"),
        ),
        (
            "float2 clippingRange = (-1,100)",
            CameraError::InvalidAttribute("clippingRange"),
        ),
        (
            "float2 clippingRange = (1,inf)",
            CameraError::InvalidAttribute("clippingRange"),
        ),
        (
            "double shutter:close = nan",
            CameraError::InvalidAttribute("shutter:close"),
        ),
        (
            "float3 xformOp:scale = (1,0,1) uniform token[] xformOpOrder = [\"xformOp:scale\"]",
            CameraError::InvalidTransform,
        ),
        (
            "double3 xformOp:translate = (nan,0,0) uniform token[] xformOpOrder = [\"xformOp:translate\"]",
            CameraError::InvalidTransform,
        ),
    ] {
        assert!(
            matches!(compute(text, Time::Default), Err(actual) if actual == error),
            "{text}: expected {error:?}"
        );
    }
    assert_eq!(
        compute("", Time::at(f64::INFINITY)).unwrap_err(),
        CameraError::InvalidTime
    );
}
#[test]
fn distant_far_planes_remain_finite_and_orthographic_near_can_be_negative() {
    let camera = compute("float2 clippingRange = (1,1e30)", Time::Default).unwrap();
    assert!(
        camera
            .frustum_planes()
            .iter()
            .flatten()
            .all(|v| v.is_finite())
    );
    assert!(camera.contains_world_point([0., 0., -1e20]));
    assert!(!camera.contains_world_point([0., 0., -2e30]));
    assert!(!camera.contains_world_point([0., 0., 0.]));
    let ortho = compute(
        "token projection = \"orthographic\" float2 clippingRange = (-2,10)",
        Time::Default,
    )
    .unwrap();
    assert!(ortho.contains_world_point([0., 0., 1.]));
    assert!(!ortho.contains_world_point([0., 0., 3.]));
}
#[test]
fn edits_and_undo_refresh_lens_and_shared_ancestor_transforms() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/camera.usda"));
    let path = store.path("/Rig/Perspective");
    let rig = store.path("/Rig");
    let mut xforms = XformCache::new(Time::Default);
    let before = Camera::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .compute_camera(&mut xforms)
        .unwrap();
    let mut t = Transaction::new();
    for (path, name, value) in [
        (path, "horizontalAperture", Value::Float(72.)),
        (rig, "xformOp:rotateXYZ", Value::Vec3f([0., 0., 0.])),
    ] {
        let p = PropertyPath::new(path, store.tokens.intern(name));
        t.set_default(EditTarget::for_layer(LayerId(1)).property(p), value);
    }
    let edit = live.apply(&mut store, &t).unwrap();
    let scene = Scene::new(live.stage(), &store);
    xforms.apply_changes(&scene, &edit.changes);
    let changed = Camera::new(&scene, path)
        .unwrap()
        .compute_camera(&mut xforms)
        .unwrap();
    assert_ne!(before.view_matrix(), changed.view_matrix());
    assert_eq!(changed.aperture()[0], 72.);
    assert_ne!(before.projection_matrix(), changed.projection_matrix());
    let undo = live.apply(&mut store, &edit.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    xforms.apply_changes(&scene, &undo.changes);
    let restored = Camera::new(&scene, path)
        .unwrap()
        .compute_camera(&mut xforms)
        .unwrap();
    assert_eq!(restored.view_matrix(), before.view_matrix());
    assert_eq!(restored.projection_matrix(), before.projection_matrix());
}
#[test]
fn shutter_intervals_preserve_sampling_policy_and_reversed_exposure() {
    let source = "double shutter:open.timeSamples = {0: -0.5, 2: 0.5} double shutter:close.timeSamples = {0: 0.5, 2: -0.5}";
    let linear = compute(source, Time::at(1.)).unwrap();
    assert_eq!(linear.shutter_interval(24.).unwrap(), [24., 24.]);
    let held = compute(source, Time::held(1.)).unwrap();
    assert_eq!(held.shutter_interval(24.).unwrap(), [23.5, 24.5]);
    let reversed = compute(source, Time::at(2.)).unwrap();
    assert_eq!(reversed.shutter_interval(24.).unwrap(), [24.5, 23.5]);
    assert_eq!(
        held.shutter_interval(f64::NAN),
        Err(CameraError::InvalidTime)
    );
    assert!(!held.contains_world_point([f64::NAN, 0., 0.]));
}
#[test]
fn pinned_camera_oracle_matches_configured_openusd() {
    let Ok(python) = std::env::var("LAYERSTACK_USD_PYTHON") else {
        return;
    };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/camera_oracle.py"
        ))
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
