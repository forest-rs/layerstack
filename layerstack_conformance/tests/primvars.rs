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

fn query_scene() -> (InMemoryStore, LiveStage) {
    support::scene(include_str!("../fixtures/primvar_queries.usda"))
}
#[test]
fn id_targets_and_index_animation_queries_match_cpp() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/primvar_queries.json")).unwrap();
    let (mut store, live) = query_scene();
    let path = store.path("/Root/Group/Mesh");
    let scene = Scene::new(live.stage(), &store);
    for name in [
        "id",
        "emptyId",
        "manyId",
        "ids",
        "oneId",
        "notString",
        "notRel",
    ] {
        let pv = Primvar::new(&scene, path, name).unwrap();
        let value = pv.value(Time::Default).unwrap();
        let actual = match value {
            None => serde_json::Value::Null,
            Some(Value::String(v)) => serde_json::json!(&*v),
            Some(Value::Float(v)) => serde_json::json!(v),
            Some(v) => serde_json::json!(
                layerstack_schemas::value::read_array(
                    &v,
                    &store.tokens,
                    layerstack_schemas::value::read_string
                )
                .unwrap()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
            ),
        };
        assert_eq!(actual, expected["idTargets"][name]["value"], "{name}");
        assert_eq!(
            pv.is_id_target(),
            expected["idTargets"][name]["isIdTarget"],
            "{name}"
        );
    }
    for name in ["indexOnly", "both", "blockedIndices", "single"] {
        let pv = Primvar::new(&scene, path, name).unwrap();
        assert_eq!(
            serde_json::json!(pv.sample_times()),
            expected["samples"][name]["times"],
            "{name}"
        );
        assert_eq!(
            pv.might_be_time_varying(),
            expected["samples"][name]["varying"],
            "{name}"
        );
        assert_eq!(
            pv.is_indexed(),
            expected["samples"][name]["indexed"],
            "{name}"
        );
    }
    let pv = Primvar::new(&scene, path, "both").unwrap();
    assert_eq!(pv.sample_times_in_interval(1., 2.), vec![1., 2.]);
    assert!(pv.sample_times_in_interval(2., 1.).is_empty());
    assert!(pv.sample_times_in_interval(f64::NAN, 2.).is_empty());
}
#[test]
fn id_target_authoring_is_atomic_and_undoable() {
    let (mut store, mut live) = query_scene();
    let path = store.path("/Root/Group/Mesh");
    let base = store.layers[&LayerId(1)].clone();
    let handle = Mesh::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let pv = handle
        .create_primvar(
            &mut edit,
            "newId",
            PropertyType::new("string", false, Value::string("")),
        )
        .unwrap();
    pv.set_id_target(&mut edit, None).unwrap();
    pv.set_id_target(&mut edit, None).unwrap(); // Reuse the queued relationship declaration.
    let bad = handle
        .create_primvar(
            &mut edit,
            "newFloat",
            PropertyType::new("float", false, Value::Float(0.)),
        )
        .unwrap();
    let checkpoint = edit.transaction().clone();
    assert!(bad.set_id_target(&mut edit, None).is_err());
    let existing = handle
        .create_primvar(
            &mut edit,
            "notRel",
            PropertyType::new("string", false, Value::string("")),
        )
        .unwrap();
    assert!(existing.set_id_target(&mut edit, None).is_err());
    assert_eq!(edit.transaction(), &checkpoint);
    let transaction = edit.finish();
    let outcome = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        Primvar::new(&scene, path, "newId")
            .unwrap()
            .value(Time::Default)
            .unwrap(),
        Some(Value::string("/Root/Group/Mesh"))
    );
    live.apply(&mut store, &outcome.inverse).unwrap();
    assert_eq!(store.layers[&LayerId(1)], base);
}

#[test]
fn traversal_fed_inheritance_matches_cpp_and_full_queries() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/primvar_queries.json")).unwrap();
    let (mut store, live) = query_scene();
    let paths = [
        "/Root",
        "/Root/Group",
        "/Root/Group/Mesh",
        "/Root/Group/Mesh/Child",
        "/Root/Group/Sibling",
    ];
    let ids: Vec<_> = paths.iter().map(|p| store.path(p)).collect();
    let missing = store.path("/Root/Missing");
    let scene = Scene::new(live.stage(), &store);
    let inherited = PrimView::new(scene, ids[0]).inheritable_primvars();
    let absent = PrimView::new(scene, missing);
    assert!(absent.inheritable_primvars().is_empty());
    assert!(absent.primvars_with_inheritance_from(&inherited).is_empty());
    assert!(absent.find_primvar_with_inheritance("tint").is_none());
    assert!(
        absent
            .find_primvar_with_inheritance_from("tint", &inherited)
            .is_none()
    );
    assert!(
        absent
            .incrementally_inheritable_primvars(&inherited)
            .unwrap()
            .is_empty()
    );
    let entries = |values: &[Primvar<'_>]| {
        serde_json::json!(
            values
                .iter()
                .map(|pv| [
                    pv.name().to_string(),
                    store
                        .paths
                        .display(pv.property().prim_path(), &store.tokens)
                ])
                .collect::<Vec<_>>()
        )
    };
    for (&path, text) in ids.iter().zip(paths) {
        let prim = PrimView::new(scene, path);
        let parent = store.paths.parent(path).unwrap();
        let inherited = PrimView::new(scene, parent).inheritable_primvars();
        let updated = prim.incrementally_inheritable_primvars(&inherited);
        let effective = updated.as_deref().unwrap_or(&inherited);
        assert_eq!(
            entries(effective),
            expected["inheritance"][text]["inheritable"],
            "{text}"
        );
        assert_eq!(entries(&prim.inheritable_primvars()), entries(effective));
        let all = prim.primvars_with_inheritance_from(&inherited);
        assert_eq!(
            entries(&all),
            expected["inheritance"][text]["all"],
            "{text}"
        );
        assert_eq!(entries(&prim.primvars_with_inheritance()), entries(&all));
        for name in ["tint", "barrier", "local", "missing"] {
            assert_eq!(
                prim.find_primvar_with_inheritance_from(name, &inherited)
                    .map(|p| p.property()),
                prim.find_primvar_with_inheritance(name)
                    .map(|p| p.property()),
                "{text}/{name}"
            );
        }
        if text.ends_with("Child") || text.ends_with("Sibling") {
            assert!(updated.is_none(), "unchanged parent set is reusable");
        }
    }
    let mesh = PrimView::new(scene, ids[2]);
    let authored = mesh.authored_primvars();
    assert!(authored.iter().any(|p| p.name() == "unvalued"));
    assert!(!authored.iter().any(|p| p.name() == "displayColor"));
    assert!(
        !mesh
            .primvars_with_authored_values()
            .iter()
            .any(|p| matches!(p.name(), "unvalued" | "blocked"))
    );
    assert!(
        mesh.primvars_with_authored_values()
            .iter()
            .any(|p| p.name() == "both")
    );
}
