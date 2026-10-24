// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `layerstack_schemagen::compile` against OpenUSD's usdGenSchema.
//!
//! - `fixtures/schema_compiler/lab` and `observatory` are downstream schema
//!   libraries: each `schema.usda` with the `generatedSchema.usda` and
//!   `plugInfo.json` the compiler writes, which usdGenSchema also writes
//!   for them. `lab` builds on `fixtures/schema_compiler/usd`, the two base
//!   classes of OpenUSD's `usd/schema.usda`, so it compiles without
//!   OpenUSD; `observatory` builds on OpenUSD's `Xformable`.
//! - `fixtures/schema_compiler/oracle.json` records what usd-core 26.8's
//!   schema registry reads from those files
//!   (`scripts/schema_compiler_oracle.py`); layerstack's registry must read
//!   the same.
//! - With `LAYERSTACK_OPENUSD_SOURCE` set to an OpenUSD 26.08 checkout,
//!   every schema domain's `schema.usda` must compile to the
//!   `generatedSchema.usda` OpenUSD checks in, byte for byte, and to its
//!   `plugInfo.json`'s types; `testUsdGenSchema`'s schemas must compile to
//!   its baselines and its failing schemas fail with usdGenSchema's error.
//!
//! Spec: AOUSD Core §13.3 (schemas and prim definitions).

#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use layerstack::schema::{SchemaDeclaration, read_generated_schema};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, PrimDefinition,
    PropertyKind, ResolvedAsset, SchemaKind, SchemaRegistry, TokenId, TokenInterner, Value,
    Variability,
};
use layerstack_schemagen::compile::{CompileError, CompileOptions, Rule, compile};
use serde::Deserialize;
use serde_json::{Value as Json, json};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/schema_compiler")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The OpenUSD checkout, when `LAYERSTACK_OPENUSD_SOURCE` names one.
fn openusd_source() -> Option<PathBuf> {
    let source = std::env::var_os("LAYERSTACK_OPENUSD_SOURCE").map(PathBuf::from);
    if source.is_none() {
        eprintln!("skipped: set LAYERSTACK_OPENUSD_SOURCE to an OpenUSD 26.08 checkout");
    }
    source
}

/// `expected` and `actual` are the same text, or the first line they
/// differ at.
fn assert_same_text(what: &str, actual: &str, expected: &str) {
    if actual == expected {
        return;
    }
    let line = actual
        .lines()
        .zip(expected.lines())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| actual.lines().count().min(expected.lines().count()));
    panic!(
        "{what} differs at line {}:\n  ours:    {:?}\n  OpenUSD: {:?}",
        line + 1,
        actual.lines().nth(line),
        expected.lines().nth(line)
    );
}

/// The `Types` of a `plugInfo.json`, its comment lines dropped.
fn plug_info_types(text: &str) -> BTreeMap<String, Json> {
    let json: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let json: Json = serde_json::from_str(&json).expect("plugInfo.json is JSON");
    let mut types = BTreeMap::new();
    for plugin in json["Plugins"].as_array().expect("Plugins") {
        if let Some(entries) = plugin["Info"]["Types"].as_object() {
            types.extend(entries.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }
    types
}

#[test]
fn fixtures_compile_to_their_committed_files() {
    let root = fixture();
    let lab = root.join("lab");
    let options = CompileOptions::new()
        .with_search_path(&root)
        .with_existing_plug_info(read(&lab.join("plugInfo.json")));
    let compiled = compile(&lab.join("schema.usda"), &options).expect("the lab schema compiles");
    assert_same_text(
        "lab/generatedSchema.usda",
        compiled.generated_schema_usda(),
        &read(&lab.join("generatedSchema.usda")),
    );
    assert_same_text(
        "lab/plugInfo.json",
        compiled.plug_info_json().expect("a plugInfo.json"),
        &read(&lab.join("plugInfo.json")),
    );
    assert!(
        compiled.library().skip_code_generation,
        "a codeless library"
    );
    let kinds: Vec<(&str, &str, Option<&str>)> = compiled
        .classes()
        .iter()
        .map(|class| {
            (
                class.name.as_str(),
                class.kind.as_str(),
                class.base.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("Instrument", "abstractTyped", Some("Typed")),
            ("Spectrometer", "concreteTyped", Some("Instrument")),
            ("ImagingSpectrometer", "concreteTyped", Some("Spectrometer")),
            ("Centrifuge", "concreteTyped", Some("Instrument")),
            ("CalibrationAPI", "singleApplyAPI", Some("APISchemaBase")),
            ("ChannelAPI", "multipleApplyAPI", Some("APISchemaBase")),
            ("SampleQueryAPI", "nonAppliedAPI", Some("APISchemaBase")),
        ],
        "the classes, in file order"
    );
    let channel = &compiled.classes()[5];
    assert_eq!(
        channel.property_namespace_prefix.as_deref(),
        Some("channel")
    );
    assert_eq!(channel.allowed_instance_names, ["red", "green", "blue"]);
    assert_eq!(
        channel.instance_can_only_apply_to,
        [("red".to_owned(), vec!["ImagingSpectrometer".to_owned()])]
    );
    let calibration = &compiled.classes()[4];
    assert_eq!(calibration.auto_apply_to, ["Centrifuge"]);
    assert_eq!(calibration.can_only_apply_to, ["Instrument"]);
    assert_eq!(compiled.classes()[1].fallback_prim_types, ["Spectrograph"]);
    assert_eq!(compiled.classes()[1].built_ins, ["CalibrationAPI"]);

    // Each class's kind, family and version are those usd-core reads.
    let oracle: Oracle =
        serde_json::from_str(&read(&root.join("oracle.json"))).expect("oracle.json");
    for class in compiled.classes() {
        let recorded = oracle
            .schemas
            .iter()
            .find(|schema| schema.name == class.name)
            .unwrap_or_else(|| panic!("{} is recorded", class.name));
        let kind = class.kind.as_str();
        let kind = kind[..1].to_uppercase() + &kind[1..];
        assert_eq!(
            (kind.as_str(), class.family.as_str(), class.version),
            (
                recorded.kind.as_str(),
                recorded.family.as_str(),
                recorded.version
            ),
            "{}",
            class.name
        );
    }
}

/// Without the plugin's own `plugInfo.json`, OpenUSD's template, with the
/// placeholders its build fills in.
#[test]
fn plug_info_starts_from_openusd_template() {
    let root = fixture();
    let compiled = compile(
        &root.join("lab/schema.usda"),
        &CompileOptions::new().with_search_path(&root),
    )
    .expect("the lab schema compiles");
    let plug_info = compiled.plug_info_json().expect("a plugInfo.json");
    for placeholder in [
        "\"LibraryPath\": \"@PLUG_INFO_LIBRARY_PATH@\"",
        "\"ResourcePath\": \"@PLUG_INFO_RESOURCE_PATH@\"",
        "\"Root\": \"@PLUG_INFO_ROOT@\"",
        "\"Type\": \"resource\"",
    ] {
        assert!(plug_info.contains(placeholder), "{placeholder}");
    }
    assert_eq!(
        plug_info_types(plug_info),
        plug_info_types(&read(&root.join("lab/plugInfo.json"))),
        "the same types"
    );
}

#[test]
fn unresolved_sublayers_are_named() {
    let root = fixture();
    let error = compile(
        &root.join("observatory/schema.usda"),
        &CompileOptions::new(),
    )
    .expect_err("usdGeom is not on the search path");
    assert!(
        matches!(
            &error,
            CompileError::UnresolvedSublayer { asset, layer }
                if asset == "usdGeom/schema.usda"
                    && Path::new(layer).ends_with(Path::new("observatory").join("schema.usda"))
        ),
        "{error:?}"
    );
}

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    instances: Vec<String>,
    schemas: Vec<OracleSchema>,
}

#[derive(Deserialize)]
struct OracleSchema {
    name: String,
    plugin: String,
    kind: String,
    family: String,
    version: u32,
    is_a: Vec<String>,
    definitions: Vec<OracleDefinition>,
    #[serde(default)]
    can_only_apply_to: Vec<(String, Vec<String>)>,
    #[serde(default)]
    allowed_instance_names: Vec<(String, bool)>,
    #[serde(default)]
    auto_apply_to: Vec<String>,
}

#[derive(Deserialize)]
struct OracleDefinition {
    #[serde(rename = "type")]
    type_name: String,
    applied: Vec<String>,
    applied_schemas: Vec<String>,
    properties: BTreeMap<String, OracleProperty>,
}

#[derive(Deserialize)]
struct OracleProperty {
    kind: String,
    type_name: Option<String>,
    variability: Option<String>,
    fallback: Option<Json>,
    #[serde(default)]
    allowed_tokens: Vec<String>,
}

struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// The base schemas the fixture plugins inherit, by C++ type name.
const UPSTREAM: &[(&str, &str)] = &[("UsdTyped", "Typed"), ("UsdGeomXformable", "Xformable")];

/// Registers a compiled plugin's schemas as a downstream reader does: its
/// `plugInfo.json` declares their kinds, bases and application rules, and
/// `read_generated_schema` reads their definitions.
fn register_plugin(
    directory: &Path,
    builder: &mut layerstack::SchemaRegistryBuilder,
    store: &mut InMemoryStore,
) {
    let types = plug_info_types(&read(&directory.join("plugInfo.json")));
    let identifier = |cpp: &str| {
        types
            .get(cpp)
            .and_then(|entry| entry["schemaIdentifier"].as_str())
            .or_else(|| UPSTREAM.iter().find(|(c, _)| *c == cpp).map(|(_, n)| *n))
            .map(str::to_owned)
    };
    let strings = |value: &Json| -> Vec<String> {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut declarations = Vec::new();
    let mut auto_applies = Vec::new();
    for entry in types.values() {
        let kind = match entry["schemaKind"].as_str() {
            Some("concreteTyped") => SchemaKind::ConcreteTyped,
            Some("abstractTyped") => SchemaKind::AbstractTyped,
            Some("singleApplyAPI") => SchemaKind::SingleApplyApi,
            Some("multipleApplyAPI") => SchemaKind::MultipleApplyApi,
            // No prim definition.
            _ => continue,
        };
        let name = entry["schemaIdentifier"].as_str().expect("an identifier");
        let mut declaration = SchemaDeclaration::new(store.tokens.intern(name), kind);
        if kind.is_typed() {
            declaration.parent = entry["bases"][0]
                .as_str()
                .and_then(identifier)
                .map(|base| store.tokens.intern(base));
        }
        let intern = |names: Vec<String>| -> Vec<TokenId> {
            names.iter().map(|n| store.tokens.intern(n)).collect()
        };
        declaration.can_only_apply_to = intern(strings(&entry["apiSchemaCanOnlyApplyTo"]));
        declaration.allowed_instance_names =
            intern(strings(&entry["apiSchemaAllowedInstanceNames"]));
        if let Some(instances) = entry["apiSchemaInstances"].as_object() {
            for (instance, info) in instances {
                declaration.instance_can_only_apply_to.push((
                    store.tokens.intern(instance),
                    intern(strings(&info["apiSchemaCanOnlyApplyTo"])),
                ));
            }
        }
        for target in strings(&entry["apiSchemaAutoApplyTo"]) {
            auto_applies.push((name.to_owned(), target));
        }
        declarations.push(declaration);
    }
    let text = read(&directory.join("generatedSchema.usda"));
    let layer = layerstack_usda::read_usda(
        &text,
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    )
    .emitted
    .layer;
    let definitions = read_generated_schema(&layer, &declarations, &store.tokens, &store.paths)
        .expect("every declared schema is defined");
    for definition in definitions {
        builder.register(definition);
    }
    for (api, target) in auto_applies {
        builder.auto_apply(store.tokens.intern(api), store.tokens.intern(target));
    }
}

/// A value as the oracle writes it.
fn value_json(value: &Value, tokens: &TokenInterner) -> Json {
    match value {
        Value::Bool(v) => json!(v),
        Value::Int(v) => json!(v),
        Value::Float(v) => json!(f64::from(*v)),
        Value::Double(v) => json!(v),
        Value::String(v) | Value::Asset(v) => json!(&**v),
        Value::Token(v) => json!(tokens.resolve(*v)),
        Value::Vec2i(v) => json!(v),
        Value::Array(_) | Value::TypedArray(_) => Json::Array(
            value
                .array_ref()
                .expect("an array")
                .iter()
                .map(|v| value_json(&v, tokens))
                .collect(),
        ),
        other => panic!("no JSON form for {other:?}"),
    }
}

#[test]
fn fixture_registry_matches_openusd() {
    let mut store = InMemoryStore::default();
    let mut builder = SchemaRegistry::builder();
    layerstack_schemas::register(
        &mut builder,
        &[
            layerstack_schemas::Domain::Usd,
            layerstack_schemas::Domain::UsdGeom,
        ],
        &store.tokens,
    )
    .expect("OpenUSD's schemas");
    for plugin in ["lab", "observatory"] {
        register_plugin(&fixture().join(plugin), &mut builder, &mut store);
    }
    let registry = builder.build(&store.tokens);
    assert_matches_oracle(&registry, &mut store.tokens);
}

/// The Rust modules `compile --rust` generates for the fixtures
/// (`layerstack_custom_schemas`) register from their static tables what
/// OpenUSD's registry reads from the compiled files.
#[test]
fn generated_registration_matches_openusd() {
    use layerstack_custom_schemas::{lab, observatory};
    let mut tokens = TokenInterner::default();
    let mut builder = SchemaRegistry::builder();
    let domains: Vec<layerstack_schemas::Domain> = lab::DEPENDENCIES
        .iter()
        .chain(observatory::DEPENDENCIES)
        .copied()
        .collect();
    layerstack_schemas::register(&mut builder, &domains, &tokens).expect("OpenUSD's schemas");
    lab::register(&mut builder, &tokens).expect("the lab schemas");
    observatory::register(&mut builder, &tokens).expect("the observatory schemas");
    let registry = builder.build(&tokens);
    assert_matches_oracle(&registry, &mut tokens);
}

/// `registry`, with OpenUSD's `Usd` and `UsdGeom` and the lab and observatory
/// fixture schemas, is what usd-core's registry reads from the fixtures
/// (`oracle.json`): every schema's kind, `IsA`, application rules,
/// auto-applies and prim definitions, with each property's kind, type,
/// variability, fallback and `allowedTokens`.
fn assert_matches_oracle(registry: &SchemaRegistry, tokens: &mut TokenInterner) {
    let oracle: Oracle =
        serde_json::from_str(&read(&fixture().join("oracle.json"))).expect("oracle.json");
    assert!(
        oracle.openusd_version.starts_with("0.26.8"),
        "{}",
        oracle.openusd_version
    );
    assert!(registry.issues().is_empty(), "{:?}", registry.issues());
    let declared_types: BTreeMap<String, Json> = ["lab", "observatory"]
        .iter()
        .flat_map(|plugin| plug_info_types(&read(&fixture().join(plugin).join("plugInfo.json"))))
        .map(|(_, entry)| {
            (
                entry["schemaIdentifier"]
                    .as_str()
                    .expect("an identifier")
                    .to_owned(),
                entry,
            )
        })
        .collect();

    let mut failures = Vec::new();
    let mut check = |what: String, ours: Json, theirs: Json| {
        if ours != theirs {
            failures.push(format!("{what}: layerstack {ours}, OpenUSD {theirs}"));
        }
    };
    let typed: Vec<&str> = oracle
        .schemas
        .iter()
        .flat_map(|s| s.is_a.iter().map(String::as_str))
        .collect();
    for schema in &oracle.schemas {
        let name = schema.name.as_str();
        let token = tokens.intern(name);
        let Some(definition) = registry.schema(token) else {
            check(
                format!(
                    "{name} ({}, {}) has no definition",
                    schema.kind, schema.plugin
                ),
                json!(true),
                json!(matches!(
                    schema.kind.as_str(),
                    "NonAppliedAPI" | "AbstractBase"
                )),
            );
            continue;
        };
        check(
            format!("{name} kind"),
            json!(format!("{:?}", definition.kind)),
            json!(match schema.kind.as_str() {
                "ConcreteTyped" => "ConcreteTyped",
                "AbstractTyped" => "AbstractTyped",
                "SingleApplyAPI" => "SingleApplyApi",
                "MultipleApplyAPI" => "MultipleApplyApi",
                other => other,
            }),
        );
        for &ancestor in &typed {
            let ours = registry.is_a(token, tokens.intern(ancestor));
            let theirs = schema.is_a.iter().any(|a| a == ancestor);
            check(format!("{name} IsA {ancestor}"), json!(ours), json!(theirs));
        }
        for (instance, types) in &schema.can_only_apply_to {
            let instance_name = (!instance.is_empty()).then_some(instance.as_str());
            let ours: Vec<&str> = registry
                .can_only_apply_to(token, instance_name, tokens)
                .iter()
                .map(|t| tokens.resolve(*t))
                .collect();
            check(
                format!("{name} {instance:?} can only apply to"),
                json!(ours),
                json!(types),
            );
        }
        for (instance, allowed) in &schema.allowed_instance_names {
            check(
                format!("{name} allows instance name {instance:?}"),
                json!(registry.is_allowed_instance_name(token, instance, tokens)),
                json!(allowed),
            );
        }
        // What usd-core auto-applies the schema to is what its plugInfo.json
        // declares; the definitions below show where it is applied.
        if definition.kind.is_applied() {
            let declared = &declared_types[&schema.name]["apiSchemaAutoApplyTo"];
            let mut declared: Vec<&str> = declared
                .as_array()
                .map(|items| items.iter().filter_map(Json::as_str).collect())
                .unwrap_or_default();
            declared.sort_unstable();
            check(
                format!("{name} auto-applies to"),
                json!(declared),
                json!(schema.auto_apply_to),
            );
        }
        for expected in &schema.definitions {
            let applied: Vec<TokenId> = expected.applied.iter().map(|a| tokens.intern(a)).collect();
            let label = format!("{name} {:?}", expected.applied);
            let ours: PrimDefinition = if definition.kind == SchemaKind::AbstractTyped {
                registry
                    .schema_definition(token)
                    .expect("an abstract definition")
                    .clone()
            } else {
                registry.intern_instance_names(&applied, tokens);
                let type_name =
                    (!expected.type_name.is_empty()).then(|| tokens.intern(&expected.type_name));
                registry.prim_definition(type_name, &applied, tokens)
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
            let allowed_tokens = tokens.intern("allowedTokens");
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
                    if t.is_array() && !t.type_name().ends_with("[]") {
                        format!("{}[]", t.type_name())
                    } else {
                        t.type_name().to_string()
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
                    json!(property.fallback.as_ref().map(|v| value_json(v, tokens))),
                    json!(expected.fallback),
                );
                let ours_tokens: Vec<String> = match property.metadata(allowed_tokens) {
                    Some(layerstack::FieldValue::Value(value)) => match value_json(value, tokens) {
                        Json::Array(items) => items
                            .iter()
                            .filter_map(|i| i.as_str().map(str::to_owned))
                            .collect(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                check(
                    what("allowedTokens"),
                    json!(ours_tokens),
                    json!(expected.allowed_tokens),
                );
            }
        }
    }
    assert_eq!(
        oracle.instances,
        ["red", "fixture"],
        "the recorded instances"
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The schema domains OpenUSD 26.08 checks a `generatedSchema.usda` and a
/// `plugInfo.json` in for.
const DOMAINS: &[&str] = &[
    "usd",
    "usdGeom",
    "usdHydra",
    "usdLod",
    "usdLux",
    "usdMedia",
    "usdMtlx",
    "usdPhysics",
    "usdProc",
    "usdProfiles",
    "usdRender",
    "usdRi",
    "usdSemantics",
    "usdShade",
    "usdSkel",
    "usdUI",
    "usdVol",
];

#[test]
fn openusd_domains_compile_to_their_checked_in_files() {
    let Some(source) = openusd_source() else {
        return;
    };
    let options = CompileOptions::new().with_openusd_source(&source);
    for domain in DOMAINS {
        let directory = source.join("pxr/usd").join(domain);
        let compiled = compile(&directory.join("schema.usda"), &options)
            .unwrap_or_else(|e| panic!("{domain}: {e}"));
        let checked_in = read(&directory.join("plugInfo.json"));
        assert_same_text(
            &format!("{domain}/generatedSchema.usda"),
            compiled.generated_schema_usda(),
            &read(&directory.join("generatedSchema.usda")),
        );
        let generated: BTreeMap<String, Json> =
            plug_info_types(compiled.plug_info_json().expect("a plugInfo.json"));
        let mut expected = plug_info_types(&checked_in);
        expected.retain(|_, entry| entry["autoGenerated"] == json!(true));
        assert_eq!(generated, expected, "{domain}/plugInfo.json types");
        // Built on the checked-in file, the whole file is that file.
        let rebuilt = compile(
            &directory.join("schema.usda"),
            &options.clone().with_existing_plug_info(checked_in.clone()),
        )
        .expect("compiles again");
        assert_same_text(
            &format!("{domain}/plugInfo.json"),
            rebuilt.plug_info_json().expect("a plugInfo.json"),
            &checked_in,
        );
    }
}

/// Compiled from OpenUSD's template, the installed form of a codeless
/// plugin is the committed one the oracle registers: placeholders filled
/// for `resources/`, strict JSON.
fn assert_installed(plugin: &str, options: CompileOptions) {
    let root = fixture();
    let compiled = compile(&root.join(plugin).join("schema.usda"), &options)
        .unwrap_or_else(|e| panic!("{plugin}: {e}"));
    let installed = root.join("installed").join(plugin).join("resources");
    let plug_info = compiled
        .installed_plug_info_json()
        .expect("a codeless library installs")
        .expect("a plugInfo.json");
    assert_same_text(
        &format!("installed/{plugin}/resources/plugInfo.json"),
        &plug_info,
        &read(&installed.join("plugInfo.json")),
    );
    assert_same_text(
        &format!("installed/{plugin}/resources/generatedSchema.usda"),
        compiled.generated_schema_usda(),
        &read(&installed.join("generatedSchema.usda")),
    );
    let json: Json = serde_json::from_str(&plug_info).expect("strict JSON");
    let entry = &json["Plugins"][0];
    assert_eq!(
        (
            &entry["Root"],
            &entry["ResourcePath"],
            &entry["LibraryPath"]
        ),
        (&Json::from(".."), &Json::from("resources"), &Json::from("")),
        "{plugin} installs into resources/ with no library"
    );
}

#[test]
fn installed_plugins_are_the_committed_ones() {
    assert_installed("lab", CompileOptions::new().with_search_path(fixture()));
    if let Some(source) = openusd_source() {
        assert_installed(
            "observatory",
            CompileOptions::new().with_openusd_source(&source),
        );
    }
}

#[test]
fn fixtures_compile_against_openusd() {
    let Some(source) = openusd_source() else {
        return;
    };
    for plugin in ["lab", "observatory"] {
        let directory = fixture().join(plugin);
        let options = CompileOptions::new()
            .with_openusd_source(&source)
            .with_existing_plug_info(read(&directory.join("plugInfo.json")));
        let compiled = compile(&directory.join("schema.usda"), &options)
            .unwrap_or_else(|e| panic!("{plugin}: {e}"));
        assert_same_text(
            &format!("{plugin}/generatedSchema.usda"),
            compiled.generated_schema_usda(),
            &read(&directory.join("generatedSchema.usda")),
        );
    }
}

/// `testUsdGenSchema`'s schemas and the baselines usdGenSchema generates
/// from them.
const BASELINES: &[(&str, &str)] = &[
    ("schema.usda", "basic"),
    ("codeless_sublayer_schema.usda", "codeless_sublayer"),
    ("ns_schema.usda", "namespace"),
    ("schema_tokensPrefix.usda", "tokensPrefix"),
    ("schema_useLiteralIdentifier.usda", "useLiteralIdentifier"),
];

#[test]
fn usd_gen_schema_tests_compile_to_their_baselines() {
    let Some(source) = openusd_source() else {
        return;
    };
    let tests = source.join("pxr/usd/usd/testenv/testUsdGenSchema");
    let options = CompileOptions::new().with_openusd_source(&source);
    for (schema, baseline) in BASELINES {
        let compiled =
            compile(&tests.join(schema), &options).unwrap_or_else(|e| panic!("{schema}: {e}"));
        let expected = tests.join("baseline").join(baseline);
        assert_same_text(
            &format!("{baseline}/generatedSchema.usda"),
            compiled.generated_schema_usda(),
            &read(&expected.join("generatedSchema.usda")),
        );
        assert_same_text(
            &format!("{baseline}/plugInfo.json"),
            compiled.plug_info_json().expect("a plugInfo.json"),
            &read(&expected.join("plugInfo.json")),
        );
    }
    let codeless = compile(&tests.join("codeless_schema.usda"), &options).expect("compiles");
    assert_same_text(
        "codeless/plugInfo.json",
        codeless.plug_info_json().expect("a plugInfo.json"),
        &read(&tests.join("baseline/codeless/plugInfo.json")),
    );
}

/// Whether a failing schema broke the rule usdGenSchema reports.
type Expected = fn(&Rule) -> bool;

#[test]
fn usd_gen_schema_failures_fail_with_its_errors() {
    let Some(source) = openusd_source() else {
        return;
    };
    let tests = source.join("pxr/usd/usd/testenv/testUsdGenSchema");
    let options = CompileOptions::new().with_openusd_source(&source);
    let fails: &[(&str, &str, Expected)] = &[
        ("schemaFail.usda", "/Base", |r| {
            matches!(r, Rule::DuplicateName { .. })
        }),
        ("schemaFail2.usda", "/Base", |r| {
            matches!(r, Rule::TokenCollision { .. })
        }),
        ("schemaFail3.usda", "/DuplicateAttrAPI", |r| {
            matches!(r, Rule::AppliedApiBase)
        }),
        ("schemaFail4.usda", "/DuplicateRelAPI", |r| {
            matches!(r, Rule::AppliedApiBase)
        }),
        ("schemaFail5.usda", "/Base", |r| {
            matches!(r, Rule::TypeNameMismatch { .. })
        }),
        ("schemaFail6.usda", "/DisallowedField", |r| {
            matches!(r, Rule::ConcreteNotTyped)
        }),
        ("schemaFail7.usda", "/ConcreteButUntyped", |r| {
            matches!(r, Rule::ConcreteNotTyped)
        }),
        ("schemaFail8.usda", "/MissingAPISuffix", |r| {
            matches!(r, Rule::MissingApiSuffix)
        }),
        ("schemaFail9.usda", "/NonAppliedAPI", |r| {
            matches!(r, Rule::NonAppliedApiBase)
        }),
        ("schemaFail10.usda", "/DerivedSingleApplyAPI", |r| {
            matches!(r, Rule::AppliedApiBase)
        }),
        ("schemaFail11.usda", "/DerivedMultipleApplyAPI", |r| {
            matches!(r, Rule::AppliedApiBase)
        }),
        ("schemaFail12.usda", "/MultipleApplyAPI", |r| {
            matches!(r, Rule::PropertiesWithoutPrefix)
        }),
        ("schemaFail13.usda", "/MultipleApplyAPI", |r| {
            matches!(r, Rule::PrefixWithoutProperties)
        }),
        ("schemaFail14.usda", "/SingleApplyAPI", |r| {
            matches!(
                r,
                Rule::MultipleApplyOnlyField {
                    field: "propertyNamespacePrefix"
                }
            )
        }),
        ("schemaFail15.usda", "/SingleApplyAPI", |r| {
            matches!(r, Rule::FallbackTypesNotConcrete)
        }),
        ("schemaFail16.usda", "/AbstractBase", |r| {
            matches!(r, Rule::FallbackTypesNotConcrete)
        }),
        ("schemaFail17.usda", "/Concrete", |r| {
            matches!(r, Rule::AutoApplyNotSingleApply)
        }),
        ("schemaFail18.usda", "/MultipleApplyAPI", |r| {
            matches!(r, Rule::AutoApplyNotSingleApply)
        }),
        ("schemaFail19.usda", "/MultipleApplyAPI", |r| {
            matches!(r, Rule::ApiSchemasNotPrepended)
        }),
        ("schemaFail20.usda", "/MultipleApplyAPI", |r| {
            matches!(r, Rule::ApiSchemasNotPrepended)
        }),
        ("schemaFail21.usda", "/VersionedTypedSchema_1", |r| {
            matches!(r, Rule::InheritsOwnFamily)
        }),
        ("schemaFail22.usda", "/InvalidVersionedId_1_2", |r| {
            matches!(r, Rule::NotAllowedIdentifier)
        }),
        (
            "schemaFail24.usda",
            "/PropertyOrderMultipleApplyAPI",
            |r| matches!(r, Rule::InvalidApiMetadata { fields, .. } if fields == &["propertyOrder"]),
        ),
    ];
    for (schema, path, rule) in fails {
        match compile(&tests.join(schema), &options) {
            Err(CompileError::Schema {
                path: at,
                rule: broken,
            }) => {
                assert_eq!(&at, path, "{schema}: {broken}");
                assert!(rule(&broken), "{schema}: {broken:?}");
            }
            other => panic!("{schema}: {other:?}"),
        }
    }
    match compile(&tests.join("schemaFail23.usda"), &options) {
        Err(CompileError::InvalidFields { class, fields }) => {
            assert_eq!(class, "/DisallowedFieldKind");
            assert_eq!(fields.len(), 1, "{fields:?}");
            assert_eq!(fields[0].field, "kind");
        }
        other => panic!("schemaFail23.usda: {other:?}"),
    }
}
