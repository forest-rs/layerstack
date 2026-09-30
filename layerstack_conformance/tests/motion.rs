// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Inherited motion settings through arbitrary prim types against OpenUSD.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::{Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{Scene, Time, usd_geom::MotionApi};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    results: Vec<ResultAtTime>,
}

#[derive(Deserialize)]
struct ResultAtTime {
    path: String,
    time: Option<f64>,
    interpolation: String,
    blur: f32,
    count: i32,
    velocity: f32,
}

#[test]
fn inherited_motion_matches_openusd() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/motion/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/motion/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let schemas = Arc::new(layerstack_schemas::openusd(&mut loaded.store.tokens));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let mut divergences = 0;
    let missing = loaded.store.path("/Root/Absent");
    let scene = Scene::new(&stage, &loaded.store);
    assert_eq!(scene.compute_motion_blur_scale(missing, Time::Default), 1.0);
    assert_eq!(
        scene.compute_nonlinear_sample_count(missing, Time::Default),
        3
    );
    assert_eq!(scene.compute_velocity_scale(missing, Time::Default), 1.0);
    for mut expected in oracle.results {
        // AOUSD Core §12.3.6 and §16.2.16.3 preserve the schema fallback
        // after sampled blocks. OpenUSD 26.8 instead fails the value read,
        // so its inheritance walk reaches /Root. See the named divergence
        // `sampled-block-drops-fallback` in docs/generic-sparse-composition.md.
        if expected.path == "/Root/SampleBlock" && expected.time.is_some_and(|t| t < 3.0) {
            assert_eq!(
                (expected.blur, expected.count, expected.velocity),
                (2.0, 7, -2.0)
            );
            (expected.blur, expected.count, expected.velocity) = (1.0, 3, 1.0);
            divergences += 1;
        }
        let path = loaded.store.path(&expected.path);
        let scene = Scene::new(&stage, &loaded.store);
        let time = expected.time.map_or(Time::Default, |t| {
            if expected.interpolation == "held" {
                Time::held(t)
            } else {
                Time::at(t)
            }
        });
        assert_eq!(
            scene.compute_motion_blur_scale(path, time),
            expected.blur,
            "blur {} {time:?}",
            expected.path
        );
        assert_eq!(
            scene.compute_nonlinear_sample_count(path, time),
            expected.count,
            "count {} {time:?}",
            expected.path
        );
        assert_eq!(
            scene.compute_velocity_scale(path, time),
            expected.velocity,
            "velocity {} {time:?}",
            expected.path
        );
        if let Some(api) = MotionApi::get(&scene, path) {
            assert_eq!(api.compute_motion_blur_scale(time), expected.blur);
            assert_eq!(api.compute_nonlinear_sample_count(time), expected.count);
            assert_eq!(api.compute_velocity_scale(time), expected.velocity);
        }
    }
    assert_eq!(divergences, 6);
}
