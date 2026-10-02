// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained renderer inputs share palettes and expose explicit upload revisions.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    InMemoryStore, LayerId, ListOp, PathId, PropertyPath, TargetPath, Value,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::{
    Scene, Time,
    skel::{
        DeformationRevisions, SkelCache, SkelError, SkinningMethod, SkinningQuery,
        apply_blend_shape_in_place, skin_points_with_method,
    },
};

fn edit(store: &mut InMemoryStore, path: PathId, name: &str, value: Value) -> Transaction {
    let property = PropertyPath::new(path, store.tokens.intern(name));
    let mut transaction = Transaction::new();
    transaction.set_default(EditTarget::for_layer(LayerId(1)).property(property), value);
    transaction
}
fn revisions(cache: &mut SkelCache, scene: &Scene<'_>, path: PathId) -> DeformationRevisions {
    let inputs = cache.deformation_inputs(scene, path).unwrap().unwrap();
    inputs.validate_point_count(2).unwrap();
    inputs.revisions()
}
#[test]
fn retained_inputs_reconstruct_points_and_reuse_shared_rig_arrays() {
    for (source, paths, method) in [
        (
            include_str!("../fixtures/skel_blend_shapes.usda"),
            ["/Rig/Geometry/Mesh", "/Rig/Rigid"],
            SkinningMethod::ClassicLinear,
        ),
        (
            include_str!("../fixtures/skel_dual_quaternion.usda"),
            ["/Rig/Geometry/Vertex", "/Rig/Rigid"],
            SkinningMethod::DualQuaternion,
        ),
    ] {
        let (mut store, live) = support::scene(source);
        let paths = paths.map(|p| store.path(p));
        let scene = Scene::new(live.stage(), &store);
        let mut cache = SkelCache::new(Time::Default);
        for time in [
            Time::Default,
            Time::at(1.),
            Time::at(2.),
            Time::held(2.),
            Time::at(3.),
        ] {
            cache.set_time(time);
            let mut shared = None;
            for path in paths {
                let prepared = cache.deformation_inputs(&scene, path).unwrap().unwrap();
                prepared.validate_point_count(2).unwrap();
                assert_eq!(prepared.time(), time);
                assert_eq!(prepared.geometry_path(), path);
                assert_eq!(prepared.binding().skinning_method(), method);
                let identity = (
                    prepared.skeleton_path(),
                    prepared.revisions().skeleton_definition,
                    prepared.revisions().pose,
                    prepared.shared_skinning_transforms().as_ptr(),
                    prepared.shared_dual_quaternions().map(<[_]>::as_ptr),
                );
                if let Some(previous) = shared {
                    assert_eq!(identity, previous);
                }
                shared = Some(identity);
                if method == SkinningMethod::DualQuaternion {
                    let dq = prepared.shared_dual_quaternions().unwrap();
                    assert_eq!(dq.len(), prepared.shared_skinning_transforms().len());
                    for joint in dq {
                        assert!(
                            (joint.real().iter().map(|v| v * v).sum::<f64>() - 1.).abs() < 1e-6
                        );
                        assert!(joint.dual().iter().all(|v| v.is_finite()));
                        assert!(
                            joint
                                .residual_scale()
                                .iter()
                                .flatten()
                                .all(|v| v.is_finite())
                        );
                    }
                } else {
                    assert!(prepared.shared_dual_quaternions().is_none());
                }
                let mut points = [[1., 0., 0.], [0., 2., 0.]];
                if let Some(shapes) = prepared.blend_shapes() {
                    assert_eq!(prepared.blend_shape_weights().len(), shapes.names().len());
                    for c in prepared.blend_shape_contributions() {
                        let sample = shapes.sample(c.shape, c.sample).unwrap();
                        if !sample.offsets.is_empty() {
                            apply_blend_shape_in_place(
                                c.weight,
                                sample.offsets,
                                sample.point_indices,
                                &mut points,
                            )
                            .unwrap();
                        }
                    }
                }
                let actual = skin_points_with_method(
                    method,
                    prepared.binding().geom_bind_transform(),
                    prepared.skinning_transforms(),
                    prepared.binding().influences(),
                    &points,
                )
                .unwrap();
                let snapshot = SkinningQuery::new(&scene, path).unwrap().unwrap();
                assert_eq!(actual, snapshot.compute_deformed_points(time).unwrap());
            }
        }
        assert_eq!(cache.stats().point_vertices, 0);
        assert_eq!(cache.stats().normal_vectors, 0);
        assert_eq!(cache.stats().influence_resolutions, 2);
        let before = cache.stats();
        for path in paths {
            revisions(&mut cache, &scene, path);
        }
        assert_eq!(cache.stats(), before);
        // CPU consumers can use the same retained inputs afterward.
        for path in paths {
            cache.deformed_points(&scene, path).unwrap().unwrap();
        }
        assert_eq!(
            cache.stats().influence_resolutions,
            before.influence_resolutions
        );
        assert_eq!(cache.stats().pose_evaluations, before.pose_evaluations);
        assert_eq!(
            cache.stats().dual_quaternion_joints,
            before.dual_quaternion_joints
        );
    }
}

#[test]
fn pose_weights_shapes_undo_and_unrelated_edits_have_independent_revisions() {
    let (mut store, mut live) =
        support::scene(&include_str!("../fixtures/skel_blend_shapes.usda").replace(
            "point3f[] points",
            "uniform token purpose = \"default\"\n point3f[] points",
        ));
    let mesh = store.path("/Rig/Geometry/Mesh");
    let rigid = store.path("/Rig/Rigid");
    let animation = store.path("/Rig/Animation");
    let smile = store.path("/Rig/Smile");
    let mut cache = SkelCache::new(Time::at(1.));
    let before = revisions(&mut cache, &Scene::new(live.stage(), &store), mesh);
    let rigid_before = revisions(&mut cache, &Scene::new(live.stage(), &store), rigid);
    assert_eq!(before.pose, rigid_before.pose);
    assert_eq!(rigid_before.blend_weights, 0);
    let guide = store.tokens.intern("guide");
    let transaction = edit(&mut store, mesh, "purpose", Value::Token(guide));
    let applied = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    assert_eq!(
        revisions(&mut cache, &Scene::new(live.stage(), &store), mesh),
        before
    );
    let transaction = edit(
        &mut store,
        animation,
        "translations",
        Value::array(vec![Value::Vec3f([2., 3., 4.]), Value::Vec3f([4., 5., 6.])]),
    );
    let applied = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    let posed = revisions(&mut cache, &Scene::new(live.stage(), &store), mesh);
    assert_ne!(posed.pose, before.pose);
    assert_eq!(
        DeformationRevisions {
            pose: before.pose,
            ..posed
        },
        before
    );
    let transaction = edit(
        &mut store,
        animation,
        "blendShapeWeights",
        Value::array(vec![
            Value::Float(0.5),
            Value::Float(0.75),
            Value::Float(0.),
        ]),
    );
    let applied = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    let weighted = revisions(&mut cache, &Scene::new(live.stage(), &store), mesh);
    assert_ne!(weighted.blend_weights, posed.blend_weights);
    assert_eq!(
        DeformationRevisions {
            blend_weights: posed.blend_weights,
            ..weighted
        },
        posed
    );
    assert_eq!(
        revisions(&mut cache, &Scene::new(live.stage(), &store), rigid).inputs,
        rigid_before.inputs
    );
    let transaction = edit(
        &mut store,
        smile,
        "offsets",
        Value::array(vec![Value::Vec3f([1., 2., 3.])]),
    );
    let applied = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    let shaped = revisions(&mut cache, &Scene::new(live.stage(), &store), mesh);
    assert_ne!(shaped.binding_definition, weighted.binding_definition);
    assert_eq!(shaped.pose, weighted.pose);
    assert_eq!(shaped.skeleton_definition, weighted.skeleton_definition);
    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    let restored = revisions(&mut cache, &Scene::new(live.stage(), &store), mesh);
    assert_ne!(restored.binding_definition, shaped.binding_definition);
    assert_ne!(restored.inputs, shaped.inputs);
    assert_eq!(restored.pose, shaped.pose);
    cache.clear();
    let cleared = revisions(&mut cache, &Scene::new(live.stage(), &store), mesh);
    assert_ne!(cleared.skeleton_definition, restored.skeleton_definition);
    assert_ne!(cleared.binding_definition, restored.binding_definition);
    assert_ne!(cleared.inputs, restored.inputs);
    assert_ne!(cleared.pose, restored.pose);
    assert_eq!(cache.stats().point_vertices, 0);
}

#[test]
fn temporal_binding_values_refresh_without_changing_definitions() {
    let source = include_str!("../fixtures/skel_blend_shapes.usda").replace(
        "int[] primvars:skel:jointWeights:indices = [1,0]",
        "int[] primvars:skel:jointWeights:indices = [1,0]\n float[] primvars:skel:jointWeights.timeSamples = {1: [0.25,0.75,1,0], 3: [0.5,0.5,0.5,0.5]}\n matrix4d primvars:skel:geomBindTransform.timeSamples = {1: ((1,0,0,0),(0,1,0,0),(0,0,1,0),(1,1,0,1)), 3: ((1,0,0,0),(0,1,0,0),(0,0,1,0),(3,3,0,1))}",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = SkelCache::new(Time::at(1.));
    let before = revisions(&mut cache, &scene, path);
    cache.set_time(Time::at(2.));
    let prepared = cache.deformation_inputs(&scene, path).unwrap().unwrap();
    let after = prepared.revisions();
    assert_eq!(after.binding_definition, before.binding_definition);
    assert_ne!(after.inputs, before.inputs);
    assert_eq!(
        prepared.binding().geom_bind_transform()[3],
        [2., 2., 0., 1.]
    );
    assert_eq!(
        prepared.binding().influences().weights,
        [0.75, 0.25, 0.375, 0.625]
    );
    let sampled = SkinningQuery::new(&scene, path)
        .unwrap()
        .unwrap()
        .binding_inputs(Time::at(2.))
        .unwrap();
    assert_eq!(
        prepared.binding().influences().weights,
        sampled.influences().weights
    );
    assert_eq!(
        prepared.binding().geom_bind_transform(),
        sampled.geom_bind_transform()
    );
    assert_eq!(cache.stats().point_vertices, 0);
}

#[test]
fn inputs_work_without_vertices_and_static_revisions_survive_time_changes() {
    let source = include_str!("../fixtures/skel_skinning.usda")
        .replace("point3f[] points = [(1,0,0), (0,2,0)]", "")
        .replace(
            "rel skel:animationSource = </Rig/Animation>",
            "rel skel:animationSource = []",
        );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Rigid");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = SkelCache::new(Time::Default);
    let before = revisions(&mut cache, &scene, path);
    for time in [Time::at(1.), Time::at(2.), Time::Default] {
        cache.set_time(time);
        assert_eq!(revisions(&mut cache, &scene, path), before);
    }
    assert_eq!(cache.stats().pose_evaluations, 1);
    assert_eq!(cache.stats().point_vertices, 0);
    assert!(cache.deformed_points(&scene, path).is_err());
}

#[test]
fn removal_and_invalid_influences_recover_with_fresh_revisions() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/skel_skinning.usda"));
    let path = store.path("/Rig/Rigid");
    let skeleton = store.path("/Rig/Skeleton");
    let mut cache = SkelCache::new(Time::Default);
    let before = revisions(&mut cache, &Scene::new(live.stage(), &store), path);
    let transaction = edit(
        &mut store,
        path,
        "primvars:skel:jointIndices",
        Value::array(vec![Value::Int(99)]),
    );
    let bad = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &bad.changes);
    // The adapter supplies the vertex count: invalid indices fail before upload.
    let scene = Scene::new(live.stage(), &store);
    let inputs = cache.deformation_inputs(&scene, path).unwrap().unwrap();
    assert!(inputs.validate_point_count(2).is_err());
    let bad_revision = inputs.revisions();
    let undone = live.apply(&mut store, &bad.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    let restored = revisions(&mut cache, &Scene::new(live.stage(), &store), path);
    assert_ne!(restored.inputs, bad_revision.inputs);
    assert_eq!(restored.pose, before.pose);
    let mut transaction = Transaction::new();
    transaction.remove_spec(EditTarget::for_layer(LayerId(1)).prim(skeleton));
    let removed = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &removed.changes);
    assert!(matches!(
        cache.deformation_inputs(&Scene::new(live.stage(), &store), path),
        Err(SkelError::InvalidTarget { .. })
    ));
    let undone = live.apply(&mut store, &removed.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    let recovered = revisions(&mut cache, &Scene::new(live.stage(), &store), path);
    assert_ne!(recovered.skeleton_definition, restored.skeleton_definition);
    assert_ne!(recovered.binding_definition, restored.binding_definition);
    assert_ne!(recovered.pose, restored.pose);
    let property = PropertyPath::new(path, store.tokens.intern("skel:skeleton"));
    let mut transaction = Transaction::new();
    transaction.set_targets(
        EditTarget::for_layer(LayerId(1)).property(property),
        ListOp::explicit(Vec::<TargetPath>::new()),
    );
    let unbound = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &unbound.changes);
    assert!(
        cache
            .deformation_inputs(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .is_none()
    );
    let undone = live.apply(&mut store, &unbound.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    let rebound = revisions(&mut cache, &Scene::new(live.stage(), &store), path);
    assert_ne!(rebound.binding_definition, recovered.binding_definition);
    assert_eq!(rebound.pose, recovered.pose);
    assert_eq!(cache.stats().point_vertices, 0);
}

#[test]
fn unmapped_joints_have_identity_matrices_and_explicit_dqs_fallback() {
    let source = include_str!("../fixtures/skel_dual_quaternion.usda").replace(
        "def Xform \"Geometry\" (prepend apiSchemas = [\"SkelBindingAPI\"]) {",
        "def Xform \"Geometry\" (prepend apiSchemas = [\"SkelBindingAPI\"]) { uniform token[] skel:joints = [\"missing\", \"b\"]",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Geometry/Vertex");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = SkelCache::new(Time::at(2.));
    let inputs = cache.deformation_inputs(&scene, path).unwrap().unwrap();
    inputs.validate_point_count(2).unwrap();
    assert_eq!(inputs.joint_mapping(), Some(&[None, Some(1)][..]));
    assert_eq!(
        inputs.skinning_transforms()[0],
        [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ]
    );
    let dq = layerstack_schemas::skel::DualQuaternionJoint::IDENTITY;
    assert_eq!(dq.real(), [1., 0., 0., 0.]);
    assert_eq!(dq.dual(), [0.; 4]);
    assert!(!dq.has_scale());
    assert_eq!(
        *dq.residual_scale(),
        [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]]
    );
    assert_eq!(inputs.shared_dual_quaternions().unwrap().len(), 2);
}
