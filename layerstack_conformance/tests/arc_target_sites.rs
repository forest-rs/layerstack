// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for how often an arc target's sites appear in a prim
//! stack.
//!
//! `fixtures/arc_target_sites/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/arc_target_sites/root.usda`
//! (`scripts/arc_target_sites_oracle.py`, which also writes those layers).
//! Layerstack must compose the same prims with the same ordered prim stacks,
//! repeats included, and resolve the same attribute defaults.
//!
//! OpenUSD builds an arc's target subgraph once, so each site appears once
//! per arc path: the selected branch of a referenced prim (`/Trunk`), an
//! asset reached through references nested in another asset (`/Grove/Oak`,
//! `/Grove/Elm`), and a class a prim inherits directly that its referenced
//! prim implies again (`/Bush`).
//!
//! A class reached twice is one site at its strongest registration,
//! whichever arc expansion reaches it first: `/Pine` and `/Fir` inherit
//! `/Bark` directly and through the specializes of `/Needle`, in either list
//! order, `/Larch` specializes it directly, and `/Spruce` reaches the first
//! shape through a reference. Each resolves `x` from the site OpenUSD ranks
//! strongest. `/Maple` repeats the first shape with a selected variant on the
//! class reached twice: the branch goes with the registration that stays and
//! appears once.
//!
//! Spec: AOUSD Core §10.4 (an arc's target ranks beneath the site that
//! authors it), §10.4.2.4 (implied class arcs); `pxr/usd/pcp/primIndex.cpp`
//! (`_AddArc`, `_IsRedundantSite`).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{Stage, StageOptions, Value};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/arc_target_sites/oracle.json");

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
            .join("layerstack_conformance/fixtures/arc_target_sites")
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
fn prim_stacks_list_each_site_once_per_arc_path() {
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
