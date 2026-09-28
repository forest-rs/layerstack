// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Common stack authoring agrees with C++ and remains an undoable source edit.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    InMemoryStore, Layer, LayerId, LayerStore, LiveStage, StageOptions, edit::EditTarget,
};
use layerstack_conformance::matrices;
use layerstack_schemas::{
    CommonTransform, RotationOrder, Scene, SchemaEdit, Time, XformOpPrecision as P,
    XformOpType as T,
    usd_geom::{Xform, Xformable, XformableEdit},
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    records: Vec<Record>,
}
#[derive(Deserialize)]
struct Record {
    name: String,
    rotation_order: String,
    setup: String,
    accepted: bool,
    order: Option<Vec<String>>,
    matrices: Option<Vec<[[f64; 4]; 4]>>,
}
fn rotation(name: &str) -> RotationOrder {
    match name {
        "XYZ" => RotationOrder::Xyz,
        "XZY" => RotationOrder::Xzy,
        "YXZ" => RotationOrder::Yxz,
        "YZX" => RotationOrder::Yzx,
        "ZXY" => RotationOrder::Zxy,
        "ZYX" => RotationOrder::Zyx,
        _ => panic!("unknown order"),
    }
}
#[test]
fn common_stack_authoring_matches_cpp_and_undoes_exactly() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/common_xform.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let paths: Vec<_> = oracle
        .records
        .iter()
        .map(|r| store.path(&format!("/{}", r.name)))
        .collect();
    let authored = store.layer(LayerId(1)).unwrap().clone();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    for (record, &path) in oracle.records.iter().zip(&paths) {
        Xform::define(&mut edit, path);
        let xform = XformableEdit::new(&edit, path).unwrap();
        match record.setup.as_str() {
            "scale" => {
                xform
                    .add_scale_op(&mut edit, P::Float)
                    .unwrap()
                    .set(&mut edit, [1.0; 3])
                    .unwrap();
            }
            "reset" => {
                xform.set_reset_xform_stack(&mut edit, true);
            }
            "suffix" => {
                xform
                    .add_op(&mut edit, T::RotateXyz, P::Float, Some("spin"), false)
                    .unwrap()
                    .set(&mut edit, [0.0; 3])
                    .unwrap();
            }
            "pivot" => {
                xform
                    .add_op(&mut edit, T::Translate, P::Double, Some("pivot"), false)
                    .unwrap()
                    .set(&mut edit, [0.0; 3])
                    .unwrap();
            }
            "wrong_order" => {
                xform.add_scale_op(&mut edit, P::Float).unwrap();
                xform.add_translate_op(&mut edit, P::Double).unwrap();
            }
            _ => {}
        }
        let before = edit.transaction().clone();
        let mut value = CommonTransform {
            translation: [1.0, 2.0, 3.0],
            rotation: [10.0, 20.0, 30.0],
            scale: [2.0, 3.0, 4.0],
            pivot: [0.5, 1.0, 1.5],
            rotation_order: rotation(&record.rotation_order),
        };
        let result = xform.set_common_transform(&mut edit, &value);
        assert_eq!(
            result.is_ok(),
            record.accepted,
            "{}: {result:?}",
            record.name
        );
        if record.accepted {
            value.translation = [-1.0, 4.0, 2.0];
            value.rotation = [30.0, 10.0, 20.0];
            value.scale = [3.0, 2.0, 1.0];
            value.pivot = [1.0, 0.5, 0.0];
            xform
                .set_common_transform_at(&mut edit, 2.0, &value)
                .unwrap();
        } else {
            assert_eq!(edit.transaction(), &before, "rejected edit is atomic");
        }
    }
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    for (record, &path) in oracle.records.iter().zip(&paths) {
        if let Some(expected) = &record.matrices {
            let view = Xformable::new(&scene, path).unwrap();
            assert_eq!(
                view.xform_op_order().unwrap(),
                *record.order.as_ref().unwrap()
            );
            for (time, matrix) in [Time::Default, Time::at(2.0)].into_iter().zip(expected) {
                let actual = view.local_transform(time);
                assert!(
                    actual.problems.is_empty(),
                    "{} {:?}",
                    record.name,
                    actual.problems
                );
                assert!(
                    matrices::agree(&actual.matrix, matrix),
                    "{} {time:?}",
                    record.name
                );
            }
        }
    }
    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    assert!(paths.iter().all(|path| !live.stage().has_prim(*path)));
    assert_eq!(store.layer(LayerId(1)).unwrap(), &authored);
    let redone = live.apply(&mut store, &undone.inverse).unwrap();
    assert!(paths.iter().all(|path| live.stage().has_prim(*path)));
    live.apply(&mut store, &redone.inverse).unwrap();
    assert!(paths.iter().all(|path| !live.stage().has_prim(*path)));
    assert_eq!(store.layer(LayerId(1)).unwrap(), &authored);
}
