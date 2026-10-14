// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Open/closed sample selection shares composed stage-time grids across domains.
#![allow(missing_docs, reason = "integration tests")]

#[path = "support/schema_scene.rs"]
mod support;

use core::ops::Bound::{Excluded, Included, Unbounded};
use layerstack_schemas::{
    LabelInterval, Scene, primvar::Primvar, usd_geom::Xformable, usd_skel::SkelAnimation,
};

const SOURCE: &str = r#"#usda 1.0
def Xform "Base" {
    uniform token[] xformOpOrder = ["xformOp:translate"]
    double3 xformOp:translate.timeSamples = {0: (0,0,0), 1: (1,0,0), 2: (2,0,0), 3: (3,0,0)}
    float[] primvars:test.timeSamples = {0: [0], 2: [2]}
    int[] primvars:test:indices.timeSamples = {1: [0], 3: [0]}
}
def Xform "Shape" (references = </Base> (offset = 10; scale = -2)) {}
def SkelAnimation "AnimationBase" {
    float3[] translations.timeSamples = {0: [(0,0,0)], 2: [(2,0,0)]}
    quatf[] rotations.timeSamples = {1: [(1,0,0,0)], 3: [(1,0,0,0)]}
    float[] blendShapeWeights.timeSamples = {0: [0], 1: [1], 2: [2], 3: [3]}
}
def SkelAnimation "Animation" (references = </AnimationBase> (offset = 10; scale = -2)) {}
"#;

#[test]
fn interval_bounds_select_effective_offset_grids_without_adding_brackets() {
    let (mut store, live) = support::scene(SOURCE);
    let shape = store.path("/Shape");
    let animation = store.path("/Animation");
    let transform = store.tokens.lookup("xformOp:translate").unwrap();
    let scene = Scene::new(live.stage(), &store);
    let xform = Xformable::new(&scene, shape).unwrap();
    let primvar = Primvar::new(&scene, shape, "test").unwrap();
    let skel = SkelAnimation::new(&scene, animation).unwrap();
    let attribute = live
        .stage()
        .prim(shape, &store)
        .unwrap()
        .attribute("xformOp:translate")
        .unwrap();
    for (bounds, expected) in [
        ((Unbounded, Unbounded), vec![4., 6., 8., 10.]),
        ((Included(6.), Included(8.)), vec![6., 8.]),
        ((Excluded(6.), Included(8.)), vec![8.]),
        ((Included(6.), Excluded(8.)), vec![6.]),
        ((Excluded(6.), Excluded(8.)), vec![]),
        ((Included(6.), Included(6.)), vec![6.]),
        ((Excluded(6.), Included(6.)), vec![]),
        ((Included(6.5), Included(7.5)), vec![]),
        (
            (Excluded(f64::NEG_INFINITY), Excluded(f64::INFINITY)),
            vec![4., 6., 8., 10.],
        ),
        ((Included(f64::NEG_INFINITY), Excluded(6.)), vec![4.]),
        ((Excluded(8.), Included(f64::INFINITY)), vec![10.]),
        ((Excluded(f64::INFINITY), Unbounded), vec![]),
        ((Unbounded, Excluded(f64::NEG_INFINITY)), vec![]),
        ((Included(f64::NAN), Unbounded), vec![]),
        ((Unbounded, Excluded(f64::NAN)), vec![]),
        ((Included(9.), Included(5.)), vec![]),
    ] {
        assert_eq!(
            live.stage()
                .property_sample_times_in_range(shape, transform, bounds),
            expected,
            "stage {bounds:?}"
        );
        assert_eq!(
            attribute.sample_times_in_range(bounds),
            expected,
            "attribute {bounds:?}"
        );
        assert_eq!(
            xform.transform_time_samples_in_range(bounds),
            expected,
            "xform {bounds:?}"
        );
        assert_eq!(
            primvar.sample_times_in_range(bounds),
            expected,
            "indexed primvar {bounds:?}"
        );
        assert_eq!(
            skel.joint_transform_time_samples_in_range(bounds),
            expected,
            "TRS union {bounds:?}"
        );
        assert_eq!(
            skel.blend_shape_weight_time_samples_in_range(bounds),
            expected,
            "weights {bounds:?}"
        );
    }
    assert_eq!(attribute.sample_times_in_range(..=6.), [4., 6.]);
    assert_eq!(xform.transform_time_samples_in_range(6.0..8.0), [6.]);
    assert_eq!(primvar.sample_times_in_range(8.0..), [8., 10.]);
    let labels = LabelInterval::new(6., 10., false, false).unwrap();
    assert_eq!(skel.joint_transform_time_samples_in_range(labels), [8.]);
}

#[test]
fn inclusive_helpers_keep_nan_reversed_and_infinite_bound_behavior() {
    let (mut store, live) = support::scene(SOURCE);
    let shape = store.path("/Shape");
    let animation = store.path("/Animation");
    let scene = Scene::new(live.stage(), &store);
    let xform = Xformable::new(&scene, shape).unwrap();
    let primvar = Primvar::new(&scene, shape, "test").unwrap();
    let skel = SkelAnimation::new(&scene, animation).unwrap();
    for (start, end, expected) in [
        (6., 8., vec![6., 8.]),
        (6., 6., vec![6.]),
        (8., 6., vec![]),
        (f64::NAN, 8., vec![]),
        (6., f64::NAN, vec![]),
        (f64::NEG_INFINITY, f64::INFINITY, vec![4., 6., 8., 10.]),
    ] {
        assert_eq!(
            xform.transform_time_samples_in_interval(start, end),
            expected
        );
        assert_eq!(primvar.sample_times_in_interval(start, end), expected);
        assert_eq!(
            skel.joint_transform_time_samples_in_interval(start, end),
            expected
        );
        assert_eq!(
            skel.blend_shape_weight_time_samples_in_interval(start, end),
            expected
        );
    }
}
