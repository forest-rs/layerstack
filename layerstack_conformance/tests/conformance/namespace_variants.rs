// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Namespace edits on a stage that composes variant sets do what OpenUSD
//! 26.11's `UsdNamespaceEditor` does, call by call.
//!
//! `scripts/namespace_variants_oracle.py` opens
//! `fixtures/namespace_variants/scene.usda` once per case, queues one move
//! or delete of a prim or property through the root layer (or through
//! `/World/A`'s `look=red` variant), and records whether the editor can
//! apply it, each layer's exported text after applying it, and the
//! composed prims with their children, properties, targets, connections
//! and variant selections; then it selects other variants and records
//! the stage again (`switched`). See the script for what OpenUSD does
//! with the specs in selected and unselected variants.
//!
//! Here each case is `NamespaceEdit::prepare` (a move) or `prepare_delete`
//! against a `LiveStage`'s snapshot and against a stage composed on its
//! own, which must agree. It must be refused exactly where OpenUSD's
//! `CanApplyEdits` fails; otherwise, applied through the `LiveStage`, it
//! must leave every layer's text as OpenUSD's, byte for byte, and the
//! same composed stage. The same selections are then authored through
//! the `LiveStage`, which must match the oracle's `switched` record and a
//! fresh composition; undoing both restores every layer. The cases in
//! `DIFFERING` are the ones layerstack does not yet do as OpenUSD does.
//!
//! Spec: AOUSD Core §8 (namespace), §10.3.2.6 (relocates), §10.5
//! (variants).

use layerstack::{
    Address, EditTarget, InMemoryStore, LayerId, LiveStage, NamespaceEdit, Path, PropertyPath,
    SpecPath, Stage, StageOptions, TargetPath, Transaction,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Map, Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/namespace_variants/oracle.json");

/// The layers each record lists.
const LAYERS: [&str; 3] = ["scene.usda", "sub.usda", "asset.usda"];

/// The selections the oracle's `switch` authors in the root layer.
const SWITCH: [(&str, &str, &str); 4] = [
    ("/World/A", "look", "blue"),
    ("/World/Host", "v", "off"),
    ("/World/N", "inner", "y"),
    ("/World/R", "mode", "two"),
];

/// The cases where layerstack does not yet do what OpenUSD does. Each
/// must still differ, so that the list stays current.
const DIFFERING: &[&str] = &[];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn load() -> LoadedStage {
    let loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/namespace_variants/scene.usda"),
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

/// Each layer as OpenUSD's `SdfLayer::ExportToString` writes it.
fn layers_record(loaded: &LoadedStage) -> Json {
    let store = &loaded.store;
    let mut out = Map::new();
    for name in LAYERS {
        let layer = store.layer(layer_named(loaded, name)).unwrap();
        let text = layerstack_usda::save_usda(layer, store.tokens(), store.paths()).expect("saves");
        out.insert(name.into(), Json::String(text));
    }
    Json::Object(out)
}

fn display(store: &InMemoryStore, paths: Vec<TargetPath>) -> Json {
    json!(
        paths
            .into_iter()
            .map(|p| p.display(store.paths(), store.tokens()))
            .collect::<Vec<_>>()
    )
}

/// Every composed prim, as the oracle's `composed_record`.
fn composed_record(store: &InMemoryStore, stage: &Stage) -> Json {
    let root = store.paths().lookup(&Path::root()).unwrap();
    let mut out = Map::new();
    for path in stage.traverse_all(root).filter(|p| *p != root) {
        let prim = stage.prim(path, store).unwrap();
        let mut targets = Map::new();
        let mut connections = Map::new();
        for property in prim.properties() {
            let name = property.name().to_string();
            if let Some(relationship) = property.as_relationship() {
                targets.insert(name, display(store, relationship.targets()));
            } else if let Some(attribute) = property.as_attribute() {
                let list = attribute.connections();
                if !list.is_empty() {
                    connections.insert(name, display(store, list));
                }
            }
        }
        out.insert(
            store.paths().display(path, store.tokens()),
            json!({
                "children": prim.all_children().map(|c| c.name().to_string()).collect::<Vec<_>>(),
                "properties": prim.property_names(),
                "targets": targets,
                "connections": connections,
                "selections": prim.variant_selections(),
            }),
        );
    }
    Json::Object(out)
}

/// The places where `got` and `want` differ, as `/`-joined keys.
fn differences(got: &Json, want: &Json, at: &str, out: &mut Vec<String>) {
    match (got, want) {
        (Json::Object(g), Json::Object(w)) => {
            for key in g.keys().chain(w.keys().filter(|k| !g.contains_key(*k))) {
                match (g.get(key), w.get(key)) {
                    (Some(a), Some(b)) => differences(a, b, &format!("{at} {key}"), out),
                    (a, b) => out.push(format!("{at} {key}: {a:?} != {b:?}")),
                }
            }
        }
        (Json::String(g), Json::String(w)) if g != w => {
            out.push(format!("{at}:\n--- got\n{g}\n--- want\n{w}"));
        }
        _ if got != want => out.push(format!("{at}: {got} != {want}")),
        _ => {}
    }
}

fn assert_same(got: &Json, want: &Json, what: &str) {
    let mut out = Vec::new();
    differences(got, want, "", &mut out);
    assert!(out.is_empty(), "{what} (got != want):\n{}", out.join("\n"));
}

fn target_path(store: &mut InMemoryStore, text: &str) -> TargetPath {
    match text.split_once('.') {
        Some((prim, property)) => {
            let prim = store.path(prim);
            let property = store.tokens().intern(property);
            TargetPath::Property(PropertyPath::new(prim, property))
        }
        None => TargetPath::Prim(store.path(text)),
    }
}

/// The case's edit against `stage`; `None` when layerstack refuses it.
fn prepare(loaded: &mut LoadedStage, stage: &Stage, case: &Json) -> Option<Transaction> {
    let store = &mut loaded.store;
    let target = match case["target"].as_str().unwrap() {
        "root" => EditTarget::for_layer(loaded.root_layer),
        "variant" => {
            let variant = SpecPath::parse("/World/A{look=red}", store.tokens(), store.paths());
            EditTarget::for_local_variant(loaded.root_layer, &variant.unwrap())
        }
        other => panic!("no target {other}"),
    };
    let source = target_path(store, case["path"].as_str().unwrap());
    let edit = match case["destination"].as_str() {
        Some(to) => {
            let destination = target_path(store, to);
            NamespaceEdit::prepare(stage, store, &target, source, destination)
        }
        None => NamespaceEdit::prepare_delete(stage, store, &target, source),
    };
    edit.ok().map(NamespaceEdit::into_transaction)
}

/// `path` after the case's edit, as the oracle's `moved`: `None` once
/// deleted.
fn moved(path: &str, case: &Json) -> Option<String> {
    let source = case["path"].as_str().unwrap();
    let beneath = path == source || path.starts_with(&format!("{source}/"));
    if source.contains('.') || !beneath {
        return Some(path.into());
    }
    let destination = case["destination"].as_str()?;
    Some(format!("{destination}{}", &path[source.len()..]))
}

/// The selections of the oracle's `switch`, authored in the root layer at
/// each prim's path after the edit, where it is still composed.
fn switch(loaded: &mut LoadedStage, stage: &Stage, case: &Json) -> Transaction {
    let mut transaction = Transaction::new();
    for (path, set, variant) in SWITCH {
        let Some(path) = moved(path, case) else {
            continue;
        };
        let store = &mut loaded.store;
        let prim = store.path(&path);
        if stage.prim(prim, &*store).is_none() {
            continue;
        }
        let at = Address::spec(loaded.root_layer, SpecPath::from_prim_path(prim));
        let set = store.tokens().intern(set);
        let variant = store.tokens().intern(variant);
        transaction.set_variant_selection(at, set, Some(variant));
    }
    transaction
}

#[test]
fn the_layers_are_the_fixtures() {
    let oracle = oracle();
    let mut loaded = load();
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert_same(
        &layers_record(&loaded),
        &oracle["before"]["layers"],
        "layers",
    );
    assert_same(
        &composed_record(&loaded.store, &stage),
        &oracle["before"]["composed"],
        "composed",
    );
}

#[test]
fn namespace_edits_on_variant_sets_agree_with_openusd_call_by_call() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 25, "every case is recorded");
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let differs = DIFFERING.contains(&name);
        // A differing case panics, which aborts where panics cannot unwind
        // (WASI), so it runs only where its panic can be caught.
        if differs && cfg!(panic = "abort") {
            continue;
        }
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| replay(&oracle, case)));
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

fn replay(oracle: &Json, case: &Json) {
    let mut loaded = load();
    let original: Vec<_> = loaded.store.layers().cloned().collect();
    let before = layers_record(&loaded);
    let mut live = LiveStage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let composed = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let prepared = prepare(&mut loaded, live.stage(), case);
    let by_composition = prepare(&mut loaded, &composed, case);
    assert_eq!(
        prepared.is_some(),
        by_composition.is_some(),
        "the live snapshot and a composed stage agree whether to edit"
    );
    let applied = case["applied"].as_bool().unwrap();
    assert_eq!(
        prepared.is_some(),
        applied,
        "applied (OpenUSD errors: {})",
        case["errors"]
    );
    let Some(transaction) = prepared else {
        assert_same(&layers_record(&loaded), &before, "refused: unchanged");
        return;
    };
    let edit = live
        .apply(&mut loaded.store, &transaction)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_same(&layers_record(&loaded), &case["layers"], "layers");
    assert_same(
        &composed_record(&loaded.store, live.stage()),
        &case["composed"],
        "composed",
    );
    let fresh = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert_same(
        &composed_record(&loaded.store, live.stage()),
        &composed_record(&loaded.store, &fresh),
        "the live stage recomposes",
    );
    let selections = switch(&mut loaded, live.stage(), case);
    let switched = live
        .apply(&mut loaded.store, &selections)
        .unwrap_or_else(|e| panic!("switch: {e:?}"));
    assert_same(
        &composed_record(&loaded.store, live.stage()),
        &case["switched"],
        "switched",
    );
    let fresh = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert_same(
        &composed_record(&loaded.store, live.stage()),
        &composed_record(&loaded.store, &fresh),
        "the switched live stage recomposes",
    );
    live.apply(&mut loaded.store, &switched.inverse).unwrap();
    live.apply(&mut loaded.store, &edit.inverse).unwrap();
    for layer in &original {
        let now = loaded.store.layer(layer.id()).unwrap();
        assert_eq!(now.prims(), layer.prims(), "undo restores prims");
        assert_eq!(
            now.variant_prims(),
            layer.variant_prims(),
            "undo restores variants"
        );
    }
    assert_same(
        &composed_record(&loaded.store, live.stage()),
        &oracle["before"]["composed"],
        "undo",
    );
}
