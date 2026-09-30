// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed default-time source selection, compared with `UsdAttribute::Get<T>`.
#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::{InterpolationType, LayerId, ResolvedValue, Stage, StageOptions, Time, Value};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::{Scene, usd_lux::DistantLight};
use serde_json::{Value as Json, json};

fn convert(value: &Value, kind: &str) -> Option<Json> {
    match (kind, value) {
        ("float", Value::Float(value)) => Some(json!(value)),
        ("double", Value::Double(value)) => Some(json!(value)),
        ("float3", Value::Vec3f(value)) => Some(json!(value)),
        ("float[]" | "double[]" | "float3[]" | "timecode[]", value) => {
            let element = kind.strip_suffix("[]")?;
            let values = value.array_ref()?;
            if let Some(typed) = values.typed() {
                convert(&typed.element_kind(), element)?;
            }
            values
                .iter()
                .map(|value| convert(&value, element))
                .collect::<Option<Vec<_>>>()
                .map(Json::Array)
        }
        ("matrix4d", Value::Matrix4d(value)) => Some(json!(value.as_chunks::<4>().0)),
        ("timecode", Value::TimeCode(value)) => Some(json!(value)),
        ("asset", Value::Asset(value)) => Some(json!(&**value)),
        ("pathExpression", Value::PathExpression(value)) => Some(json!(&**value)),
        _ => None,
    }
}

fn fixture() -> (LoadedStage, Stage, LayerId) {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/typed_reads/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    // Match the oracle's SetField mutations: actual values can differ from
    // the declared type, and typed reads inspect actual storage.
    let dictionary = loaded.store.property_path("/WrongDictionary.value");
    loaded
        .store
        .layers
        .get_mut(&loaded.root_layer)
        .unwrap()
        .property_mut(dictionary)
        .unwrap()
        .default = Some(Value::Dictionary(vec![("key".into(), Value::Int(1))]));
    let timecode = loaded.store.property_path("/MisdeclaredTime.value");
    let weak_marker = loaded.store.property_path("/Skip.inputs:intensity");
    let weak = *loaded
        .store
        .layers
        .iter()
        .find(|(_, layer)| {
            layer
                .property(weak_marker)
                .is_some_and(|spec| spec.default == Some(Value::Float(3.0)))
        })
        .unwrap()
        .0;
    loaded
        .store
        .layers
        .get_mut(&weak)
        .unwrap()
        .property_mut(timecode)
        .unwrap()
        .default = Some(Value::TimeCode(2.0));
    let schemas = layerstack_schemas::openusd(&mut loaded.store.tokens);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(Arc::new(schemas)),
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    (loaded, stage, weak)
}

#[test]
fn typed_defaults_skip_incompatible_opinions_without_numeric_retry() {
    let oracle: Json =
        serde_json::from_str(include_str!("../fixtures/typed_reads/oracle.json")).unwrap();
    let (mut loaded, stage, weak) = fixture();
    for (text, record) in oracle.as_object().unwrap() {
        let (prim, name) = text.rsplit_once('.').unwrap();
        let path = loaded.store.path(prim);
        let field = loaded.store.tokens.lookup(name).unwrap();
        let kind = match prim {
            "/Vector" => "float3",
            "/Array" => "float3[]",
            "/SparseCompatibleBase" => "float[]",
            "/Matrix" => "matrix4d",
            "/Time" | "/MisdeclaredTime" => "timecode",
            "/Asset" => "asset",
            "/Expression" | "/ExpressionBelow" => "pathExpression",
            _ => "float",
        };
        let default = stage.read_property(
            layerstack::PropertyPath::new(path, field),
            Time::Default,
            |value| convert(value, kind),
        );
        let numeric = stage
            .read_property(
                layerstack::PropertyPath::new(path, field),
                Time::at(9.0),
                |value| convert(value, kind),
            )
            .map(|resolved| resolved.value);
        // Preserve the named AOUSD divergence `default-time-block-hides-fallback`:
        // typed retries do not make a block cease suppressing weaker opinions.
        let expected_default = if matches!(prim, "/Block" | "/BelowBlock") {
            assert!(record["default"].is_null());
            json!(50000.0)
        } else {
            record["default"].clone()
        };
        assert_eq!(
            default
                .as_ref()
                .map(|r| r.value.clone())
                .unwrap_or(Json::Null),
            expected_default,
            "{text}: typed default"
        );
        if prim == "/SparseCompatibleBase" {
            // C++'s numeric typed result is covered by the separate ignored
            // regression below. Default-time selection is corrected here.
            assert_eq!(record["numeric"], json!([4.0]));
        } else {
            assert_eq!(
                numeric.unwrap_or(Json::Null),
                record["numeric"],
                "{text}: numeric"
            );
        }
        if expected_default.is_null() {
            assert!(
                default.is_none(),
                "{text}: no compatible source or fallback"
            );
        } else if matches!(prim, "/Fallback" | "/Block" | "/BelowBlock" | "/Samples") {
            assert!(
                default.unwrap().provenance.is_none(),
                "{text}: fallback source"
            );
        } else {
            let provenance = default.unwrap().provenance.unwrap();
            let expected_layer = if matches!(prim, "/Compatible" | "/SparseCompatibleBase") {
                loaded.root_layer
            } else {
                weak
            };
            assert_eq!(provenance.layer, expected_layer, "{text}: winning source");
        }
        if name == "inputs:intensity" {
            let scene = Scene::new(&stage, &loaded.store);
            let light = DistantLight::new(&scene, path).unwrap().light_api();
            assert_eq!(
                light.intensity().map(|v| json!(v)).unwrap_or(Json::Null),
                expected_default
            );
            assert_eq!(
                light
                    .intensity_at(9.0, InterpolationType::Linear)
                    .map(|v| json!(v))
                    .unwrap_or(Json::Null),
                record["numeric"]
            );
        }
    }
    // Typed selection does not change the raw value or its stronger source.
    let path = loaded.store.path("/Skip");
    let field = loaded.store.tokens.lookup("inputs:intensity").unwrap();
    let raw = stage
        .resolve_value_with_schema(path, field, &loaded.store)
        .unwrap();
    assert_eq!(raw.value, ResolvedValue::Scalar(Value::Double(2.0)));
    assert_eq!(raw.provenance.unwrap().layer, loaded.root_layer);
}

#[test]
#[ignore = "typed numeric sparse composition must finalize retained edits after an incompatible selected base"]
fn typed_numeric_sparse_composition_matches_cpp_without_weaker_retry() {
    let oracle: Json =
        serde_json::from_str(include_str!("../fixtures/typed_reads/oracle.json")).unwrap();
    let (mut loaded, stage, _) = fixture();
    let path = loaded.store.path("/SparseCompatibleBase");
    let field = loaded.store.tokens.lookup("value").unwrap();
    let actual = stage
        .read_property(
            layerstack::PropertyPath::new(path, field),
            Time::at(9.0),
            |value| convert(value, "float[]"),
        )
        .map(|resolved| resolved.value);
    // The weaker float[] base contains 1. C++ stops at the incompatible
    // stronger double[] base, then finalizes the retained append over empty:
    // [4], never [1, 4]. This is a limitation, not an intentional divergence.
    assert_eq!(
        actual.unwrap_or(Json::Null),
        oracle["/SparseCompatibleBase.value"]["numeric"]
    );
}
