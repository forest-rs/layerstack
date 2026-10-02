// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Combined normal deformation, corner expansion and independent retained work.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    InMemoryStore, LayerId, PathId, PropertyPath, Value,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::{
    Scene, Time,
    skel::{
        SkelCache, SkinningQuery, skin_face_varying_normals_with_method, skin_normals_with_method,
    },
};
fn source(corners: bool, dqs: bool) -> String {
    let normals = if corners {
        "normal3f[] normals = [(0,0,1),(0,0,1),(0,0,1)] (interpolation = \"faceVarying\")\n int[] faceVertexIndices = [1,0,1]"
    } else {
        "normal3f[] normals = [(0,0,1),(0,0,1)]"
    };
    let mut text = include_str!("../fixtures/skel_blend_shapes.usda").replace(
        "point3f[] points = [(1,0,0), (0,2,0)]",
        &format!("point3f[] points = [(1,0,0), (0,2,0)]\n {normals}"),
    );
    if dqs {
        text = text.replace("rel skel:skeleton = </Rig/Skeleton>", "rel skel:skeleton = </Rig/Skeleton>\n token primvars:skel:skinningMethod = \"dualQuaternion\"");
    }
    text
}
fn edit(store: &mut InMemoryStore, path: PathId, name: &str, value: Value) -> Transaction {
    let mut t = Transaction::new();
    let at = EditTarget::for_layer(LayerId(1))
        .property(PropertyPath::new(path, store.tokens.intern(name)));
    t.set_default(at, value);
    t
}
#[test]
fn combined_normals_match_cpp_shape_offsets_followed_by_skinning() {
    // The pinned JSON normal offsets come from OpenUSD's C++ blend-shape kernel.
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_blend_shapes.json")).unwrap();
    for corners in [false, true] {
        for dqs in [false, true] {
            let (mut store, live) = support::scene(&source(corners, dqs));
            let path = store.path("/Rig/Geometry/Mesh");
            let scene = Scene::new(live.stage(), &store);
            let q = SkinningQuery::new(&scene, path).unwrap().unwrap();
            let mut cache = SkelCache::new(Time::Default);
            for row in oracle["frames"].as_array().unwrap() {
                let time = row["time"].as_f64().map_or(Time::Default, Time::at);
                let offsets: Vec<[f32; 3]> =
                    serde_json::from_value(row["normals"].clone()).unwrap();
                let binding = q.binding_inputs(time).unwrap();
                let palette = q.skeleton_query().skinning_transforms(time).unwrap();
                let transforms: Vec<_> = q
                    .joint_mapping()
                    .unwrap()
                    .iter()
                    .map(|i| palette[i.unwrap()])
                    .collect();
                let expected = if corners {
                    skin_face_varying_normals_with_method(
                        binding.skinning_method(),
                        binding.geom_bind_transform(),
                        &transforms,
                        binding.influences(),
                        2,
                        &[1, 0, 1],
                        &[offsets[1], offsets[0], offsets[1]],
                    )
                    .unwrap()
                } else {
                    skin_normals_with_method(
                        binding.skinning_method(),
                        binding.geom_bind_transform(),
                        &transforms,
                        binding.influences(),
                        &offsets,
                    )
                    .unwrap()
                };
                if !corners && !dqs {
                    let cpp: Vec<[f32; 3]> =
                        serde_json::from_value(row["skinned_normals"].clone()).unwrap();
                    for (actual, want) in expected.iter().zip(cpp) {
                        for (a, b) in actual.iter().zip(want) {
                            assert!((*a - b).abs() < 3e-6);
                        }
                    }
                }
                assert_eq!(q.compute_deformed_normals(time).unwrap(), expected);
                cache.set_time(time);
                assert_eq!(
                    cache.deformed_normals(&scene, path).unwrap().unwrap(),
                    expected
                );
                let before = cache.stats();
                cache.deformed_normals(&scene, path).unwrap().unwrap();
                assert_eq!(cache.stats().normal_vectors, before.normal_vectors);
                assert_eq!(cache.stats().normal_matrices, before.normal_matrices);
            }
        }
    }
}
#[test]
fn weights_refresh_combined_outputs_but_preserve_skin_only_results_and_palettes() {
    let (mut store, mut live) = support::scene(&source(true, true));
    let path = store.path("/Rig/Geometry/Mesh");
    let anim = store.path("/Rig/Animation");
    let mut cache = SkelCache::new(Time::Default);
    let scene = Scene::new(live.stage(), &store);
    let skin_only = cache
        .skinned_normals(&scene, path)
        .unwrap()
        .unwrap()
        .to_vec();
    let combined = cache
        .deformed_normals(&scene, path)
        .unwrap()
        .unwrap()
        .to_vec();
    let inputs = cache.normal_inputs(&scene, path).unwrap().unwrap();
    assert_eq!(
        inputs.dual_quaternions().unwrap().len(),
        inputs.skinning_transforms().len()
    );
    let stamps = (inputs.pose_revision(), inputs.input_revision());
    let before = cache.stats();
    let t = edit(
        &mut store,
        anim,
        "blendShapeWeights",
        Value::array(vec![Value::Float(0.5), Value::Float(1.), Value::Float(0.)]),
    );
    let applied = live.apply(&mut store, &t).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    assert_eq!(
        cache.skinned_normals(&scene, path).unwrap().unwrap(),
        skin_only
    );
    assert_eq!(cache.stats().normal_vectors, before.normal_vectors);
    assert_ne!(
        cache.deformed_normals(&scene, path).unwrap().unwrap(),
        combined
    );
    let inputs = cache.normal_inputs(&scene, path).unwrap().unwrap();
    assert_eq!((inputs.pose_revision(), inputs.input_revision()), stamps);
    assert_eq!(cache.stats().normal_matrices, before.normal_matrices);
    assert_eq!(
        cache.stats().dual_quaternion_joints,
        before.dual_quaternion_joints
    );
    let undo = live.apply(&mut store, &applied.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undo.changes);
    assert_eq!(
        cache.deformed_normals(&scene, path).unwrap().unwrap(),
        combined
    );
}
#[test]
fn normal_inputs_need_no_buffers_and_bad_corner_offsets_fail_without_stale_output() {
    let no_buffers = source(false, true)
        .replace("normal3f[] normals = [(0,0,1),(0,0,1)]", "")
        .replace("point3f[] points = [(1,0,0), (0,2,0)]", "");
    let (mut store, live) = support::scene(&no_buffers);
    let path = store.path("/Rig/Geometry/Mesh");
    let mut cache = SkelCache::new(Time::Default);
    cache
        .normal_inputs(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .unwrap();
    assert_eq!(cache.stats().normal_vectors, 0);
    assert_eq!(cache.stats().point_vertices, 0);
    let (mut store, mut live) = support::scene(&source(true, false));
    let path = store.path("/Rig/Geometry/Mesh");
    let shape = store.path("/Rig/Smile");
    let mut cache = SkelCache::new(Time::Default);
    let original = cache
        .deformed_normals(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .unwrap()
        .to_vec();
    let t = edit(
        &mut store,
        shape,
        "pointIndices",
        Value::array(vec![Value::Int(9)]),
    );
    let bad = live.apply(&mut store, &t).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &bad.changes);
    assert!(cache.deformed_normals(&scene, path).is_err());
    let undo = live.apply(&mut store, &bad.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undo.changes);
    assert_eq!(
        cache.deformed_normals(&scene, path).unwrap().unwrap(),
        original
    );
}
