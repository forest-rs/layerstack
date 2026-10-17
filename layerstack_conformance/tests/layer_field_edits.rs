// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sublayer and relocates edits agree with OpenUSD 26.08, call by call.
//!
//! `scripts/layer_field_edits_oracle.py` opens
//! `fixtures/layer_field_edits/scene.usda` once per case and edits its root
//! layer's `subLayers`, `subLayerOffsets` and `layerRelocates` through
//! `SdfLayer`, recording whether each call was accepted, the fields it
//! leaves, and every prim the stage then composes. Here each call is one
//! `Transaction` applied through a `LiveStage`, which must accept or reject
//! it as OpenUSD does, author the same fields and compose the same prims in
//! the same order; the live stage must match a fresh composition after
//! every call, and applying the inverses in reverse order must restore the
//! root layer.
//!
//! OpenUSD accepts a sublayer cycle and relocates that composition then
//! ignores, reporting them only when composing; layerstack rejects both
//! when they are authored (`DELIBERATE`).
//!
//! Spec: AOUSD Core §10.3.1 (layer stacks), §10.3.2.6 (relocates),
//! §12.3.2.1 (layer offsets).

#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    LayerId, LayerOffset, LiveStage, Path, Relocate, Stage, StageOptions, SublayerEntry,
    Transaction,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../fixtures/layer_field_edits/oracle.json");

/// Cases where layerstack rejects edits OpenUSD accepts, with the calls it
/// accepts. Nothing is authored, so the fields and prims stay as the scene
/// has them.
const DELIBERATE: &[(&str, &[bool])] = &[
    ("sublayer_self_cycle", &[false]),
    ("relocates_invalid_entries", &[false; 5]),
];

fn load() -> LoadedStage {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/layer_field_edits/scene.usda");
    let loaded = load_entry_usda(&path);
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

fn sublayer(loaded: &LoadedStage, asset: &str) -> SublayerEntry {
    let layer = match asset {
        "" => LayerId::UNRESOLVED,
        asset => layer_named(loaded, asset),
    };
    SublayerEntry::with_asset(layer, asset, LayerOffset::IDENTITY)
}

fn transaction(loaded: &mut LoadedStage, step: &Json) -> Transaction {
    let root = loaded.root_layer;
    let index = || usize::try_from(step["index"].as_u64().unwrap()).unwrap();
    let mut txn = Transaction::new();
    match step["call"].as_str().unwrap() {
        "insert_sublayer" => {
            let entry = sublayer(loaded, step["asset"].as_str().unwrap());
            txn.insert_sublayer(root, index(), entry)
        }
        "remove_sublayer" => txn.remove_sublayer(root, index()),
        "set_sublayer_offset" => txn.set_sublayer_offset(
            root,
            index(),
            LayerOffset {
                offset: step["offset"].as_f64().unwrap(),
                scale: step["scale"].as_f64().unwrap(),
            },
        ),
        "set_sublayers" => {
            let assets = step["value"].as_array().unwrap();
            let entries = assets
                .iter()
                .map(|a| sublayer(loaded, a.as_str().unwrap()))
                .collect();
            txn.set_sublayers(root, entries)
        }
        "set_relocates" => {
            let pairs = step["value"].as_array().unwrap();
            let relocates = pairs
                .iter()
                .map(|pair| {
                    let target = pair[1].as_str().unwrap();
                    Relocate {
                        source: loaded.store.path(pair[0].as_str().unwrap()),
                        target: (!target.is_empty()).then(|| loaded.store.path(target)),
                    }
                })
                .collect();
            txn.set_layer_relocates(root, relocates)
        }
        "clear_relocates" => txn.clear_layer_relocates(root),
        other => panic!("no call {other}"),
    };
    txn
}

/// The root layer's sublayers, offsets and relocates, and the composed
/// prims, as the oracle records them.
fn snapshot(loaded: &LoadedStage, stage: &Stage) -> Json {
    let store = &loaded.store;
    let layer = &store.layers[&loaded.root_layer];
    let path = |p: layerstack::PathId| store.paths.display(p, &store.tokens);
    let root = store.paths.lookup(&Path::root()).unwrap();
    json!({
        "sublayers": layer.sublayers.iter().map(|s| s.asset.clone().unwrap_or_default()).collect::<Vec<_>>(),
        "offsets": layer.sublayers.iter().map(|s| json!([s.offset.offset, s.offset.scale])).collect::<Vec<_>>(),
        "relocates": layer.relocates.iter().map(|r| json!([path(r.source), r.target.map(path).unwrap_or_default()])).collect::<Vec<_>>(),
        "prims": stage.traverse_all(root).filter(|p| *p != root).map(path).collect::<Vec<_>>(),
    })
}

fn assert_fresh(live: &LiveStage, loaded: &mut LoadedStage, context: &str) {
    let fresh = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert_eq!(
        snapshot(loaded, live.stage()),
        snapshot(loaded, &fresh),
        "{context}"
    );
}

#[test]
fn layer_field_edits_agree_with_openusd_call_by_call() {
    let oracle: Json = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 8);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load();
        let original = loaded.store.layers[&loaded.root_layer].clone();
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let before = snapshot(&loaded, live.stage());
        let mut accepted = Vec::new();
        let mut inverses = Vec::new();
        for step in case["steps"].as_array().unwrap() {
            let txn = transaction(&mut loaded, step);
            match live.apply(&mut loaded.store, &txn) {
                Ok(applied) => {
                    accepted.push(true);
                    inverses.push(applied.inverse);
                }
                Err(_) => accepted.push(false),
            }
            let context = format!("{name}: step {} recomposes", accepted.len());
            assert_fresh(&live, &mut loaded, &context);
        }
        let deliberate = DELIBERATE.iter().find(|(n, _)| *n == name);
        let (want_accepted, want) = match deliberate {
            Some((_, accepted)) => (accepted.to_vec(), before),
            None => (
                serde_json::from_value::<Vec<bool>>(case["accepted"].clone()).unwrap(),
                json!({
                    "sublayers": case["sublayers"],
                    "offsets": case["offsets"],
                    "relocates": case["relocates"],
                    "prims": case["prims"],
                }),
            ),
        };
        assert_eq!(accepted, want_accepted, "{name}: accepted calls");
        assert_eq!(snapshot(&loaded, live.stage()), want, "{name}: result");

        for inverse in inverses.iter().rev() {
            live.apply(&mut loaded.store, inverse)
                .unwrap_or_else(|e| panic!("{name}: undo: {e}"));
        }
        assert_fresh(&live, &mut loaded, &format!("{name}: undo recomposes"));
        let layer = &loaded.store.layers[&loaded.root_layer];
        assert_eq!(layer.sublayers, original.sublayers, "{name}: undo");
        assert_eq!(layer.relocates, original.relocates, "{name}: undo");
    }
}
