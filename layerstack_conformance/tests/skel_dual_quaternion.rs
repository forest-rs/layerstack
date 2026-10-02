// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! DQS kernels, bindings and retained palettes match OpenUSD 26.8.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    LayerId, PropertyPath, Value,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::{
    Scene, Time,
    skel::{
        InfluenceInterpolation, JointInfluences, SkelCache, SkelError, SkinningMethod,
        SkinningQuery, rigid_skinning_transform_with_method, skin_normals_in_place_with_method,
        skin_normals_with_method, skin_points_in_place_with_method, skin_points_with_method,
    },
    usd_skel::SkelRoot,
};
const DQ: SkinningMethod = SkinningMethod::DualQuaternion;
fn read<T: serde::de::DeserializeOwned>(row: &serde_json::Value, key: &str) -> T {
    serde_json::from_value(row[key].clone()).unwrap()
}
fn close(actual: &[[f32; 3]], expected: &[[f32; 3]], name: &str) {
    assert_eq!(actual.len(), expected.len(), "{name}");
    for (a, b) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert!(
            (a - b).abs() <= 4e-6 * b.abs().max(1.),
            "{name}: {a} != {b}"
        );
    }
}
fn matrix_close(actual: [[f64; 4]; 4], expected: [[f64; 4]; 4], name: &str) {
    for (a, b) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert!(
            (a - b).abs() <= 4e-6 * b.abs().max(1.),
            "{name}: {a} != {b}"
        );
    }
}
#[test]
fn reference_kernels_cover_twist_hemispheres_scale_shear_and_degeneracies() {
    let data: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_dual_quaternion.json")).unwrap();
    for row in data["kernels"].as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        let bind = read(row, "bind");
        let joints: Vec<_> = read::<Vec<[[f64; 4]; 4]>>(row, "joints");
        let indices = read::<Vec<i32>>(row, "indices");
        let weights = read::<Vec<f32>>(row, "weights");
        let points = read::<Vec<[f32; 3]>>(row, "source_points");
        let want = read::<Vec<[f32; 3]>>(row, "points");
        for interpolation in [
            InfluenceInterpolation::Constant,
            InfluenceInterpolation::Vertex,
        ] {
            let copies = if interpolation == InfluenceInterpolation::Constant {
                1
            } else {
                points.len()
            };
            let ids = indices.repeat(copies);
            let ws = weights.repeat(copies);
            let inf = JointInfluences {
                indices: &ids,
                weights: &ws,
                element_size: indices.len(),
                interpolation,
            };
            close(
                &skin_points_with_method(DQ, &bind, &joints, inf, &points).unwrap(),
                &want,
                name,
            );
            let mut buffer = points.clone();
            skin_points_in_place_with_method(DQ, &bind, &joints, inf, &mut buffer).unwrap();
            close(&buffer, &want, name);
            if row.get("normals").is_some() {
                let ns = read::<Vec<[f32; 3]>>(row, "source_normals");
                close(
                    &skin_normals_with_method(DQ, &bind, &joints, inf, &ns).unwrap(),
                    &read::<Vec<[f32; 3]>>(row, "normals"),
                    name,
                );
            }
        }
        let inf = JointInfluences {
            indices: &indices,
            weights: &weights,
            element_size: indices.len(),
            interpolation: InfluenceInterpolation::Constant,
        };
        matrix_close(
            rigid_skinning_transform_with_method(DQ, &bind, &joints, inf).unwrap(),
            read(row, "rigid"),
            name,
        );
    }
    let twist = &data["kernels"][0];
    let points = read::<Vec<[f32; 3]>>(twist, "points");
    assert!(
        (points[0][0] * points[0][0] + points[0][1] * points[0][1] - 1.).abs() < 1e-6,
        "DQS preserves the twist radius"
    );
}
#[test]
fn sampled_bindings_normals_and_rigid_transforms_match_cpp() {
    let data: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_dual_quaternion.json")).unwrap();
    let (mut store, live) = support::scene(include_str!("../fixtures/skel_dual_quaternion.usda"));
    let mut cache = SkelCache::new(Time::Default);
    for row in data["frames"].as_array().unwrap() {
        let path = store.path(row["path"].as_str().unwrap());
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        cache.set_time(time);
        let scene = Scene::new(live.stage(), &store);
        let q = SkinningQuery::new(&scene, path).unwrap().unwrap();
        assert_eq!(q.skinning_method().unwrap(), DQ);
        let points = q.compute_skinned_points(time).unwrap();
        let normals = q.compute_skinned_normals(time).unwrap();
        close(
            &points,
            &read::<Vec<[f32; 3]>>(row, "points"),
            "bound points",
        );
        close(
            &normals,
            &read::<Vec<[f32; 3]>>(row, "normals"),
            "bound normals",
        );
        assert_eq!(
            cache.deformed_points(&scene, path).unwrap().unwrap(),
            points
        );
        assert_eq!(
            cache.skinned_normals(&scene, path).unwrap().unwrap(),
            normals
        );
        if row.get("rigid").is_some() {
            matrix_close(
                q.compute_rigid_transform(time).unwrap(),
                read(row, "rigid"),
                "bound rigid",
            );
        }
    }
    assert_eq!(
        cache.stats().dual_quaternion_joints,
        16,
        "two shared joint palettes, for points/normals at four times"
    );
}
#[test]
fn retained_custom_subsets_keep_scale_flags_local_and_refresh_pose_edits() {
    let source=include_str!("../fixtures/skel_dual_quaternion.usda")
        .replace("half3[] scales = [(2,0.5,1), (1,3,0.5)]","half3[] scales = [(1,1,1), (1,3,0.5)]")
        .replace("int[] primvars:skel:jointIndices = [0,1] (elementSize = 2)","uniform token[] skel:joints = [\"a\"]\n int[] primvars:skel:jointIndices = [0,0] (elementSize = 2)")
        .replace("float[] primvars:skel:jointWeights = [0.3,0.7]","float[] primvars:skel:jointWeights = [-2,0]");
    let (mut store, mut live) = support::scene(&source);
    let root = store.path("/Rig");
    let anim = store.path("/Rig/Animation");
    let rigid = store.path("/Rig/Rigid");
    let mut cache = SkelCache::new(Time::Default);
    let compare =
        |cache: &mut SkelCache, store: &layerstack::InMemoryStore, live: &layerstack::LiveStage| {
            let scene = Scene::new(live.stage(), store);
            let time = cache.time();
            for q in SkelRoot::new(&scene, root)
                .unwrap()
                .skinning_queries()
                .unwrap()
            {
                assert_eq!(
                    cache
                        .deformed_points(&scene, q.geometry_path())
                        .unwrap()
                        .unwrap(),
                    q.compute_skinned_points(time).unwrap()
                );
                assert_eq!(
                    cache
                        .skinned_normals(&scene, q.geometry_path())
                        .unwrap()
                        .unwrap(),
                    q.compute_skinned_normals(time).unwrap()
                );
            }
        };
    compare(&mut cache, &store, &live);
    for (path, name, value) in [
        (
            anim,
            "translations",
            Value::Array(vec![Value::Vec3f([0., 9., 0.]), Value::Vec3f([8., 0., 0.])]),
        ),
        (
            rigid,
            "primvars:skel:skinningMethod",
            Value::Token(store.tokens.intern("classicLinear")),
        ),
    ] {
        let token = store.tokens.intern(name);
        let mut transaction = Transaction::new();
        transaction.set_default(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(path, token)),
            value,
        );
        let applied = live.apply(&mut store, &transaction).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
        compare(&mut cache, &store, &live);
        let undone = live.apply(&mut store, &applied.inverse).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
        compare(&mut cache, &store, &live);
    }
}
#[test]
fn dqs_validation_preserves_reusable_buffers() {
    let identity = [
        [1., 0., 0., 0.],
        [0., 1., 0., 0.],
        [0., 0., 1., 0.],
        [0., 0., 0., 1.],
    ];
    let original = [[1., 2., 3.], [4., 5., 6.]];
    let mut buffer = original;
    let inf = JointInfluences {
        indices: &[0, 9],
        weights: &[1., 0.],
        element_size: 1,
        interpolation: InfluenceInterpolation::Vertex,
    };
    assert!(
        skin_points_in_place_with_method(DQ, &identity, &[identity], inf, &mut buffer).is_err()
    );
    assert_eq!(buffer, original);
    assert!(
        skin_normals_in_place_with_method(DQ, &identity, &[identity], inf, &mut buffer).is_err()
    );
    assert_eq!(buffer, original);
    let singular = [[0.; 4]; 4];
    let inf = JointInfluences {
        indices: &[0, 0],
        ..inf
    };
    assert_eq!(
        skin_normals_in_place_with_method(DQ, &identity, &[singular], inf, &mut buffer),
        Err(SkelError::SingularNormalTransform { joint: Some(0) })
    );
    assert_eq!(buffer, original);
}

#[test]
fn blend_shape_animation_precedes_dqs_in_snapshot_and_retained_evaluation() {
    let data: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_dual_quaternion.json")).unwrap();
    let source = include_str!("../fixtures/skel_blend_shapes.usda").replace("rel skel:skeleton = </Rig/Skeleton>", "rel skel:skeleton = </Rig/Skeleton>\n token primvars:skel:skinningMethod = \"dualQuaternion\"");
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let q = SkinningQuery::new(&scene, path).unwrap().unwrap();
    let mut cache = SkelCache::new(Time::Default);
    for row in data["blends"].as_array().unwrap() {
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        cache.set_time(time);
        let points = q.compute_deformed_points(time).unwrap();
        close(
            &points,
            &read::<Vec<[f32; 3]>>(row, "points"),
            "blend shape before DQS",
        );
        assert_eq!(
            cache.deformed_points(&scene, path).unwrap().unwrap(),
            points
        );
    }
}
