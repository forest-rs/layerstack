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

#![allow(
    clippy::cast_possible_truncation,
    reason = "samples narrow JSON numbers to their declared types"
)]

use std::collections::BTreeMap;

use layerstack::{
    AssetResolveError, AssetResolver, FieldEntry, FieldValue, InMemoryStore, LayerId, ListOp,
    MetadataDefinition, MetadataField, MetadataTarget, MetadataType, PathInterner, ResolvedAsset,
    SchemaRegistry, TokenInterner, Value, is_spec_field,
};
use layerstack_conformance::workspace_root;
use layerstack_usda::{read_usda_with_registry, save::save_usda};
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    builtins: BTreeMap<String, Builtin>,
    registration_errors: Vec<String>,
    registration: BTreeMap<String, Registration>,
    reading: Vec<Reading>,
    authoring: Vec<Authoring>,
    unauthored: Vec<Unauthored>,
}

/// A `SetMetadata` call with a value of a type, and what OpenUSD did.
#[derive(Deserialize)]
struct Authoring {
    spec: String,
    field: String,
    #[serde(rename = "type")]
    type_name: String,
    value: Json,
    error: Option<String>,
    stored: Option<Json>,
    written: Option<String>,
}

/// What `GetMetadata` and `HasMetadata` answer for a field no layer
/// authors.
#[derive(Deserialize)]
struct Unauthored {
    object: String,
    field: String,
    value: Option<Json>,
    has: bool,
}

/// A declared field as OpenUSD registers it: no `type` for one it
/// refuses, and no `specs` for one it registers for no spec.
#[derive(Deserialize)]
struct Registration {
    specs: Vec<String>,
    #[serde(rename = "type")]
    type_name: Option<String>,
    /// The declaration's `type`, when it is a string.
    declared: Option<String>,
    fallback: Option<Json>,
}

/// A metadata statement on a spec, in a layer of its own, and what
/// OpenUSD's text parser made of it.
#[derive(Deserialize)]
struct Reading {
    spec: String,
    statement: String,
    error: Option<String>,
    #[serde(default)]
    unregistered: bool,
    #[serde(default)]
    absent: bool,
    value: Option<Json>,
    written: Option<String>,
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
pub(crate) fn encode(value: &FieldValue, tokens: &TokenInterner) -> Json {
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
        FieldValue::UnregisteredListOp(op) => list(op, |s| json!(&**s)),
        FieldValue::PathListOp(_) => panic!("no metadata field holds paths"),
        other => panic!("no encoding for {other:?}"),
    }
}

/// A [`Value`] as the oracle records it.
pub(crate) fn encode_value(value: &Value, tokens: &TokenInterner) -> Json {
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
    assert_eq!(
        oracle.openusd_version,
        format!("0.{}", layerstack_schemas::OPENUSD_VERSION)
    );
    let tokens = TokenInterner::default();
    for (name, builtin) in &oracle.builtins {
        if builtin.type_name == "permission" {
            // An enum of Sdf's own, which no metadata type holds: kept as
            // the token it is written as.
            assert!(MetadataField::builtin(name).is_none());
            continue;
        }
        if builtin.type_name == "payloadlistop" || name == "typeName" {
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

fn target(spec: &str) -> MetadataTarget {
    TARGETS
        .into_iter()
        .find(|target| spec_name(*target) == spec)
        .unwrap_or_else(|| panic!("no kind of spec {spec}"))
}

/// Whether OpenUSD refuses the declaration of `name` (the second of two
/// declarations of one field included).
fn refused(oracle: &Oracle, name: &str) -> bool {
    oracle
        .registration_errors
        .iter()
        .any(|message| message.contains(&format!("\"{name}\"")))
}

/// The registry OpenUSD's registration of the fixture plugins amounts to:
/// each field it registers, with its type, spec kinds and fallback.
fn registry(oracle: &Oracle, tokens: &TokenInterner) -> SchemaRegistry {
    let mut builder = SchemaRegistry::builder();
    for (name, registration) in &oracle.registration {
        let registered = !registration.specs.is_empty() || !refused(oracle, name);
        let (true, Some(declared)) = (registered, &registration.declared) else {
            continue;
        };
        let value_type = MetadataType::parse(declared).unwrap();
        let mut definition = MetadataDefinition::new(
            tokens.intern(name),
            value_type,
            registration.specs.iter().map(|spec| target(spec)),
        );
        // The fallback, as the plugin's default.
        if let (Some(fallback), Some(type_name)) = (&registration.fallback, &registration.type_name)
            && matches!(value_type, MetadataType::Value(_) | MetadataType::Array(_))
            && fallback.get("opaque").is_none()
            && let FieldValue::Value(value) = sample(type_name, fallback, tokens)
        {
            definition.default = Some(value);
        }
        builder.register_metadata(definition, tokens).unwrap();
    }
    builder.build(tokens)
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

/// The layer the oracle reads `statement` on a spec of kind `spec` from.
fn layer_text(spec: &str, statement: &str) -> String {
    match spec {
        "layer" => format!("#usda 1.0\n(\n    {statement}\n)\n"),
        "prim" => format!("#usda 1.0\ndef \"P\" (\n    {statement}\n)\n{{\n}}\n"),
        "variant" => format!(
            "#usda 1.0\ndef \"P\" (\n    variants = {{\n        string v = \"x\"\n    }}\n    \
             prepend variantSets = \"v\"\n)\n{{\n    variantSet \"v\" = {{\n        \"x\" (\n            \
             {statement}\n        ) {{\n        }}\n    }}\n}}\n"
        ),
        "attribute" => {
            format!("#usda 1.0\ndef \"P\"\n{{\n    float a (\n        {statement}\n    )\n}}\n")
        }
        "relationship" => {
            format!("#usda 1.0\ndef \"P\"\n{{\n    rel r (\n        {statement}\n    )\n}}\n")
        }
        other => panic!("no kind of spec {other}"),
    }
}

/// The field a statement sets.
fn field_of(statement: &str) -> &str {
    let first = statement.lines().next().unwrap();
    first
        .split('=')
        .next()
        .unwrap()
        .split_whitespace()
        .last()
        .unwrap()
}

/// The fields of the spec of kind `spec` in a layer the oracle reads.
fn spec_fields<'a>(
    layer: &'a layerstack::Layer,
    store: &InMemoryStore,
    prim: layerstack::PathId,
    spec: &str,
) -> &'a [FieldEntry] {
    match spec {
        "layer" => layer.metadata_fields(),
        "prim" => &layer.prims()[&prim].fields,
        "variant" => {
            let prim = &layer.prims()[&prim];
            let set = prim
                .variant_sets()
                .get(&store.tokens().lookup("v").unwrap())
                .unwrap();
            &set.variants[&store.tokens().lookup("x").unwrap()].fields
        }
        "attribute" | "relationship" => {
            let name = if spec == "attribute" { "a" } else { "r" };
            let prim = &layer.prims()[&prim];
            &layerstack::property::get_property(
                &prim.properties,
                store.tokens().lookup(name).unwrap(),
            )
            .unwrap()
            .metadata
        }
        other => panic!("no kind of spec {other}"),
    }
}

/// The lines of `field`'s statements in the text a layer saves as, as the
/// oracle collects OpenUSD's: stripped, a dictionary's block whole.
fn written(text: &str, field: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let statement = ["prepend ", "append ", "delete ", "add ", "reorder "]
            .iter()
            .find_map(|op| trimmed.strip_prefix(op))
            .unwrap_or(trimmed);
        if !statement.starts_with(&format!("{field} = ")) {
            continue;
        }
        out.push(line.trim());
        if line.trim_end().ends_with('{') {
            let indent = line.len() - trimmed.len();
            for rest in &lines[i + 1..] {
                out.push(rest.trim());
                if rest.trim() == "}" && rest.len() - rest.trim_start().len() == indent {
                    break;
                }
            }
        }
    }
    out.join("\n")
}

#[test]
fn metadata_statements_read_and_save_as_openusd_reads_and_writes_them() {
    let oracle = oracle();
    for case in &oracle.reading {
        let store = InMemoryStore::default();
        let registry = registry(&oracle, store.tokens());
        let source = layer_text(&case.spec, &case.statement);
        let read = read_usda_with_registry(
            &source,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
            &registry,
        );
        let what = format!("{} on a {}", case.statement, case.spec);
        if let Some(error) = &case.error {
            assert!(read.emitted.rejected, "{what} is rejected");
            let messages: Vec<String> = read
                .emitted
                .diagnostics
                .iter()
                .filter_map(|d| d.metadata.as_ref().map(|m| m.openusd_message()))
                .collect();
            assert_eq!(messages, std::slice::from_ref(error), "{what}");
            continue;
        }
        assert!(
            read.parse_diagnostics.is_empty() && !read.emitted.rejected,
            "{what}: {:?} {:?}",
            read.parse_diagnostics,
            read.emitted.diagnostics
        );
        let field = field_of(&case.statement);
        let layer = &read.emitted.layer;
        let key = store.tokens().intern(field);
        let prim = store.path("/P");
        let value = layerstack::doc::get_field(spec_fields(layer, &store, prim, &case.spec), &key);
        if case.absent {
            assert!(value.is_none(), "{what}");
            continue;
        }
        // Prim members, and a prim's type, which no field holds.
        let member = matches!(field, "instanceable" | "typeName");
        if !member {
            let value = value.unwrap_or_else(|| panic!("{what} authors {field}"));
            // A variant's field registered for variants alone is no field
            // the text parser types (it reads a variant's metadata as a
            // prim's), and OpenUSD keeps its text.
            let unregistered = case.unregistered
                || case
                    .value
                    .as_ref()
                    .is_some_and(|value| value.get("unregistered").is_some());
            if !unregistered {
                assert_eq!(
                    Some(encode(value, store.tokens())),
                    case.value,
                    "{what}'s value"
                );
            }
        }
        let text = save_usda(layer, store.tokens(), store.paths())
            .unwrap_or_else(|e| panic!("{what} saves: {e:?}"));
        assert_eq!(
            Some(written(&text, field)),
            case.written,
            "{what} saves as OpenUSD writes it:\n{text}"
        );
        let again = read_usda_with_registry(
            &text,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
            &registry,
        );
        assert_eq!(&again.emitted.layer, layer, "{what} reopens as it was");
    }
}

/// The fixture plugins read with `layerstack_schemagen::metadata::read`,
/// which needs the schema generator: like it, not for wasm.
#[cfg(not(target_arch = "wasm32"))]
mod registration {
    use super::*;
    use layerstack_schemagen::metadata::read;

    #[test]
    fn plugin_declarations_register_as_openusd_registers_them() {
        let oracle = oracle();
        let fixture = workspace_root().join("layerstack_conformance/fixtures/registry_metadata");
        let good = std::fs::read_to_string(fixture.join("plugInfo.json")).unwrap();
        let bad = std::fs::read_to_string(fixture.join("rejected/plugInfo.json")).unwrap();
        let tokens = TokenInterner::default();
        let declared = read(
            [("good", good.as_str()), ("rejected", bad.as_str())],
            &tokens,
        )
        .unwrap();

        // The errors naming a declaration (OpenUSD raises a detail before
        // some of them, which names none).
        let mut expected: Vec<&str> = oracle
            .registration_errors
            .iter()
            .map(String::as_str)
            .filter(|m| m.contains("(at \"") || m.contains("is already a registered field"))
            .collect();
        expected.sort_unstable();
        let mut messages: Vec<String> = declared
            .rejected
            .iter()
            .filter_map(|error| error.openusd_message())
            .collect();
        messages.sort();
        assert_eq!(messages, expected, "{:#?}", declared.rejected);

        let mut accepted: Vec<&str> = Vec::new();
        for declaration in &declared.accepted {
            let definition = &declaration.definition;
            let name = tokens.resolve(definition.name);
            accepted.push(name);
            let registration = &oracle.registration[name];
            let specs: Vec<&str> = TARGETS
                .into_iter()
                .filter(|target| definition.applies_to(*target))
                .map(spec_name)
                .collect();
            assert_eq!(specs, registration.specs, "{name}'s spec kinds");
            if specs.is_empty() {
                continue;
            }
            assert_eq!(
                Some(definition.value_type.storage().name()),
                registration.type_name.as_deref(),
                "{name}'s type"
            );
            let fallback = definition.fallback().map(|value| encode(&value, &tokens));
            // An `opaque` field holds no value: OpenUSD's fallback is the
            // empty `SdfOpaqueValue`, which layerstack has no value for.
            let opaque = Some(json!({ "opaque": true }));
            if registration.fallback != opaque {
                assert_eq!(fallback, registration.fallback, "{name}'s fallback");
            } else {
                assert_eq!(fallback, None, "{name}'s fallback");
            }
        }
        let registered: Vec<&str> = oracle
            .registration
            .iter()
            // Registered for some spec, or else for none and not refused
            // (a field registered twice is refused only the second time).
            .filter(|(name, registration)| {
                !registration.specs.is_empty()
                    || !expected.iter().any(|m| m.contains(&format!("\"{name}\"")))
            })
            .map(|(name, _)| name.as_str())
            .collect();
        accepted.sort_unstable();
        assert_eq!(accepted, registered);
        let layout = declared
            .accepted
            .iter()
            .find(|d| tokens.resolve(d.definition.name) == "metaFloat")
            .unwrap();
        assert_eq!(&*layout.definition.display_group, "Shape");
    }
}

/// A sample's text, or an encoded fallback's (`{"asset": "a.usda"}`).
fn text(value: &Json, key: &str) -> String {
    value
        .as_str()
        .or_else(|| value.get(key).and_then(Json::as_str))
        .unwrap()
        .to_owned()
}

/// `sample` as a value of the type `ty`, as the oracle builds it in Python.
fn sample(ty: &str, sample: &Json, tokens: &TokenInterner) -> FieldValue {
    let f = |v: &Json| v.as_f64().unwrap();
    let floats =
        |v: &Json| -> Vec<f32> { v.as_array().unwrap().iter().map(|x| f(x) as f32).collect() };
    let strings = |v: &Json| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| text(x, "asset"))
            .collect()
    };
    let list = |v: &Json| {
        let mut op = ListOp::default();
        for (part, items) in v.as_object().unwrap() {
            let items: Vec<Json> = items.as_array().unwrap().clone();
            match part.as_str() {
                "explicit" => op.explicit = Some(items),
                "prepended" => op.prepend = items,
                "appended" => op.append = items,
                "deleted" => op.delete = items,
                "added" => op.add = items,
                "ordered" => op.reorder = items,
                other => panic!("{other}"),
            }
        }
        op
    };
    fn map<T>(op: ListOp<Json>, item: impl Fn(&Json) -> T) -> Box<ListOp<T>> {
        let all = |items: &[Json]| items.iter().map(&item).collect();
        Box::new(ListOp {
            explicit: op.explicit.as_deref().map(all),
            prepend: all(&op.prepend),
            append: all(&op.append),
            delete: all(&op.delete),
            add: all(&op.add),
            reorder: all(&op.reorder),
        })
    }
    FieldValue::Value(match ty {
        "bool" => Value::Bool(sample.as_bool().unwrap()),
        "uchar" => Value::UChar(u8::try_from(sample.as_u64().unwrap()).unwrap()),
        "int" => Value::Int(i32::try_from(sample.as_i64().unwrap()).unwrap()),
        "uint" => Value::UInt(u32::try_from(sample.as_u64().unwrap()).unwrap()),
        "int64" => Value::Int64(sample.as_i64().unwrap()),
        "uint64" => Value::UInt64(sample.as_u64().unwrap()),
        "half" => Value::Half(layerstack::half::from_f32(f(sample) as f32)),
        "float" => Value::Float(f(sample) as f32),
        "double" => Value::Double(f(sample)),
        "timecode" => Value::TimeCode(f(sample)),
        "string" => Value::string(sample.as_str().unwrap()),
        "token" => Value::Token(tokens.intern(sample.as_str().unwrap())),
        // A sample's text, or an encoded fallback's.
        "asset" => Value::Asset(text(sample, "asset").into()),
        "pathExpression" => Value::PathExpression(text(sample, "expression").into()),
        "float3" | "color3f" => Value::Vec3f(floats(sample).try_into().unwrap()),
        "quatf" => {
            let q = floats(sample);
            Value::Quatf([q[1], q[2], q[3], q[0]])
        }
        "matrix4d" => {
            let m: Vec<f64> = sample
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|row| row.as_array().unwrap().iter().map(f))
                .collect();
            Value::Matrix4d(Box::new(m.try_into().unwrap()))
        }
        "bool[]" => Value::from(
            sample
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_bool().unwrap())
                .collect::<Vec<_>>(),
        ),
        "int[]" => Value::from(
            sample
                .as_array()
                .unwrap()
                .iter()
                .map(|n| i32::try_from(n.as_i64().unwrap()).unwrap())
                .collect::<Vec<_>>(),
        ),
        "float[]" => Value::from(floats(sample)),
        "token[]" => Value::array_from_iter(
            strings(sample)
                .iter()
                .map(|t| Value::Token(tokens.intern(t))),
            None,
        ),
        "string[]" => Value::array_from_iter(
            strings(sample).iter().map(|t| Value::string(t.as_str())),
            None,
        ),
        "asset[]" => Value::Array(
            strings(sample)
                .iter()
                .map(|t| Value::Asset(t.as_str().into()))
                .collect(),
        ),
        "float3[]" => Value::from(
            sample
                .as_array()
                .unwrap()
                .iter()
                .map(|v| <[f32; 3]>::try_from(floats(v)).unwrap())
                .collect::<Vec<_>>(),
        ),
        "matrix2d[]" => Value::Array(
            sample
                .as_array()
                .unwrap()
                .iter()
                .map(|m| {
                    let m: Vec<f64> = m
                        .as_array()
                        .unwrap()
                        .iter()
                        .flat_map(|row| row.as_array().unwrap().iter().map(f))
                        .collect();
                    Value::Matrix2d(Box::new(m.try_into().unwrap()))
                })
                .collect(),
        ),
        "dictionary" => Value::Dictionary(
            sample
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, typed)| {
                    let (ty, value) = typed.as_object().unwrap().iter().next().unwrap();
                    let FieldValue::Value(value) = self::sample(ty, value, tokens) else {
                        panic!("a dictionary holds values");
                    };
                    (key.as_str().into(), value)
                })
                .collect(),
        ),
        "tokenlistop" => {
            return FieldValue::TokenListOp(map(list(sample), |t| {
                tokens.intern(t.as_str().unwrap())
            }));
        }
        "stringlistop" => {
            return FieldValue::StringListOp(map(list(sample), |t| t.as_str().unwrap().into()));
        }
        "intlistop" => {
            return FieldValue::IntListOp(map(list(sample), |n| {
                i32::try_from(n.as_i64().unwrap()).unwrap()
            }));
        }
        "uintlistop" => {
            return FieldValue::UIntListOp(map(list(sample), |n| {
                u32::try_from(n.as_u64().unwrap()).unwrap()
            }));
        }
        "int64listop" => {
            return FieldValue::Int64ListOp(map(list(sample), |n| n.as_i64().unwrap()));
        }
        "uint64listop" => {
            return FieldValue::UInt64ListOp(map(list(sample), |n| n.as_u64().unwrap()));
        }
        other => panic!("no sample of {other}"),
    })
}

/// The stage the oracle authors on: `/P`, with the attribute `a` and the
/// relationship `r`, composed with `registry`.
fn authoring_stage(registry: SchemaRegistry, store: &mut InMemoryStore) -> layerstack::LiveStage {
    let text = "#usda 1.0\ndef \"P\"\n{\n    custom float a\n    custom rel r\n}\n";
    let read = read_usda_with_registry(
        text,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
        &registry,
    );
    assert!(!read.emitted.rejected, "{:?}", read.emitted.diagnostics);
    store.insert_layer(read.emitted.layer);
    layerstack::LiveStage::compose(
        store,
        LayerId(1),
        layerstack::StageOptions {
            schemas: Some(std::sync::Arc::new(registry)),
            ..layerstack::StageOptions::default()
        },
    )
}

#[test]
fn metadata_edits_are_checked_and_saved_as_openusd_checks_and_writes_them() {
    let oracle = oracle();
    for call in &oracle.authoring {
        let mut store = InMemoryStore::default();
        let mut live = authoring_stage(registry(&oracle, store.tokens()), &mut store);
        let value = sample(&call.type_name, &call.value, store.tokens());
        let what = format!(
            "{} on a {} as {} {}",
            call.field, call.spec, call.type_name, call.value
        );
        let prim = store.path("/P");
        let refused: Option<layerstack::MetadataEditError> = if call.spec == "stage" {
            let key = store.tokens().intern(&call.field);
            match live.set_stage_metadata(&mut store, LayerId(1), key, value.clone()) {
                Ok(_) => None,
                Err(layerstack::StageMetadataError::Field(error)) => Some(error),
                Err(other) => panic!("{what}: {other:?}"),
            }
        } else {
            let mut edit = layerstack_schemas::SchemaEdit::new(
                live.stage(),
                &mut store,
                layerstack::EditTarget::for_layer(LayerId(1)),
            );
            let view = layerstack_schemas::PrimEdit::new(&edit, prim)
                .expect("outside any instance")
                .expect("the prim exists");
            let result = match call.spec.as_str() {
                "prim" => view.write_metadata(&mut edit, &call.field, value.clone()),
                "attribute" => {
                    view.write_property_metadata(&mut edit, "a", &call.field, value.clone())
                }
                _ => view.write_property_metadata(&mut edit, "r", &call.field, value.clone()),
            };
            let refused = match result {
                Ok(_) => None,
                Err(layerstack_schemas::MetadataError::Field(error)) => Some(error),
                Err(other) => panic!("{what}: {other:?}"),
            };
            if refused.is_some() {
                assert!(
                    edit.transaction().is_empty(),
                    "{what}: a refusal collects nothing"
                );
            }
            let transaction = edit.finish();
            live.apply(&mut store, &transaction).unwrap();
            refused
        };
        match (&call.error, refused) {
            (None, None) => {}
            (Some(expected), Some(error)) => {
                // The Python binding's message names the value and its
                // C++ type, which a Rust value has not.
                if expected.starts_with("Invalid value") {
                    assert!(
                        matches!(
                            error.reason,
                            layerstack::MetadataEditReason::ValueType { .. }
                        ),
                        "{what}: {error:?}"
                    );
                } else {
                    assert_eq!(&error.openusd_message(), expected, "{what}");
                }
                continue;
            }
            (expected, refused) => panic!("{what}: OpenUSD {expected:?}, layerstack {refused:?}"),
        }
        let layer = store.layer(LayerId(1)).unwrap().clone();
        if !value.is_list_op() {
            assert_eq!(
                Some(encode(&value, store.tokens())),
                call.stored,
                "{what}: stored"
            );
        }
        let text = save_usda(&layer, store.tokens(), store.paths())
            .unwrap_or_else(|e| panic!("{what} saves: {e:?}"));
        assert_eq!(
            Some(written(&text, &call.field)),
            call.written,
            "{what} saves:\n{text}"
        );
        let again = read_usda_with_registry(
            &text,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
            &registry(&oracle, store.tokens()),
        );
        assert_eq!(again.emitted.layer, layer, "{what} reopens as it was");
    }
}

#[test]
fn unauthored_fields_read_as_openusd_reads_them() {
    let oracle = oracle();
    let mut store = InMemoryStore::default();
    let live = authoring_stage(registry(&oracle, store.tokens()), &mut store);
    let scene = layerstack_schemas::Scene::new(live.stage(), &store);
    let prim = layerstack_schemas::PrimView::new(scene, store.path("/P"));
    for case in &oracle.unauthored {
        let what = format!("{} on the {}", case.field, case.object);
        let (value, has) = match case.object.as_str() {
            "stage" => {
                let value = scene.metadata().value(&case.field);
                let key = store.tokens().lookup(&case.field).unwrap();
                (value, live.stage().has_metadata(key, &store))
            }
            "prim" => (prim.metadata().value(&case.field), false),
            "attribute" | "relationship" => {
                let name = if case.object == "attribute" { "a" } else { "r" };
                let metadata = prim.property_metadata(name).unwrap();
                (
                    metadata.value(&case.field),
                    metadata.has_authored(&case.field),
                )
            }
            other => panic!("{other}"),
        };
        let value = value.map(|value| encode(&FieldValue::Value(value), store.tokens()));
        assert_eq!(value, case.value, "{what}");
        if case.object == "stage" {
            assert_eq!(has, case.has, "{what}");
        }
    }
}
