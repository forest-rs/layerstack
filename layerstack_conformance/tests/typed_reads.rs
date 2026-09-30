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
    for (path, samples) in [
        (
            "/SparseWrongLower.value",
            vec![
                (0.0, Value::array(vec![Value::Double(2.0)])),
                (10.0, Value::array(vec![Value::Float(6.0)])),
            ],
        ),
        (
            "/SparseWrongUpper.value",
            vec![
                (0.0, Value::array(vec![Value::Float(2.0)])),
                (10.0, Value::array(vec![Value::Double(6.0)])),
            ],
        ),
    ] {
        let property = loaded.store.property_path(path);
        loaded
            .store
            .layers
            .get_mut(&weak)
            .unwrap()
            .property_mut(property)
            .unwrap()
            .time_samples = Some(samples);
    }
    for (path, default, samples) in [
        ("/MisdeclaredNumeric.value", Value::TimeCode(2.0), None),
        (
            "/MisdeclaredTimeArray.value",
            Value::array(vec![Value::TimeCode(2.0)]),
            Some(vec![
                (0.0, Value::array(vec![Value::TimeCode(2.0)])),
                (10.0, Value::array(vec![Value::TimeCode(4.0)])),
            ]),
        ),
    ] {
        let property = loaded.store.property_path(path);
        let spec = loaded
            .store
            .layers
            .get_mut(&weak)
            .unwrap()
            .property_mut(property)
            .unwrap();
        spec.default = Some(default);
        spec.time_samples = samples;
    }
    for (layer, path, name, prototype) in [
        (
            loaded.root_layer,
            "/SparseMisdeclaredEdit.value",
            "double",
            Value::Double(0.0),
        ),
        (
            weak,
            "/SparseMisdeclaredWeakEdit.value",
            "float",
            Value::Float(0.0),
        ),
    ] {
        let path = loaded.store.property_path(path);
        loaded
            .store
            .layers
            .get_mut(&layer)
            .unwrap()
            .property_mut(path)
            .unwrap()
            .type_name = Some(layerstack::PropertyType::new(name, true, prototype));
    }
    let mixed = loaded.store.property_path("/SparseMixedEdits.value");
    let wrong = loaded.store.layers[&weak]
        .property(mixed)
        .unwrap()
        .default
        .clone()
        .unwrap();
    let upper = loaded.store.property_path("/SparseWrongEditUpper.value");
    let spec = loaded
        .store
        .layers
        .get_mut(&weak)
        .unwrap()
        .property_mut(upper)
        .unwrap();
    spec.time_samples.as_mut().unwrap()[1].1 = wrong;
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
            "/Vector" | "/EmptySamplesXform" => "float3",
            "/Array" | "/SparseResizeKind" | "/SparseDeleteKind" => "float3[]",
            "/SparseCompatibleBase"
            | "/SparseSampledBase"
            | "/SparseScalarBase"
            | "/SparseWrongDense"
            | "/SparseWrongLower"
            | "/SparseWrongUpper"
            | "/SparseFillDeclaration"
            | "/SparseMixedEdits"
            | "/SparseMisdeclaredEdit"
            | "/SparseMisdeclaredWeakEdit"
            | "/SparseDeclaration"
            | "/SparseConnection"
            | "/SparseContribution"
            | "/SparseWrongEditUpper"
            | "/SparseEmptySamples"
            | "/SparseEmptyDeclaration" => "float[]",
            "/Matrix" => "matrix4d",
            "/Time" | "/MisdeclaredTime" | "/MisdeclaredNumeric" => "timecode",
            "/MisdeclaredTimeArray" | "/SparseTimeResize" | "/SparseTimeLiteral" => "timecode[]",
            "/Asset" => "asset",
            "/Expression" | "/ExpressionBelow" => "pathExpression",
            _ => "float",
        };
        let default = stage.read_property(
            layerstack::PropertyPath::new(path, field),
            Time::Default,
            |value| convert(value, kind),
        );
        let numeric = stage.read_property(
            layerstack::PropertyPath::new(path, field),
            Time::at(9.0),
            |value| convert(value, kind),
        );
        if matches!(
            prim,
            "/SparseDeclaration"
                | "/SparseConnection"
                | "/SparseContribution"
                | "/SparseEmptySamples"
                | "/SparseEmptyDeclaration"
        ) {
            let expected_layer = if matches!(prim, "/SparseContribution" | "/SparseEmptySamples") {
                loaded.root_layer
            } else {
                weak
            };
            assert_eq!(
                numeric.as_ref().unwrap().provenance.as_ref().unwrap().layer,
                expected_layer,
                "{text}: numeric contributing source"
            );
        }
        if prim.starts_with("/Sparse") {
            let property = layerstack::PropertyPath::new(path, field);
            let raw_default =
                stage
                    .resolve_property_path(property)
                    .and_then(|resolved| match resolved.value {
                        ResolvedValue::Scalar(value) => {
                            convert(&value, record["rawDefaultKind"].as_str().unwrap())
                        }
                        _ => None,
                    });
            let raw_numeric = stage
                .resolve_property_path_at_time(property, 9.0, InterpolationType::Linear)
                .and_then(|resolved| {
                    convert(&resolved.value, record["rawNumericKind"].as_str().unwrap())
                });
            let explained_default = stage
                .explain_property_value(property)
                .unwrap()
                .value
                .and_then(|value| match value {
                    ResolvedValue::Scalar(value) => {
                        convert(&value, record["rawDefaultKind"].as_str().unwrap())
                    }
                    _ => None,
                });
            let explained_numeric = stage
                .explain_property_value_at_time(property, 9.0, InterpolationType::Linear)
                .unwrap()
                .value
                .and_then(|value| convert(&value, record["rawNumericKind"].as_str().unwrap()));
            assert_eq!(
                explained_default, raw_default,
                "{text}: default explanation agrees with resolution"
            );
            assert_eq!(
                explained_numeric, raw_numeric,
                "{text}: numeric explanation agrees with resolution"
            );
            assert_eq!(
                raw_default.unwrap_or(Json::Null),
                record["rawDefault"],
                "{text}: raw default storage and value"
            );
            assert_eq!(
                raw_numeric.unwrap_or(Json::Null),
                record["rawNumeric"],
                "{text}: raw numeric storage and value"
            );
        }
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
        assert_eq!(
            numeric.map(|resolved| resolved.value).unwrap_or(Json::Null),
            record["numeric"],
            "{text}: numeric"
        );
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
            let expected_layer = if matches!(
                prim,
                "/Compatible"
                    | "/SparseCompatibleBase"
                    | "/SparseSampledBase"
                    | "/SparseScalarBase"
                    | "/SparseWrongLower"
                    | "/SparseWrongUpper"
                    | "/SparseMisdeclaredEdit"
                    | "/SparseContribution"
                    | "/SparseWrongEditUpper"
                    | "/SparseEmptySamples"
            ) {
                loaded.root_layer
            } else if matches!(prim, "/SparseMixedEdits" | "/SparseMisdeclaredWeakEdit") {
                *loaded
                    .store
                    .layers
                    .iter()
                    .find(|(_, layer)| {
                        layer
                            .property(layerstack::PropertyPath::new(path, field))
                            .is_some_and(|spec| {
                                spec.default.as_ref().is_some_and(|value| {
                                    value.array_ref().is_some_and(|array| {
                                        array.len() == 1
                                            && matches!(&*array.get(0).unwrap(), Value::Float(1.0))
                                    })
                                })
                            })
                    })
                    .unwrap()
                    .0
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
    // [4], never [1, 4].
    assert_eq!(
        actual.unwrap_or(Json::Null),
        oracle["/SparseCompatibleBase.value"]["numeric"]
    );
}

#[test]
fn typed_sparse_reads_can_omit_provenance() {
    let (mut loaded, _, _) = fixture();
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    for name in [
        "/SparseDeclaration.value",
        "/SparseConnection.value",
        "/SparseContribution.value",
    ] {
        let path = loaded.store.property_path(name);
        for time in [Time::Default, Time::at(9.0)] {
            let value = stage
                .read_property(path, time, |value| convert(value, "float[]"))
                .unwrap();
            assert!(value.provenance.is_none());
        }
    }
}

#[test]
fn tagged_sparse_numeric_reads_convert_only_the_final_array() {
    let (mut loaded, stage, _) = fixture();
    let property = loaded.store.property_path("/SparseContribution.value");
    for time in [Time::Default, Time::at(9.0)] {
        let converted = std::cell::Cell::new(0);
        let resolved = stage
            .read_property(property, time, |value| {
                let values = value.array_ref()?.typed()?.as_float()?;
                converted.set(converted.get() + values.len());
                Some(values.to_vec())
            })
            .unwrap();
        assert_eq!(resolved.value, [1.0, 2.0, 4.0]);
        let expected = if time == Time::Default { 5 } else { 3 };
        assert_eq!(
            converted.get(),
            expected,
            "numeric selection uses the actual edit kind; default selection also checks the candidate"
        );
    }
}

#[test]
fn empty_sample_maps_preserve_default_sources_when_flattened() {
    use layerstack::ValueSource;
    use layerstack::stage::flatten::FlattenRequirements;

    let (mut loaded, stage, weak) = fixture();
    for (name, expected, source) in [
        ("/SparseEmptySamples.value", vec![9.0], loaded.root_layer),
        ("/SparseEmptyDeclaration.value", vec![1.0, 2.0], weak),
    ] {
        let property = loaded.store.property_path(name);
        let explained = stage
            .explain_property_value_at_time(property, 9.0, InterpolationType::Linear)
            .unwrap();
        assert!(
            matches!(explained.source, ValueSource::Default),
            "{name}: default source"
        );
        let resolved = stage
            .resolve_property_path_at_time(property, 9.0, InterpolationType::Linear)
            .unwrap();
        assert_eq!(resolved.provenance.unwrap().layer, source);
        assert_eq!(convert(&resolved.value, "float[]"), Some(json!(expected)));
    }
    let flat = stage
        .flatten(
            &mut loaded.store,
            loaded.root_layer,
            LayerId(1000),
            &FlattenRequirements::default(),
        )
        .unwrap();
    for (name, expected) in [
        ("/SparseEmptySamples.value", vec![9.0]),
        ("/SparseEmptyDeclaration.value", vec![1.0, 2.0]),
    ] {
        let property = loaded.store.property_path(name);
        let spec = flat.layer.property(property).unwrap();
        assert!(spec.time_samples.is_none(), "{name}: no sampled source");
        assert_eq!(
            convert(spec.default.as_ref().unwrap(), "float[]"),
            Some(json!(expected))
        );
    }
}

#[test]
fn empty_sample_maps_do_not_mask_transform_animation() {
    let (mut loaded, stage, _) = fixture();
    let path = loaded.store.path("/EmptySamplesXform");
    let scene = Scene::new(&stage, &loaded.store);
    let xform = layerstack_schemas::usd_geom::Xformable::new(&scene, path).unwrap();
    assert!(xform.transform_might_be_time_varying());
    assert_eq!(xform.transform_time_samples(), vec![7.0, 11.0]);
    assert_eq!(xform.local_transform(Time::at(9.0)).matrix[3][0], 2.0);
}
