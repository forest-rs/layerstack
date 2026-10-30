// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A namespace edit reports exactly the authored paths that OpenUSD
//! 26.11's `UsdNamespaceEditor` leaves naming the edited object.
//!
//! `scripts/namespace_unrepaired_oracle.py` opens
//! `fixtures/namespace_unrepaired/scene.usda` once per case, moves or
//! deletes `/World/Lamp` (or a prim or property beside it) through the
//! root layer, and records each layer's text after the edit and every
//! path the layers then author that names the edited object or something
//! beneath it: those in `/World/Room`'s unselected `off` variant, of
//! every kind, and in `asset.usda`'s unselected variant and unreferenced
//! `/Unused`. See the script for why OpenUSD leaves them. The arcs
//! `asset.usda`'s composed specs author back into the stage are
//! repaired, and the specs over the lamp through them move or go.
//!
//! Here each case is `NamespaceEdit::prepare` (a move) or
//! `prepare_delete` against a `LiveStage`'s snapshot and against a stage
//! composed on its own: both must write every layer as OpenUSD does, byte
//! for byte, and `NamespaceEdit::unrepaired_paths` must list exactly the
//! recorded paths, at the specs where the edit leaves them. The cases in
//! `DIFFERING` are the ones layerstack does not yet do as OpenUSD does.
//!
//! One divergence is deliberate and has no case here: when the asset
//! authoring the arc back into the stage is a package member
//! (`props.usdz[asset.usda]`), `UsdNamespaceEditor`
//! (`pxr/usd/usd/namespaceEditor.cpp`) repairs it in memory with
//! `CanApplyEdits` approving, and saving the package then fails in
//! `SdfLayer::_CheckFormatWritability` (`pxr/usd/sdf/layer.cpp`).
//! layerstack refuses such a move or delete before any layer changes
//! (`NamespaceError::ReadOnlyLayer`), as `layerstack`'s and
//! `layerstack_io`'s unit tests check.
//!
//! Spec: AOUSD Core §8 (namespace), §10.5 (variants).

use layerstack::{
    EditTarget, InMemoryStore, LiveStage, NamespaceEdit, PathField, PropertyPath, Stage,
    StageOptions, TargetPath,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/namespace_unrepaired/oracle.json");

/// The layers each record lists.
const LAYERS: [&str; 2] = ["scene.usda", "asset.usda"];

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
        &workspace_root().join("layerstack_conformance/fixtures/namespace_unrepaired/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

/// The layer's file name, `/` separated on every platform.
fn layer_name(loaded: &LoadedStage, id: layerstack::LayerId) -> String {
    let name = loaded.layer_names[&id].replace('\\', "/");
    name.rsplit('/').next().unwrap().to_string()
}

fn layers_record(loaded: &LoadedStage) -> Json {
    let store = &loaded.store;
    let mut out = serde_json::Map::new();
    for id in loaded.layer_names.keys() {
        let name = layer_name(loaded, *id);
        if LAYERS.contains(&name.as_str()) {
            let layer = store.layer(*id).unwrap();
            let text =
                layerstack_usda::save_usda(layer, store.tokens(), store.paths()).expect("saves");
            out.insert(name, Json::String(text));
        }
    }
    Json::Object(out)
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

/// The OpenUSD field name of a `PathField`.
fn field_name(field: PathField) -> &'static str {
    match field {
        PathField::References => "references",
        PathField::Payloads => "payload",
        PathField::Inherits => "inheritPaths",
        PathField::Specializes => "specializes",
        PathField::Targets => "targetPaths",
        PathField::Connections => "connectionPaths",
        PathField::Default => "default",
        PathField::TimeSamples => "timeSamples",
        other => panic!("no recorded field for {other:?}"),
    }
}

/// The case's edit against `stage`, which OpenUSD applied.
fn prepare(loaded: &mut LoadedStage, stage: &Stage, case: &Json) -> NamespaceEdit {
    let store = &mut loaded.store;
    let target = EditTarget::for_layer(loaded.root_layer);
    let source = target_path(store, case["path"].as_str().unwrap());
    let edit = match case["destination"].as_str() {
        Some(to) => {
            let destination = target_path(store, to);
            NamespaceEdit::prepare(stage, store, &target, source, destination)
        }
        None => NamespaceEdit::prepare_delete(stage, store, &target, source),
    };
    edit.unwrap_or_else(|e| panic!("{e}"))
}

/// The report as the oracle records it: `[layer, spec, field, path]`,
/// sorted.
fn report(loaded: &LoadedStage, edit: &NamespaceEdit) -> Json {
    let store = &loaded.store;
    let mut rows: Vec<Json> = edit
        .unrepaired_paths()
        .iter()
        .map(|left| {
            json!([
                layer_name(loaded, left.layer),
                left.spec.display(store.paths(), store.tokens()),
                field_name(left.field),
                left.path.display(store.paths(), store.tokens()),
            ])
        })
        .collect();
    rows.sort_by_key(ToString::to_string);
    Json::Array(rows)
}

fn sorted(rows: &Json) -> Json {
    let mut rows = rows.as_array().unwrap().clone();
    rows.sort_by_key(ToString::to_string);
    Json::Array(rows)
}

fn replay(case: &Json) {
    let mut loaded = load();
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
    live.apply(&mut loaded.store, prepared.transaction())
        .unwrap_or_else(|e| panic!("{e:?}"));
    let layers = layers_record(&loaded);
    for name in LAYERS {
        assert_eq!(
            layers[name], case["layers"][name],
            "{name} is written as OpenUSD writes it"
        );
    }

    let want = sorted(&case["unrepaired"]);
    assert_eq!(
        report(&loaded, &prepared),
        want,
        "the live snapshot's report"
    );
    assert_eq!(
        report(&loaded, &by_composition),
        want,
        "a composed stage's report"
    );
}

#[test]
fn the_layers_are_the_fixtures() {
    let oracle = oracle();
    let loaded = load();
    assert_eq!(layers_record(&loaded), oracle["before"]);
}

#[test]
fn namespace_edits_report_the_paths_openusd_leaves() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 9, "every case is recorded");
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
