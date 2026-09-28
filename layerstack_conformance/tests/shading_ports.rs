// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Port authoring preserves layered opinions and authored-state undo.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    InMemoryStore, Layer, LayerId, LayerStore, LiveStage, PropertyPath, PropertyType, StageOptions,
    Value, edit::EditTarget,
};
use layerstack_schemas::{
    Scene, SchemaEdit, Time,
    shading::{Port, PortEdit, PortError, PortKind, ValueSourceKind},
    usd_shade::{Material, Shader},
};
use std::sync::Arc;

#[test]
fn layered_connections_match_cpp_and_undo_authored_absence() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/shading_ports.json")).unwrap();
    assert_eq!(oracle["version"], layerstack_schemas::OPENUSD_VERSION);
    let mut store = InMemoryStore::default();
    let mut strong = Layer::new(LayerId(1));
    strong
        .sublayers
        .push(layerstack::SublayerEntry::new(LayerId(2)));
    store.insert_layer(strong);
    store.insert_layer(Layer::new(LayerId(2)));
    let material = store.path("/Material");
    let shader = store.path("/Material/Shader");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let ty = PropertyType::new("float", false, Value::Float(0.));
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(2)));
    let m = Material::define(&mut edit, material);
    let s = Shader::define(&mut edit, shader);
    let interface = m.create_input(&mut edit, "roughness", ty.clone()).unwrap();
    let input = s.create_input(&mut edit, "roughness", ty.clone()).unwrap();
    let output = s.create_output(&mut edit, "result", ty.clone()).unwrap();
    interface.set(&mut edit, Value::Float(0.25)).unwrap();
    input.set(&mut edit, Value::Float(0.75)).unwrap();
    output.set(&mut edit, Value::Float(0.5)).unwrap();
    input
        .set_sources(&mut edit, std::slice::from_ref(&interface))
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let path = store.property_path("/Material/Shader.inputs:roughness");
    let original = store.layer(LayerId(1)).unwrap().clone();
    let weak = store.layer(LayerId(2)).unwrap().clone();
    let mut inverses = Vec::new();
    for record in oracle["records"].as_array().unwrap() {
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let handle = PortEdit::get(&mut edit, path).unwrap();
        match record["name"].as_str().unwrap() {
            "inherited" => {}
            "disconnected" => {
                handle.disconnect_sources(&mut edit).unwrap();
            }
            "cleared" | "cleared_again" => {
                handle.clear_sources(&mut edit).unwrap();
            }
            "replaced" => {
                handle
                    .set_sources(&mut edit, &[output.clone(), interface.clone()])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let transaction = edit.finish();
        inverses.push(live.apply(&mut store, &transaction).unwrap().inverse);
        let scene = Scene::new(live.stage(), &store);
        let port = Port::get(&scene, path).unwrap();
        let display = |p: PropertyPath| p.display(&store.paths, &store.tokens);
        assert_eq!(
            serde_json::json!(
                port.connected_sources()
                    .sources
                    .into_iter()
                    .map(display)
                    .collect::<Vec<_>>()
            ),
            record["connections"]
        );
        assert_eq!(
            serde_json::json!(
                port.value_sources()
                    .sources
                    .into_iter()
                    .map(|s| display(s.attribute))
                    .collect::<Vec<_>>()
            ),
            record["providers"]
        );
        let authored = store
            .layer(LayerId(1))
            .unwrap()
            .property(path)
            .is_some_and(|p| p.targets.is_some());
        assert_eq!(serde_json::json!(authored), record["local_connections"]);
        assert_eq!(
            port.value(Time::Default),
            Some(Value::Float(0.75)),
            "own value does not follow connections"
        );
        assert_eq!(port.property_type(), Some(ty.clone()));
        assert_eq!(port.name(), "roughness");
        assert_eq!(port.kind(), PortKind::Input);
        let view = Shader::new(&scene, shader).unwrap();
        assert_eq!(view.input("roughness").unwrap().path(), path);
        assert_eq!(view.output("result").unwrap().name(), "result");
        assert_eq!(view.ports(PortKind::Input).len(), 1);
        assert_eq!(view.ports(PortKind::Output).len(), 1);
        if record["name"] == "disconnected" {
            assert_eq!(
                port.value_sources().sources[0].kind,
                ValueSourceKind::AuthoredValue
            );
        }
    }
    for inverse in inverses.iter().rev() {
        live.apply(&mut store, inverse).unwrap();
    }
    assert_eq!(store.layer(LayerId(1)).unwrap(), &original);
    assert_eq!(store.layer(LayerId(2)).unwrap(), &weak);
}

#[test]
fn port_preflight_rejects_invalid_names_relationships_and_types_without_edits() {
    let mut store = InMemoryStore::default();

    let path = store.path("/Shader");
    let relationship = store.tokens.intern("inputs:relationship");
    let shader_type = store.tokens.intern("Shader");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        path,
        layerstack::PrimSpec::def()
            .with_type_name(shader_type)
            .with_property(relationship, layerstack::PropertySpec::relationship()),
    );
    store.insert_layer(layer);
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let shader = Shader::define(&mut edit, path);
    let ty = PropertyType::new("float", false, Value::Float(0.));
    let input = shader.create_input(&mut edit, "value", ty.clone()).unwrap();
    for name in ["", ":bad", "bad:", "bad::name", "a/b"] {
        let before = edit.transaction().clone();
        assert!(matches!(
            shader.create_input(&mut edit, name, ty.clone()),
            Err(PortError::InvalidName(_))
        ));
        assert_eq!(edit.transaction(), &before);
    }
    let before = edit.transaction().clone();
    assert!(matches!(
        shader.create_input(&mut edit, "relationship", ty.clone()),
        Err(PortError::InvalidAttribute { .. })
    ));
    assert_eq!(edit.transaction(), &before);
    assert!(PortEdit::get(&mut edit, PropertyPath::new(path, relationship)).is_none());
    assert!(matches!(
        shader.create_input(
            &mut edit,
            "value",
            PropertyType::new("double", false, Value::Double(0.))
        ),
        Err(PortError::TypeMismatch { .. })
    ));
    assert_eq!(edit.transaction(), &before);
    assert_eq!(
        shader.create_input(&mut edit, "value", ty.clone()).unwrap(),
        input
    );
    assert_eq!(
        edit.transaction(),
        &before,
        "getting an existing port does not reauthor it"
    );
    shader.create_input(&mut edit, "nested:value", ty).unwrap();
}
