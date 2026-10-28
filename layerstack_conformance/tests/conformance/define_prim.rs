// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `SchemaEdit::define_prim` (and so every generated `<Schema>::define`)
//! authors what OpenUSD 26.08's `UsdStage::DefinePrim` authors, call by
//! call, and refuses what it refuses.
//!
//! `scripts/define_prim_oracle.py` writes `fixtures/define_prim` and
//! records, for each case on fresh copies of its layers, whether each
//! `DefinePrim` call raised an error and returned a defined prim, and after
//! each step the edit target layer's exported text and the default
//! traversal (see the script for every case). Here each step is one
//! `SchemaEdit` applied through a `LiveStage`, and must leave the same
//! layer text, byte for byte, and the same traversal, live and freshly
//! composed. A call is refused (`Err`) exactly when OpenUSD raised without
//! authoring: beneath an instance or in a prototype. Beneath an inactive
//! prim OpenUSD authors the prim and then raises, as it is not on the
//! stage; layerstack authors the same and accepts. Undoing the steps one by
//! one, last first, restores each step's text before it.
//!
//! Spec: AOUSD Core §7.6 (specifier), §11.3.3 (instancing), §11.5 (defined
//! prims), §12.2.1 (specifier resolution).

use std::sync::Arc;

use layerstack::{
    EditTarget, InMemoryStore, LayerId, LiveStage, PathId, Prim, PrimPredicate, Stage, StageOptions,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::SchemaEdit;
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/define_prim/oracle.json");

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn load() -> LoadedStage {
    let loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/define_prim/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

fn live_of(loaded: &mut LoadedStage) -> LiveStage {
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(loaded.store.tokens()))),
        ..StageOptions::default()
    };
    LiveStage::compose(&mut loaded.store, loaded.root_layer, options)
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
fn text(loaded: &LoadedStage, layer: LayerId) -> String {
    let store = &loaded.store;
    layerstack_usda::save::save_usda(store.layer(layer).unwrap(), store.tokens(), store.paths())
        .expect("saves")
}

/// The default traversal below the pseudo-root, as `UsdStage::Traverse`.
fn traversal(stage: &Stage, store: &InMemoryStore, root: PathId) -> Vec<String> {
    stage
        .prim_range(root, store, PrimPredicate::DEFAULT)
        .filter(|&prim| prim != root)
        .map(|prim| store.paths().display(prim, store.tokens()))
        .collect()
}

#[test]
fn the_layers_are_the_fixtures() {
    let oracle = oracle();
    let loaded = load();
    for name in ["scene.usda", "weak.usda"] {
        assert_eq!(
            text(&loaded, layer_named(&loaded, name)),
            oracle["before"][name].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn define_prim_agrees_with_openusd_call_by_call() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 20, "every case is recorded");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load();
        let mut live = live_of(&mut loaded);
        let layer = layer_named(&loaded, case["target"].as_str().unwrap());
        let target = EditTarget::for_layer(layer);
        let root = loaded.store.path("/");
        let mut previous = text(&loaded, layer);
        let mut undo = Vec::new();
        for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let calls = step["calls"].as_array().unwrap();
            let want = step["text"].as_str().unwrap();
            let paths: Vec<_> = calls
                .iter()
                .map(|call| loaded.store.path(call["path"].as_str().unwrap()))
                .collect();
            let mut edit = SchemaEdit::new(live.stage(), &mut loaded.store, target.clone());
            let refused: Vec<bool> = calls
                .iter()
                .zip(&paths)
                .map(|(call, &path)| {
                    edit.define_prim(path, call["type"].as_str().unwrap())
                        .is_err()
                })
                .collect();
            let transaction = edit.finish();
            let applied = live
                .apply(&mut loaded.store, &transaction)
                .unwrap_or_else(|e| panic!("{name} step {index}: {e}"));
            undo.push((applied.inverse, previous.clone()));

            for ((call, &path), refused) in calls.iter().zip(&paths).zip(refused) {
                let raised = call["raised"].as_bool().unwrap();
                // OpenUSD raises after authoring a prim it cannot then find
                // on the stage; it refuses only when it authors nothing.
                let openusd_refused = raised && (calls.len() > 1 || want == previous);
                assert_eq!(refused, openusd_refused, "{name} step {index}: {call}");
                if !refused {
                    let defined = live
                        .stage()
                        .prim(path, &loaded.store)
                        .is_some_and(Prim::is_defined);
                    assert_eq!(
                        json(defined),
                        call["defined"],
                        "{name} step {index}: {call}"
                    );
                }
            }
            let now = text(&loaded, layer);
            assert_eq!(now, want, "{name} step {index}: layer text");
            assert_eq!(
                json(traversal(live.stage(), &loaded.store, root)),
                step["traversal"],
                "{name} step {index}: live traversal"
            );
            let fresh = live_of(&mut loaded);
            assert_eq!(
                json(traversal(fresh.stage(), &loaded.store, root)),
                step["traversal"],
                "{name} step {index}: fresh traversal"
            );
            previous = now;
        }
        for (index, (inverse, before)) in undo.iter().enumerate().rev() {
            live.apply(&mut loaded.store, inverse)
                .unwrap_or_else(|e| panic!("{name}: undo step {index}: {e}"));
            assert_eq!(
                &text(&loaded, layer),
                before,
                "{name}: undoing step {index}"
            );
        }
    }
}

fn json(value: impl serde::Serialize) -> Json {
    serde_json::to_value(value).unwrap()
}
