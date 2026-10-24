// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A layer saves as USDA in the order OpenUSD 26.08's text writer gives it,
//! whatever order it was authored or read in, byte for byte.
//!
//! `scripts/usda_field_order_oracle.py` authors layers step by step in
//! deliberately scrambled orders through OpenUSD's `Sdf` API and records
//! each layer's `ExportToString` (see the script for every step). Here
//! each step is one `Transaction` on an empty layer; the saved USDA must be
//! OpenUSD's text. OpenUSD sorts metadata fields, properties, variants and
//! dictionary entries, and keeps child prims, variant sets, list items and
//! targets in their order (`pxr/usd/sdf/fileIO_Common.h`,
//! `usdaFileFormat.cpp`).
//!
//! `scrambled.usda`, a file written out of order, and `scrambled.usdc`,
//! OpenUSD's crate file of it (which keeps the file's order), must both
//! save as OpenUSD exports them, and so must `unregistered.usda`, whose
//! unregistered metadata sorts in byte order on the layer and in
//! dictionary order on a prim.
//!
//! Spec: AOUSD Core §6.6.2 (dictionaries are unordered), §8.2 (element
//! ordering), §11.3.2 (property order is the sorted names, then
//! `propertyOrder`), §16.2 (USDA).

#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::doc::{FieldValue, Layer, LayerId, Reference, Specifier, Value};
use layerstack::listop::ListOp;
use layerstack::path::TargetPath;
use layerstack::property::{PropertySpec, PropertyType};
use layerstack::spec_path::SpecPath;
use layerstack::{Address, InMemoryStore, LayerOffset, ListPosition, Transaction};
use layerstack_conformance::save_corpus::Imported;
use layerstack_conformance::workspace_root;
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../fixtures/usda_field_order/oracle.json");
const SCENE: LayerId = LayerId(1);
const ASSET: LayerId = LayerId(2);

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

/// A step value as the layer holds it.
fn value(store: &InMemoryStore, json: &Json) -> Value {
    let item = &json["value"];
    #[allow(clippy::cast_possible_truncation, reason = "`float` values")]
    let float = |v: &Json| v.as_f64().unwrap() as f32;
    let floats = |v: &Json| v.as_array().unwrap().iter().map(float).collect::<Vec<_>>();
    match json["type"].as_str().unwrap() {
        "bool" => Value::Bool(item.as_bool().unwrap()),
        "int" => Value::Int(i32::try_from(item.as_i64().unwrap()).unwrap()),
        "float" => Value::Float(float(item)),
        "double" => Value::Double(item.as_f64().unwrap()),
        "token" => Value::Token(store.tokens.intern(item.as_str().unwrap())),
        "string" => Value::String(Arc::from(item.as_str().unwrap())),
        "point3f[]" => Value::Array(
            item.as_array()
                .unwrap()
                .iter()
                .map(|v| Value::Vec3f(floats(v).try_into().unwrap()))
                .collect(),
        ),
        "texCoord2f[]" => Value::Array(
            item.as_array()
                .unwrap()
                .iter()
                .map(|v| Value::Vec2f(floats(v).try_into().unwrap()))
                .collect(),
        ),
        "dictionary" => Value::Dictionary(
            item.as_object()
                .unwrap()
                .iter()
                .map(|(key, entry)| (Arc::from(key.as_str()), value(store, entry)))
                .collect(),
        ),
        other => panic!("no value type {other}"),
    }
}

/// The step's call as a transaction on the scene layer.
fn transaction(store: &InMemoryStore, step: &Json) -> Transaction {
    let text = |key: &str| step[key].as_str().unwrap();
    let token = |key: &str| store.tokens.intern(text(key));
    let path = text("path");
    let at = || {
        let spec = SpecPath::parse(path, &store.tokens, &store.paths).expect("a spec path");
        Address::spec(SCENE, spec)
    };
    let target = |text: &str| TargetPath::parse(text, &store.tokens, &store.paths).unwrap();
    let targets = || {
        let items = step["targets"].as_array().unwrap();
        ListOp::explicit(items.iter().map(|t| target(t.as_str().unwrap())).collect())
    };
    let names = || {
        let items = step["names"].as_array().unwrap();
        items
            .iter()
            .map(|n| store.tokens.intern(n.as_str().unwrap()))
            .collect()
    };
    let mut edit = Transaction::new();
    match text("op") {
        "prim" => {
            let specifier = match step["specifier"].as_str().unwrap_or("over") {
                "def" => Specifier::Def,
                "over" => Specifier::Over,
                "class" => Specifier::Class,
                other => panic!("no specifier {other}"),
            };
            let type_name = step["type"].as_str().map(|t| store.tokens.intern(t));
            edit.create_prim(at(), specifier, type_name)
        }
        "attribute" => {
            let declared = PropertyType::parse(text("type")).expect("a value type");
            let mut spec = PropertySpec::typed_attribute(declared);
            if step["custom"].as_bool().unwrap_or(false) {
                spec = spec.custom();
            }
            if step["uniform"].as_bool().unwrap_or(false) {
                spec = spec.uniform();
            }
            if !step["default"].is_null() {
                spec = spec.with_default(value(store, &step["default"]));
            }
            edit.create_property(at(), spec)
        }
        "relationship" => {
            let mut spec = PropertySpec::relationship().with_targets(targets());
            if step["custom"].as_bool().unwrap_or(false) {
                spec = spec.custom();
            }
            edit.create_property(at(), spec)
        }
        "sample" => edit.set_time_sample(
            at(),
            step["time"].as_f64().unwrap(),
            value(store, &step["value"]),
        ),
        "connect" => edit.set_targets(at(), targets()),
        "metadata" if path == "/" => edit.set_layer_metadata(
            SCENE,
            token("key"),
            FieldValue::Value(value(store, &step["value"])),
        ),
        "metadata" => edit.set_metadata(
            at(),
            token("key"),
            FieldValue::Value(value(store, &step["value"])),
        ),
        "active" => edit.set_active(at(), step["value"].as_bool().unwrap()),
        "instanceable" => edit.set_instanceable(at(), step["value"].as_bool().unwrap()),
        "api_schema" => edit.add_applied_schema(at(), token("name")),
        op @ ("reference" | "payload") => {
            let prim = store.path(text("target"));
            let mut arc = match step["asset"].as_str() {
                Some(asset) => Reference::with_asset(ASSET, prim, asset),
                None => Reference::new(SCENE, prim),
            };
            arc.layer_offset = LayerOffset {
                offset: step["offset"].as_f64().unwrap_or(0.0),
                scale: 1.0,
            };
            if op == "reference" {
                edit.add_reference(at(), arc, ListPosition::BackOfPrepend)
            } else {
                edit.add_payload(at(), arc, ListPosition::BackOfPrepend)
            }
        }
        "inherit" => edit.add_inherit(
            at(),
            store.path(text("target")),
            ListPosition::BackOfPrepend,
        ),
        "specialize" => edit.add_specialize(
            at(),
            store.path(text("target")),
            ListPosition::BackOfPrepend,
        ),
        "variant_set" => edit.add_variant_set(at(), token("name"), ListPosition::BackOfPrepend),
        "select" => edit.set_variant_selection(at(), token("set"), Some(token("variant"))),
        "property_order" => edit.set_property_order(at(), names()),
        "prim_order" => edit.set_prim_order(at(), names()),
        other => panic!("no step {other}"),
    };
    edit
}

/// Each case, authored step by step, saves as OpenUSD's text.
#[test]
fn scrambled_authoring_saves_as_openusd_writes_it() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 6, "the oracle's cases");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(SCENE));
        store.insert_layer(Layer::new(ASSET));
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let edit = transaction(&store, step);
            if let Err(error) = edit.apply(&mut store) {
                panic!("{name}: step {i} ({step}) is rejected: {error:?}");
            }
        }
        let text = layerstack_usda::save::save_usda(
            store.layer(SCENE).unwrap(),
            &store.tokens,
            &store.paths,
        )
        .expect("saves");
        assert_eq!(text, case["text"].as_str().unwrap(), "{name}");
    }
}

/// A file written out of order, read as USDA or as OpenUSD's crate file of
/// it, saves as OpenUSD's text of it; so does a file of unregistered
/// metadata.
#[test]
fn scrambled_files_save_as_openusd_writes_them() {
    let oracle = oracle();
    let dir = workspace_root().join("layerstack_conformance/fixtures/usda_field_order");
    let usda = std::fs::read_to_string(dir.join("scrambled.usda")).unwrap();
    let usdc = std::fs::read(dir.join("scrambled.usdc")).unwrap();
    let unregistered = std::fs::read_to_string(dir.join("unregistered.usda")).unwrap();
    for (format, imported) in [
        ("usda", Imported::usda(&usda)),
        ("usdc", Imported::usdc(&usdc)),
        ("unregistered", Imported::usda(&unregistered)),
    ] {
        let text = imported.save_usda().expect("saves");
        let expected = oracle["round_trip"][format].as_str().unwrap();
        assert_eq!(text, expected, "{format}");
        // Saved again, the text is unchanged.
        assert_eq!(Imported::usda(&text).save_usda().unwrap(), text, "{format}");
    }
}
