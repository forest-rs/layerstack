// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Time codes, durations and path expressions are mapped between layers
//! and the stage as OpenUSD 26.11 maps them, reading and authoring.
//!
//! `scripts/value_transforms_oracle.py` writes `fixtures/value_transforms`:
//! `/Root` references `ref.usda</Ref>` with `(offset = 10; scale = 2)`,
//! `/Inst` inherits `/Class`, `/Rock` selects a local variant, and `/Sub`
//! is authored in a sublayer with `(offset = 4; scale = 0.5)`; each source
//! spec authors time codes, durations and path expressions in defaults,
//! time samples, arrays and nested `customData` dictionaries. It records
//! what OpenUSD reads on each prim, `UsdStage::Flatten`'s text, and, for
//! each edit target, what a series of `UsdAttribute::Set`,
//! `UsdPrim::CreateAttribute`, `SetMetadata` and `SetMetadataByDictKey`
//! calls through it write in its layer and what the stage then reads.
//!
//! Here the stage must read the same values; the flattened layer must
//! save as the same text; and the same calls, made through
//! `layerstack_schemas`' `PrimEdit` (and a `Transaction` for the
//! attributes created), applied through a `LiveStage`, must be accepted
//! as OpenUSD's were, leave the target layer's text byte for byte as
//! OpenUSD's, and read back as OpenUSD reads, as a fresh composition
//! does. The cases in `DIFFERING` are the ones layerstack does not yet do
//! as OpenUSD does.
//!
//! Spec: AOUSD Core §10 (arcs map namespace), §12.2 (metadata
//! resolution), §12.3.2.1 (layer offsets). OpenUSD: `_FieldValueToStageXf`
//! and `_StageValueToFieldXf` in `pxr/usd/usd/stage.cpp`.

use std::sync::Arc;

use layerstack::edit::{EditTarget, Transaction};
use layerstack::stage::flatten::FlattenRequirements;
use layerstack::{
    ArcKind, FieldValue, InMemoryStore, LayerId, LiveStage, PathId, PropertySpec, PropertyType,
    ResolvedValue, SpecPath, Stage, StageOptions, Time, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::{PrimEdit, SchemaEdit};
use serde_json::{Map, Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/value_transforms/oracle.json");

/// The prims the oracle reads.
const PRIMS: [&str; 4] = ["/Root", "/Inst", "/Rock", "/Sub"];

/// The cases where layerstack does not yet do what OpenUSD does. Each
/// must still differ, so that the list stays current.
///
/// Through an edit target that maps a prim into a variant, OpenUSD maps a
/// path expression's prefixes into the variant, selection and all
/// (`/Rock{look=a}Child`, which names no prim); layerstack strips the
/// selection, as both strip it from relationship targets. OpenUSD cannot
/// read its own output back: its USDA reader rejects the saved layer
/// ("Ill-formed path expression </Rock{look=a}Child> ... expected '}' to
/// close predicate expression"), as `SdfPathExpression` parses `{` as the
/// start of a predicate.
const DIFFERING: &[&str] = &[
    "reference_variant_expressions",
    "variant_expressions",
    "local_direct_variant_expressions",
];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn load() -> LoadedStage {
    let loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/value_transforms/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

fn layer_named(loaded: &LoadedStage, name: &str) -> LayerId {
    *loaded
        .layer_names
        .iter()
        .find(|(_, n)| n.ends_with(&format!("/{name}")) || n.as_str() == name)
        .unwrap_or_else(|| panic!("no layer {name}"))
        .0
}

/// `value` as the oracle's `encode` writes it.
fn encode(value: &Value) -> Json {
    match value {
        Value::TimeCode(time) => json!({ "timecode": time }),
        Value::Duration(length) => json!({ "duration": length }),
        Value::PathExpression(text) => json!({ "pathExpression": &**text }),
        Value::Asset(path) => json!({ "asset": &**path }),
        Value::Double(v) => json!(v),
        Value::Dictionary(entries) => dictionary(entries),
        other => match other.array_ref() {
            Some(items) => Json::Array(items.iter().map(|item| encode(&item)).collect()),
            None => panic!("no JSON rendering for {other:?}"),
        },
    }
}

fn dictionary(entries: &[(Arc<str>, FieldValue)]) -> Json {
    let entries: Map<String, Json> = entries
        .iter()
        .map(|(key, entry)| {
            let value = entry.as_value().expect("a plain entry");
            (key.to_string(), encode(value))
        })
        .collect();
    json!({ "dictionary": entries })
}

fn resolved_dictionary(value: Option<ResolvedValue>) -> Json {
    match value {
        Some(ResolvedValue::Dictionary(entries)) => dictionary(&entries),
        None => json!({ "dictionary": {} }),
        Some(other) => panic!("not a dictionary: {other:?}"),
    }
}

/// What the stage reads on `path`, as the oracle's `prim_record`.
fn prim_record(store: &InMemoryStore, stage: &Stage, path: &str) -> Json {
    let prim = stage.prim(store.path(path), store).expect("a prim");
    let mut attributes = Map::new();
    for property in prim.properties() {
        let Some(attribute) = property.as_attribute() else {
            continue;
        };
        let samples: Vec<Json> = attribute
            .sample_times()
            .into_iter()
            .map(|time| {
                let value = attribute.get(Time::at(time)).map(|r| encode(&r.value));
                json!([time, value])
            })
            .collect();
        attributes.insert(
            property.name().to_string(),
            json!({
                "default": attribute.get(Time::Default).map(|r| encode(&r.value)),
                "samples": samples,
                "customData": resolved_dictionary(property.metadata("customData").map(|r| r.value)),
            }),
        );
    }
    json!({
        "customData": resolved_dictionary(prim.metadata("customData").map(|r| r.value)),
        "assetInfo": resolved_dictionary(prim.metadata("assetInfo").map(|r| r.value)),
        "attributes": attributes,
    })
}

/// A value of the oracle's `encode`.
fn decode(value: &Json) -> Value {
    match value {
        Json::Object(map) => {
            let (kind, inner) = map.iter().next().expect("a tagged value");
            match kind.as_str() {
                "timecode" => Value::TimeCode(inner.as_f64().unwrap()),
                "duration" => Value::Duration(inner.as_f64().unwrap()),
                "pathExpression" => Value::PathExpression(inner.as_str().unwrap().into()),
                "asset" => Value::Asset(inner.as_str().unwrap().into()),
                "dictionary" => Value::Dictionary(
                    inner
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(key, item)| (Arc::from(key.as_str()), decode(item).into()))
                        .collect(),
                ),
                other => panic!("no value kind {other}"),
            }
        }
        Json::Array(items) => {
            let items: Vec<Value> = items.iter().map(decode).collect();
            let element = items.first().cloned();
            Value::array_from_iter(items, element.as_ref())
        }
        other => panic!("no value for {other}"),
    }
}

/// The edit target a case authors through, as the oracle builds OpenUSD's.
fn edit_target(loaded: &LoadedStage, stage: &Stage, prim: PathId, kind: &str) -> EditTarget {
    let store = &loaded.store;
    let sub = layer_named(loaded, "sub.usda");
    let arc = match kind {
        "root" => return EditTarget::for_layer(loaded.root_layer),
        "layer" => return EditTarget::for_layer(sub),
        "local_direct_variant" => {
            let variant = SpecPath::parse("/Rock{look=a}", store.tokens(), store.paths())
                .expect("a variant path");
            return EditTarget::for_local_variant(loaded.root_layer, &variant);
        }
        "local_layer" => ArcKind::Local,
        "reference" => ArcKind::References,
        "variant" => ArcKind::Variants,
        "inherit" => ArcKind::Inherits,
        other => panic!("no edit target {other}"),
    };
    let graph = stage.prim(prim, store).expect("a prim").composition_graph();
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| graph.node(*id).is_some_and(|n| n.arc_kind() == arc))
        .expect("a node of the arc");
    if kind == "local_layer" {
        return EditTarget::for_node_layer(stage, store, prim, node, sub).expect("an edit target");
    }
    EditTarget::for_node(stage, prim, node).expect("an edit target")
}

/// Makes one of the oracle's calls through `target`; whether it applied.
fn author(
    loaded: &mut LoadedStage,
    live: &mut LiveStage,
    target: &EditTarget,
    prim: PathId,
    call: &Json,
) -> bool {
    let kind = call["call"].as_str().unwrap();
    let name = call["name"].as_str().unwrap();
    let value = decode(&call["value"]);
    if kind == "create" {
        let ty = PropertyType::parse(call["key"].as_str().unwrap()).expect("a type");
        let property = loaded.store.property_path(&format!(
            "{}.{name}",
            loaded
                .store
                .paths()
                .resolve(prim)
                .display(loaded.store.tokens())
        ));
        let mut transaction = Transaction::new();
        transaction.create_property(
            target.property(property),
            PropertySpec::typed_attribute(ty).custom(),
        );
        transaction.set_default(target.property(property), value);
        return live.apply(&mut loaded.store, &transaction).is_ok();
    }
    let mut edit = SchemaEdit::new(live.stage(), &mut loaded.store, target.clone());
    let handle = PrimEdit::new(&edit, prim).unwrap().expect("a prim");
    let accepted = match kind {
        "set" => {
            match call["key"].as_f64() {
                Some(time) => handle.write_value_at(&mut edit, name, time, value),
                None => handle.write_value(&mut edit, name, value),
            }
            true
        }
        "prim_key" => {
            let key = call["key"].as_str().unwrap();
            handle
                .write_metadata_by_key(&mut edit, name, key, value)
                .is_ok()
        }
        "prim_field" => handle.write_metadata(&mut edit, name, value).is_ok(),
        "attr_key" => {
            let key = call["key"].as_str().unwrap();
            handle
                .write_property_metadata_by_key(&mut edit, name, "customData", key, value)
                .is_ok()
        }
        "attr_field" => {
            let field = call["key"].as_str().unwrap();
            handle
                .write_property_metadata(&mut edit, name, field, value)
                .is_ok()
        }
        other => panic!("no call {other}"),
    };
    let transaction = edit.finish();
    accepted && live.apply(&mut loaded.store, &transaction).is_ok()
}

fn assert_same(actual: &Json, expected: &Json, what: &str) {
    assert!(
        actual == expected,
        "{what}:\n  layerstack {}\n  OpenUSD    {}",
        serde_json::to_string(actual).unwrap(),
        serde_json::to_string(expected).unwrap()
    );
}

#[test]
fn values_read_through_arcs_and_offsets_as_openusd_reads_them() {
    let oracle = oracle();
    let mut loaded = load();
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    for path in PRIMS {
        assert_same(
            &prim_record(&loaded.store, &stage, path),
            &oracle["read"][path],
            path,
        );
    }
}

#[test]
fn flattening_writes_the_values_openusd_writes() {
    let oracle = oracle();
    let mut loaded = load();
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let flat = stage
        .flatten(
            &mut loaded.store,
            loaded.root_layer,
            LayerId(999_999),
            &FlattenRequirements::default(),
        )
        .expect("flattens");
    let text = layerstack_usda::save_usda(&flat.layer, loaded.store.tokens(), loaded.store.paths())
        .expect("saves");
    assert_eq!(text, oracle["flatten"].as_str().unwrap());
}

#[test]
fn values_authored_through_edit_targets_agree_with_openusd_call_by_call() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 11, "every case is recorded");
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let differs = DIFFERING.contains(&name);
        // A differing case panics, which aborts where panics cannot unwind
        // (WASI), so it runs only where its panic can be caught.
        if differs && cfg!(panic = "abort") {
            continue;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| replay(case)));
        if result.is_ok() && differs {
            failures.push(format!("{name}: agrees, so it leaves `DIFFERING`"));
        }
        if let (Err(error), false) = (result, differs) {
            let message = error
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            failures.push(format!("{name}: {message}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases:\n\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}

fn replay(case: &Json) {
    let mut loaded = load();
    let prim_text = case["prim"].as_str().unwrap();
    let prim = loaded.store.path(prim_text);
    let mut live = LiveStage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let target = edit_target(
        &loaded,
        live.stage(),
        prim,
        case["target"].as_str().unwrap(),
    );
    let accepted: Vec<bool> = case["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|call| author(&mut loaded, &mut live, &target, prim, call))
        .collect();
    assert_eq!(json!(accepted), case["accepted"], "accepted");
    let layer = layer_named(&loaded, case["layer"].as_str().unwrap());
    let store = &loaded.store;
    let text =
        layerstack_usda::save_usda(store.layer(layer).unwrap(), store.tokens(), store.paths())
            .expect("saves");
    assert_eq!(text, case["text"].as_str().unwrap(), "layer text");
    assert_same(
        &prim_record(&loaded.store, live.stage(), prim_text),
        &case["read"],
        "read back",
    );
    let fresh = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert_same(
        &prim_record(&loaded.store, live.stage(), prim_text),
        &prim_record(&loaded.store, &fresh, prim_text),
        "the live stage recomposes",
    );
}
