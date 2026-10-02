// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ordered shutter-sample input visits share work and short-circuit adapter errors.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{
    Scene, Time,
    skel::{BlendShapeCache, SkelCache, SkelError},
};
#[test]
fn ordered_samples_preserve_duplicates_unbound_parts_and_shared_pose_work() {
    let (mut store, live) = support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
    let paths = ["/Rig/Geometry/Mesh", "/Rig/Rigid", "/Rig/Unbound/Child"].map(|p| store.path(p));
    let scene = Scene::new(live.stage(), &store);
    let times = [Time::at(1.), Time::at(2.), Time::at(2.)];
    let mut cache = SkelCache::new(Time::Default);
    let mut rows = Vec::new();
    cache
        .for_each_deformation_sample(&scene, &paths, &times, |time, path, inputs| {
            let record = inputs.map(|i| {
                i.validate_point_count(2).unwrap();
                (i.revisions(), i.shared_skinning_transforms().to_vec())
            });
            rows.push((time, path, record));
            Ok::<_, SkelError>(())
        })
        .unwrap();
    assert_eq!(rows.len(), 9);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.0, times[i / 3]);
        assert_eq!(row.1, paths[i % 3]);
        assert_eq!(row.2.is_none(), i % 3 == 2);
    }
    assert_eq!(rows[3].2, rows[6].2);
    assert_eq!(rows[4].2, rows[7].2);
    assert_eq!(
        rows[0].2.as_ref().unwrap().0.pose,
        rows[1].2.as_ref().unwrap().0.pose
    );
    assert_eq!(cache.stats().pose_evaluations, 2);
    assert_eq!(cache.stats().influence_resolutions, 2);
    assert_eq!(cache.stats().point_vertices, 0);
    assert_eq!(cache.time(), times[2]);
    let before = cache.stats();
    cache
        .for_each_deformation_sample(&scene, &[], &[Time::at(99.)], |_, _, _| {
            Ok::<_, SkelError>(())
        })
        .unwrap();
    assert_eq!(cache.time(), times[2]);
    assert_eq!(cache.stats(), before);
}
#[test]
fn visitor_errors_stop_before_later_parts_or_samples() {
    let (mut store, live) = support::scene(include_str!("../fixtures/skel_skinning.usda"));
    let paths = ["/Rig/Geometry/Mesh", "/Rig/Rigid"].map(|p| store.path(p));
    let scene = Scene::new(live.stage(), &store);
    let mut cache = SkelCache::new(Time::Default);
    let mut calls = 0;
    let error = SkelError::InvalidDeformation {
        element: None,
        reason: "adapter rejected upload",
    };
    let result = cache.for_each_deformation_sample(
        &scene,
        &paths,
        &[Time::at(1.), Time::at(2.)],
        |_, _, _| {
            calls += 1;
            Err(error.clone())
        },
    );
    assert_eq!(result, Err(error));
    assert_eq!(calls, 1);
    assert_eq!(cache.stats().binding_builds, 1);
    assert_eq!(cache.stats().pose_evaluations, 1);
    assert_eq!(cache.time(), Time::at(1.));
}
#[test]
fn morph_only_sampling_keeps_definition_revisions_and_reuses_equal_times() {
    let source = include_str!("../fixtures/skel_blend_shapes.usda").replace(
        "rel skel:skeleton = </Rig/Skeleton>",
        "rel skel:skeleton = []",
    );
    let (mut store, live) = support::scene(&source);
    let paths = [store.path("/Rig/Geometry/Mesh")];
    let scene = Scene::new(live.stage(), &store);
    let mut cache = BlendShapeCache::new(Time::Default);
    let mut rows = Vec::new();
    cache
        .for_each_sample(
            &scene,
            &paths,
            &[Time::at(1.), Time::held(2.), Time::held(2.)],
            |time, _, inputs| {
                let i = inputs.unwrap();
                i.validate_point_count(2)?;
                rows.push((
                    time,
                    i.definition_revision(),
                    i.weight_revision(),
                    i.weights().to_vec(),
                ));
                Ok::<_, SkelError>(())
            },
        )
        .unwrap();
    assert_eq!(rows[0].1, rows[1].1);
    assert_ne!(rows[0].2, rows[1].2);
    assert_eq!(rows[1], rows[2]);
    assert_eq!(cache.stats().weight_evaluations, 2);
    assert_eq!(cache.stats().point_vertices, 0);
}
