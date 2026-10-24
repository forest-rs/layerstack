// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property metadata authoring against OpenUSD's `SetMetadata`, call by call.
//!
//! `scripts/property_metadata_oracle.py` authors core Sdf fields, plugin
//! fields (`bindMaterialAs`, `connectability`, `weight`, `elementSize`,
//! `interpolation`), an unregistered key and values of the wrong type on an
//! attribute, a relationship, an unauthored schema attribute and a missing
//! property, recording what C++ accepts and reads back. The test replays
//! every call through `PrimEdit::write_property_metadata`. One difference
//! is deliberate: C++ also writes spec fields (`typeName`, `custom`,
//! `default`) through `SetMetadata`, even a `default` on a relationship;
//! layerstack refuses them, since they have their own edits.
//!
//! Spec: AOUSD Core §7.4 (metadata), §12.2 (metadata resolution).

use crate::support::schema_scene as support;

use std::sync::Arc;

use layerstack::{EditTarget, LayerId, ResolvedValue, Value};
use layerstack::{MetadataEditError, MetadataEditReason, MetadataTarget, MetadataType};
use layerstack_schemas::{MetadataError, PrimEdit, SchemaEdit};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/property_metadata/oracle.json");

const SCENE: &str = r#"#usda 1.0
def Mesh "P"
{
    custom float a
    rel r
}
"#;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    calls: Vec<Call>,
    schema_spec: SchemaSpec,
}

#[derive(Deserialize)]
struct Call {
    property: String,
    key: String,
    #[serde(rename = "type")]
    ty: String,
    value: serde_json::Value,
    accepted: bool,
    read: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct SchemaSpec {
    exists: bool,
    custom: bool,
    variability: String,
    type_name: String,
}

/// The Rust value the oracle's tagged value stands for.
fn value(ty: &str, json: &serde_json::Value, tokens: &mut layerstack::TokenInterner) -> Value {
    match ty {
        "string" => Value::String(Arc::from(json.as_str().unwrap())),
        "token" => Value::Token(tokens.intern(json.as_str().unwrap())),
        "int" => Value::Int(i32::try_from(json.as_i64().unwrap()).unwrap()),
        "bool" => Value::Bool(json.as_bool().unwrap()),
        // The oracle's floats are exact in `f32` (0.5).
        "float" => Value::Float(json.to_string().parse().unwrap()),
        "double" => Value::Double(json.as_f64().unwrap()),
        "token[]" => Value::Array(
            json.as_array()
                .unwrap()
                .iter()
                .map(|item| Value::Token(tokens.intern(item.as_str().unwrap())))
                .collect(),
        ),
        "dictionary" => Value::Dictionary(
            json.as_object()
                .unwrap()
                .iter()
                .map(|(key, item)| {
                    (
                        Arc::from(key.as_str()),
                        Value::Int(i32::try_from(item.as_i64().unwrap()).unwrap()),
                    )
                })
                .collect(),
        ),
        other => panic!("untagged {other}"),
    }
}

/// The composed metadata value as the oracle writes it.
fn json(value: &ResolvedValue, tokens: &layerstack::TokenInterner) -> serde_json::Value {
    fn scalar(value: &Value, tokens: &layerstack::TokenInterner) -> serde_json::Value {
        match value {
            Value::String(text) => text.as_ref().into(),
            Value::Token(token) => tokens.resolve(*token).into(),
            Value::Int(n) => (*n).into(),
            Value::Bool(b) => (*b).into(),
            Value::Float(x) => f64::from(*x).into(),
            Value::Double(x) => (*x).into(),
            Value::Array(items) => items.iter().map(|item| scalar(item, tokens)).collect(),
            Value::TypedArray(items) => items
                .as_token()
                .unwrap()
                .iter()
                .map(|token| serde_json::Value::from(tokens.resolve(*token)))
                .collect(),
            Value::Dictionary(entries) => entries
                .iter()
                .map(|(key, item)| (key.to_string(), scalar(item, tokens)))
                .collect::<serde_json::Map<_, _>>()
                .into(),
            other => panic!("unexpected {other:?}"),
        }
    }
    match value {
        ResolvedValue::Scalar(value) => scalar(value, tokens),
        ResolvedValue::Dictionary(entries) => scalar(&Value::Dictionary(entries.clone()), tokens),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn property_metadata_authoring_matches_openusd_call_by_call() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, mut live) = support::scene(SCENE);
    let prim = store.path("/P");
    for call in &oracle.calls {
        let label = format!(
            "{}.{} = {} {}",
            call.property, call.key, call.ty, call.value
        );
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let value = value(&call.ty, &call.value, edit.tokens());
        let result = PrimEdit::new(&edit, prim)
            .expect("outside any instance")
            .unwrap()
            .write_property_metadata(&mut edit, &call.property, &call.key, value);
        if result.is_err() {
            assert!(
                edit.transaction().is_empty(),
                "{label}: a refusal collects nothing"
            );
        }
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let target = if call.property == "a" {
            MetadataTarget::Attribute
        } else {
            MetadataTarget::Relationship
        };
        let refused = |reason| {
            Err(MetadataError::Field(MetadataEditError {
                field: call.key.as_str().into(),
                target,
                reason,
            }))
        };
        if matches!(call.key.as_str(), "typeName" | "custom" | "default") {
            // A field of the spec with its own edit, or one the kind of
            // spec has not (a relationship's `typeName`).
            let reason = if layerstack::is_spec_field(&call.key, target) {
                MetadataEditReason::SpecField
            } else {
                MetadataEditReason::NotForSpec
            };
            assert_eq!(result, refused(reason), "{label}");
            continue;
        }
        assert_eq!(result.is_ok(), call.accepted, "{label}: {result:?}");
        match (&result, call.property.as_str(), call.key.as_str()) {
            (Ok(()), ..) => {
                let property = store.property_path(&format!("/P.{}", call.property));
                let key = store.tokens.lookup(&call.key).unwrap();
                let read = live
                    .view(&store)
                    .property(property)
                    .and_then(|property| property.metadata(key))
                    .unwrap();
                assert_eq!(Some(json(&read.value, &store.tokens)), call.read, "{label}");
            }
            (Err(error), "nope", _) => assert_eq!(*error, MetadataError::NoSuchProperty),
            (Err(_), _, "unknownKey") => {
                assert_eq!(result, refused(MetadataEditReason::Unregistered), "{label}");
            }
            (Err(_), _, "documentation") => assert_eq!(
                result,
                refused(MetadataEditReason::ValueType {
                    expected: MetadataType::parse("string").unwrap()
                }),
                "{label}"
            ),
            (Err(_), ..) => {
                assert_eq!(result, refused(MetadataEditReason::NotForSpec), "{label}");
            }
        }
    }
    // An unauthored schema attribute gets its spec from the definition.
    let spec = &oracle.schema_spec;
    assert!(spec.exists);
    let property = store.property_path("/P.doubleSided");
    let declaration = live
        .view(&store)
        .property(property)
        .and_then(|property| property.declaration())
        .unwrap();
    assert_eq!(declaration.custom, spec.custom);
    assert_eq!(spec.variability, "Sdf.VariabilityUniform");
    assert_eq!(declaration.variability, layerstack::Variability::Uniform);
    assert_eq!(
        declaration.type_name.unwrap().type_name(),
        spec.type_name.as_str()
    );
}

/// Clearing removes the field from the edit target; clearing where no spec
/// exists collects nothing.
#[test]
fn property_metadata_clears_only_target_specs() {
    let (mut store, mut live) = support::scene(SCENE);
    let prim = store.path("/P");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = PrimEdit::new(&edit, prim)
        .expect("outside any instance")
        .unwrap();
    handle
        .write_property_metadata(&mut edit, "a", "displayName", Value::string("A"))
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    handle
        .clear_property_metadata(&mut edit, "doubleSided", "documentation")
        .unwrap();
    assert!(edit.transaction().is_empty(), "no spec, nothing to clear");
    handle
        .clear_property_metadata(&mut edit, "a", "displayName")
        .unwrap();
    assert_eq!(
        handle.clear_property_metadata(&mut edit, "a", "noLoadHint"),
        Err(MetadataError::Field(MetadataEditError {
            field: "noLoadHint".into(),
            target: MetadataTarget::Attribute,
            reason: MetadataEditReason::NotForSpec,
        }))
    );
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let property = store.property_path("/P.a");
    let key = store.tokens.lookup("displayName").unwrap();
    assert!(
        live.view(&store)
            .property(property)
            .and_then(|property| property.metadata(key))
            .is_none()
    );
    assert!(
        live.stage()
            .prim(prim, &store)
            .unwrap()
            .property("doubleSided")
            .is_some()
    );
}
