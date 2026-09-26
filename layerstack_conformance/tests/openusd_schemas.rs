// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! OpenUSD's schema registry against `layerstack_schemas`.
//!
//! `fixtures/openusd_schemas/registry.json` records, for every schema of
//! the domains `layerstack_schemas` ships, what OpenUSD 26.08's
//! `UsdSchemaRegistry` reports (`scripts/openusd_schemas_oracle.py`): its
//! kind, family and version, the typed schemas it `IsA`, and its prim
//! definitions (a typed schema's own; a typeless prim applying an applied
//! schema, once per recorded instance name for a multiple-apply one), each
//! with its applied schemas in order and every property's kind, type,
//! variability and fallback. The registry `layerstack_schemas::openusd`
//! builds must report the same.
//!
//! The differences are listed, each with its reason, in [`LEFT_OUT`],
//! [`KINDS`] and [`VERSIONED`]; anything else is a failure.
//!
//! Spec: AOUSD Core §13.3.1 (typed schemas, `IsA`), §13.3.2 (applied
//! schemas), §13.3.2.3 (the prim definition), §13.3.2.4 (fallbacks).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{
    PrimDefinition, PropertyKind, SchemaKind, TokenId, TokenInterner, Value, Variability,
};
use serde::Deserialize;
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../fixtures/openusd_schemas/registry.json");

/// Schemas OpenUSD registers without a prim definition, which
/// `layerstack_schemas` leaves out: an applied-schema registry has nothing
/// to hold for them.
const LEFT_OUT: &[(&str, &str)] = &[
    (
        "APISchemaBase",
        "AbstractBase: the base of API schemas, never applied",
    ),
    ("ClipsAPI", "NonAppliedAPI: no prim definition"),
    ("ConnectableAPI", "NonAppliedAPI: no prim definition"),
    ("ModelAPI", "NonAppliedAPI: no prim definition"),
    ("PrimvarsAPI", "NonAppliedAPI: no prim definition"),
    ("XformCommonAPI", "NonAppliedAPI: no prim definition"),
];

/// Schemas whose kind layerstack names differently: `Typed` is OpenUSD's
/// `AbstractBase` of every typed schema. Layerstack has no such kind and
/// registers it as the abstract typed root, which is what `IsA` needs
/// (AOUSD Core §13.3.1: a prim "is of" every ancestor of its type).
const KINDS: &[(&str, &str, SchemaKind)] = &[("Typed", "AbstractBase", SchemaKind::AbstractTyped)];

/// Schemas that are a later version of a family. Layerstack registers each
/// version as its own schema, as OpenUSD registers each identifier, but does
/// not model families: it neither reports them nor refuses a prim applying
/// two versions of one family, which OpenUSD does.
const VERSIONED: &[(&str, &str, u32)] = &[
    ("Capsule_1", "Capsule", 1),
    ("Cylinder_1", "Cylinder", 1),
    ("DomeLight_1", "DomeLight", 1),
];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    instances: Vec<String>,
    schemas: Vec<Schema>,
}

#[derive(Deserialize)]
struct Schema {
    name: String,
    kind: String,
    family: String,
    version: u32,
    is_a: Vec<String>,
    definitions: Vec<Definition>,
}

#[derive(Deserialize)]
struct Definition {
    #[serde(rename = "type")]
    type_name: String,
    applied: Vec<String>,
    applied_schemas: Vec<String>,
    properties: BTreeMap<String, Property>,
}

#[derive(Deserialize)]
struct Property {
    kind: String,
    type_name: Option<String>,
    variability: Option<String>,
    fallback: Option<Json>,
}

/// A value as the oracle writes it: quaternions as `[real, i, j, k]`, halves
/// as floats, matrices as their rows flattened.
fn json(value: &Value, tokens: &TokenInterner) -> Json {
    let half = |bits: &u16| json!(f64::from(layerstack::half::to_f32(*bits)));
    let floats = |values: &[f32]| json!(values.iter().map(|v| f64::from(*v)).collect::<Vec<_>>());
    match value {
        Value::Bool(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::UInt(v) => json!(v),
        Value::Int64(v) => json!(v),
        Value::UInt64(v) => json!(v),
        Value::UChar(v) => json!(v),
        Value::Half(v) => half(v),
        Value::Float(v) => json!(f64::from(*v)),
        Value::Double(v) | Value::TimeCode(v) => json!(v),
        Value::String(v) | Value::Asset(v) | Value::PathExpression(v) => json!(&**v),
        Value::Token(v) => json!(tokens.resolve(*v)),
        Value::Vec2f(v) => floats(v),
        Value::Vec3f(v) => floats(v),
        Value::Vec4f(v) => floats(v),
        Value::Vec2d(v) => json!(v),
        Value::Vec3d(v) => json!(v),
        Value::Vec4d(v) => json!(v),
        Value::Vec2h(v) => Json::Array(v.iter().map(half).collect()),
        Value::Vec3h(v) => Json::Array(v.iter().map(half).collect()),
        Value::Vec4h(v) => Json::Array(v.iter().map(half).collect()),
        Value::Vec2i(v) => json!(v),
        Value::Vec3i(v) => json!(v),
        Value::Vec4i(v) => json!(v),
        Value::Matrix2d(v) => json!(&v[..]),
        Value::Matrix3d(v) => json!(&v[..]),
        Value::Matrix4d(v) => json!(&v[..]),
        Value::Quatd([i, j, k, r]) => json!([r, i, j, k]),
        Value::Quatf([i, j, k, r]) => floats(&[*r, *i, *j, *k]),
        Value::Quath([i, j, k, r]) => Json::Array([r, i, j, k].into_iter().map(half).collect()),
        Value::Array(items) => Json::Array(items.iter().map(|v| json(v, tokens)).collect()),
        other => panic!("no JSON form for {other:?}"),
    }
}

fn kind_of(name: &str) -> Option<SchemaKind> {
    Some(match name {
        "ConcreteTyped" => SchemaKind::ConcreteTyped,
        "AbstractTyped" => SchemaKind::AbstractTyped,
        "SingleApplyAPI" => SchemaKind::SingleApplyApi,
        "MultipleApplyAPI" => SchemaKind::MultipleApplyApi,
        _ => return None,
    })
}

#[test]
fn openusd_schema_registry_matches() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("registry.json");
    assert!(
        oracle.openusd_version.starts_with("0.26.8"),
        "oracle from OpenUSD {}, layerstack_schemas from {}",
        oracle.openusd_version,
        layerstack_schemas::OPENUSD_VERSION
    );
    assert_eq!(
        layerstack_schemas::OPENUSD_VERSION,
        "26.8",
        "the generated release"
    );
    assert_eq!(oracle.instances.len(), 2, "a plain and a nested instance");

    let mut tokens = TokenInterner::default();
    let registry = layerstack_schemas::openusd(&mut tokens);
    assert!(registry.issues().is_empty(), "{:?}", registry.issues());

    let mut failures: Vec<String> = Vec::new();
    let mut check = |what: String, ours: Json, theirs: Json| {
        if ours != theirs {
            failures.push(format!("{what}: layerstack {ours}, OpenUSD {theirs}"));
        }
    };

    let typed: Vec<&str> = oracle
        .schemas
        .iter()
        .filter(|s| !s.is_a.is_empty())
        .map(|s| s.name.as_str())
        .collect();
    let mut registered = 0;
    for schema in &oracle.schemas {
        let name = schema.name.as_str();
        let token = tokens.intern(name);
        let versioned = VERSIONED
            .iter()
            .find(|(n, ..)| *n == name)
            .map(|(_, family, version)| (*family, *version));
        check(
            format!("{name} family and version"),
            json!(versioned.unwrap_or((name, 0))),
            json!((&schema.family, schema.version)),
        );
        if LEFT_OUT.iter().any(|(n, _)| *n == name) {
            check(
                format!("{name} left out"),
                json!(registry.schema(token).is_none()),
                json!(true),
            );
            continue;
        }
        let expected_kind = KINDS
            .iter()
            .find(|(n, kind, _)| *n == name && *kind == schema.kind)
            .map(|(.., ours)| *ours)
            .or_else(|| kind_of(&schema.kind));
        let Some(definition) = registry.schema(token) else {
            check(format!("{name} registered"), json!(false), json!(true));
            continue;
        };
        registered += 1;
        check(
            format!("{name} kind"),
            json!(format!("{:?}", definition.kind)),
            json!(
                format!("{expected_kind:?}")
                    .trim_start_matches("Some(")
                    .trim_end_matches(')')
            ),
        );
        for &ancestor in &typed {
            let ours = registry.is_a(token, tokens.intern(ancestor));
            let theirs = schema.is_a.iter().any(|a| a == ancestor);
            check(format!("{name} IsA {ancestor}"), json!(ours), json!(theirs));
        }

        for expected in &schema.definitions {
            let applied: Vec<TokenId> = expected.applied.iter().map(|a| tokens.intern(a)).collect();
            let label = format!("{name} {:?}", expected.applied);
            let ours: PrimDefinition = if definition.kind == SchemaKind::AbstractTyped {
                registry
                    .schema_definition(token)
                    .expect("abstract definition")
                    .clone()
            } else {
                registry.intern_instance_names(&applied, &mut tokens);
                let type_name =
                    (!expected.type_name.is_empty()).then(|| tokens.intern(&expected.type_name));
                registry.prim_definition(type_name, &applied, &tokens)
            };
            let applied_schemas: Vec<&str> = ours
                .applied_schemas()
                .iter()
                .map(|a| tokens.resolve(a.name))
                .collect();
            check(
                format!("{label} applied schemas"),
                json!(applied_schemas),
                json!(expected.applied_schemas),
            );
            let mut names: Vec<&str> = ours
                .properties()
                .iter()
                .map(|p| tokens.resolve(p.name))
                .collect();
            names.sort_unstable();
            let expected_names: Vec<&str> =
                expected.properties.keys().map(String::as_str).collect();
            check(
                format!("{label} property names"),
                json!(names),
                json!(expected_names),
            );
            for (property_name, expected) in &expected.properties {
                let Some(property) = tokens.lookup(property_name).and_then(|t| ours.property(t))
                else {
                    continue;
                };
                let what = |field: &str| format!("{label} {property_name} {field}");
                let kind = match property.kind {
                    PropertyKind::Attribute => "attribute",
                    PropertyKind::Relationship => "relationship",
                };
                check(what("kind"), json!(kind), json!(expected.kind));
                if property.kind == PropertyKind::Relationship {
                    continue;
                }
                let type_name = property.type_name.as_ref().map(|t| {
                    if t.is_array && !t.type_name.ends_with("[]") {
                        format!("{}[]", t.type_name)
                    } else {
                        t.type_name.to_string()
                    }
                });
                check(what("type"), json!(type_name), json!(expected.type_name));
                let variability = match property.variability {
                    Variability::Varying => "varying",
                    Variability::Uniform => "uniform",
                };
                check(
                    what("variability"),
                    json!(variability),
                    json!(expected.variability),
                );
                check(
                    what("fallback"),
                    json!(property.fallback.as_ref().map(|v| json(v, &tokens))),
                    json!(expected.fallback),
                );
            }
        }
    }
    assert_eq!(
        registered + LEFT_OUT.len(),
        oracle.schemas.len(),
        "every schema is registered or left out"
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
