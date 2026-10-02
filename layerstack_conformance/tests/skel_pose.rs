// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sparse joint animation and inherited bindings match the C++ skeletal query.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::Time;
use layerstack_schemas::{
    Scene,
    skel::{JointTopology, SkelError},
    usd_skel::{SkelBindingApi, Skeleton},
};

fn matrices(actual: &[[[f64; 4]; 4]], expected: &serde_json::Value) {
    assert_eq!(
        actual.len(),
        expected.as_array().unwrap().len(),
        "joint count matches OpenUSD"
    );
    for (matrix, expected) in actual.iter().zip(expected.as_array().unwrap()) {
        for (row, expected) in matrix.iter().zip(expected.as_array().unwrap()) {
            for (cell, expected) in row.iter().zip(expected.as_array().unwrap()) {
                assert!(
                    (cell - expected.as_f64().unwrap()).abs() < 2e-6,
                    "{cell} != {expected}"
                );
            }
        }
    }
}
#[test]
fn sparse_reordered_joint_poses_and_bindings_match_cpp() {
    let (mut store, live) = support::scene(include_str!("../fixtures/skel_pose.usda"));
    let skeleton = store.path("/Rig/Skeleton");
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_pose.json")).unwrap();
    let binding_paths: Vec<_> = expected["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| store.path(row["path"].as_str().unwrap()))
        .collect();
    let scene = Scene::new(live.stage(), &store);
    let query = Skeleton::new(&scene, skeleton).unwrap().query().unwrap();
    assert_eq!(query.topology().parents(), &[None, Some(0), None]);
    assert_eq!(query.skeleton_path(), skeleton);
    for row in expected["poses"].as_array().unwrap() {
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        matrices(&query.local_transforms(time).unwrap(), &row["local"]);
        matrices(&query.skeleton_transforms(time).unwrap(), &row["skeleton"]);
        matrices(&query.skinning_transforms(time).unwrap(), &row["skinning"]);
    }
    for (path, row) in binding_paths
        .into_iter()
        .zip(expected["bindings"].as_array().unwrap())
    {
        let binding = SkelBindingApi::get(&scene, path).unwrap();
        let skel = binding
            .inherited_skeleton()
            .unwrap()
            .map(|s| store.paths.resolve(s.path()).display(&store.tokens));
        let anim = binding
            .inherited_animation_source()
            .unwrap()
            .map(|s| store.paths.resolve(s.path()).display(&store.tokens));
        assert_eq!(serde_json::json!(skel), row["skeleton"]);
        assert_eq!(serde_json::json!(anim), row["animation"]);
    }
}
#[test]
fn topology_validates_parent_order_duplicates_and_nearest_listed_ancestors() {
    let topology = JointTopology::new(&["root", "root/missing/child", "other"]).unwrap();
    assert_eq!(topology.parents(), &[None, Some(0), None]);
    for joints in [
        &["root/child", "root"][..],
        &["root", "root"],
        &["root//child"],
        &["/root"],
        &["root.attr"],
    ] {
        assert!(matches!(
            JointTopology::new(joints),
            Err(SkelError::InvalidTopology { .. })
        ));
    }
    assert!(topology.concatenate(&[]).is_none());
    assert!(
        JointTopology::new(&[])
            .unwrap()
            .concatenate(&[])
            .unwrap()
            .is_empty()
    );
}
#[test]
fn rest_fallback_and_singular_bind_errors_are_explicit() {
    let source = include_str!("../fixtures/skel_pose.usda")
        .replace(
            "float3[] translations = [(0,4,0), (2,0,0)]",
            "float3[] translations = None",
        )
        .replace(
            "float3[] translations.timeSamples = {1: [(0,4,0),(2,0,0)], 3: [(0,8,0),(6,0,0)]}",
            "",
        );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Skeleton");
    let scene = Scene::new(live.stage(), &store);
    let query = Skeleton::new(&scene, path).unwrap().query().unwrap();
    assert_eq!(
        query.local_transforms(Time::at(2.)).unwrap(),
        query.rest_transforms().unwrap()
    );
    let source = source.replace(
        "((1,0,0,0),(0,1,0,0),(0,0,1,0),(1,0,0,1))",
        "((0,0,0,0),(0,1,0,0),(0,0,1,0),(1,0,0,1))",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Skeleton");
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        Skeleton::new(&scene, path)
            .unwrap()
            .query()
            .unwrap()
            .skinning_transforms(Time::Default),
        Err(SkelError::SingularBind { joint: 0 })
    );
}
