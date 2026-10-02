// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Inverse-transpose normals, corner mapping and animation introspection.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::Time;
use layerstack_schemas::{
    Scene,
    skel::{
        InfluenceInterpolation, JointInfluences, SkelError, SkinningQuery,
        rigid_skinning_transform, skin_face_varying_normals, skin_normals_in_place,
    },
    usd_skel::SkelAnimation,
};
const ID: [[f64; 4]; 4] = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];
fn vectors<const N: usize>(actual: &[[f32; N]], expected: &serde_json::Value) {
    assert_eq!(
        actual.len(),
        expected.as_array().unwrap().len(),
        "normal count matches OpenUSD"
    );
    for (row, want) in actual.iter().zip(expected.as_array().unwrap()) {
        for (v, w) in row.iter().zip(want.as_array().unwrap()) {
            assert!(
                (f64::from(*v) - w.as_f64().unwrap()).abs() < 3e-6,
                "{v} != {w}"
            );
        }
    }
}
fn matrix(actual: &[[f64; 4]; 4], expected: &serde_json::Value) {
    for (row, want) in actual.iter().zip(expected.as_array().unwrap()) {
        for (v, w) in row.iter().zip(want.as_array().unwrap()) {
            assert!((*v - w.as_f64().unwrap()).abs() < 3e-6, "{v} != {w}");
        }
    }
}
#[test]
fn normals_rigid_transforms_and_standalone_animation_match_cpp() {
    let (mut store, live) = support::scene(include_str!("../fixtures/skel_normals.usda"));
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_normals.json")).unwrap();
    let paths: Vec<_> = expected["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| store.path(r["path"].as_str().unwrap()))
        .collect();
    let animation = store.path("/Rig/Animation");
    let scene = Scene::new(live.stage(), &store);
    for (row, path) in expected["frames"].as_array().unwrap().iter().zip(paths) {
        let q = SkinningQuery::new(&scene, path).unwrap().unwrap();
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        vectors(&q.compute_skinned_normals(time).unwrap(), &row["normals"]);
        if row.get("transform").is_some() {
            matrix(&q.compute_rigid_transform(time).unwrap(), &row["transform"]);
        }
    }
    let anim = SkelAnimation::new(&scene, animation).unwrap();
    assert_eq!(
        serde_json::json!(anim.joint_transform_time_samples()),
        expected["joint_samples"]
    );
    assert_eq!(
        serde_json::json!(anim.blend_shape_weight_time_samples()),
        expected["weight_samples"]
    );
    assert_eq!(
        serde_json::json!(anim.joint_transforms_might_be_time_varying()),
        expected["joint_varying"]
    );
    assert_eq!(
        serde_json::json!(anim.blend_shape_weights_might_be_time_varying()),
        expected["weight_varying"]
    );
    assert_eq!(anim.joint_transform_attributes().len(), 3);
    for row in expected["poses"].as_array().unwrap() {
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        let actual = anim.compute_joint_local_transforms(time).unwrap().unwrap();
        for (m, want) in actual.iter().zip(row["local"].as_array().unwrap()) {
            matrix(m, want);
        }
        assert_eq!(
            serde_json::json!(anim.compute_blend_shape_weights(time).unwrap().unwrap()),
            row["weights"]
        );
    }
}
#[test]
fn invalid_normal_inputs_leave_reusable_buffers_unchanged() {
    let inf = JointInfluences {
        indices: &[0],
        weights: &[1.],
        element_size: 1,
        interpolation: InfluenceInterpolation::Constant,
    };
    let original = [[1., 1., 0.], [0., 0., 0.]];
    let mut buffer = original;
    let mut singular = ID;
    singular[0][0] = 0.;
    assert_eq!(
        skin_normals_in_place(&ID, &[singular], inf, &mut buffer),
        Err(SkelError::SingularNormalTransform { joint: Some(0) })
    );
    assert_eq!(buffer, original);
    assert!(
        skin_normals_in_place(
            &ID,
            &[ID],
            JointInfluences {
                indices: &[-1],
                ..inf
            },
            &mut buffer
        )
        .is_err()
    );
    assert_eq!(buffer, original);
    skin_normals_in_place(&ID, &[ID], inf, &mut buffer).unwrap();
    assert_eq!(buffer[1], [0.; 3]);
    assert!((buffer[0][0] - core::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    assert!(skin_face_varying_normals(&ID, &[ID], inf, 1, &[0, 1], &original).is_err());
    assert!(skin_face_varying_normals(&ID, &[ID], inf, 1, &[0], &original).is_err());
    assert!(
        rigid_skinning_transform(
            &ID,
            &[ID],
            JointInfluences {
                interpolation: InfluenceInterpolation::Vertex,
                ..inf
            }
        )
        .is_err()
    );
    assert_eq!(rigid_skinning_transform(&ID, &[ID], inf).unwrap(), ID);
}

#[test]
fn sample_helpers_apply_offsets_masking_and_single_sample_variability() {
    for (body, expected) in [
        ("", vec![4., 6., 8.]),
        ("float3[] translations = [(0,0,0),(0,0,0)]", vec![6.]),
    ] {
        let text = format!(
            "{}\ndef SkelAnimation \"Offset\" (references = </Rig/Animation> (offset = 10; scale = -2)) {{\n{body}\n}}\n",
            include_str!("../fixtures/skel_normals.usda")
        );
        let (mut store, live) = support::scene(&text);
        let path = store.path("/Offset");
        let scene = Scene::new(live.stage(), &store);
        let anim = SkelAnimation::new(&scene, path).unwrap();
        assert_eq!(anim.joint_transform_time_samples(), expected);
        assert_eq!(anim.joint_transform_time_samples_in_interval(5., 7.), [6.]);
        assert_eq!(anim.blend_shape_weight_time_samples(), [4., 8.]);
        assert!(
            anim.joint_transform_time_samples_in_interval(7., 5.)
                .is_empty()
        );
        assert!(
            anim.blend_shape_weight_time_samples_in_interval(f64::NAN, 8.)
                .is_empty()
        );
        if !body.is_empty() {
            assert!(!anim.joint_transforms_might_be_time_varying());
        }
    }
}
#[test]
fn unavailable_and_malformed_animation_components_are_distinct() {
    let text = include_str!("../fixtures/skel_normals.usda").replace(
        "half3[] scales = [(2,0.5,1), (1,3,0.5)]",
        "half3[] scales = None",
    );
    let (mut store, live) = support::scene(&text);
    let path = store.path("/Rig/Animation");
    let scene = Scene::new(live.stage(), &store);
    assert!(
        SkelAnimation::new(&scene, path)
            .unwrap()
            .compute_joint_local_transforms(Time::Default)
            .unwrap()
            .is_none()
    );
    let text = include_str!("../fixtures/skel_normals.usda").replace(
        "half3[] scales = [(2,0.5,1), (1,3,0.5)]",
        "half3[] scales = [(1,1,1)]",
    );
    let (mut store, live) = support::scene(&text);
    let path = store.path("/Rig/Animation");
    let scene = Scene::new(live.stage(), &store);
    assert!(matches!(
        SkelAnimation::new(&scene, path)
            .unwrap()
            .compute_joint_local_transforms(Time::Default),
        Err(SkelError::ArrayLength {
            property: "scales",
            ..
        })
    ));
}
