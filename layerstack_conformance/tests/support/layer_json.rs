// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A layer's authored prim specs as the JSON the skel oracles record from
//! `Sdf`, and a comparison of two such documents within a float tolerance.
//!
//! Numbers are compared within a stated tolerance, never as formatted
//! text: OpenUSD builds on other platforms round differently.
#![allow(dead_code, reason = "each test uses part of the support module")]

use layerstack::doc::{FieldValue, Layer, PrimSpec};
use layerstack::property::{PropertyKind, PropertySpec, Variability};
use layerstack::{InMemoryStore, LayerId, PathId, Value, half};
use serde_json::{Map, Value as Json, json};

/// A value as the oracles' `value()` renders it: scalars as numbers,
/// vectors, quaternions (`[i, j, k, r]`) and arrays as lists, matrices as
/// lists of rows, tokens and strings as text.
pub(crate) fn value(value: &Value, store: &InMemoryStore) -> Json {
    match value {
        Value::Bool(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::UInt(v) => json!(v),
        Value::Int64(v) => json!(v),
        Value::UChar(v) => json!(v),
        Value::Half(bits) => json!(f64::from(half::to_f32(*bits))),
        Value::Float(v) => json!(f64::from(*v)),
        Value::Double(v) | Value::TimeCode(v) | Value::Duration(v) => json!(v),
        Value::Token(token) => json!(store.tokens.resolve(*token)),
        Value::String(text) | Value::Asset(text) => json!(&**text),
        Value::Vec2f(v) => floats(v.map(f64::from)),
        Value::Vec3f(v) => floats(v.map(f64::from)),
        Value::Vec4f(v) | Value::Quatf(v) => floats(v.map(f64::from)),
        Value::Vec2d(v) => floats(*v),
        Value::Vec3d(v) => floats(*v),
        Value::Vec4d(v) | Value::Quatd(v) => floats(*v),
        Value::Vec2h(v) => floats(v.map(|b| f64::from(half::to_f32(b)))),
        Value::Vec3h(v) => floats(v.map(|b| f64::from(half::to_f32(b)))),
        Value::Vec4h(v) | Value::Quath(v) => floats(v.map(|b| f64::from(half::to_f32(b)))),
        Value::Vec2i(v) => json!(v),
        Value::Vec3i(v) => json!(v),
        Value::Vec4i(v) => json!(v),
        Value::Matrix2d(m) => rows(&m[..], 2),
        Value::Matrix3d(m) => rows(&m[..], 3),
        Value::Matrix4d(m) => rows(&m[..], 4),
        Value::Array(items) => Json::Array(items.iter().map(|v| self::value(v, store)).collect()),
        Value::TypedArray(items) => {
            Json::Array(items.values().map(|v| self::value(&v, store)).collect())
        }
        other => panic!("no JSON rendering for {other:?}"),
    }
}

fn floats<const N: usize>(values: [f64; N]) -> Json {
    Json::Array(values.into_iter().map(|v| json!(v)).collect())
}

fn rows(flat: &[f64], n: usize) -> Json {
    Json::Array(flat.chunks(n).map(|row| json!(row)).collect())
}

/// A property metadata field's value, if the spec authors it.
fn metadata(spec: &PropertySpec, key: &str, store: &InMemoryStore) -> Json {
    spec.metadata
        .iter()
        .find(|entry| store.tokens.resolve(entry.name) == key)
        .map_or(Json::Null, |entry| match &entry.value {
            FieldValue::Value(v) => value(v, store),
            other => panic!("unexpected {key} field {other:?}"),
        })
}

/// The time sample key the oracles use: Python's `repr(float(t))`.
pub(crate) fn time_key(time: f64) -> String {
    format!("{time:?}")
}

/// One attribute spec: its type, `interpolation`, `elementSize`, default
/// and time samples (the oracles' `snapshot` entry).
pub(crate) fn attribute(spec: &PropertySpec, store: &InMemoryStore) -> Json {
    let samples: Map<String, Json> = spec
        .time_samples
        .get()
        .into_iter()
        .flat_map(|samples| samples.iter())
        .map(|(time, v)| (time_key(*time), value(v, store)))
        .collect();
    json!({
        "type": spec.type_name.as_ref().map(|ty| ty.type_name().to_string()),
        "interpolation": metadata(spec, "interpolation", store),
        "elementSize": metadata(spec, "elementSize", store),
        "default": spec.default.as_ref().map_or(Json::Null, |v| value(v, store)),
        "timeSamples": samples,
    })
}

/// Every attribute spec of the prim spec at `path`, by name; empty when the
/// layer has no spec there.
pub(crate) fn prim_attributes(store: &InMemoryStore, layer: LayerId, path: PathId) -> Json {
    let layer: &Layer = store.layer(layer).expect("the layer");
    let Some(spec) = layer.prim(path) else {
        return json!({});
    };
    Json::Object(
        spec.properties
            .iter()
            .filter(|entry| entry.spec.kind == PropertyKind::Attribute)
            .map(|entry| {
                (
                    store.tokens.resolve(entry.name).to_string(),
                    attribute(&entry.spec, store),
                )
            })
            .collect(),
    )
}

/// One prim spec in full: specifier, type, `apiSchemas`, `kind` and every
/// property (attributes as [`attribute`] renders them, with their
/// variability; relationships as their explicit or appended targets).
pub(crate) fn prim(spec: &PrimSpec, store: &InMemoryStore) -> Json {
    let mut properties = Map::new();
    for entry in &spec.properties {
        let name = store.tokens.resolve(entry.name).to_string();
        let property = &entry.spec;
        let rendered = match property.kind {
            PropertyKind::Attribute => {
                let mut attribute = attribute(property, store);
                attribute["uniform"] = json!(property.variability == Variability::Uniform);
                attribute
            }
            PropertyKind::Relationship => {
                let targets: Vec<String> = property
                    .targets
                    .iter()
                    .flat_map(|op| op.apply_to(&[]))
                    .map(|target| target.display(&store.paths, &store.tokens))
                    .collect();
                json!({ "targets": targets })
            }
        };
        properties.insert(name, rendered);
    }
    let field = |key: &str| {
        spec.fields
            .iter()
            .find(|entry| store.tokens.resolve(entry.name) == key)
            .map(|entry| &entry.value)
    };
    let api_schemas: Vec<String> = match field("apiSchemas") {
        Some(FieldValue::TokenListOp(op)) => op
            .apply_to(&[])
            .into_iter()
            .map(|token| store.tokens.resolve(token).to_string())
            .collect(),
        _ => Vec::new(),
    };
    let kind = match field("kind") {
        Some(FieldValue::Value(Value::Token(token))) => json!(store.tokens.resolve(*token)),
        _ => Json::Null,
    };
    json!({
        "specifier": spec.specifier.map(|s| format!("{s:?}").to_lowercase()),
        "type": spec.type_name.map(|ty| store.tokens.resolve(ty).to_string()),
        "apiSchemas": api_schemas,
        "kind": kind,
        "properties": properties,
    })
}

/// Every prim spec of `layer` below the pseudo-root, by path.
pub(crate) fn layer(store: &InMemoryStore, layer: LayerId) -> Json {
    let layer: &Layer = store.layer(layer).expect("the layer");
    Json::Object(
        layer
            .prims()
            .iter()
            .map(|(path, spec)| (store.paths.display(*path, &store.tokens), prim(spec, store)))
            .filter(|(path, _)| path != "/")
            .collect(),
    )
}

/// Where `got` and `want` first differ, numbers compared within `tolerance`
/// (absolute, or relative to the larger magnitude above one); `None` when
/// they agree.
pub(crate) fn difference(got: &Json, want: &Json, tolerance: f64) -> Option<String> {
    match (got, want) {
        (Json::Number(a), Json::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            let scale = a.abs().max(b.abs()).max(1.0);
            ((a - b).abs() > tolerance * scale).then(|| format!("{a} != {b}"))
        }
        (Json::Array(a), Json::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("length {} != {}", a.len(), b.len()));
            }
            a.iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (a, b))| difference(a, b, tolerance).map(|d| format!("[{i}] {d}")))
        }
        (Json::Object(a), Json::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            keys.into_iter()
                .find_map(|key| match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => {
                        difference(a, b, tolerance).map(|d| format!(".{key} {d}"))
                    }
                    (Some(_), None) => Some(format!(".{key} only here")),
                    (None, _) => Some(format!(".{key} only in OpenUSD")),
                })
        }
        (a, b) => (a != b).then(|| format!("{a} != {b}")),
    }
}
