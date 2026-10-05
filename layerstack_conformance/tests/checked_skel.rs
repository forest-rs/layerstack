// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained skeletal inputs fail explicitly instead of invoking absence fallback.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    ArrayReadError, DeferredArraySource, EditTarget, LayerId, PropertyPath, Transaction,
    TypedArray, Value,
};
use layerstack_schemas::{
    Scene, Time,
    skel::{BlendShapeCache, BlendShapeQuery, SkelCache, SkelError, SkinningQuery},
    usd_skel::{SkelAnimation, Skeleton},
};
use std::sync::Arc;

#[derive(Debug)]
struct FailedArray {
    kind: Value,
    error: ArrayReadError,
}
impl DeferredArraySource for FailedArray {
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
        Err(&self.error)
    }
    fn element_kind(&self) -> Value {
        self.kind.clone()
    }
}
fn failed(kind: Value) -> Value {
    Value::TypedArray(TypedArray::Deferred(Arc::new(FailedArray {
        kind,
        error: ArrayReadError::BudgetExceeded { limit: 23 },
    })))
}
fn expected(property: PropertyPath) -> SkelError {
    SkelError::Decode {
        property,
        error: ArrayReadError::BudgetExceeded { limit: 23 },
    }
}

#[test]
fn malformed_weights_do_not_become_missing_animation() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
    let animation = store.path("/Rig/Animation");
    let property = store.property_path("/Rig/Animation.blendShapeWeights");
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        failed(Value::Float(0.)),
    );
    live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let result = SkelAnimation::new(&scene, animation)
        .unwrap()
        .compute_blend_shape_weights(Time::Default);
    assert_eq!(result, Err(expected(property)));
}

#[test]
fn numeric_pose_inputs_preserve_decode_errors_in_queries_and_retained_cache() {
    for (name, kind) in [
        ("translations", Value::Vec3f([0.; 3])),
        ("rotations", Value::Quatf([0.; 4])),
        ("scales", Value::Vec3h([0; 3])),
    ] {
        let (mut store, mut live) =
            support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
        let animation = store.path("/Rig/Animation");
        let skeleton = store.path("/Rig/Skeleton");
        let property = store.property_path(&format!("/Rig/Animation.{name}"));
        let mut cache = SkelCache::new(Time::Default);
        cache
            .skinning_transforms(&Scene::new(live.stage(), &store), skeleton)
            .unwrap();
        let mut edit = Transaction::new();
        edit.set_default(
            EditTarget::for_layer(LayerId(1)).property(property),
            failed(kind),
        );
        let report = live.apply(&mut store, &edit).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &report.changes);
        assert_eq!(
            SkelAnimation::new(&scene, animation)
                .unwrap()
                .compute_joint_local_transform_components(Time::Default),
            Err(expected(property))
        );
        let query = Skeleton::new(&scene, skeleton).unwrap().query().unwrap();
        assert_eq!(
            query.local_transforms(Time::Default),
            Err(expected(property))
        );
        assert_eq!(
            cache.skinning_transforms(&scene, skeleton),
            Err(expected(property))
        );
        let undo = live.apply(&mut store, &report.inverse).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &undo.changes);
        assert!(cache.skinning_transforms(&scene, skeleton).is_ok());
    }
}

#[test]
fn rest_and_bind_snapshot_decode_errors_do_not_become_missing_transforms() {
    for name in ["restTransforms", "bindTransforms"] {
        let (mut store, mut live) =
            support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
        let skeleton = store.path("/Rig/Skeleton");
        let property = store.property_path(&format!("/Rig/Skeleton.{name}"));
        let mut edit = Transaction::new();
        edit.set_default(
            EditTarget::for_layer(LayerId(1)).property(property),
            failed(Value::Matrix4d(Box::new([0.; 16]))),
        );
        live.apply(&mut store, &edit).unwrap();
        let scene = Scene::new(live.stage(), &store);
        assert_eq!(
            Skeleton::new(&scene, skeleton).unwrap().query().map(|_| ()),
            Err(expected(property))
        );
        assert_eq!(
            SkelCache::new(Time::Default).skinning_transforms(&scene, skeleton),
            Err(expected(property))
        );
    }
}

#[test]
fn optional_shape_arrays_and_inbetween_arrays_do_not_hide_decode_failures() {
    for (name, kind) in [
        ("pointIndices", Value::Int(0)),
        ("offsets", Value::Vec3f([0.; 3])),
        ("normalOffsets", Value::Vec3f([0.; 3])),
        ("inbetweens:half", Value::Vec3f([0.; 3])),
        ("inbetweens:half:normalOffsets", Value::Vec3f([0.; 3])),
    ] {
        let (mut store, mut live) =
            support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
        let mesh = store.path("/Rig/Geometry/Mesh");
        let property = store.property_path(&format!("/Rig/Smile.{name}"));
        let mut cache = BlendShapeCache::new(Time::Default);
        cache
            .inputs(&Scene::new(live.stage(), &store), mesh)
            .unwrap();
        let mut edit = Transaction::new();
        edit.set_default(
            EditTarget::for_layer(LayerId(1)).property(property),
            failed(kind),
        );
        let report = live.apply(&mut store, &edit).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &report.changes);
        assert_eq!(
            BlendShapeQuery::new(&scene, mesh).map(|_| ()),
            Err(expected(property))
        );
        assert_eq!(
            cache.inputs(&scene, mesh).map(|_| ()),
            Err(expected(property))
        );
    }
}

#[test]
fn weights_fail_closed_in_skeleton_and_morph_caches_and_recover_after_undo() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
    let mesh = store.path("/Rig/Geometry/Mesh");
    let property = store.property_path("/Rig/Animation.blendShapeWeights");
    let mut morph = BlendShapeCache::new(Time::Default);
    let mut skel = SkelCache::new(Time::Default);
    let scene = Scene::new(live.stage(), &store);
    morph.inputs(&scene, mesh).unwrap();
    skel.deformation_inputs(&scene, mesh).unwrap();
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        failed(Value::Float(0.)),
    );
    let report = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    morph.apply_changes(&scene, &report.changes);
    skel.apply_changes(&scene, &report.changes);
    assert_eq!(
        morph.inputs(&scene, mesh).map(|_| ()),
        Err(expected(property))
    );
    assert_eq!(
        skel.deformation_inputs(&scene, mesh).map(|_| ()),
        Err(expected(property))
    );
    morph.set_time(Time::at(1.));
    skel.set_time(Time::at(1.));
    assert!(morph.inputs(&scene, mesh).unwrap().is_some());
    assert!(skel.deformation_inputs(&scene, mesh).unwrap().is_some());
    morph.set_time(Time::Default);
    skel.set_time(Time::Default);
    assert_eq!(
        morph.inputs(&scene, mesh).map(|_| ()),
        Err(expected(property))
    );
    assert_eq!(
        skel.deformation_inputs(&scene, mesh).map(|_| ()),
        Err(expected(property))
    );
    let undo = live.apply(&mut store, &report.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    morph.apply_changes(&scene, &undo.changes);
    skel.apply_changes(&scene, &undo.changes);
    assert!(morph.inputs(&scene, mesh).unwrap().is_some());
    assert!(skel.deformation_inputs(&scene, mesh).unwrap().is_some());
}

#[test]
fn inherited_influence_values_and_index_arrays_retain_source_decode_errors() {
    for (name, kind) in [
        ("primvars:skel:jointIndices", Value::Int(0)),
        ("primvars:skel:jointIndices:indices", Value::Int(0)),
        ("primvars:skel:jointWeights", Value::Float(0.)),
        ("primvars:skel:jointWeights:indices", Value::Int(0)),
    ] {
        let (mut store, mut live) =
            support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
        let mesh = store.path("/Rig/Geometry/Mesh");
        let property = store.property_path(&format!("/Rig/Geometry.{name}"));
        let mut edit = Transaction::new();
        edit.set_default(
            EditTarget::for_layer(LayerId(1)).property(property),
            failed(kind),
        );
        live.apply(&mut store, &edit).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let query = SkinningQuery::new(&scene, mesh).unwrap().unwrap();
        let error = SkelError::Primvar {
            prim: property.prim_path(),
            source: layerstack_schemas::primvar::PrimvarError::Decode {
                property,
                error: ArrayReadError::BudgetExceeded { limit: 23 },
            },
        };
        assert_eq!(
            query.binding_inputs(Time::Default).map(|_| ()),
            Err(error.clone())
        );
        assert_eq!(
            SkelCache::new(Time::Default)
                .deformation_inputs(&scene, mesh)
                .map(|_| ()),
            Err(error)
        );
    }
}

#[test]
fn default_time_skips_incompatible_stronger_values_before_preserving_decode_errors() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
    let animation = store.path("/Rig/Animation");
    let property = store.property_path("/Rig/Animation.blendShapeWeights");
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        failed(Value::Float(0.)),
    );
    live.apply(&mut store, &edit).unwrap();
    let mut session = layerstack::Layer::new(LayerId(2));
    session.insert_prim(
        animation,
        layerstack::PrimSpec::over().with_property(
            property.property(),
            layerstack::PropertySpec::attribute().with_default(Value::String("wrong type".into())),
        ),
    );
    store.insert_layer(session);
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let stage = layerstack::Stage::compose(
        &mut store,
        LayerId(1),
        layerstack::StageOptions {
            session_layer: Some(LayerId(2)),
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    let scene = Scene::new(&stage, &store);
    let query = SkelAnimation::new(&scene, animation).unwrap();
    assert_eq!(
        query.compute_blend_shape_weights(Time::Default),
        Err(expected(property))
    );
    assert_eq!(
        query.compute_blend_shape_weights(Time::at(1.)),
        Ok(None),
        "numeric source selection does not retry weaker values"
    );
}

#[test]
fn point_buffers_fail_explicitly_in_snapshot_and_retained_deformation() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
    let mesh = store.path("/Rig/Geometry/Mesh");
    let property = store.property_path("/Rig/Geometry/Mesh.points");
    let mut skel = SkelCache::new(Time::Default);
    let mut morph = BlendShapeCache::new(Time::Default);
    let scene = Scene::new(live.stage(), &store);
    skel.deformed_points(&scene, mesh).unwrap();
    morph.deformed_points(&scene, mesh).unwrap();
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        failed(Value::Vec3f([0.; 3])),
    );
    let report = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    skel.apply_changes(&scene, &report.changes);
    morph.apply_changes(&scene, &report.changes);
    let query = SkinningQuery::new(&scene, mesh).unwrap().unwrap();
    assert_eq!(
        query.compute_skinned_points(Time::Default),
        Err(expected(property))
    );
    assert_eq!(
        query.compute_deformed_points(Time::Default),
        Err(expected(property))
    );
    assert_eq!(skel.deformed_points(&scene, mesh), Err(expected(property)));
    assert_eq!(morph.deformed_points(&scene, mesh), Err(expected(property)));
}

#[test]
fn influence_default_time_retries_typed_sources_and_retains_weaker_decode_failures() {
    for (name, kind) in [
        ("primvars:skel:jointIndices", Value::Int(0)),
        ("primvars:skel:jointIndices:indices", Value::Int(0)),
        ("primvars:skel:jointWeights", Value::Float(0.)),
        ("primvars:skel:jointWeights:indices", Value::Int(0)),
    ] {
        for corrupt in [false, true] {
            let (mut store, mut live) =
                support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
            let mesh = store.path("/Rig/Geometry/Mesh");
            let source = store.path("/Rig/Geometry");
            let property = store.property_path(&format!("/Rig/Geometry.{name}"));
            let scene = Scene::new(live.stage(), &store);
            let baseline = SkinningQuery::new(&scene, mesh)
                .unwrap()
                .unwrap()
                .binding_inputs(Time::Default)
                .unwrap();
            if corrupt {
                let mut edit = Transaction::new();
                edit.set_default(
                    EditTarget::for_layer(LayerId(1)).property(property),
                    failed(kind.clone()),
                );
                live.apply(&mut store, &edit).unwrap();
            }
            let mut session = layerstack::Layer::new(LayerId(2));
            session.insert_prim(
                source,
                layerstack::PrimSpec::over().with_property(
                    property.property(),
                    layerstack::PropertySpec::attribute()
                        .with_default(Value::String("wrong numeric array type".into())),
                ),
            );
            store.insert_layer(session);
            let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
            let stage = layerstack::Stage::compose(
                &mut store,
                LayerId(1),
                layerstack::StageOptions {
                    session_layer: Some(LayerId(2)),
                    schemas: Some(schemas),
                    ..Default::default()
                },
            );
            let scene = Scene::new(&stage, &store);
            let query = SkinningQuery::new(&scene, mesh).unwrap().unwrap();
            let result = query.binding_inputs(Time::Default);
            if corrupt {
                assert_eq!(
                    result.map(|_| ()),
                    Err(SkelError::Primvar {
                        prim: source,
                        source: layerstack_schemas::primvar::PrimvarError::Decode {
                            property,
                            error: ArrayReadError::BudgetExceeded { limit: 23 },
                        },
                    }),
                    "{name}"
                );
            } else {
                let actual = result.unwrap();
                assert_eq!(
                    actual.influences().indices,
                    baseline.influences().indices,
                    "{name}"
                );
                assert_eq!(
                    actual.influences().weights,
                    baseline.influences().weights,
                    "{name}"
                );
            }
            assert!(
                query.binding_inputs(Time::at(1.)).is_err(),
                "numeric source selection for {name} must not retry weaker values"
            );
        }
    }
}

#[test]
fn normal_buffers_fail_explicitly_in_snapshot_and_retained_deformation() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/skel_blend_shapes.usda"));
    let mesh = store.path("/Rig/Geometry/Mesh");
    let property = store.property_path("/Rig/Geometry/Mesh.normals");
    let target = EditTarget::for_layer(LayerId(1));
    let mut author = Transaction::new();
    author.create_property(
        target.property(property),
        layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "normal3f",
            true,
            Value::Vec3f([0.; 3]),
        ))
        .with_default(Value::from(vec![[0_f32, 1., 0.]; 2])),
    );
    live.apply(&mut store, &author).unwrap();
    let mut skel = SkelCache::new(Time::Default);
    let mut morph = BlendShapeCache::new(Time::Default);
    let scene = Scene::new(live.stage(), &store);
    skel.skinned_normals(&scene, mesh).unwrap();
    skel.deformed_normals(&scene, mesh).unwrap();
    morph.deformed_normals(&scene, mesh).unwrap();
    let mut edit = Transaction::new();
    edit.set_default(target.property(property), failed(Value::Vec3f([0.; 3])));
    let report = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    skel.apply_changes(&scene, &report.changes);
    morph.apply_changes(&scene, &report.changes);
    let query = SkinningQuery::new(&scene, mesh).unwrap().unwrap();
    assert_eq!(
        query.compute_skinned_normals(Time::Default),
        Err(expected(property))
    );
    assert_eq!(
        query.compute_deformed_normals(Time::Default),
        Err(expected(property))
    );
    assert_eq!(skel.skinned_normals(&scene, mesh), Err(expected(property)));
    assert_eq!(skel.deformed_normals(&scene, mesh), Err(expected(property)));
    assert_eq!(
        morph.deformed_normals(&scene, mesh),
        Err(expected(property))
    );
}
