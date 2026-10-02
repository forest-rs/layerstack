// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Primvars match a pinned C++ oracle and retain transactional authoring.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    InMemoryStore, InterpolationType, LayerId, LiveStage, PropertyType, Time, Value,
    edit::EditTarget,
};
use layerstack_schemas::{
    PrimView, Scene, SchemaEdit,
    primvar::{Primvar, PrimvarError},
    usd_geom::Mesh,
};

#[path = "support/schema_scene.rs"]
mod support;
fn scene() -> (InMemoryStore, LiveStage) {
    support::scene(include_str!("../fixtures/primvar_behavior.usda"))
}
#[test]
fn inheritance_flattening_and_animation_match_cpp() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/primvar_behavior.json")).unwrap();
    let (mut store, live) = scene();
    let path = store.path("/Root/Group/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let prim = PrimView::new(scene, path);
    assert!(Primvar::new(&scene, path, "packed:indices").is_none());
    for name in ["packed", "scalar", "animated", "blockedIndices", "empty"] {
        let var = Primvar::new(&scene, path, name).unwrap();
        let value = var.compute_flattened(Time::Default).unwrap();
        assert_eq!(var.interpolation(), expected[name]["interpolation"]);
        assert_eq!(var.element_size(), expected[name]["elementSize"]);
        assert_eq!(var.is_indexed(), expected[name]["indexed"]);
        if let Some(value) = value {
            let actual = if let Some(array) = value.array_ref() {
                serde_json::json!(
                    array
                        .iter()
                        .map(|v| match &*v {
                            Value::Float(v) => *v,
                            _ => panic!("float array"),
                        })
                        .collect::<Vec<_>>()
                )
            } else {
                match value {
                    Value::Float(v) => serde_json::json!(v),
                    _ => panic!("scalar"),
                }
            };
            assert_eq!(actual, expected[name]["default"]);
        } else {
            assert!(expected[name]["default"].is_null());
        }
    }
    let packed = Primvar::new(&scene, path, "packed").unwrap();
    assert_eq!(packed.unauthored_values_index(), 1);
    let empty = Primvar::new(&scene, path, "empty")
        .unwrap()
        .compute_flattened(Time::Default)
        .unwrap()
        .unwrap();
    assert!(matches!(
        empty,
        Value::TypedArray(layerstack::TypedArray::Float(_))
    ));
    let animated = Primvar::new(&scene, path, "animated").unwrap();
    let value = animated
        .compute_flattened(Time::At {
            code: 1.0,
            interpolation: InterpolationType::Linear,
        })
        .unwrap()
        .unwrap();
    let values = layerstack_schemas::value::read_float_array(&value, &store.tokens).unwrap();
    assert_eq!(serde_json::json!(values), expected["animated"]["at1"]);
    assert_eq!(
        prim.find_primvar_with_inheritance("tint")
            .unwrap()
            .property()
            .prim_path(),
        store
            .paths
            .parent(store.paths.parent(path).unwrap())
            .unwrap()
    );
    assert!(prim.find_primvar_with_inheritance("barrier").is_none());
    let names: Vec<_> = prim
        .primvars_with_inheritance()
        .iter()
        .map(|p| p.name())
        .collect();
    assert_eq!(serde_json::json!(names), expected["inheritedNames"]);
    assert!(matches!(
        Primvar::new(&scene, path, "invalid")
            .unwrap()
            .compute_flattened(Time::Default),
        Err(PrimvarError::InvalidIndex {
            position: 0,
            index: -1
        })
    ));
}
#[test]
fn primvar_authoring_and_rejections_preserve_undo() {
    let (mut store, mut live) = scene();
    let path = store.path("/Root/Group/Mesh");
    let mesh = Mesh::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .edit();
    let before = store.layers[&LayerId(1)].clone();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let var = mesh
        .create_primvar(
            &mut edit,
            "custom:uv",
            PropertyType::new("float", true, Value::Float(0.)),
        )
        .unwrap();
    var.set(
        &mut edit,
        Value::array(vec![Value::Float(1.), Value::Float(2.)]),
    )
    .unwrap();
    var.set_interpolation(&mut edit, "vertex").unwrap();
    var.set_element_size(&mut edit, 1).unwrap();
    var.set_indices(&mut edit, &[1, 0], None).unwrap();
    let checkpoint = edit.transaction().clone();
    assert!(var.set_interpolation(&mut edit, "bogus").is_err());
    assert!(var.set_element_size(&mut edit, 0).is_err());
    assert!(
        mesh.create_primvar(
            &mut edit,
            "custom:uv",
            PropertyType::new("int", true, Value::Int(0))
        )
        .is_err()
    );
    assert!(
        mesh.create_primvar(
            &mut edit,
            "x:indices",
            PropertyType::new("int", true, Value::Int(0))
        )
        .is_err()
    );
    assert_eq!(edit.transaction(), &checkpoint);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let var = Primvar::new(&scene, path, "custom:uv").unwrap();
    assert_eq!(var.interpolation(), "vertex");
    let flattened = var.compute_flattened(Time::Default).unwrap().unwrap();
    assert_eq!(
        layerstack_schemas::value::read_float_array(&flattened, &store.tokens),
        Some(vec![2., 1.])
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(store.layers[&LayerId(1)], before);
}
