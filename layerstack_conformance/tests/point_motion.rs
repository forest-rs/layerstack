// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ordinary point motion against the C++ reference and shared sparse motion sources.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{Scene, Time, point_motion::PointMotionError, usd_geom::PointBased};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    path: String,
    base: Option<f64>,
    times: Vec<Option<f64>>,
    points: Option<Vec<Vec<[f32; 3]>>>,
}
#[test]
fn batch_point_motion_matches_openusd() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/point_motion/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, live) = support::scene(include_str!("../fixtures/point_motion/scene.usda"));
    for row in oracle.rows {
        let path = store.path(&row.path);
        let scene = Scene::new(live.stage(), &store);
        let q = PointBased::new(&scene, path).unwrap();
        let base = row.base.map_or(Time::Default, Time::at);
        let times: Vec<_> = row
            .times
            .into_iter()
            .map(|t| t.map_or(Time::Default, Time::at))
            .collect();
        let actual = q.compute_points_at_times(&times, base);
        match row.points {
            Some(expected) => assert_eq!(actual.unwrap(), expected, "{} base {:?}", row.path, base),
            None => assert!(actual.is_err(), "{} base {:?}", row.path, base),
        }
    }
}
#[test]
fn sparse_motion_reads_compose_before_integration_and_reject_mixed_times() {
    let (mut store, live) = support::scene(
        "#usda 1.0\n(timeCodesPerSecond = 4)\ndef Points \"Base\" {\n point3f[] points.timeSamples = {0:[(0,0,0)],2:[(20,0,0)]}\n vector3f[] velocities.timeSamples = {0:[(8,0,0)],2:[(8,0,0)]}\n}\ndef Points \"P\" (references = </Base>) {\n point3f[] points = edit [write (10,0,0) to [0]]\n}\n",
    );
    let path = store.path("/P");
    let scene = Scene::new(live.stage(), &store);
    let q = PointBased::new(&scene, path).unwrap();
    let inputs = q.motion_inputs(Time::at(0.)).unwrap();
    assert_eq!(inputs.points, [[10., 0., 0.]]);
    assert_eq!(inputs.evaluate(Time::at(1.)).unwrap(), [[12., 0., 0.]]);
    assert_eq!(
        q.compute_points_at_time(Time::at(1.), Time::at(0.))
            .unwrap(),
        [[12., 0., 0.]]
    );
    assert_eq!(
        q.compute_points_at_times(&[Time::Default], Time::at(0.)),
        Err(PointMotionError::InvalidTime)
    );
    assert_eq!(
        q.compute_points_at_time(Time::at(f64::NAN), Time::at(0.)),
        Err(PointMotionError::InvalidTime)
    );
    let mut bad = inputs;
    bad.velocities.push([0.; 3]);
    assert_eq!(
        bad.evaluate(Time::at(1.)),
        Err(PointMotionError::LengthMismatch)
    );
}

#[test]
fn velocity_sample_alignment_uses_cpp_absolute_epsilon_tolerance() {
    for (offset, expected) in [(1e-16, 1.), (f64::EPSILON, 50.), (3e-16, 50.)] {
        let (mut store, live) = support::scene(&format!(
            "#usda 1.0\n(timeCodesPerSecond = 24)\ndef Points \"P\" {{\n point3f[] points.timeSamples = {{0:[(0,0,0)],2:[(100,0,0)]}}\n vector3f[] velocities.timeSamples = {{{offset}:[(24,0,0)],2:[(24,0,0)]}}\n}}\n"
        ));
        let path = store.path("/P");
        let scene = Scene::new(live.stage(), &store);
        let q = PointBased::new(&scene, path).unwrap();
        assert_eq!(
            q.compute_points_at_time(Time::at(1.), Time::at(1.))
                .unwrap(),
            [[expected, 0., 0.]],
            "offset {offset}"
        );
    }
}
