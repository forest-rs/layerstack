// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage APIs agree with native OpenUSD and fresh retained snapshots.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    FieldEntry, FieldValue, InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, Value,
};
use std::{process::Command, sync::Arc};

#[test]
fn fallback_types_select_concrete_schemas_without_changing_authored_type() {
    let mut store = InMemoryStore::default();
    let registry = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let future = store.tokens.intern("FutureMesh");
    let mesh = store.tokens.intern("Mesh");
    let unknown = store.tokens.intern("OtherFuture");
    let abstract_type = store.tokens.intern("Imageable");
    let sphere = store.tokens.intern("Sphere");
    let fallback = store.tokens.intern("fallbackPrimTypes");
    let prim = store.path("/Future");
    let known = store.path("/Known");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(prim, PrimSpec::def().with_type_name(future));
    layer.insert_prim(known, PrimSpec::def().with_type_name(mesh));
    layer.metadata.push(FieldEntry {
        name: fallback,
        value: FieldValue::Value(Value::Dictionary(vec![
            (
                "FutureMesh".into(),
                Value::Array(vec![
                    Value::Token(unknown),
                    Value::Token(abstract_type),
                    Value::Token(mesh),
                ]),
            ),
            ("Mesh".into(), Value::Array(vec![Value::Token(sphere)])),
        ])),
    });
    store.insert_layer(layer);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(registry),
            ..Default::default()
        },
    );
    assert_eq!(stage.resolve_type_name(prim, &store), Some(future));
    assert!(stage.prim_definition_ref(prim).unwrap().is_a(mesh));
    assert!(stage.prim_definition_ref(known).unwrap().is_a(mesh));
    assert!(!stage.prim_definition_ref(known).unwrap().is_a(sphere));
    let subdivision = store.tokens.lookup("subdivisionScheme").unwrap();
    assert_eq!(
        stage
            .resolve_field_with_schema(prim, subdivision, &store)
            .unwrap()
            .value,
        Value::Token(store.tokens.lookup("catmullClark").unwrap())
    );
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Usd"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return;
    }
    let output = Command::new(python).args(["-c", r#"
from pxr import Usd, UsdGeom, Vt
s=Usd.Stage.CreateInMemory()
s.SetMetadata('fallbackPrimTypes', {'FutureMesh':Vt.TokenArray(['OtherFuture','Imageable','Mesh']), 'Mesh':Vt.TokenArray(['Sphere'])})
p=s.DefinePrim('/Future','FutureMesh'); k=s.DefinePrim('/Known','Mesh')
assert p.GetTypeName()=='FutureMesh' and p.IsA(UsdGeom.Mesh)
assert k.IsA(UsdGeom.Mesh) and not k.IsA(UsdGeom.Sphere)
assert p.GetAttribute('subdivisionScheme').Get()=='catmullClark'
"#]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stage_metadata_edits_are_atomic_field_scoped_and_session_aware() {
    use layerstack::{EditTarget, LiveStage, Specifier, StageMetadataError, Transaction};
    let mut store = InMemoryStore::default();
    let prim = store.path("/World");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(prim, PrimSpec::def());
    store.insert_layer(root);
    store.insert_layer(Layer::new(LayerId(2)));
    store.insert_layer(Layer::new(LayerId(3)));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            session_layer: Some(LayerId(2)),
            ..Default::default()
        },
    );
    let data = store.tokens.intern("customLayerData");
    let comment = store.tokens.intern("comment");
    let root_edit = live
        .set_stage_metadata_dict_key(
            &mut store,
            LayerId(1),
            data,
            "render:exposure",
            Some(Value::Double(1.0)),
        )
        .unwrap();
    live.set_stage_metadata(
        &mut store,
        LayerId(1),
        comment,
        FieldValue::Value(Value::string("unrelated")),
    )
    .unwrap();
    live.apply(&mut store, &root_edit.inverse).unwrap();
    assert!(store.layers[&LayerId(1)].metadata(data).is_none());
    assert!(store.layers[&LayerId(1)].metadata(comment).is_some());
    let generation = store.layers[&LayerId(1)].generation();
    live.set_stage_metadata_dict_key(&mut store, LayerId(1), data, "absent:key", None)
        .unwrap();
    assert_eq!(store.layers[&LayerId(1)].generation(), generation);
    assert!(matches!(
        live.set_stage_metadata_dict_key(&mut store, LayerId(3), data, "key", None),
        Err(StageMetadataError::InvalidLayer(LayerId(3)))
    ));
    live.set_stage_metadata_dict_key(
        &mut store,
        LayerId(1),
        data,
        "render:exposure",
        Some(Value::Double(1.0)),
    )
    .unwrap();
    live.set_stage_metadata_dict_key(
        &mut store,
        LayerId(2),
        data,
        "render:quality",
        Some(Value::Int(4)),
    )
    .unwrap();
    assert_eq!(
        live.stage()
            .metadata_dict_key(data, "render:exposure", &store),
        Some(Value::Double(1.0))
    );
    assert!(
        live.stage()
            .has_authored_metadata_dict_key(data, "render:quality", &store)
    );
    live.set_start_time_code(&mut store, LayerId(1), 10.0)
        .unwrap();
    live.set_end_time_code(&mut store, LayerId(2), 50.0)
        .unwrap();
    assert!(live.stage().has_authored_time_code_range(&store));
    assert_eq!(live.stage().start_time_code(&store), 10.0);
    assert_eq!(live.stage().end_time_code(&store), 50.0);
    live.set_frames_per_second(&mut store, LayerId(2), 48.0)
        .unwrap();
    assert_eq!(live.stage().time_codes_per_second(&store), 48.0);
    let name = store.tokens.intern("World");
    let default = live.set_default_prim(&mut store, Some(name)).unwrap();
    assert_eq!(live.stage().default_prim(&mut store), Some(prim));
    live.apply(&mut store, &default.inverse).unwrap();
    assert!(!live.stage().has_authored_default_prim(&store));
    let mut rejected = Transaction::new();
    rejected.set_layer_metadata(
        LayerId(1),
        comment,
        FieldValue::Value(Value::string("rolled back")),
    );
    rejected.create_prim(
        EditTarget::for_layer(LayerId(1)).prim(prim),
        Specifier::Def,
        None,
    );
    let before = store.layers[&LayerId(1)].clone();
    assert!(live.apply(&mut store, &rejected).is_err());
    assert_eq!(store.layers[&LayerId(1)], before);
    let mut guard = Transaction::new();
    guard.expect_layer_metadata(LayerId(1), comment, None);
    guard.clear_layer_metadata(LayerId(1), comment);
    assert!(guard.apply(&mut store).is_err());
}

#[test]
fn fallback_metadata_changes_rebuild_schema_definitions_and_undo() {
    use layerstack::{LiveStage, Transaction};
    let mut store = InMemoryStore::default();
    let registry = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let name = store.tokens.intern("Future");
    let mesh = store.tokens.intern("Mesh");
    let key = store.tokens.intern("fallbackPrimTypes");
    let prim = store.path("/Object");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(prim, PrimSpec::def().with_type_name(name));
    store.insert_layer(root);
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(registry),
            ..Default::default()
        },
    );
    assert!(!live.stage().prim_definition_ref(prim).unwrap().is_a(mesh));
    let mut edit = Transaction::new();
    edit.set_layer_metadata(
        LayerId(1),
        key,
        FieldValue::Value(Value::Dictionary(vec![(
            "Future".into(),
            Value::Array(vec![Value::Token(mesh)]),
        )])),
    );
    let inverse = live.apply(&mut store, &edit).unwrap().inverse;
    assert!(live.stage().prim_definition_ref(prim).unwrap().is_a(mesh));
    live.apply(&mut store, &inverse).unwrap();
    assert!(!live.stage().prim_definition_ref(prim).unwrap().is_a(mesh));
}
