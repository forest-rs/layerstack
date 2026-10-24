// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed schemas with more than one base, against usd-core 26.8.
//!
//! `fixtures/schema_bases` is a hand-written codeless schema plugin whose
//! `plugInfo.json` gives typed schemas several `bases`, which usdGenSchema
//! never writes (it rejects a class with more than one `inherits`), with a
//! base that is no schema, a base nothing declares, and bases in an order
//! their own bases contradict. `fixtures/schema_bases/oracle.json`
//! (`scripts/schema_bases_oracle.py`) records what OpenUSD's registry makes
//! of it: which schemas each type `IsA`, its ancestors in C3 order, its prim
//! definition, what can be applied to it, and the errors registration
//! raises. layerstack's registry, built from the same files with
//! `read_generated_schema`, must answer the same.
//!
//! Spec: AOUSD Core §13.3.1 (typed schemas). OpenUSD: `TfType` ancestry
//! (`pxr/base/tf/type.cpp`), `UsdSchemaRegistry`
//! (`pxr/usd/usd/schemaRegistry.cpp`).

use std::collections::BTreeMap;

use layerstack::schema::{SchemaDeclaration, read_generated_schema};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, InvalidBaseReason, LayerId, PathInterner,
    ResolvedAsset, SchemaIssue, SchemaKind, SchemaRegistry, TokenId, TokenInterner,
};
use layerstack_conformance::workspace_root;
use layerstack_schemas::Domain;
use serde::Deserialize;

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
}

#[derive(Deserialize)]
struct TypeInfo {
    #[serde(default)]
    bases: Vec<String>,
    #[serde(rename = "schemaIdentifier")]
    identifier: Option<String>,
    #[serde(rename = "schemaKind")]
    kind: Option<String>,
    #[serde(rename = "apiSchemaAutoApplyTo", default)]
    auto_apply_to: Vec<String>,
    #[serde(rename = "apiSchemaCanOnlyApplyTo", default)]
    can_only_apply_to: Vec<String>,
}

#[derive(Deserialize)]
struct Oracle {
    registry_errors: Vec<String>,
    types: BTreeMap<String, Recorded>,
}

#[derive(Deserialize)]
struct Recorded {
    schema: bool,
    is_a: Vec<String>,
    #[serde(default)]
    ancestors: Option<Vec<String>>,
    #[serde(default)]
    ancestor_errors: Vec<String>,
    #[serde(default)]
    properties: Vec<String>,
    #[serde(default)]
    applied_schemas: Vec<String>,
    can_apply: BTreeMap<String, bool>,
}

/// Rejects every asset path: the generated schema layer has none.
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

fn read(file: &str) -> String {
    let path = workspace_root()
        .join("layerstack_conformance/fixtures/schema_bases")
        .join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// OpenUSD's base types the plugin names, by schema identifier.
const OPENUSD_BASES: &[(&str, &str)] = &[
    ("UsdSchemaBase", "SchemaBase"),
    ("UsdTyped", "Typed"),
    ("UsdAPISchemaBase", "APISchemaBase"),
];

/// Whether `cpp_name` derives from `UsdSchemaBase` through any of its
/// bases, which is what makes OpenUSD's registry know a plugin type as a
/// schema (`PlugRegistry::GetAllDerivedTypes`).
fn is_schema(cpp_name: &str, types: &BTreeMap<String, TypeInfo>) -> bool {
    OPENUSD_BASES.iter().any(|(name, _)| *name == cpp_name)
        || types
            .get(cpp_name)
            .is_some_and(|info| info.bases.iter().any(|base| is_schema(base, types)))
}

/// A base by schema identifier: OpenUSD's and the plugin's schemas by
/// theirs, any other type by its C++ name, which names no schema.
fn base_name<'a>(cpp_name: &'a str, types: &'a BTreeMap<String, TypeInfo>) -> &'a str {
    OPENUSD_BASES
        .iter()
        .find(|(name, _)| *name == cpp_name)
        .map(|(_, identifier)| *identifier)
        .or_else(|| types.get(cpp_name)?.identifier.as_deref())
        .unwrap_or(cpp_name)
}

/// The fixture plugin's registry, built on OpenUSD's `usd` domain (for
/// `Typed`) as usd-core builds it.
fn registry(store: &mut InMemoryStore) -> SchemaRegistry {
    let plug_info: PlugInfo = serde_json::from_str(&read("plugInfo.json")).expect("plugInfo");
    let types = &plug_info.plugins[0].info.types;
    let mut declared = Vec::new();
    for (cpp_name, info) in types {
        let (Some(identifier), Some(kind)) = (&info.identifier, &info.kind) else {
            continue;
        };
        if !is_schema(cpp_name, types) {
            continue;
        }
        let kind = match kind.as_str() {
            "concreteTyped" => SchemaKind::ConcreteTyped,
            "abstractTyped" => SchemaKind::AbstractTyped,
            "singleApplyAPI" => SchemaKind::SingleApplyApi,
            other => panic!("schema kind {other}"),
        };
        let mut declaration = SchemaDeclaration::new(store.tokens.intern(identifier), kind);
        if kind.is_typed() {
            let mut bases = info
                .bases
                .iter()
                .map(|base| store.tokens.intern(base_name(base, types)));
            declaration.parent = bases.next();
            declaration.additional_bases = bases.collect();
        }
        declaration.can_only_apply_to = info
            .can_only_apply_to
            .iter()
            .map(|name| store.tokens.intern(name))
            .collect();
        declared.push(declaration);
    }
    let source = read("generatedSchema.usda");
    let parsed = layerstack_usda::parser::parse(&source);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(u64::MAX),
        &store.tokens,
        &store.paths,
        &mut NoAssets,
    );
    assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
    let definitions = read_generated_schema(&emitted.layer, &declared, &store.tokens, &store.paths)
        .expect("every declared schema");
    let mut builder = SchemaRegistry::builder();
    layerstack_schemas::register(&mut builder, &[Domain::Usd], &store.tokens).expect("usd");
    for definition in definitions {
        builder.register(definition);
    }
    for info in types.values() {
        for target in &info.auto_apply_to {
            let api = info.identifier.as_deref().expect("an API schema");
            builder.auto_apply(store.tokens.intern(api), store.tokens.intern(target));
        }
    }
    builder.build(&store.tokens)
}

fn names(ids: &[TokenId], tokens: &TokenInterner) -> Vec<String> {
    ids.iter()
        .map(|id| tokens.resolve(*id).to_string())
        .collect()
}

#[test]
fn multiple_bases_answer_as_openusd_does() {
    let oracle: Oracle = serde_json::from_str(&read("oracle.json")).expect("oracle");
    let mut store = InMemoryStore::default();
    let registry = registry(&mut store);
    let tokens = &store.tokens;
    let typed: Vec<&str> = oracle
        .types
        .keys()
        .map(String::as_str)
        .chain(["Typed"])
        .collect();
    let mut failures = Vec::new();
    let mut inconsistent = Vec::new();
    for (name, recorded) in &oracle.types {
        let id = tokens.intern(name);
        let mut check = |what: &str, ours: String, openusd: String| {
            if ours != openusd {
                failures.push(format!("{name} {what}: ours {ours}, OpenUSD {openusd}"));
            }
        };
        check(
            "is a schema",
            format!("{}", registry.schema(id).is_some()),
            format!("{}", recorded.schema),
        );
        // A prim typed with it: typeless for an abstract or unknown type.
        let prim = registry.prim_definition(Some(id), &[], tokens);
        let is_a: Vec<&str> = typed
            .iter()
            .copied()
            .filter(|schema| prim.is_a(tokens.intern(schema)))
            .collect();
        let mut sorted = is_a.clone();
        sorted.sort_unstable();
        check("IsA", format!("{sorted:?}"), format!("{:?}", recorded.is_a));
        for (api, allowed) in &recorded.can_apply {
            let ours = registry
                .can_apply(prim.type_name(), tokens.intern(api), None, tokens)
                .is_ok();
            check(
                &format!("CanApplyAPI({api})"),
                format!("{ours}"),
                format!("{allowed}"),
            );
        }
        if !recorded.schema {
            continue;
        }
        let ancestors = names(registry.ancestors(id), tokens);
        match &recorded.ancestors {
            Some(openusd) => check(
                "ancestors",
                format!("{ancestors:?}"),
                format!("{openusd:?}"),
            ),
            // OpenUSD reports the order inconsistent; the schema still is
            // each ancestor, the type first.
            None => {
                assert!(!recorded.ancestor_errors.is_empty(), "{name}");
                inconsistent.push(id);
                check("first ancestor", ancestors[0].clone(), name.clone());
            }
        }
        let definition = registry.schema_definition(id).expect("a definition");
        let mut properties = names(
            &definition
                .properties()
                .iter()
                .map(|property| property.name)
                .collect::<Vec<_>>(),
            tokens,
        );
        properties.sort_unstable();
        check(
            "properties",
            format!("{properties:?}"),
            format!("{:?}", recorded.properties),
        );
        let applied: Vec<String> = definition
            .applied_schemas()
            .iter()
            .map(|applied| tokens.resolve(applied.name).to_string())
            .collect();
        check(
            "applied schemas",
            format!("{applied:?}"),
            format!("{:?}", recorded.applied_schemas),
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    // What OpenUSD's registration reports: each base without schema info
    // (a failed verification), and, when asked for ancestors, each
    // inconsistent order.
    let mut unregistered = Vec::new();
    for issue in registry.issues() {
        match issue {
            SchemaIssue::InvalidBase {
                schema,
                base,
                reason: InvalidBaseReason::Unregistered,
            } => unregistered.push(format!(
                "Failed verification: ' schemaInfo ' -- Could not find schema info for base \
                 type '{}' of schema '{}'",
                tokens.resolve(*base),
                tokens.resolve(*schema)
            )),
            SchemaIssue::InconsistentAncestry { schema, .. } => {
                assert!(inconsistent.contains(schema), "{issue:?}");
                inconsistent.retain(|id| id != schema);
            }
            other => panic!("unexpected issue {other:?}"),
        }
    }
    unregistered.sort_unstable();
    assert_eq!(unregistered, oracle.registry_errors);
    assert!(inconsistent.is_empty(), "unreported: {inconsistent:?}");
}
