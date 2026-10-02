// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Animation introspection follows sparse composition and dense masking.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{Scene, Time, usd_skel::SkelAnimation};
const BASE: &str = r#"#usda 1.0
def SkelAnimation "Base" {
    uniform token[] joints = ["a", "b"]
    uniform token[] blendShapes = ["smile", "blink"]
    float3[] translations.timeSamples = {0: [(0,0,0),(0,0,0)], 2: [(0,0,0),(20,0,0)]}
    quatf[] rotations.timeSamples = {0: [(1,0,0,0),(1,0,0,0)], 2: [(1,0,0,0),(0,0,0,1)]}
    half3[] scales.timeSamples = {0: [(1,1,1),(1,1,1)], 2: [(1,1,1),(2,3,4)]}
    float[] blendShapeWeights.timeSamples = {0: [0,0], 2: [0,1]}
}
"#;
const FIXED_RS: &str = r#"quatf[] rotations = [(1,0,0,0),(1,0,0,0)]
half3[] scales = [(1,1,1),(1,1,1)]"#;
#[test]
fn sparse_defaults_expose_weaker_trs_and_weight_grids_in_stage_time() {
    for (offset, times, first, last) in [
        ("", [0., 2.], 0., 2.),
        ("(offset = 10; scale = -2)", [6., 10.], 10., 6.),
    ] {
        let source = format!(
            r#"{BASE}
def SkelAnimation "Sparse" (references = </Base> {offset}) {{
    float3[] translations = edit [write (5,0,0) to [0]]
    quatf[] rotations = edit [write (1,0,0,0) to [0]]
    half3[] scales = edit [write (1,1,1) to [0]]
    float[] blendShapeWeights = edit [write 0.5 to [0]]
}}"#
        );
        let (mut store, live) = support::scene(&source);
        let path = store.path("/Sparse");
        let scene = Scene::new(live.stage(), &store);
        let a = SkelAnimation::new(&scene, path).unwrap();
        assert_eq!(
            a.joint_transform_time_samples(),
            times,
            "sparse defaults preserve weaker TRS grids"
        );
        assert_eq!(
            a.blend_shape_weight_time_samples(),
            times,
            "sparse weights preserve weaker grids"
        );
        assert!(a.joint_transforms_might_be_time_varying());
        assert!(a.blend_shape_weights_might_be_time_varying());
        let first = a
            .compute_joint_local_transform_components(Time::at(first))
            .unwrap()
            .unwrap();
        let last = a
            .compute_joint_local_transform_components(Time::at(last))
            .unwrap()
            .unwrap();
        assert_eq!(first.translations, [[5., 0., 0.], [0., 0., 0.]]);
        assert_eq!(last.translations, [[5., 0., 0.], [20., 0., 0.]]);
        assert_eq!(first.rotations[1], [0., 0., 0., 1.]);
        assert_eq!(last.rotations[1], [0., 0., 1., 0.]);
        assert_eq!(last.scales[1], [2., 3., 4.]);
        assert_eq!(
            a.compute_blend_shape_weights(Time::at(times[0]))
                .unwrap()
                .unwrap()[0],
            0.5
        );
        assert_eq!(
            a.joint_transform_time_samples_in_interval(times[0], times[0]),
            [times[0]]
        );
    }
}
#[test]
fn sparse_samples_union_grids_but_dense_regions_and_blocks_mask_weaker_times() {
    for (translations, weights, times, varying) in [
        (
            ".timeSamples = {1: edit [write (5,0,0) to [0]], 3: edit [write (7,0,0) to [0]]}",
            ".timeSamples = {1: edit [write 0.5 to [0]], 3: edit [write 0.7 to [0]]}",
            vec![0., 1., 2., 3.],
            true,
        ),
        (
            ".timeSamples = {0: [(0,0,0),(0,0,0)], 4: edit []}",
            ".timeSamples = {0: [0,0], 4: edit []}",
            vec![0., 4.],
            true,
        ),
        ("= [(0,0,0),(0,0,0)]", "= [0,0]", vec![], false),
        ("= None", "= None", vec![], false),
        (
            ".timeSamples = {1: [(0,0,0),(0,0,0)], 3: [(1,0,0),(1,0,0)]}",
            ".timeSamples = {1: [0,0], 3: [1,1]}",
            vec![1., 3.],
            true,
        ),
    ] {
        let source = format!(
            "{BASE}\ndef SkelAnimation \"Sparse\" (references = </Base>) {{\n{FIXED_RS}\nfloat3[] translations{translations}\nfloat[] blendShapeWeights{weights}\n}}"
        );
        let (mut store, live) = support::scene(&source);
        let path = store.path("/Sparse");
        let scene = Scene::new(live.stage(), &store);
        let a = SkelAnimation::new(&scene, path).unwrap();
        assert_eq!(a.joint_transform_time_samples(), times, "{translations}");
        assert_eq!(a.blend_shape_weight_time_samples(), times, "{weights}");
        assert_eq!(a.joint_transforms_might_be_time_varying(), varying);
        assert_eq!(a.blend_shape_weights_might_be_time_varying(), varying);
    }
}
#[test]
fn sparse_edits_stop_at_intermediate_dense_sources_and_single_samples_are_constant() {
    for (middle, sparse, times, varying) in [
        (
            "float3[] translations = [(0,0,0),(0,0,0)]",
            "float3[] translations.timeSamples = {1: edit [write (5,0,0) to [0]], 3: edit [write (7,0,0) to [0]]}",
            vec![1., 3.],
            true,
        ),
        (
            "float3[] translations.timeSamples = {2: [(0,0,0),(0,0,0)]}",
            "float3[] translations.timeSamples = {0: edit [write (5,0,0) to [0]]}",
            vec![0., 2.],
            false,
        ),
    ] {
        let source = format!(
            "{BASE}\ndef SkelAnimation \"Middle\" (references = </Base>) {{\n{middle}\n}}\ndef SkelAnimation \"Sparse\" (references = </Middle>) {{\n{FIXED_RS}\n{sparse}\n}}"
        );
        let (mut store, live) = support::scene(&source);
        let path = store.path("/Sparse");
        let scene = Scene::new(live.stage(), &store);
        let a = SkelAnimation::new(&scene, path).unwrap();
        assert_eq!(a.joint_transform_time_samples(), times);
        assert_eq!(a.joint_transforms_might_be_time_varying(), varying);
    }
}
