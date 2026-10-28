// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Local dense/sparse blend shapes and inbetweens precede joint skinning.
use crate::support::schema_scene as support;
use layerstack::Time;
use layerstack_schemas::{
    Scene,
    skel::{BlendShapeQuery, SkelError, SkinningQuery, apply_blend_shape},
};
fn vectors(actual: &[[f32; 3]], expected: &serde_json::Value) {
    assert_eq!(
        actual.len(),
        expected.as_array().unwrap().len(),
        "deformed point count matches OpenUSD"
    );
    for (point, expected) in actual.iter().zip(expected.as_array().unwrap()) {
        for (value, expected) in point.iter().zip(expected.as_array().unwrap()) {
            assert!(
                (f64::from(*value) - expected.as_f64().unwrap()).abs() < 4e-6,
                "{value} != {expected}"
            );
        }
    }
}
#[test]
fn mapped_animation_inbetweens_normals_and_combined_skinning_match_cpp() {
    let (store, live) = support::scene(include_str!("../../fixtures/skel_blend_shapes.usda"));
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let skin = SkinningQuery::new(&scene, path).unwrap().unwrap();
    let blend = skin.blend_shape_query().unwrap();
    assert_eq!(blend.sample_count(), 5);
    assert!(
        blend
            .targets()
            .iter()
            .all(|p| skin.dependencies().contains(p))
    );
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../../fixtures/skel_blend_shapes.json")).unwrap();
    let points = [[1., 0., 0.], [0., 2., 0.]];
    for row in expected["frames"].as_array().unwrap() {
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        let weights = skin
            .skeleton_query()
            .blend_shape_weights(time, blend.names())
            .unwrap();
        assert_eq!(serde_json::json!(weights), row["weights"]);
        vectors(
            &blend.deform_points(&weights, &points).unwrap(),
            &row["local"],
        );
        vectors(
            &blend.deform_normals(&weights, &[[0., 0., 1.]; 2]).unwrap(),
            &row["normals"],
        );
        vectors(
            &skin.compute_deformed_points(time).unwrap(),
            &row["skinned"],
        );
    }
    for row in expected["extrapolation"].as_array().unwrap() {
        vectors(
            &blend
                .deform_points(
                    &[
                        serde_json::from_value::<f32>(row["weight"].clone()).unwrap(),
                        0.,
                    ],
                    &points,
                )
                .unwrap(),
            &row["points"],
        );
    }
}
#[test]
fn bindings_are_local_and_malformed_active_shapes_return_no_partial_result() {
    let source = include_str!("../../fixtures/skel_blend_shapes.usda").replace(
        "uniform int[] pointIndices = [1]",
        "uniform int[] pointIndices = [99]",
    );
    let (store, live) = support::scene(&source);
    let geometry = store.path("/Rig/Geometry");
    let mesh = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    assert!(BlendShapeQuery::new(&scene, geometry).unwrap().is_none());
    let query = BlendShapeQuery::new(&scene, mesh).unwrap().unwrap();
    assert!(matches!(
        query.deform_points(&[0.25, 1.], &[[0.; 3]; 2]),
        Err(SkelError::InvalidDeformation { .. })
    ));
    assert!(query.compute_weights(&[1.]).is_err());
    assert!(query.compute_weights(&[f32::NAN, 0.]).is_err());
    assert!(apply_blend_shape(1., &[[1.; 3]; 2], &[0, 0], &[[0.; 3]; 2]).is_err());
    assert!(apply_blend_shape(1., &[[1.; 3]], &[], &[[0.; 3]; 2]).is_err());
    assert_eq!(
        apply_blend_shape(0., &[], &[], &[[0.; 3]; 2]).unwrap(),
        [[0.; 3]; 2]
    );
}
/// An inbetween at weight 0 or 1, or not finite, is skipped and reported,
/// as C++ `UsdSkelBlendShapeQuery` skips it with a warning; the shape still
/// blends with its other samples.
#[test]
fn invalid_inbetween_knots_are_skipped_and_reported() {
    use layerstack_schemas::skel::SkelProblemReason;
    let (store, live) = support::scene(include_str!("../../fixtures/skel_blend_shapes.usda"));
    let path = store.path("/Rig/Geometry/Mesh");
    let valid = BlendShapeQuery::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .unwrap();
    for weight in ["0", "1", "nan"] {
        let source = include_str!("../../fixtures/skel_blend_shapes.usda")
            .replace("weight = 0.5", &format!("weight = {weight}"));
        let (store, live) = support::scene(&source);
        let path = store.path("/Rig/Geometry/Mesh");
        let query = BlendShapeQuery::new(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .unwrap();
        assert_eq!(query.sample_count(), valid.sample_count() - 1, "{weight}");
        let [problem] = query.problems() else {
            panic!("one skipped inbetween at weight {weight}");
        };
        assert!(matches!(
            problem.reason,
            SkelProblemReason::InvalidInbetweenWeight(_)
        ));
        assert_eq!(
            problem
                .property
                .map(|p| p.display(store.paths(), store.tokens()).to_string()),
            Some(format!(
                "{}.inbetweens:half",
                store.paths().display(problem.prim, store.tokens())
            ))
        );
    }
}

#[test]
fn blend_buffers_validate_before_mutation_and_snapshots_refresh_after_edits() {
    use layerstack::{LayerId, edit::EditTarget};
    use layerstack_schemas::{
        SchemaEdit, skel::apply_blend_shape_in_place, usd_skel::BlendShapeEdit,
    };
    let original = [[1., 0., 0.], [0., 2., 0.]];
    let mut points = original;
    assert!(apply_blend_shape_in_place(1., &[[1.; 3]; 2], &[0, 8], &mut points).is_err());
    assert_eq!(points, original);
    apply_blend_shape_in_place(0.7, &[[1.; 3]; 2], &[], &mut points).unwrap();
    assert_eq!(
        points.as_slice(),
        apply_blend_shape(0.7, &[[1.; 3]; 2], &[], &original).unwrap()
    );
    let (mut store, mut live) =
        support::scene(include_str!("../../fixtures/skel_blend_shapes.usda"));
    let path = store.path("/Rig/Geometry/Mesh");
    let shape = store.path("/Rig/Smile");
    let snapshot = BlendShapeQuery::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .unwrap();
    let before = snapshot.deform_points(&[1., 0.], &original).unwrap();
    let mut too_short = [[1., 0., 0.]];
    let untouched = too_short;
    assert!(
        snapshot
            .deform_points_in_place(&[1., 0.], &mut too_short)
            .is_err()
    );
    assert_eq!(too_short, untouched);
    let mut points = original;
    snapshot
        .deform_points_in_place(&[1., 0.], &mut points)
        .unwrap();
    assert_eq!(points.as_slice(), before);
    let mut normals = original;
    snapshot
        .deform_normals_in_place(&[1., 0.], &mut normals)
        .unwrap();
    assert_eq!(
        normals.as_slice(),
        snapshot.deform_normals(&[1., 0.], &original).unwrap()
    );

    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = BlendShapeEdit::new(&edit, shape)
        .expect("outside any instance")
        .expect("the prim");
    handle.set_offsets(&mut edit, &[[0., 8., 0.]]);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let fresh = BlendShapeQuery::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .unwrap();
    assert_ne!(fresh.deform_points(&[1., 0.], &original).unwrap(), before);
    assert_eq!(
        snapshot.deform_points(&[1., 0.], &original).unwrap(),
        before
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    let restored = BlendShapeQuery::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .unwrap();
    assert_eq!(
        restored.deform_points(&[1., 0.], &original).unwrap(),
        before
    );
}
