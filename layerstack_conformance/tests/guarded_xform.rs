// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Compound transform authoring and its rejection boundary.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec, PropertyType, StageOptions,
    SublayerEntry, Value, edit::EditTarget,
};
use layerstack_schemas::{
    MatrixXformError, Scene, SchemaEdit, Time, XformCache, usd_geom::XformableEdit,
};
use std::sync::Arc;

fn fixture(
    order: Option<PropertySpec>,
    matrix: Option<PropertySpec>,
) -> (InMemoryStore, LiveStage, layerstack::PathId) {
    let mut store = InMemoryStore::default();
    let path = store.path("/P");
    let mut root = Layer::new(LayerId(1));
    root.sublayers_mut().push(SublayerEntry::new(LayerId(2)));
    let mut prim = PrimSpec::def().with_type_name(store.tokens.intern("Xform"));
    if let Some(order) = order {
        prim = prim.with_property(store.tokens.intern("xformOpOrder"), order);
    }
    if let Some(matrix) = matrix {
        prim = prim.with_property(store.tokens.intern("xformOp:transform"), matrix);
    }
    root.insert_prim(path, prim);
    store.insert_layer(root);
    store.insert_layer(Layer::new(LayerId(2)));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, LayerId(1), options);
    (store, live, path)
}

fn identity() -> [[f64; 4]; 4] {
    core::array::from_fn(|row| core::array::from_fn(|col| if row == col { 1. } else { 0. }))
}

#[test]
fn stronger_order_rejects_without_collecting_any_part_of_the_replacement() {
    let (mut store, live, path) = fixture(
        Some(PropertySpec::attribute().with_default(Value::Array(vec![]))),
        None,
    );
    let original = store.layers().cloned().collect::<Vec<_>>();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(2)));
    let xform = XformableEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    xform
        .add_scale_op(&mut edit, layerstack_schemas::XformOpPrecision::Double)
        .unwrap();
    let before = edit.transaction().clone();
    let error = xform
        .set_matrix_transform(&mut edit, identity())
        .unwrap_err();
    assert!(
        matches!(error, MatrixXformError::StrongerSource { attribute: "xformOpOrder", source } if source.key.layer_id == LayerId(1))
    );
    assert_eq!(edit.transaction(), &before);
    assert!(
        xform
            .get_op(
                &mut edit,
                layerstack_schemas::XformOpType::Scale,
                None,
                false
            )
            .is_some(),
        "failed child group preserves the parent's declaration overlay"
    );
    assert_eq!(store.layers().cloned().collect::<Vec<_>>(), original);
}

#[test]
fn sample_only_matrix_masks_numeric_replacement_but_not_default_replacement() {
    let sampled = PropertySpec::typed_attribute(PropertyType::parse("matrix4d").unwrap())
        .with_time_samples(vec![(0., Value::Matrix4d(Box::new([0.; 16])))]);
    let (mut store, mut live, path) = fixture(None, Some(sampled));
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(2)));
    let xform = XformableEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    let before = edit.transaction().clone();
    assert!(matches!(
        xform.set_matrix_transform_at(&mut edit, 1., identity()),
        Err(MatrixXformError::StrongerSource {
            attribute: "xformOp:transform",
            ..
        })
    ));
    assert_eq!(edit.transaction(), &before);
    let mut matrix = identity();
    matrix[3][0] = 7.;
    xform.set_matrix_transform(&mut edit, matrix).unwrap();
    let transaction = edit.finish();
    let undo = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let mut cache = XformCache::new(Time::Default);
    assert_eq!(cache.local_transform(&scene, path).unwrap().matrix, matrix);
    live.apply(&mut store, &undo.inverse).unwrap();
    assert!(
        store.layer(LayerId(2)).unwrap().prims().is_empty(),
        "undo removes created weak specs"
    );
}

#[test]
fn replacement_at_strongest_site_is_atomic_and_rejects_invalid_inputs() {
    let (mut store, mut live, path) = fixture(None, None);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let xform = XformableEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    let before = edit.transaction().clone();
    let mut invalid = identity();
    invalid[0][0] = f64::NAN;
    assert_eq!(
        xform.set_matrix_transform(&mut edit, invalid),
        Err(MatrixXformError::NonfiniteMatrix)
    );
    assert!(matches!(
        xform.set_matrix_transform_at(&mut edit, f64::INFINITY, identity()),
        Err(MatrixXformError::Inspection(
            layerstack::AuthoringInspectionError::InvalidTime
        ))
    ));
    assert_eq!(edit.transaction(), &before);
    let mut matrix = identity();
    matrix[3][1] = 3.;
    xform
        .set_matrix_transform_at(&mut edit, 2., matrix)
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let mut cache = XformCache::new(Time::at(2.));
    assert_eq!(cache.local_transform(&scene, path).unwrap().matrix, matrix);
}

#[test]
fn referenced_matrix_replacement_maps_namespace_and_stage_time_together() {
    let mut store = InMemoryStore::default();
    let use_ = store.path("/Use");
    let asset = store.path("/Asset");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        use_,
        PrimSpec::def().with_reference(
            layerstack::Reference::new(LayerId(2), asset).with_layer_offset(
                layerstack::LayerOffset {
                    offset: 10.,
                    scale: 2.,
                },
            ),
        ),
    );
    let mut referenced = Layer::new(LayerId(2));
    referenced.insert_prim(
        asset,
        PrimSpec::def().with_type_name(store.tokens.intern("Xform")),
    );
    store.insert_layer(root);
    store.insert_layer(referenced);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let node = live
        .stage()
        .prim(use_, &store)
        .map(|p| p.composition_graph())
        .unwrap()
        .nodes()
        .find(|(_, node)| node.arc_kind() == layerstack::ArcKind::References)
        .unwrap()
        .0;
    let target = EditTarget::for_node(live.stage(), use_, node).unwrap();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    let xform = XformableEdit::new(&edit, use_)
        .expect("outside any instance")
        .unwrap();
    let mut matrix = identity();
    matrix[3][2] = 11.;
    xform
        .set_matrix_transform_at(&mut edit, 14., matrix)
        .unwrap();
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let transform = store.tokens.lookup("xformOp:transform").unwrap();
    let spec = store.layer(LayerId(2)).unwrap().prims()[&asset]
        .property(transform)
        .unwrap();
    assert_eq!(spec.time_samples.as_ref().unwrap()[0].0, 2.);
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        XformCache::new(Time::at(14.))
            .local_transform(&scene, use_)
            .unwrap()
            .matrix,
        matrix
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert!(
        store.layer(LayerId(2)).unwrap().prims()[&asset]
            .properties
            .is_empty()
    );
}

#[test]
fn untyped_existing_matrix_rejects_before_collecting_edits() {
    let (mut store, live, path) = fixture(
        None,
        Some(PropertySpec::attribute().with_default(Value::Matrix4d(Box::new([0.; 16])))),
    );
    let original = store.layers().cloned().collect::<Vec<_>>();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let xform = XformableEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    xform
        .add_scale_op(&mut edit, layerstack_schemas::XformOpPrecision::Double)
        .unwrap();
    let before = edit.transaction().clone();
    assert!(
        matches!(
            xform.set_matrix_transform(&mut edit, identity()),
            Err(MatrixXformError::IncompatibleType {
                attribute: "xformOp:transform",
                ..
            })
        ),
        "an existing untyped attribute must not queue a duplicate property declaration"
    );
    assert_eq!(
        edit.transaction(),
        &before,
        "preflight preserves prior edits"
    );
    assert!(
        xform
            .get_op(
                &mut edit,
                layerstack_schemas::XformOpType::Scale,
                None,
                false
            )
            .is_some(),
        "preflight preserves declaration overlays"
    );
    assert_eq!(
        store.layers().cloned().collect::<Vec<_>>(),
        original,
        "preflight does not mutate layers"
    );
}
