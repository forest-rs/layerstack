// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ImageableEdit::make_visible` and `make_invisible` author what OpenUSD
//! 26.08's `UsdGeomImageable::MakeVisible` and `MakeInvisible` author, call
//! by call.
//!
//! `scripts/make_visible_oracle.py` writes `fixtures/make_visible`, a root
//! layer over a hierarchy with invisible, untyped, inactive, undefined and
//! animated prims, and records after each step of each case the root
//! layer's exported text and `ComputeVisibility` of each call's prim (see
//! the script for every case). Here each step is one `SchemaEdit` applied
//! through a `LiveStage`; it must leave the same text, byte for byte, and
//! compute the same visibility, live and freshly composed. Undoing the
//! steps one by one, last first, restores each step's text before it.
//!
//! Spec: AOUSD Core §12.3 (value resolution), §13.3.1 (typed schemas).

#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::{EditTarget, LiveStage, Stage, StageOptions, Time};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::usd_geom::{Imageable, ImageableEdit};
use layerstack_schemas::{Scene, SchemaEdit};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../fixtures/make_visible/oracle.json");

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn load() -> LoadedStage {
    let loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/make_visible/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

fn live_of(loaded: &mut LoadedStage) -> LiveStage {
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&loaded.store.tokens))),
        ..StageOptions::default()
    };
    LiveStage::compose(&mut loaded.store, loaded.root_layer, options)
}

/// The root layer as OpenUSD's `SdfLayer::ExportToString` writes it.
fn text(loaded: &LoadedStage) -> String {
    let store = &loaded.store;
    layerstack_usda::save::save_usda(
        store.layer(loaded.root_layer).unwrap(),
        &store.tokens,
        &store.paths,
    )
    .expect("saves")
}

fn time_of(call: &Json) -> Time {
    call["time"].as_f64().map_or(Time::Default, Time::at)
}

/// `ComputeVisibility` of the call's prim at its time, as OpenUSD spells it.
fn visibility(stage: &Stage, loaded: &mut LoadedStage, call: &Json) -> Json {
    if call["call"] == "define" {
        return Json::Null;
    }
    let path = loaded.store.path(call["path"].as_str().unwrap());
    let scene = Scene::new(stage, &loaded.store);
    let imageable = Imageable::new(&scene, path).expect("an imageable prim");
    Json::from(imageable.compute_visibility(time_of(call)).as_str())
}

#[test]
fn the_root_layer_is_the_fixture() {
    assert_eq!(text(&load()), oracle()["before"].as_str().unwrap());
}

#[test]
fn make_visible_agrees_with_openusd_call_by_call() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 10, "every case is recorded");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load();
        let mut live = live_of(&mut loaded);
        let target = EditTarget::for_layer(loaded.root_layer);
        let mut previous = text(&loaded);
        let mut undo = Vec::new();
        for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let calls = step["calls"].as_array().unwrap();
            let paths: Vec<_> = calls
                .iter()
                .map(|call| loaded.store.path(call["path"].as_str().unwrap()))
                .collect();
            let mut edit = SchemaEdit::new(live.stage(), &mut loaded.store, target.clone());
            for (call, &path) in calls.iter().zip(&paths) {
                let time = time_of(call);
                match call["call"].as_str().unwrap() {
                    "define" => {
                        edit.define_prim(path, call["type"].as_str().unwrap())
                            .expect("outside any instance");
                    }
                    "make_visible" => {
                        ImageableEdit::new(&edit, path)
                            .expect("outside any instance")
                            .expect("an imageable prim")
                            .make_visible(&mut edit, time);
                    }
                    "make_invisible" => {
                        ImageableEdit::new(&edit, path)
                            .expect("outside any instance")
                            .expect("an imageable prim")
                            .make_invisible(&mut edit, time);
                    }
                    other => panic!("no call {other}"),
                }
            }
            let transaction = edit.finish();
            let applied = live
                .apply(&mut loaded.store, &transaction)
                .unwrap_or_else(|e| panic!("{name} step {index}: {e}"));
            undo.push((applied.inverse, previous.clone()));

            let now = text(&loaded);
            assert_eq!(now, step["text"].as_str().unwrap(), "{name} step {index}");
            let fresh = live_of(&mut loaded);
            for call in calls {
                let want = &call["visibility"];
                let stage = live.stage();
                assert_eq!(
                    &visibility(stage, &mut loaded, call),
                    want,
                    "{name} step {index}: live {call}"
                );
                assert_eq!(
                    &visibility(fresh.stage(), &mut loaded, call),
                    want,
                    "{name} step {index}: fresh {call}"
                );
            }
            previous = now;
        }
        for (index, (inverse, before)) in undo.iter().enumerate().rev() {
            live.apply(&mut loaded.store, inverse)
                .unwrap_or_else(|e| panic!("{name}: undo step {index}: {e}"));
            assert_eq!(&text(&loaded), before, "{name}: undoing step {index}");
        }
    }
}
