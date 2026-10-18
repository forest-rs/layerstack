// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Edits addressed to the pseudo-root agree with OpenUSD 26.08's authoring
//! calls, call by call, or differ for a reason named here.
//!
//! `scripts/pseudo_root_edits_oracle.py` makes, on a fresh copy of
//! `fixtures/pseudo_root_edits/scene.usda`, the OpenUSD call matching each
//! `Transaction` edit addressed to `/` (or to the property `/.height`), at
//! the Usd level (the stage's pseudo-root `UsdPrim`) and at the Sdf level
//! (the layer's pseudo-root spec). It records whether each call was
//! accepted and what it left. Here each edit is one transaction applied
//! through a `LiveStage`.
//!
//! layerstack accepts the root prim order (`set_prim_order`,
//! `clear_prim_order`), as OpenUSD does, and must leave the same order and
//! compose the same root prims. Every other edit is rejected with
//! `Rejection::PseudoRoot` and changes nothing. Where OpenUSD accepts the
//! call instead, the case is listed in `DELIBERATE` with the reason, which
//! the oracle's record must bear out:
//!
//! - `LayerField`: OpenUSD writes a field registered on layers as the
//!   layer's metadata. layerstack keeps one edit for layer metadata,
//!   `Transaction::set_layer_metadata` (and `clear_layer_metadata`), which
//!   must leave what OpenUSD left;
//! - `Unregistered`: OpenUSD writes a prim field or property the
//!   pseudo-root spec type does not register, which the layer's other
//!   readers and writers do not expect (some of these layers OpenUSD's
//!   own text reader rejects);
//! - `NoOp`: OpenUSD reports success and changes nothing: it returns the
//!   existing pseudo-root, or clears a spec that cannot exist.
//!
//! OpenUSD's `SdfAttributeSpec::New` rejects an attribute on the
//! pseudo-root only while no property path of that name exists yet in the
//! process; the fixture's `/Garden.height` makes it accept `height`. Its
//! `UsdRelationship::SetTargets` and `ClearTargets` write the pseudo-root's
//! property list before failing; layerstack rejects without writing.
//!
//! Spec: AOUSD Core §5.1 (the pseudo-root), §7.6.1 (layer metadata), §7.6.2
//! (prim spec fields).

#![allow(missing_docs, reason = "integration tests")]

use layerstack::edit::{EditError, Rejection};
use layerstack::{
    EditTarget, FieldValue, InMemoryStore, LayerId, ListOp, ListPosition, LiveStage, PathId,
    PropertyPath, PropertySpec, PropertyType, Reference, Specifier, StageOptions, TargetPath,
    TokenId, Transaction, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../fixtures/pseudo_root_edits/oracle.json");

#[derive(Clone, Copy, Debug, PartialEq)]
enum Deviation {
    LayerField,
    Unregistered,
    NoOp,
}

use Deviation::{LayerField, NoOp, Unregistered};

/// Calls OpenUSD accepts on the pseudo-root and layerstack rejects.
const DELIBERATE: &[(&str, Deviation)] = &[
    ("create_prim", NoOp),
    ("copy_spec_to_pseudo_root", Unregistered),
    ("set_metadata_documentation", LayerField),
    ("set_metadata_start_time_code", LayerField),
    ("clear_metadata_documentation", LayerField),
    ("remove_variant_set", NoOp),
    ("set_variant_set_names", Unregistered),
    ("add_reference", Unregistered),
    ("add_payload", Unregistered),
    ("create_attribute", Unregistered),
    ("create_relationship", Unregistered),
    ("clear_default", NoOp),
    ("remove_time_sample", NoOp),
    ("clear_property_metadata", NoOp),
];

/// Edits layerstack accepts on the pseudo-root.
const ACCEPTED: &[&str] = &["set_prim_order", "clear_prim_order"];

fn load() -> LoadedStage {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/pseudo_root_edits/scene.usda");
    let loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

fn token(loaded: &mut LoadedStage, name: &str) -> TokenId {
    loaded.store.tokens.intern(name)
}

fn names(loaded: &mut LoadedStage, value: &Json) -> Vec<TokenId> {
    let items = value.as_array().unwrap();
    items
        .iter()
        .map(|name| token(loaded, name.as_str().unwrap()))
        .collect()
}

/// The value a `set_metadata` edit names.
fn metadata_value(loaded: &mut LoadedStage, edit: &Json) -> FieldValue {
    let value = if let Some(text) = edit["string"].as_str() {
        Value::String(text.into())
    } else if let Some(number) = edit["double"].as_f64() {
        Value::Double(number)
    } else if let Some(name) = edit["token"].as_str() {
        Value::Token(token(loaded, name))
    } else {
        Value::Bool(edit["bool"].as_bool().unwrap())
    };
    FieldValue::Value(value)
}

/// The transaction making the case's edit on the root layer.
fn transaction(loaded: &mut LoadedStage, edit: &Json) -> Transaction {
    let layer = EditTarget::for_layer(loaded.root_layer);
    let root = loaded.store.path("/");
    let (rock, at) = (loaded.store.path("/Rock"), layer.prim(root));
    let height = token(loaded, "height");
    let prop = layer.property(PropertyPath::new(root, height));
    let (season, schema) = (
        token(loaded, "season"),
        token(loaded, "CollectionAPI:plants"),
    );
    let double = PropertyType::parse("double").unwrap();
    let internal = Reference::new(loaded.root_layer, rock);
    let mut txn = Transaction::new();
    match edit["op"].as_str().unwrap() {
        "create_prim" => txn.create_prim(at, Specifier::Def, None),
        "remove_spec" if edit["property"].as_bool() == Some(true) => txn.remove_spec(prop),
        "remove_spec" => txn.remove_spec(at),
        "copy_spec" => {
            let from = loaded.store.path(edit["from"].as_str().unwrap());
            let to = loaded.store.path(edit["to"].as_str().unwrap());
            txn.copy_spec(layer.prim(from), layer.prim(to))
        }
        "set_metadata" => {
            let key = token(loaded, edit["key"].as_str().unwrap());
            let value = metadata_value(loaded, edit);
            txn.set_metadata(at, key, value)
        }
        "clear_metadata" => {
            let key = token(loaded, edit["key"].as_str().unwrap());
            txn.clear_metadata(at, key)
        }
        "set_variant_selection" => {
            let variant = edit["variant"].as_str().map(|v| token(loaded, v));
            txn.set_variant_selection(at, season, variant)
        }
        "add_variant_set" => txn.add_variant_set(at, season, ListPosition::BackOfPrepend),
        "remove_variant_set" => txn.remove_variant_set(at, season),
        "set_variant_set_names" => {
            let mut list = ListOp::default();
            list.prepend.push(season);
            txn.set_variant_set_names(at, list)
        }
        "add_applied_schema" => txn.add_applied_schema(at, schema),
        "remove_applied_schema" => txn.remove_applied_schema(at, schema),
        "set_specifier" => txn.set_specifier(at, Specifier::Over),
        "set_type_name" => {
            let scope = token(loaded, "Scope");
            txn.set_type_name(at, scope)
        }
        "clear_type_name" => txn.clear_type_name(at),
        "set_active" => txn.set_active(at, false),
        "clear_active" => txn.clear_active(at),
        "set_instanceable" => txn.set_instanceable(at, true),
        "set_property_order" => txn.set_property_order(at, vec![height]),
        "set_prim_order" => {
            let order = names(loaded, &edit["order"]);
            txn.set_prim_order(at, order)
        }
        "clear_prim_order" => txn.clear_prim_order(at),
        "add_reference" => txn.add_reference(at, internal, ListPosition::FrontOfPrepend),
        "set_references" => txn.set_references(at, ListOp::explicit(vec![internal])),
        "add_payload" => txn.add_payload(at, internal, ListPosition::FrontOfPrepend),
        "add_inherit" => txn.add_inherit(at, rock, ListPosition::FrontOfPrepend),
        "add_specialize" => txn.add_specialize(at, rock, ListPosition::FrontOfPrepend),
        "create_attribute" => txn.create_property(prop, PropertySpec::typed_attribute(double)),
        "create_relationship" => txn.create_property(prop, PropertySpec::relationship()),
        "set_default" => txn.set_default(prop, Value::Double(2.0)),
        "set_time_sample" => txn.set_time_sample(prop, 12.0, Value::Double(2.0)),
        "block_attribute" => txn.block_attribute(prop),
        "clear_default" => txn.clear_default(prop),
        "remove_time_sample" => txn.remove_time_sample(prop, 12.0),
        "set_property_metadata" => {
            let key = token(loaded, "documentation");
            txn.set_metadata(
                prop,
                key,
                FieldValue::Value(Value::String("How tall".into())),
            )
        }
        "clear_property_metadata" => {
            let key = token(loaded, "documentation");
            txn.clear_metadata(prop, key)
        }
        "set_targets" => {
            let target = TargetPath::Prim(rock);
            txn.set_targets(prop, ListOp::explicit(vec![target]))
        }
        "clear_targets" => txn.clear_targets(prop),
        other => panic!("no edit {other}"),
    };
    txn
}

/// The root layer's root prim order, as the oracle records it.
fn root_prim_order(store: &InMemoryStore, layer: LayerId, root: PathId) -> Vec<String> {
    let order = store.layers[&layer]
        .prim(root)
        .and_then(|spec| spec.prim_order.as_ref());
    order
        .into_iter()
        .flatten()
        .map(|name| store.tokens.resolve(*name).to_string())
        .collect()
}

/// The composed root prims, in order.
fn root_prims(live: &LiveStage, store: &InMemoryStore, root: PathId) -> Vec<String> {
    let children = live.stage().children_of(root).unwrap_or_default();
    children
        .iter()
        .map(|path| {
            let name = store.paths.resolve(*path).leaf().unwrap();
            store.tokens.resolve(name).to_string()
        })
        .collect()
}

/// The layer metadata field `key` of `layer`, as the oracle records it.
fn layer_field(store: &InMemoryStore, layer: LayerId, key: &str) -> Json {
    let key = store.tokens.lookup(key).expect("an interned key");
    match store.layers[&layer].metadata(key) {
        None => Json::Null,
        Some(FieldValue::Value(Value::String(text))) => Json::from(&**text),
        Some(FieldValue::Value(Value::Double(number))) => Json::from(*number),
        Some(other) => panic!("unexpected layer field {other:?}"),
    }
}

/// Checks that a level's record bears out `deviation`.
fn check_deviation(name: &str, level: &str, record: &Json, deviation: Deviation, case: &Json) {
    match deviation {
        LayerField => {
            assert!(record["changed"].as_bool().unwrap(), "{name} {level}");
            assert!(record["reads_back"].as_bool().unwrap(), "{name} {level}");
            assert!(
                case["edit"]["key"].is_string(),
                "{name}: a layer field names a key"
            );
        }
        Unregistered => {
            let fields = record["prim_fields"].as_array();
            assert!(
                fields.is_some_and(|fields| !fields.is_empty()),
                "{name} {level}: OpenUSD left no prim field on the pseudo-root"
            );
        }
        NoOp => assert!(!record["changed"].as_bool().unwrap(), "{name} {level}"),
    }
}

/// Makes the layer metadata edit standing for a `LayerField` case, and
/// checks it leaves the field OpenUSD left.
fn check_layer_field(case: &Json) {
    let name = case["name"].as_str().unwrap();
    let edit = &case["edit"];
    let mut loaded = load();
    let layer = loaded.root_layer;
    let key = edit["key"].as_str().unwrap();
    let key_token = token(&mut loaded, key);
    let mut txn = Transaction::new();
    match edit["op"].as_str().unwrap() {
        "set_metadata" => {
            let value = metadata_value(&mut loaded, edit);
            txn.set_layer_metadata(layer, key_token, value)
        }
        "clear_metadata" => txn.clear_layer_metadata(layer, key_token),
        other => panic!("{name}: no layer edit for {other}"),
    };
    let mut live = LiveStage::compose(&mut loaded.store, layer, StageOptions::default());
    live.apply(&mut loaded.store, &txn)
        .unwrap_or_else(|e| panic!("{name}: the layer edit is rejected: {e}"));
    for level in ["usd", "sdf"] {
        assert_eq!(
            layer_field(&loaded.store, layer, key),
            case[level]["field"],
            "{name}: {level} layer field"
        );
    }
}

#[test]
fn pseudo_root_edits_agree_with_openusd() {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    let cases = oracle["cases"].as_array().unwrap();
    for (name, _) in DELIBERATE {
        assert!(
            cases.iter().any(|c| c["name"] == *name),
            "DELIBERATE names no case {name}"
        );
    }
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load();
        let (layer, root) = (loaded.root_layer, loaded.store.path("/"));
        let txn = transaction(&mut loaded, &case["edit"]);
        let mut live = LiveStage::compose(&mut loaded.store, layer, StageOptions::default());
        let before = loaded.store.layers[&layer].clone();
        let result = live.apply(&mut loaded.store, &txn);
        let accepted = result.is_ok();
        let accepts = ACCEPTED.contains(&name);
        match &result {
            Ok(_) => assert!(accepts, "{name}: accepted"),
            Err(EditError::Rejected {
                reason: Rejection::PseudoRoot,
                ..
            }) => assert!(!accepts, "{name}: rejected"),
            Err(other) => panic!("{name}: {other:?}, not `Rejection::PseudoRoot`"),
        }
        let deviation = DELIBERATE.iter().find(|(n, _)| *n == name).map(|(_, d)| *d);
        let mut differs = false;
        for level in ["usd", "sdf"] {
            let record = &case[level];
            if record.is_null() {
                continue;
            }
            if record["accepted"].as_bool().unwrap() == accepted {
                continue;
            }
            differs = true;
            let deviation =
                deviation.unwrap_or_else(|| panic!("{name}: {level} accepts it: {accepted}"));
            assert!(!accepted, "{name}: a deliberate difference rejects");
            check_deviation(name, level, record, deviation, case);
        }
        assert_eq!(
            differs,
            deviation.is_some(),
            "{name}: DELIBERATE lists it while OpenUSD agrees"
        );
        if deviation == Some(LayerField) {
            check_layer_field(case);
        }

        let Ok(applied) = result else {
            assert_eq!(
                loaded.store.layers[&layer], before,
                "{name}: a rejection changes nothing"
            );
            continue;
        };
        // What the accepted edit leaves: OpenUSD's root prim order, and the
        // root prims it composes, live and on a fresh stage.
        for level in ["usd", "sdf"] {
            let expected = &case[level]["root_prim_order"];
            assert_eq!(
                Json::from(root_prim_order(&loaded.store, layer, root)),
                *expected,
                "{name}: {level} root prim order"
            );
        }
        let composed = root_prims(&live, &loaded.store, root);
        assert_eq!(
            Json::from(composed.clone()),
            case["usd"]["root_prims"],
            "{name}"
        );
        let fresh = LiveStage::compose(&mut loaded.store, layer, StageOptions::default());
        assert_eq!(root_prims(&fresh, &loaded.store, root), composed, "{name}");
        let saved = layerstack_usda::save::save_usda(
            &loaded.store.layers[&layer],
            &loaded.store.tokens,
            &loaded.store.paths,
        );
        assert!(saved.is_ok(), "{name}: {saved:?}");
        live.apply(&mut loaded.store, &applied.inverse).unwrap();
        assert_eq!(
            loaded.store.layers[&layer].prims(),
            before.prims(),
            "{name}: undone"
        );
    }
}
