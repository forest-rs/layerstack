// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CPU LBS and inherited/indexed influence binding match OpenUSD.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::Time;
use layerstack_schemas::{
    Scene,
    skel::{InfluenceInterpolation, JointInfluences, SkelError, SkinningQuery, skin_points},
    usd_skel::SkelRoot,
};
const IDENTITY: [[f64; 4]; 4] = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];
#[test]
fn inherited_indexed_and_constant_influences_match_cpp() {
    let (mut store, live) = support::scene(include_str!("../fixtures/skel_skinning.usda"));
    let root = store.path("/Rig");
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_skinning.json")).unwrap();
    let paths: Vec<_> = expected
        .as_array()
        .unwrap()
        .iter()
        .map(|row| store.path(row["path"].as_str().unwrap()))
        .collect();
    let scene = Scene::new(live.stage(), &store);
    let queries = SkelRoot::new(&scene, root)
        .unwrap()
        .skinning_queries()
        .unwrap();
    assert_eq!(queries.len(), 2);
    for (row, path) in expected.as_array().unwrap().iter().zip(paths) {
        let query = SkinningQuery::new(&scene, path).unwrap().unwrap();
        assert!(
            query
                .dependencies()
                .contains(&query.skeleton_query().animation_path().unwrap())
        );
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        let actual = query.compute_skinned_points(time).unwrap();
        assert_eq!(actual.len(), row["points"].as_array().unwrap().len());
        for (point, expected) in actual.iter().zip(row["points"].as_array().unwrap()) {
            for (cell, expected) in point.iter().zip(expected.as_array().unwrap()) {
                assert!(
                    (f64::from(*cell) - expected.as_f64().unwrap()).abs() < 3e-6,
                    "{cell} != {expected}"
                );
            }
        }
    }
}
#[test]
fn malformed_influences_return_no_partial_points_and_weights_are_not_normalized() {
    let base = JointInfluences {
        indices: &[0],
        weights: &[2.],
        element_size: 1,
        interpolation: InfluenceInterpolation::Constant,
    };
    assert_eq!(
        skin_points(&IDENTITY, &[IDENTITY], base, &[[1., 2., 3.]]).unwrap(),
        [[2., 4., 6.]]
    );
    for influences in [
        JointInfluences {
            indices: &[-1],
            ..base
        },
        JointInfluences {
            indices: &[1],
            weights: &[0.],
            ..base
        },
        JointInfluences {
            weights: &[f32::NAN],
            ..base
        },
        JointInfluences {
            element_size: 0,
            ..base
        },
        JointInfluences {
            weights: &[],
            ..base
        },
    ] {
        assert!(matches!(
            skin_points(&IDENTITY, &[IDENTITY], influences, &[[1., 2., 3.]]),
            Err(SkelError::InvalidDeformation { .. })
        ));
    }
}
#[test]
fn unsupported_skinning_methods_and_bad_influence_metadata_are_explicit() {
    let source = include_str!("../fixtures/skel_skinning.usda").replace(
        "float[] primvars:skel:jointWeights = [1]",
        "float[] primvars:skel:jointWeights = [1] (interpolation = \"uniform\")",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Rigid");
    let scene = Scene::new(live.stage(), &store);
    assert!(SkinningQuery::new(&scene, path).is_err());
    let source = include_str!("../fixtures/skel_skinning.usda").replace("float[] primvars:skel:jointWeights = [1]", "float[] primvars:skel:jointWeights = [1]\n token primvars:skel:skinningMethod = \"customUnsupported\"");
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Rigid");
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        SkinningQuery::new(&scene, path)
            .unwrap()
            .unwrap()
            .compute_skinned_points(Time::Default),
        Err(SkelError::UnsupportedSkinningMethod { prim: path })
    );
}

#[test]
fn reusable_point_buffers_validate_before_mutation() {
    use layerstack_schemas::skel::skin_points_in_place;
    let original = [[1., 2., 3.], [4., 5., 6.]];
    let mut points = original;
    let influences = JointInfluences {
        indices: &[0, 9],
        weights: &[1., 1.],
        element_size: 1,
        interpolation: InfluenceInterpolation::Vertex,
    };
    assert!(skin_points_in_place(&IDENTITY, &[IDENTITY], influences, &mut points).is_err());
    assert_eq!(points, original);
    let influences = JointInfluences {
        indices: &[0, 0],
        ..influences
    };
    skin_points_in_place(&IDENTITY, &[IDENTITY], influences, &mut points).unwrap();
    assert_eq!(
        points,
        skin_points(&IDENTITY, &[IDENTITY], influences, &original)
            .unwrap()
            .as_slice()
    );
}

#[test]
fn discovery_inherits_binding_inputs_only_within_its_requested_root() {
    let (mut store, live) = support::scene(include_str!("../fixtures/skel_binding_scope.usda"));
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_binding_scope.json")).unwrap();
    let rows: Vec<_> = expected
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let root = store.path(row["root"].as_str().unwrap());
            let geometries: Vec<_> = row["geometries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| (store.path(g["path"].as_str().unwrap()), g["points"].clone()))
                .collect();
            (root, geometries)
        })
        .collect();
    let unbound_mesh = store.path("/Outside/AboveOnly/Mesh");
    let outside = store.path("/Outside");
    let scene = Scene::new(live.stage(), &store);
    for (root, expected) in rows {
        let queries = SkelRoot::new(&scene, root)
            .unwrap()
            .skinning_queries()
            .unwrap();
        assert_eq!(queries.len(), expected.len(), "root {root:?}");
        for (query, (path, points)) in queries.iter().zip(expected) {
            assert_eq!(query.geometry_path(), path);
            assert_eq!(
                serde_json::json!(query.compute_skinned_points(Time::Default).unwrap()),
                points
            );
        }
    }
    // Standalone queries retain the unrestricted inherited-binding contract.
    let standalone = SkinningQuery::new(&scene, unbound_mesh).unwrap().unwrap();
    assert!(standalone.dependencies().contains(&outside));
    assert_eq!(
        standalone.compute_skinned_points(Time::Default),
        Err(SkelError::UnsupportedSkinningMethod { prim: unbound_mesh })
    );
}
