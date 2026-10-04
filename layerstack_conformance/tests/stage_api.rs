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
