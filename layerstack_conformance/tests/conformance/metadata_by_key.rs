// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim and property metadata by dictionary key path against OpenUSD
//! 26.11, call by call, authored in a root layer over a weaker one.
use crate::support::schema_scene as support;
use layerstack::{FieldValue, InMemoryStore, LayerId, PathId, Value, edit::EditTarget};
use layerstack_schemas::{MetadataError, PrimEdit, PrimView, Scene, SchemaEdit};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use std::sync::Arc;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    calls: Vec<Call>,
}
#[derive(Deserialize)]
struct Call {
    call: String,
    path: String,
    field: String,
    key_path: String,
    value: Json,
    accepted: bool,
    local_custom_data: Option<Json>,
    local_asset_info: Option<Json>,
    composed_custom_data: Json,
    composed_entry: Option<Json>,
}

fn to_json(value: &Value, store: &InMemoryStore) -> Json {
    match value {
        Value::Dictionary(entries) => Json::Object(
            entries
                .iter()
                .map(|(key, value)| (key.to_string(), to_json(value, store)))
                .collect::<Map<_, _>>(),
        ),
        Value::Int(v) => json!(v),
        Value::String(text) => json!(&**text),
        Value::Token(token) => json!(store.tokens.resolve(*token)),
        Value::Asset(path) => json!({ "asset": &**path }),
        other => panic!("no JSON rendering for {other:?}"),
    }
}

fn from_json(value: &Json) -> Value {
    match value {
        Json::Number(n) => Value::Int(i32::try_from(n.as_i64().unwrap()).unwrap()),
        Json::String(text) => Value::String(Arc::from(text.as_str())),
        Json::Object(map) => Value::Asset(Arc::from(map["asset"].as_str().unwrap())),
        other => panic!("no value for {other}"),
    }
}

/// The root layer's field `key` on the prim or property spec at `path`.
fn local(store: &InMemoryStore, prim: PathId, property: Option<&str>, key: &str) -> Option<Json> {
    let spec = store.layer(LayerId(1)).unwrap().prim(prim)?;
    let key = store.tokens.lookup(key)?;
    let fields = match property {
        Some(name) => {
            let name = store.tokens.lookup(name)?;
            spec.properties
                .iter()
                .find(|entry| entry.name == name)?
                .spec
                .metadata
                .as_slice()
        }
        None => spec.fields.as_slice(),
    };
    match layerstack::get_field(fields, &key)? {
        FieldValue::Value(value) => Some(to_json(value, store)),
        other => panic!("unexpected field {other:?}"),
    }
}

#[test]
fn metadata_by_key_matches_cpp_call_by_call() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../../fixtures/metadata_by_key/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, mut live) =
        support::layered_scene(include_str!("../../fixtures/metadata_by_key/weak.usda"));
    for call in &oracle.calls {
        let (prim_text, property) = match call.path.split_once('.') {
            Some((prim, property)) => (prim, Some(property)),
            None => (call.path.as_str(), None),
        };
        let prim = store.path(prim_text);
        let before = store.layer(LayerId(1)).unwrap().clone();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let handle = PrimEdit::new(&edit, prim).unwrap().unwrap();
        let (field, key_path) = (call.field.as_str(), call.key_path.as_str());
        let result = match (call.call.as_str(), property) {
            ("set", None) => {
                handle.write_metadata_by_key(&mut edit, field, key_path, from_json(&call.value))
            }
            ("set", Some(name)) => handle.write_property_metadata_by_key(
                &mut edit,
                name,
                field,
                key_path,
                from_json(&call.value),
            ),
            ("clear", None) => handle.clear_metadata_by_key(&mut edit, field, key_path),
            ("clear", Some(name)) => {
                handle.clear_property_metadata_by_key(&mut edit, name, field, key_path)
            }
            (other, _) => panic!("unknown call {other}"),
        };
        let transaction = edit.finish();
        let label = format!("{} {} {} {}", call.call, call.path, field, key_path);
        // C++ accepts a by-key edit of the string `documentation` field and
        // a key path with an empty part, and authors junk; both are refused.
        let junk = field == "documentation" || key_path.split(':').any(str::is_empty);
        if junk {
            assert!(call.accepted, "{label}");
            assert!(
                matches!(
                    result,
                    Err(MetadataError::Field(_) | MetadataError::InvalidKeyPath(_))
                ),
                "{label}: {result:?}"
            );
            assert!(transaction.is_empty(), "{label}");
            assert_eq!(*store.layer(LayerId(1)).unwrap(), before, "{label}");
            continue;
        }
        assert_eq!(result.is_ok(), call.accepted, "{label}: {result:?}");
        live.apply(&mut store, &transaction).unwrap();
        assert_eq!(
            local(&store, prim, property, "customData"),
            call.local_custom_data,
            "{label}"
        );
        assert_eq!(
            local(&store, prim, property, "assetInfo"),
            call.local_asset_info,
            "{label}"
        );
        let scene = Scene::new(live.stage(), &store);
        let view = PrimView::new(scene, prim);
        let (composed, entry) = match property {
            Some(name) => {
                let metadata = view.property_metadata(name).unwrap();
                (
                    metadata.value("customData"),
                    metadata.value_by_key(field, key_path),
                )
            }
            None => (
                view.metadata_value("customData"),
                view.metadata_by_key(field, key_path),
            ),
        };
        assert_eq!(
            composed.map_or_else(|| json!({}), |value| to_json(&value, &store)),
            call.composed_custom_data,
            "{label}"
        );
        assert_eq!(
            entry.map(|value| to_json(&value, &store)),
            call.composed_entry,
            "{label}"
        );
    }
}

#[test]
fn by_key_edits_in_one_edit_build_on_each_other() {
    let (mut store, mut live) =
        support::layered_scene(include_str!("../../fixtures/metadata_by_key/weak.usda"));
    let prim = store.path("/Q");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = PrimEdit::new(&edit, prim).unwrap().unwrap();
    handle
        .write_metadata_by_key(&mut edit, "customData", "a:b", Value::Int(1))
        .unwrap();
    handle
        .write_metadata_by_key(&mut edit, "customData", "a:c", Value::Int(2))
        .unwrap();
    handle
        .clear_metadata_by_key(&mut edit, "customData", "a:b")
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    assert_eq!(
        local(&store, prim, None, "customData"),
        Some(json!({ "a": { "c": 2 } }))
    );
    // A whole-field write earlier in the same edit is where a key starts.
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let whole = Value::Dictionary(vec![(Arc::from("z"), Value::Int(9))]);
    handle
        .write_metadata(&mut edit, "customData", whole)
        .unwrap();
    handle
        .write_metadata_by_key(&mut edit, "customData", "y", Value::Int(8))
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    assert_eq!(
        local(&store, prim, None, "customData"),
        Some(json!({ "z": 9, "y": 8 }))
    );
}
