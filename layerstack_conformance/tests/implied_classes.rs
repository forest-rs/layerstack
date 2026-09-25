// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for classes implied into stronger layer stacks.
//!
//! `fixtures/implied_classes/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/implied_classes/root.usda`
//! (`scripts/implied_classes_oracle.py`, which also writes those layers).
//! Layerstack must compose the same prims with the same ordered prim stacks
//! and resolve the same attribute defaults.
//!
//! A class arc authored inside a reference or payload target is implied into
//! every stronger layer stack on the way to the root, beneath the node of
//! that layer stack, so its opinions there outrank the arc that implies it:
//!
//! - `/Grove` reaches `bark.usda /Trunk`, which inherits `/_class_Bark`,
//!   through two references; `root.usda /_class_Bark` outranks
//!   `stand.usda /Stand`, and `stand.usda /_class_Bark` outranks
//!   `bark.usda /Trunk`.
//! - `/Field` reaches `meadow.usda /Meadow`, which inherits `/_class_Grass`,
//!   through a payload.
//! - `/Cliff/Face` inherits the class `/_class_Boulder/_class_Face` nested in
//!   `/_class_Boulder`, which `rock.usda /Boulder` inherits: the class is
//!   implied across that ancestral inherit as `/Boulder/_class_Face`, then
//!   across the reference as `/Cliff/_class_Face`, whose local opinion wins.
//!
//! Spec: AOUSD Core §10.4.2.4 (implied class arcs); OpenUSD
//! `_EvalImpliedClasses` in `pxr/usd/pcp/primIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{Stage, StageOptions, Value};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/implied_classes/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, i32>,
}

#[derive(Deserialize)]
struct Prim {
    path: String,
    prim_stack: Vec<(String, String)>,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert!(
        oracle.openusd_version.starts_with("0.26."),
        "oracle from OpenUSD {}",
        oracle.openusd_version
    );
    oracle
}

fn compose(oracle: &Oracle) -> (LoadedStage, Stage) {
    let mut loaded = load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/implied_classes")
            .join(&oracle.root),
    );
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    (loaded, stage)
}

/// A layer's file name without its directory.
fn layer_name(loaded: &LoadedStage, layer: layerstack::LayerId) -> String {
    let name = &loaded.layer_names[&layer];
    name.rsplit('/').next().unwrap_or(name).to_string()
}

#[test]
fn prim_stacks_rank_implied_classes_with_their_layer_stack() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let pseudo_root = loaded.store.path("/");
    let mut composed: Vec<String> = stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .map(|prim| loaded.store.paths.display(prim, &loaded.store.tokens))
        .collect();
    composed.sort();
    let mut expected: Vec<&str> = oracle.prims.iter().map(|p| p.path.as_str()).collect();
    expected.sort_unstable();
    assert_eq!(composed, expected, "composed prims");

    let mut mismatches = Vec::new();
    for prim in &oracle.prims {
        let id = loaded.store.path(&prim.path);
        let stack: Vec<(String, String)> = stage
            .explain_prim(id)
            .expect("composed prim")
            .iter()
            .map(|key| {
                (
                    layer_name(&loaded, key.layer_id),
                    key.spec_path.display(&loaded.store.tokens),
                )
            })
            .collect();
        if stack != prim.prim_stack {
            mismatches.push(format!(
                "{}\n    expected {:?}\n    actual   {stack:?}",
                prim.path, prim.prim_stack
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "prim stacks differ from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn values_match_openusd() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let mut mismatches = Vec::new();
    for (attr, expected) in &oracle.values {
        let property = loaded.store.property_path(attr);
        let value = stage
            .resolve_field_path(property)
            .map(|resolved| resolved.value);
        if value != Some(Value::Int(*expected)) {
            mismatches.push(format!("{attr}: expected {expected}, got {value:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "values differ from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}
