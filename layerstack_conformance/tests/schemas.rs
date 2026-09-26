// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for prim definitions.
//!
//! Each directory under `fixtures/schemas` is a codeless OpenUSD schema
//! plugin (`plugInfo.json`, `generatedSchema.usda`), a `scene.usda` that
//! uses it, and `oracle.json`: what OpenUSD 26.08 reports for every prim of
//! the scene (`scripts/schemas_oracle.py`, which also writes the other
//! files). Layerstack builds its registry from the same plugin, reading
//! `generatedSchema.usda` with `layerstack::schema::read_generated_schema`
//! and the kinds, bases and auto-applies from `plugInfo.json`, and must
//! report the same prim definitions: each prim's type (or none), `IsA`,
//! applied schemas, `HasAPI` by schema and by instance, property names,
//! and for each property whether a schema defines it, its kind, type,
//! variability, fallback and resolved default value.
//!
//! The sets cover the AOUSD Core §13.3 examples (`typed_and_applied`,
//! `inclusions`, `fallback_order`) and a set exercising the rest of §13.3
//! (`coverage`): abstract bases, unknown and abstract type names, nested
//! built-ins, multiple-apply templates and instance names containing `:`,
//! built-ins by type and by named instance, override properties, auto-applies
//! to an abstract base and to an API schema, an inclusion cycle, invalid
//! `apiSchemas` entries, and fallbacks shadowed by authored values and
//! blocks.
//!
//! The `openusd` set has no plugin: its scene (a mesh, a sphere, a material
//! with a shader, a light, and a prim applying `CollectionAPI:foo` and
//! `MaterialBindingAPI`) uses OpenUSD's own schemas, which layerstack takes
//! from `layerstack_schemas`.
//!
//! One result differs on purpose: at the default time a default block
//! resolves the fallback (AOUSD Core §12.3.6, §16.2.16.2), where OpenUSD
//! resolves no value, the divergence `default-time-block-hides-fallback`
//! (`docs/generic-sparse-composition.md`).
//!
//! Spec: AOUSD Core §13.3.1–§13.3.2.4.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use layerstack::schema::{SchemaDeclaration, read_generated_schema};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, PropertyKind,
    ResolvedAsset, ResolvedValue, SchemaIssue, SchemaKind, SchemaRegistry, Stage, StageOptions,
    TokenId, TokenInterner, Value, Variability,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde::Deserialize;
use serde_json::{Value as Json, json};

const SETS: [&str; 5] = [
    "typed_and_applied",
    "inclusions",
    "fallback_order",
    "coverage",
    "openusd",
];

/// The properties whose default-time value layerstack resolves to the
/// fallback where OpenUSD resolves none, as `(set, prim, property)`: a
/// default block over a schema fallback.
const DEFAULT_TIME_BLOCKS: [(&str, &str, &str); 2] = [
    ("coverage", "/Panel", "depth"),
    ("coverage", "/Composed", "depth"),
];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    set: String,
    prims: Vec<Prim>,
}

#[derive(Deserialize)]
struct Prim {
    path: String,
    type_name: String,
    schema_type: String,
    is_a: BTreeMap<String, bool>,
    applied_schemas: Vec<String>,
    has_api: BTreeMap<String, bool>,
    has_api_instance: Vec<(String, String, bool)>,
    property_names: Vec<String>,
    properties: BTreeMap<String, Property>,
}

#[derive(Deserialize)]
struct Property {
    defined: bool,
    kind: String,
    type_name: Option<String>,
    variability: Option<String>,
    value: Option<Json>,
    fallback: Option<Json>,
}

#[derive(Deserialize)]
struct PlugInfo {
    #[serde(rename = "Plugins")]
    plugins: Vec<Plugin>,
}

#[derive(Deserialize)]
struct Plugin {
    #[serde(rename = "Info")]
    info: Info,
}

#[derive(Deserialize)]
struct Info {
    #[serde(rename = "Types")]
    types: BTreeMap<String, TypeInfo>,
    #[serde(rename = "AutoApplyAPISchemas", default)]
    auto_apply: BTreeMap<String, AutoApply>,
}

#[derive(Deserialize)]
struct TypeInfo {
    bases: Vec<String>,
    #[serde(rename = "schemaIdentifier")]
    identifier: String,
    #[serde(rename = "schemaKind")]
    kind: String,
    #[serde(rename = "apiSchemaAutoApplyTo", default)]
    auto_apply_to: Vec<String>,
}

#[derive(Deserialize)]
struct AutoApply {
    #[serde(rename = "apiSchemaAutoApplyTo")]
    to: Vec<String>,
}

/// Rejects every asset path: generated schema layers have none.
struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

fn set_dir(set: &str) -> PathBuf {
    workspace_root()
        .join("layerstack_conformance/fixtures/schemas")
        .join(set)
}

fn read(set: &str, file: &str) -> String {
    std::fs::read_to_string(set_dir(set).join(file)).expect("fixture file")
}

/// The registry OpenUSD builds from the set's plugin: its
/// `generatedSchema.usda`, with the kinds, bases and auto-applies its
/// `plugInfo.json` declares.
fn registry(set: &str, store: &mut InMemoryStore) -> SchemaRegistry {
    if set == "openusd" {
        return layerstack_schemas::openusd(&mut store.tokens);
    }
    let plug_info: PlugInfo = serde_json::from_str(&read(set, "plugInfo.json")).expect("plugInfo");
    let info = &plug_info.plugins[0].info;
    let mut declared = Vec::new();
    for info_type in info.types.values() {
        let kind = match info_type.kind.as_str() {
            "concreteTyped" => SchemaKind::ConcreteTyped,
            "abstractTyped" => SchemaKind::AbstractTyped,
            "singleApplyAPI" => SchemaKind::SingleApplyApi,
            "multipleApplyAPI" => SchemaKind::MultipleApplyApi,
            other => panic!("schema kind {other}"),
        };
        let parent = info_type
            .bases
            .iter()
            .find_map(|base| info.types.get(base))
            .map(|base| store.tokens.intern(&base.identifier));
        declared.push(SchemaDeclaration {
            name: store.tokens.intern(&info_type.identifier),
            kind,
            parent,
        });
    }

    let source = read(set, "generatedSchema.usda");
    let parsed = layerstack_usda::parser::parse(&source);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(u64::MAX),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
    let definitions =
        read_generated_schema(&emitted.layer, &declared, &mut store.tokens, &store.paths)
            .expect("every declared schema");

    let mut builder = SchemaRegistry::builder();
    for definition in definitions {
        builder.register(definition);
    }
    let mut token = |name: &str| store.tokens.intern(name);
    for info_type in info.types.values() {
        for target in &info_type.auto_apply_to {
            builder.auto_apply(token(&info_type.identifier), token(target));
        }
    }
    for (schema, auto_apply) in &info.auto_apply {
        for target in &auto_apply.to {
            builder.auto_apply(token(schema), token(target));
        }
    }
    builder.build(&mut store.tokens)
}

/// A resolved value as the oracle writes it.
fn json(value: &Value, tokens: &TokenInterner) -> Json {
    match value {
        Value::Bool(b) => json!(b),
        Value::Int(i) => json!(i),
        Value::Half(bits) => json!(f64::from(layerstack::half::to_f32(*bits))),
        Value::Float(f) => json!(f64::from(*f)),
        Value::Double(d) => json!(d),
        Value::String(s) => json!(&**s),
        Value::Token(t) => json!(tokens.resolve(*t)),
        Value::Vec2f(v) => json!(v.map(f64::from)),
        Value::Vec3f(v) => json!(v.map(f64::from)),
        Value::Vec3d(v) => json!(v),
        Value::Asset(s) | Value::PathExpression(s) => json!(&**s),
        Value::TimeCode(t) => json!(t),
        Value::Quatf([i, j, k, r]) => json!([r, i, j, k].map(|v| f64::from(*v))),
        Value::Matrix4d(m) => json!(&m[..]),
        Value::Array(items) => Json::Array(items.iter().map(|v| json(v, tokens)).collect()),
        other => panic!("no JSON form for {other:?}"),
    }
}

fn variability(variability: Variability) -> &'static str {
    match variability {
        Variability::Varying => "varying",
        Variability::Uniform => "uniform",
    }
}

fn kind(kind: PropertyKind) -> &'static str {
    match kind {
        PropertyKind::Attribute => "attribute",
        PropertyKind::Relationship => "relationship",
    }
}

/// A declared type name as OpenUSD spells it, `[]` included for arrays.
fn type_name(declared: Option<&layerstack::PropertyType>) -> Option<String> {
    let declared = declared?;
    let name = declared.type_name.to_string();
    Some(if declared.is_array && !name.ends_with("[]") {
        format!("{name}[]")
    } else {
        name
    })
}

/// Differences between layerstack and the oracle for one set.
fn check_set(set: &str, failures: &mut Vec<String>) {
    let oracle: Oracle = serde_json::from_str(&read(set, "oracle.json")).expect("oracle.json");
    assert!(
        oracle.openusd_version.starts_with("0.26."),
        "oracle from OpenUSD {}",
        oracle.openusd_version
    );
    assert_eq!(oracle.set, set, "the oracle of another set");

    let mut loaded = load_entry_usda(&set_dir(set).join("scene.usda"));
    let store = &mut loaded.store;
    let registry = Arc::new(registry(set, store));
    let options = StageOptions {
        schemas: Some(registry.clone()),
        ..StageOptions::default()
    };
    let stage = Stage::compose(store, loaded.root_layer, options);

    let mut check = |what: String, ours: Json, theirs: Json| {
        if ours != theirs {
            failures.push(format!("{set} {what}: layerstack {ours}, OpenUSD {theirs}"));
        }
    };
    let names = |tokens: &TokenInterner, ids: &[TokenId]| -> Vec<String> {
        ids.iter()
            .map(|id| tokens.resolve(*id).to_string())
            .collect()
    };

    let pseudo_root = store.path("/");
    let mut composed: Vec<String> = stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .map(|prim| store.paths.display(prim, &store.tokens))
        .collect();
    composed.sort();
    let mut expected: Vec<String> = oracle.prims.iter().map(|p| p.path.clone()).collect();
    expected.sort();
    check("prims".into(), json!(composed), json!(expected));

    for expected in &oracle.prims {
        let path = &expected.path;
        let prim = store.path(path);
        let definition = stage
            .prim_definition(prim, store)
            .expect("prim on the stage");
        let tokens = &store.tokens;
        let resolved_type = stage.resolve_type_name(prim, store);
        check(
            format!("{path} type name"),
            json!(resolved_type.map_or("", |t| tokens.resolve(t))),
            json!(expected.type_name),
        );
        check(
            format!("{path} schema type"),
            json!(definition.type_name().map_or("", |t| tokens.resolve(t))),
            json!(expected.schema_type),
        );
        for (schema, is_a) in &expected.is_a {
            let ours = tokens.lookup(schema).is_some_and(|s| definition.is_a(s));
            check(format!("{path} IsA {schema}"), json!(ours), json!(is_a));
        }
        let applied: Vec<TokenId> = definition
            .applied_schemas()
            .iter()
            .map(|a| a.name)
            .collect();
        check(
            format!("{path} applied schemas"),
            json!(names(tokens, &applied)),
            json!(expected.applied_schemas),
        );
        for (schema, has) in &expected.has_api {
            let ours = tokens.lookup(schema).is_some_and(|s| definition.has_api(s));
            check(format!("{path} HasAPI {schema}"), json!(ours), json!(has));
        }
        for (schema, instance, has) in &expected.has_api_instance {
            let ours = tokens
                .lookup(schema)
                .zip(tokens.lookup(instance))
                .is_some_and(|(s, i)| definition.has_api_instance(s, i));
            check(
                format!("{path} HasAPI {schema}:{instance}"),
                json!(ours),
                json!(has),
            );
        }

        let property_names = stage.property_names(prim, store);
        let tokens = &store.tokens;
        check(
            format!("{path} property names"),
            json!(names(tokens, &property_names)),
            json!(expected.property_names),
        );

        for (name, expected) in &expected.properties {
            let what = |field: &str| format!("{path}.{name} {field}");
            let Some(property) = tokens.lookup(name) else {
                check(what("interned"), json!(false), json!(true));
                continue;
            };
            let defined = stage.property_definition(prim, property, store);
            check(
                what("defined by the prim definition"),
                json!(defined.as_ref() == definition.property(property)),
                json!(true),
            );
            check(
                what("defined"),
                json!(defined.is_some()),
                json!(expected.defined),
            );
            let declaration = stage.resolve_property_declaration(prim, property);
            let (property_kind, declared_type, declared_variability) =
                match (&defined, &declaration) {
                    (Some(d), _) => (d.kind, type_name(d.type_name.as_ref()), d.variability),
                    (None, Some(d)) => (d.kind, type_name(d.type_name.as_ref()), d.variability),
                    (None, None) => {
                        check(what("defined or authored"), json!(false), json!(true));
                        continue;
                    }
                };
            check(
                what("kind"),
                json!(kind(property_kind)),
                json!(expected.kind),
            );
            if property_kind == PropertyKind::Relationship {
                continue;
            }
            check(
                what("type"),
                json!(declared_type),
                json!(expected.type_name),
            );
            check(
                what("variability"),
                json!(variability(declared_variability)),
                json!(expected.variability),
            );
            let fallback = defined
                .as_ref()
                .and_then(|d| d.fallback.as_ref())
                .map(|v| json(v, tokens));
            check(what("fallback"), json!(fallback), json!(expected.fallback));

            let value = stage
                .resolve_value_with_schema(prim, property, store)
                .map(|resolved| match resolved.value {
                    ResolvedValue::Scalar(value) => json(&value, tokens),
                    other => panic!("{path}.{name} resolves to {other:?}"),
                });
            let theirs = if DEFAULT_TIME_BLOCKS.contains(&(set, path.as_str(), name.as_str())) {
                // `default-time-block-hides-fallback`: OpenUSD resolves no
                // value; the fallback resolves here.
                check(what("OpenUSD value"), json!(expected.value), json!(null));
                expected.fallback.clone()
            } else {
                expected.value.clone()
            };
            check(what("value"), json!(value), json!(theirs));
        }
    }
}

#[test]
fn prim_definitions_match_openusd() {
    let mut failures = Vec::new();
    for set in SETS {
        check_set(set, &mut failures);
    }
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// OpenUSD warns about the same skipped inclusions and overrides the
/// registry reports.
#[test]
fn registry_reports_what_it_skips() {
    let mut store = InMemoryStore::default();
    let registry = registry("coverage", &mut store);
    let token = |name: &str| store.tokens.lookup(name).expect("interned");
    let issues = registry.issues();
    for expected in [
        SchemaIssue::IgnoredOverride {
            schema: token("OverrideAPI"),
            property: token("ghost"),
        },
        SchemaIssue::IgnoredOverride {
            schema: token("Tile"),
            property: token("style:weight"),
        },
        SchemaIssue::IgnoredOverride {
            schema: token("Panel"),
            property: token("style:weight"),
        },
        SchemaIssue::InclusionCycle {
            schema: token("CycleTwoAPI"),
            included: token("CycleOneAPI"),
        },
        SchemaIssue::InclusionCycle {
            schema: token("CycleOneAPI"),
            included: token("CycleTwoAPI"),
        },
    ] {
        assert!(issues.contains(&expected), "{expected:?} in {issues:?}");
    }
    assert_eq!(issues.len(), 5, "{issues:?}");
}
