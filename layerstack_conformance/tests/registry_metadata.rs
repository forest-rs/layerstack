// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Metadata fields typed by their registration, against usd-core 26.8.
//!
//! `fixtures/registry_metadata/oracle.json`
//! (`scripts/registry_metadata_oracle.py`) records what OpenUSD makes of
//! two codeless plugins' `SdfMetadata`, and of the fields Sdf and usd-core's
//! own plugins register. This replays it: the built-in fields layerstack
//! types metadata by must be usd-core's, kind of spec by kind of spec, with
//! the same types and fallbacks.
//!
//! Spec: AOUSD Core §7.4 (metadata), §7.6 (the fields of each spec type).
//! OpenUSD: `SdfSchemaBase` (`pxr/usd/sdf/schema.cpp`).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{
    FieldValue, ListOp, MetadataField, MetadataTarget, TokenInterner, Value, is_spec_field,
};
use layerstack_conformance::workspace_root;
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    builtins: BTreeMap<String, Builtin>,
}

#[derive(Deserialize)]
struct Builtin {
    #[serde(rename = "type")]
    type_name: String,
    specs: Vec<String>,
    fallback: Option<Json>,
}

fn oracle() -> Oracle {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/registry_metadata/oracle.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// The oracle's name of a kind of spec.
fn spec_name(target: MetadataTarget) -> &'static str {
    match target {
        MetadataTarget::Layer => "layer",
        MetadataTarget::Prim => "prim",
        MetadataTarget::Variant => "variant",
        MetadataTarget::Attribute => "attribute",
        MetadataTarget::Relationship => "relationship",
    }
}

const TARGETS: [MetadataTarget; 5] = [
    MetadataTarget::Layer,
    MetadataTarget::Prim,
    MetadataTarget::Variant,
    MetadataTarget::Attribute,
    MetadataTarget::Relationship,
];

/// A value as the oracle records it, by content.
pub fn encode(value: &FieldValue, tokens: &TokenInterner) -> Json {
    fn list<T>(op: &ListOp<T>, item: impl Fn(&T) -> Json) -> Json {
        let mut out = serde_json::Map::new();
        let items = |items: &[T]| Json::Array(items.iter().map(&item).collect());
        if let Some(explicit) = &op.explicit {
            out.insert("explicit".into(), items(explicit));
        }
        for (name, part) in [
            ("prepended", &op.prepend),
            ("appended", &op.append),
            ("deleted", &op.delete),
            ("added", &op.add),
            ("ordered", &op.reorder),
        ] {
            if !part.is_empty() {
                out.insert(name.into(), items(part));
            }
        }
        json!({ "list_op": out })
    }
    match value {
        FieldValue::Value(value) => encode_value(value, tokens),
        FieldValue::TokenListOp(op) => list(op, |t| json!(tokens.resolve(*t))),
        FieldValue::StringListOp(op) => list(op, |s| json!(&**s)),
        FieldValue::IntListOp(op) => list(op, |v| json!(v)),
        FieldValue::UIntListOp(op) => list(op, |v| json!(v)),
        FieldValue::Int64ListOp(op) => list(op, |v| json!(v)),
        FieldValue::UInt64ListOp(op) => list(op, |v| json!(v)),
        FieldValue::PathListOp(_) => panic!("no metadata field holds paths"),
    }
}

/// A [`Value`] as the oracle records it.
pub fn encode_value(value: &Value, tokens: &TokenInterner) -> Json {
    let half = |h: u16| f64::from(layerstack::half::to_f32(h));
    match value.materialized() {
        Value::Bool(v) => json!(v),
        Value::UChar(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::UInt(v) => json!(v),
        Value::Int64(v) => json!(v),
        Value::UInt64(v) => json!(v),
        Value::Half(v) => json!(half(*v)),
        Value::Float(v) => json!(f64::from(*v)),
        Value::Double(v) | Value::TimeCode(v) => json!(v),
        Value::String(v) => json!(&**v),
        Value::Token(v) => json!(tokens.resolve(*v)),
        Value::Asset(v) => json!({ "asset": &**v }),
        Value::PathExpression(v) => json!({ "expression": &**v }),
        Value::Vec2f(v) => json!(v.map(f64::from)),
        Value::Vec3f(v) => json!(v.map(f64::from)),
        Value::Vec4f(v) => json!(v.map(f64::from)),
        Value::Vec2d(v) => json!(v),
        Value::Vec3d(v) => json!(v),
        Value::Vec4d(v) => json!(v),
        Value::Vec2i(v) => json!(v),
        Value::Vec3i(v) => json!(v),
        Value::Vec4i(v) => json!(v),
        Value::Vec2h(v) => json!(v.map(half)),
        Value::Vec3h(v) => json!(v.map(half)),
        Value::Vec4h(v) => json!(v.map(half)),
        // Stored (i, j, k, real); OpenUSD writes the real part first.
        Value::Quatf(v) => json!([v[3], v[0], v[1], v[2]].map(f64::from)),
        Value::Quatd(v) => json!([v[3], v[0], v[1], v[2]]),
        Value::Quath(v) => json!([v[3], v[0], v[1], v[2]].map(half)),
        Value::Matrix2d(m) => json!(m.chunks(2).collect::<Vec<_>>()),
        Value::Matrix3d(m) => json!(m.chunks(3).collect::<Vec<_>>()),
        Value::Matrix4d(m) => json!(m.chunks(4).collect::<Vec<_>>()),
        Value::Dictionary(entries) => {
            let mut sorted: Vec<_> = entries.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            json!({
                "dictionary": sorted
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), encode_value(v, tokens)))
                    .collect::<serde_json::Map<_, _>>()
            })
        }
        value @ (Value::Array(_) | Value::TypedArray(_)) => Json::Array(
            value
                .array_ref()
                .expect("an array")
                .iter()
                .map(|item| encode_value(&item, tokens))
                .collect(),
        ),
        other => panic!("no oracle encoding of {other:?}"),
    }
}

#[test]
fn builtin_fields_are_the_fields_usd_core_registers() {
    let oracle = oracle();
    assert_eq!(oracle.openusd_version, "0.26.8");
    let tokens = TokenInterner::default();
    for (name, builtin) in &oracle.builtins {
        if matches!(builtin.type_name.as_str(), "payloadlistop" | "permission")
            || name == "typeName"
        {
            // Metadata with routes of their own, which no metadata
            // statement or generic edit sets.
            for spec in &builtin.specs {
                let target = TARGETS.into_iter().find(|t| spec_name(*t) == spec).unwrap();
                assert!(is_spec_field(name, target), "{name} on {spec}");
            }
            continue;
        }
        let field = MetadataField::builtin(name).unwrap_or_else(|| panic!("{name} is built in"));
        assert_eq!(
            field.value_type().storage().name(),
            builtin.type_name,
            "{name}'s type"
        );
        let specs: Vec<&str> = TARGETS
            .into_iter()
            .filter(|target| field.applies_to(*target))
            .map(spec_name)
            .collect();
        assert_eq!(specs, builtin.specs, "{name}'s spec kinds");
        let fallback = field.fallback(&tokens).map(|value| encode(&value, &tokens));
        assert_eq!(fallback, builtin.fallback, "{name}'s fallback");
    }
    for (name, _) in MetadataField::builtins() {
        assert!(
            oracle.builtins.contains_key(name),
            "usd-core registers {name}"
        );
    }
}

#[test]
fn registry_metadata_oracle_matches_configured_openusd() {
    let Ok(python) = std::env::var("LAYERSTACK_USD_PYTHON") else {
        return;
    };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/registry_metadata_oracle.py"
        ))
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
