// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A `Transaction` plans each edit against the prim definition its earlier
//! edits leave, as OpenUSD 26.11 recomposes after each authoring call:
//! defining or retyping a prim, or applying a schema to it, then setting
//! one of that schema's properties works in one transaction.
//!
//! `scripts/transaction_schemas_oracle.py` writes
//! `fixtures/transaction_schemas` and records, call by call, whether
//! OpenUSD accepts each call, every layer's exported text after it, and
//! what OpenUSD reads back of the prim and the property. Here call `k` of a
//! case is replayed as one transaction of the calls before it that OpenUSD
//! accepted followed by call `k`, on fresh layers: an accepted call must
//! apply and leave every layer's text byte for byte, and the same read
//! back; a refused one must reject the transaction at that call, with
//! nothing authored.
//!
//! Spec: AOUSD Core §12.2 (strongest opinion), §13.3.2.3 (the prim
//! definition declares the prim's schema properties). OpenUSD:
//! `UsdStage::_CreateAttributeSpecForEditing`, `UsdStage::_SetValue`,
//! `UsdRelationship::_CreateSpec`.

use std::sync::Arc;

use layerstack::{
    ArcKind, EditError, EditTarget, LayerId, ListPosition, LiveStage, PrimField, Rejection,
    Specifier, StageOptions, Time, Transaction, Value, Variability,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::{SchemaEdit, usd_geom::Sphere};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/transaction_schemas/oracle.json");
const LAYERS: [&str; 3] = ["scene.usda", "weak.usda", "asset.usda"];

/// The case OpenUSD accepts and layerstack refuses: layerstack's variant
/// specs hold no type name (`SdfVariantSpec::GetPrimSpec` holds one in
/// OpenUSD, though USDA cannot write it), so retyping a prim through a
/// variant edit target is refused ([`Rejection::NotOnVariant`]).
const VARIANT_TYPE: &str = "variant_type";

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    assert_eq!(
        oracle["openusd_version"].as_str().unwrap(),
        layerstack_schemas::OPENUSD_VERSION,
        "the oracle is from the OpenUSD the schemas come from"
    );
    oracle
}

fn load() -> (LoadedStage, LiveStage) {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/transaction_schemas/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(loaded.store.tokens()))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options);
    (loaded, live)
}

fn layer_named(loaded: &LoadedStage, name: &str) -> LayerId {
    *loaded
        .layer_names
        .iter()
        .find(|(_, n)| n.ends_with(name))
        .unwrap_or_else(|| panic!("no layer {name}"))
        .0
}

/// The layer as OpenUSD's `SdfLayer::ExportToString` writes it.
fn text(loaded: &LoadedStage, name: &str) -> String {
    let store = &loaded.store;
    let layer = store.layer(layer_named(loaded, name)).unwrap();
    layerstack_usda::save_usda(layer, store.tokens(), store.paths()).expect("saves")
}

fn texts(loaded: &LoadedStage) -> Json {
    LAYERS
        .iter()
        .map(|name| (name.to_string(), json!(text(loaded, name))))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// The edit target a case names: a layer, or the reference or variant node
/// of a prim's index.
fn edit_target(loaded: &mut LoadedStage, live: &LiveStage, target: &str) -> EditTarget {
    let node = |loaded: &mut LoadedStage, path: &str, kind: ArcKind| {
        let prim = loaded.store.path(path);
        let graph = live
            .stage()
            .prim(prim, &loaded.store)
            .unwrap()
            .composition_graph();
        let (node, _) = graph
            .nodes()
            .find(|(_, node)| node.arc_kind() == kind)
            .unwrap_or_else(|| panic!("no {kind:?} node of {path}"));
        EditTarget::for_node(live.stage(), prim, node).unwrap()
    };
    if let Some(path) = target.strip_prefix("ref:") {
        node(loaded, path, ArcKind::References)
    } else if let Some(path) = target.strip_prefix("variant:") {
        node(loaded, path, ArcKind::Variants)
    } else {
        EditTarget::for_layer(layer_named(loaded, target))
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the oracle's `float` values are `f32` values written as JSON numbers"
)]
fn value(loaded: &LoadedStage, value: &Json) -> Value {
    let (kind, value) = value.as_object().unwrap().iter().next().unwrap();
    match kind.as_str() {
        "double" => Value::Double(value.as_f64().unwrap()),
        "float" => Value::Float(value.as_f64().unwrap() as f32),
        "token" => Value::Token(loaded.store.tokens().intern(value.as_str().unwrap())),
        "string" => Value::String(Arc::from(value.as_str().unwrap())),
        other => panic!("no value of type {other}"),
    }
}

/// Adds the transaction edit `call` makes through `target`.
fn push(edit: &mut Transaction, loaded: &LoadedStage, target: &EditTarget, call: &Json) {
    let call = call.as_array().unwrap();
    let kind = call[0].as_str().unwrap();
    let path = call[1].as_str().unwrap();
    let store = &loaded.store;
    let token = |at: usize| store.tokens().intern(call[at].as_str().unwrap());
    match kind {
        "define" => {
            edit.create_prim(
                target.prim(store.path(path)),
                Specifier::Def,
                Some(token(2)),
            );
        }
        "set_type" => {
            edit.set_type_name(target.prim(store.path(path)), token(2));
        }
        "clear_type" => {
            edit.clear_type_name(target.prim(store.path(path)));
        }
        "apply" => {
            edit.add_applied_schema(target.prim(store.path(path)), token(2));
        }
        "unapply" => {
            edit.remove_applied_schema(target.prim(store.path(path)), token(2));
        }
        "set" => {
            let at = target.property(store.property_path(path));
            edit.set_default(at, value(loaded, &call[2]));
        }
        "sample" => {
            let at = target.property(store.property_path(path));
            edit.set_time_sample(at, call[2].as_f64().unwrap(), value(loaded, &call[3]));
        }
        "add_target" => {
            let at = target.property(store.property_path(path));
            let to = store.target_path(call[2].as_str().unwrap());
            edit.add_target(at, to, ListPosition::BackOfPrepend);
        }
        other => panic!("unknown call {other}"),
    }
}

fn plain(loaded: &LoadedStage, value: &Value) -> Json {
    match value {
        Value::Double(v) => json!(v),
        Value::Float(v) => json!(f64::from(*v)),
        Value::Token(token) => json!(loaded.store.tokens().resolve(*token)),
        other => panic!("no JSON rendering for {other:?}"),
    }
}

/// What layerstack reads back of the prim and property `call` edits, as
/// the oracle records OpenUSD's.
fn read_back(loaded: &LoadedStage, live: &LiveStage, call: &Json) -> Json {
    let call = call.as_array().unwrap();
    let kind = call[0].as_str().unwrap();
    let path = call[1].as_str().unwrap();
    let store = &loaded.store;
    let prim_text = path.split_once('.').map_or(path, |(prim, _)| prim);
    let prim = live
        .stage()
        .prim(store.path(prim_text), store)
        .expect("the prim composes");
    let applied: Vec<&str> = prim.definition().map_or_else(Vec::new, |definition| {
        definition
            .applied_schemas()
            .iter()
            .map(|schema| store.tokens().resolve(schema.name))
            .collect()
    });
    let mut record = json!({
        "type_name": prim.type_name().unwrap_or(""),
        "applied": applied,
    });
    let view = live.view(store);
    match kind {
        "set" | "sample" => {
            let time = match kind {
                "sample" => Time::at(call[2].as_f64().unwrap()),
                _ => Time::Default,
            };
            record["attribute"] =
                view.attribute(store.property_path(path))
                    .map_or(Json::Null, |attribute| {
                        json!({
                            "type": attribute.type_name().unwrap_or(""),
                            "variability": match attribute.variability() {
                                Variability::Uniform => "uniform",
                                _ => "varying",
                            },
                            "custom": attribute.is_custom(),
                            "value": attribute
                                .get(time)
                                .map_or(Json::Null, |value| plain(loaded, &value.value)),
                        })
                    });
        }
        "add_target" => {
            record["relationship"] =
                view.relationship(store.property_path(path))
                    .map_or(Json::Null, |relationship| {
                        let targets: Vec<String> = relationship
                            .targets()
                            .iter()
                            .map(|target| target.display(store.paths(), store.tokens()))
                            .collect();
                        json!({ "custom": relationship.is_custom(), "targets": targets })
                    });
        }
        _ => {}
    }
    record
}

#[test]
fn the_layers_are_the_fixtures() {
    let oracle = oracle();
    let (loaded, _) = load();
    assert_eq!(texts(&loaded), oracle["before"]);
}

#[test]
fn transactions_plan_against_earlier_schema_edits_as_openusd_call_by_call() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 15, "every case is recorded");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let calls = case["calls"].as_array().unwrap();
        if name == VARIANT_TYPE {
            let (mut loaded, mut live) = load();
            let target = edit_target(&mut loaded, &live, case["target"].as_str().unwrap());
            let mut edit = Transaction::new();
            push(&mut edit, &loaded, &target, &calls[0]["call"]);
            assert!(calls[0]["accepted"].as_bool().unwrap());
            assert_eq!(calls[0]["type_name"], "Sphere");
            assert!(
                matches!(
                    live.apply(&mut loaded.store, &edit),
                    Err(EditError::Rejected {
                        op: 0,
                        reason: Rejection::NotOnVariant(PrimField::TypeName),
                    })
                ),
                "{name} still differs from OpenUSD"
            );
            continue;
        }
        for (k, record) in calls.iter().enumerate() {
            let label = format!("{name} call {k}: {}", record["call"]);
            let (mut loaded, mut live) = load();
            let target = edit_target(&mut loaded, &live, case["target"].as_str().unwrap());
            let mut edit = Transaction::new();
            for earlier in calls[..k]
                .iter()
                .filter(|earlier| earlier["accepted"].as_bool().unwrap())
            {
                push(&mut edit, &loaded, &target, &earlier["call"]);
            }
            push(&mut edit, &loaded, &target, &record["call"]);
            let last = edit.len() - 1;
            let before = texts(&loaded);
            let applied = live.apply(&mut loaded.store, &edit);
            if record["accepted"].as_bool().unwrap() {
                applied.unwrap_or_else(|e| panic!("{label}: {e}"));
                assert_eq!(texts(&loaded), record["texts"], "{label}: layer text");
                let mut want = record.clone();
                let want = want.as_object_mut().unwrap();
                for key in ["call", "accepted", "texts"] {
                    want.remove(key);
                }
                assert_eq!(
                    read_back(&loaded, &live, &record["call"]),
                    Json::Object(want.clone()),
                    "{label}: read back"
                );
                continue;
            }
            // OpenUSD authors nothing for a refused call: the layers are as
            // the accepted calls before it left them.
            let accepted_before = calls[..k]
                .iter()
                .rev()
                .find(|earlier| earlier["accepted"].as_bool().unwrap())
                .map_or(&oracle["before"], |earlier| &earlier["texts"]);
            assert_eq!(
                &record["texts"], accepted_before,
                "{label}: OpenUSD authored"
            );
            match applied {
                Err(EditError::Rejected { op, reason }) => {
                    assert_eq!(op, last, "{label}");
                    let undeclared = record["attribute"].is_null();
                    assert!(
                        match reason {
                            Rejection::UndeclaredType(_) => undeclared,
                            Rejection::TypeMismatch { .. } => !undeclared,
                            _ => false,
                        },
                        "{label}: {reason:?}"
                    );
                }
                other => panic!("{label}: {other:?}"),
            }
            assert_eq!(texts(&loaded), before, "{label}: rolled back");
        }
    }
}

/// A `SchemaEdit` that defines a prim and sets one of its type's
/// attributes authors what the same calls author in OpenUSD, as the raw
/// transaction does (`UsdGeomSphere::Define`, then
/// `CreateRadiusAttr().Set`, which passes the schema's type itself).
#[test]
fn schema_edit_defines_then_sets_as_openusd() {
    let oracle = oracle();
    let case = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "define_then_set")
        .unwrap();
    let (mut loaded, mut live) = load();
    let root = layer_named(&loaded, "scene.usda");
    let path = loaded.store.path("/New");
    let mut edit = SchemaEdit::new(live.stage(), &mut loaded.store, EditTarget::for_layer(root));
    let sphere = Sphere::define(&mut edit, path).expect("outside any instance");
    sphere.set_radius(&mut edit, 2.0);
    let transaction = edit.finish();
    live.apply(&mut loaded.store, &transaction).unwrap();
    assert_eq!(texts(&loaded), case["calls"][1]["texts"]);
}
