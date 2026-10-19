// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Spec copies agree with OpenUSD 26.08's `SdfCopySpec`.
//!
//! `scripts/copy_spec_oracle.py` reloads `fixtures/copy_spec/src.usda` and
//! `dst.usda` for each case, makes one `Sdf.CopySpec` call, and records
//! whether it was accepted and every spec of the destination layer with
//! its path-bearing fields. Here `Transaction::copy_spec` must accept or
//! reject the same copies and leave the same destination layer, and its
//! inverse must restore it.
//!
//! Two differences are deliberate (`deliberate`): a copied path
//! expression that names the copied namespace follows the copy, where
//! OpenUSD leaves it naming the source; and a property copied to a prim
//! the destination lacks creates that prim as an `over`, as every other
//! transaction edit creates missing specs, where OpenUSD fails.
//!
//! Spec: AOUSD Core §7 (scene description), §8 (spec paths).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{
    Address, Layer, LayerId, ListOp, PrimSpec, PropertySpec, ReferenceTarget, SpecPath, TargetPath,
    Transaction, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../fixtures/copy_spec/oracle.json");

fn load() -> LoadedStage {
    let path = workspace_root().join("layerstack_conformance/fixtures/copy_spec/scene.usda");
    let loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

fn layer_named(loaded: &LoadedStage, name: &str) -> LayerId {
    *loaded
        .layer_names
        .iter()
        .find(|(_, n)| {
            n.as_str() == format!("{name}.usda") || n.ends_with(&format!("/{name}.usda"))
        })
        .unwrap_or_else(|| panic!("no layer {name}"))
        .0
}

fn list_op<T>(op: &ListOp<T>, item: impl Fn(&T) -> Json) -> Json {
    let items = |list: &[T]| list.iter().map(&item).collect::<Vec<_>>();
    json!({
        "explicit": op.explicit.as_deref().map(items),
        "prepend": items(&op.prepend),
        "append": items(&op.append),
        "delete": items(&op.delete),
    })
}

/// Every prim and variant spec of `layer`, recorded as the oracle records
/// them.
fn dump(loaded: &LoadedStage, layer: &Layer) -> Json {
    let store = &loaded.store;
    let path = |p: &layerstack::PathId| json!(store.paths.display(*p, &store.tokens));
    let target = |t: &TargetPath| match t {
        TargetPath::Prim(p) => json!(store.paths.display(*p, &store.tokens)),
        TargetPath::Property(p) => json!(p.display(&store.paths, &store.tokens)),
    };
    let properties = |entries: &[layerstack::PropertyEntry]| {
        let mut out = serde_json::Map::new();
        for entry in entries {
            let spec: &PropertySpec = &entry.spec;
            let mut record = serde_json::Map::new();
            match &spec.default {
                Some(Value::Double(v)) => {
                    record.insert("default".into(), json!(v));
                }
                Some(Value::PathExpression(text)) => {
                    record.insert("default".into(), json!(text.to_string()));
                }
                Some(other) => panic!("unexpected default {other:?}"),
                None => {}
            }
            if let Some(targets) = &spec.targets {
                record.insert("targets".into(), list_op(targets, target));
            }
            out.insert(
                store.tokens.resolve(entry.name).into(),
                Json::Object(record),
            );
        }
        out
    };
    let mut out = BTreeMap::new();
    let mut add = |text: String, spec: Option<&PrimSpec>, record: serde_json::Map<String, Json>| {
        let mut record = record;
        if let Some(spec) = spec {
            let specifier = match spec.specifier {
                Some(layerstack::Specifier::Def) => "def",
                Some(layerstack::Specifier::Class) => "class",
                _ => "over",
            };
            record.insert("specifier".into(), json!(specifier));
        }
        out.insert(text, Json::Object(record));
    };
    let arcs = |record: &mut serde_json::Map<String, Json>,
                refs: &ListOp<layerstack::Reference>,
                inherits: &ListOp<layerstack::PathId>,
                specializes: &ListOp<layerstack::PathId>,
                props: &[layerstack::PropertyEntry]| {
        if *inherits != ListOp::default() {
            record.insert("inheritPaths".into(), list_op(inherits, path));
        }
        if *specializes != ListOp::default() {
            record.insert("specializes".into(), list_op(specializes, path));
        }
        if *refs != ListOp::default() {
            record.insert(
                "references".into(),
                list_op(refs, |r| {
                    let target = match r.target {
                        ReferenceTarget::Prim(p) => store.paths.display(p, &store.tokens),
                        _ => String::new(),
                    };
                    json!([r.asset_path().unwrap_or_default(), target])
                }),
            );
        }
        let props = properties(props);
        if !props.is_empty() {
            record.insert("properties".into(), Json::Object(props));
        }
    };
    let specs = layer.prims().iter().chain(
        layer
            .variant_prims()
            .iter()
            .flat_map(|(p, specs)| specs.iter().map(move |s| (p, s))),
    );
    for (prim, spec) in specs {
        // The spec path: the namespace path with its enclosing selections.
        let mut text = String::new();
        let segments = store.paths.resolve(*prim).segments().to_vec();
        for (depth, name) in segments.iter().enumerate() {
            if !text.ends_with('}') {
                text.push('/');
            }
            text.push_str(store.tokens.resolve(*name));
            let host = layerstack::Path::root().join(&segments[..=depth]);
            for site in &spec.outer_variant_sites {
                if *store.paths.resolve(site.host_path) == host {
                    text.push_str(&format!(
                        "{{{}={}}}",
                        store.tokens.resolve(site.set),
                        store.tokens.resolve(site.variant)
                    ));
                }
            }
        }
        if text.is_empty() {
            continue;
        }
        let mut record = serde_json::Map::new();
        arcs(
            &mut record,
            &spec.references,
            &spec.inherits,
            &spec.specializes,
            &spec.properties,
        );
        if let Some(name) = spec.type_name {
            record.insert("typeName".into(), json!(store.tokens.resolve(name)));
        }
        for (set, variants) in &spec.variant_sets {
            for (variant, branch) in &variants.variants {
                let mut record = serde_json::Map::new();
                arcs(
                    &mut record,
                    &branch.references,
                    &branch.inherits,
                    &branch.specializes,
                    &branch.properties,
                );
                let key = format!(
                    "{text}{{{}={}}}",
                    store.tokens.resolve(*set),
                    store.tokens.resolve(*variant)
                );
                add(key, None, record);
            }
        }
        add(text, Some(spec), record);
    }
    json!(out)
}

/// What layerstack authors where it differs on purpose (see the module
/// docs): the oracle's record, with the copied path expressions following
/// the copy, and the `over` a property copy creates.
fn deliberate(name: &str, accepted: bool, mut layer: Json) -> (bool, Json) {
    let expression = |copy: &str| json!(format!("{copy}/Child /Outside"));
    match name {
        "prim_replaces_destination" => layer["/B"]["properties"]["e"]["default"] = expression("/B"),
        "prim_into_another_layer" => {
            layer["/P/Q"]["properties"]["e"]["default"] = expression("/P/Q");
        }
        "property_to_missing_prim" => {
            layer.as_object_mut().unwrap().insert(
                "/Nope".into(),
                json!({"specifier": "over", "properties": {"y": {"default": 1.0}}}),
            );
            return (true, layer);
        }
        _ => {}
    }
    (accepted, layer)
}

#[test]
fn copies_agree_with_openusd_copy_spec() {
    let oracle: Json = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 7);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load();
        let address = |loaded: &mut LoadedStage, end: &Json| {
            let layer = layer_named(loaded, end[0].as_str().unwrap());
            let path = SpecPath::parse(
                end[1].as_str().unwrap(),
                &loaded.store.tokens,
                &loaded.store.paths,
            )
            .or_else(|_| {
                // Paths the fixture does not author yet.
                let text = end[1].as_str().unwrap();
                match text.split_once('.') {
                    Some((prim, property)) => {
                        let prim = loaded.store.path(prim);
                        let property = loaded.store.tokens.intern(property);
                        Ok::<_, ()>(SpecPath::from_prim_path(prim).with_property(property))
                    }
                    None => Ok(SpecPath::from_prim_path(loaded.store.path(text))),
                }
            })
            .unwrap();
            (layer, Address::spec(layer, path))
        };
        let (_, from) = address(&mut loaded, &case["from"]);
        let (dst, to) = address(&mut loaded, &case["to"]);
        let before = loaded.store.layer(dst).unwrap().clone();
        let mut txn = Transaction::new();
        txn.copy_spec(from, to);
        let result = txn.apply(&mut loaded.store);
        let (accepted, want) = deliberate(
            name,
            case["accepted"].as_bool().unwrap(),
            case["layer"].clone(),
        );
        assert_eq!(result.is_ok(), accepted, "{name}: accepted ({result:?})");
        assert_eq!(
            dump(&loaded, loaded.store.layer(dst).unwrap()),
            want,
            "{name}: destination"
        );
        if let Ok(inverse) = result {
            inverse.apply(&mut loaded.store).unwrap();
            let after = &loaded.store.layer(dst).unwrap();
            assert_eq!(after.prims(), before.prims(), "{name}: undo");
            assert_eq!(
                after.variant_prims(),
                before.variant_prims(),
                "{name}: undo"
            );
        }
    }
}
