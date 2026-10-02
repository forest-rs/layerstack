// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prepared inputs reproduce pinned USD deformation without private binding logic.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::Time;
use layerstack_schemas::{
    Scene,
    skel::{SkinningMethod, SkinningQuery, apply_blend_shape_in_place, skin_points_with_method},
};

const IDENTITY: [[f64; 4]; 4] = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];

#[test]
fn public_binding_and_shape_samples_reconstruct_the_cpp_oracle() {
    let (mut store, live) = support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let query = SkinningQuery::new(&scene, path).unwrap().unwrap();
    assert_eq!(
        query.joint_mapping(),
        Some(&[Some(2), Some(0), Some(1)][..])
    );
    let shapes = query.blend_shape_query().unwrap();
    assert_eq!(
        shapes
            .samples(0)
            .unwrap()
            .map(|s| s.knot_weight)
            .collect::<Vec<_>>(),
        [0., 0.5, 1.]
    );
    let half = shapes.sample(0, 1).unwrap();
    assert_eq!(half.point_indices, [1]);
    assert_eq!(half.offsets, [[0., 1., 1.]]);
    assert_eq!(half.normal_offsets, [[0., 0.2, 0.1]]);
    assert!(shapes.sample(0, 0).unwrap().offsets.is_empty());
    assert!(shapes.sample(100, 0).is_none());
    assert!(shapes.samples(100).is_none());
    assert!(shapes.sample(0, 100).is_none());
    shapes.validate_point_count(2).unwrap();
    assert!(shapes.validate_point_count(1).is_err());
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_blend_shapes.json")).unwrap();
    for row in expected["frames"].as_array().unwrap() {
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        let binding = query.binding_inputs(time).unwrap();
        assert_eq!(binding.skinning_method(), SkinningMethod::ClassicLinear);
        binding
            .validate(2, query.joint_mapping().unwrap().len())
            .unwrap();
        assert_eq!(binding.influences().indices, [0, 2, 1, 2]);
        let palette = query.skeleton_query().skinning_transforms(time).unwrap();
        let mapped: Vec<_> = query
            .joint_mapping()
            .unwrap()
            .iter()
            .map(|i| i.map_or(IDENTITY, |i| palette[i]))
            .collect();
        let weights = query
            .skeleton_query()
            .blend_shape_weights(time, shapes.names())
            .unwrap();
        let mut points = [[1., 0., 0.], [0., 2., 0.]];
        for contribution in shapes.compute_weights(&weights).unwrap() {
            let sample = shapes
                .sample(contribution.shape, contribution.sample)
                .unwrap();
            if !sample.offsets.is_empty() {
                apply_blend_shape_in_place(
                    contribution.weight,
                    sample.offsets,
                    sample.point_indices,
                    &mut points,
                )
                .unwrap();
            }
        }
        let actual = skin_points_with_method(
            binding.skinning_method(),
            binding.geom_bind_transform(),
            &mapped,
            binding.influences(),
            &points,
        )
        .unwrap();
        for (p, e) in actual.iter().zip(row["skinned"].as_array().unwrap()) {
            for (v, e) in p.iter().zip(e.as_array().unwrap()) {
                assert!((f64::from(*v) - e.as_f64().unwrap()).abs() < 4e-6);
            }
        }
    }
}

#[test]
fn binding_values_do_not_require_vertex_buffers_or_valid_cpu_bind_palettes() {
    let source = include_str!("../fixtures/skel_skinning.usda")
        .replace("point3f[] points = [(1,0,0), (0,2,0)]", "")
        .replace(
            "uniform matrix4d[] bindTransforms = [",
            "uniform matrix4d[] bindTransforms = []\n uniform matrix4d[] ignoredBind = [",
        );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let query = SkinningQuery::new(&scene, path).unwrap().unwrap();
    let binding = query.binding_inputs(Time::at(2.)).unwrap();
    binding
        .validate(2, query.joint_mapping().unwrap().len())
        .unwrap();
    assert!(
        query
            .skeleton_query()
            .skinning_transforms(Time::at(2.))
            .is_err()
    );
}

#[test]
fn full_shape_upload_validation_includes_inactive_samples() {
    let source = include_str!("../fixtures/skel_blend_shapes.usda").replace(
        "uniform int[] pointIndices = [1]",
        "uniform int[] pointIndices = [99]",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let query = SkinningQuery::new(&scene, path).unwrap().unwrap();
    let shapes = query.blend_shape_query().unwrap();
    assert!(shapes.deform_points(&[0., 0.], &[[0.; 3]; 2]).is_ok());
    assert!(shapes.validate_point_count(2).is_err());
}
