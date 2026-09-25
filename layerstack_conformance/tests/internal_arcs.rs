// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for the layer stack internal arcs target.
//!
//! `fixtures/internal_arcs/oracle.json` records what OpenUSD 26.08 composes
//! from `fixtures/internal_arcs/root.usda` (`scripts/internal_arcs_oracle.py`,
//! which also writes those layers). Layerstack must compose the same prims
//! with the same ordered prim stacks, and resolve the same attribute defaults
//! and time-sampled values.
//!
//! Every internal reference or payload in the fixture is authored in a
//! sublayer: of the root layer stack (`sub.usda`), of a referenced asset
//! (`asset_sub.usda`), and of assets referenced two deep (`outer_sub.usda`,
//! `inner_sub.usda`). Each must read the whole layer stack containing it, so
//! a stronger layer's opinions at the target win, each sublayer's time
//! offset applies, `<>` names the stack root layer's `defaultPrim`, and a
//! stronger layer can delete the arc.
//!
//! Where Layerstack's strength order is known to differ from OpenUSD's for a
//! reason other than anchoring, [`ORDER_DIVERGENCES`] names the prim: its
//! stack must hold the oracle's sites in another order, and the test fails
//! once the order matches so the entry cannot go stale.
//!
//! Spec: AOUSD Core §10.3.2.1 ("the layer stack containing the reference is
//! assumed"), §10.3.2.2; `pxr/usd/pcp/primIndex.cpp`
//! (`_EvalRefOrPayloadArcs`).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{InterpolationType, Stage, StageOptions, Value};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/internal_arcs/oracle.json");

/// Prims whose prim stack holds the oracle's sites in a different order,
/// with the reason.
const ORDER_DIVERGENCES: &[(&str, &str)] = &[(
    "/Nested",
    "NestedArcDepth in `composition_strict`: `inner.usda /Core`, three arcs \
     deep, shares its strength key with `inner_sub.usda /Inner`, two deep, \
     so the two interleave by layer strength instead of ranking the target \
     beneath the site that authors its arc",
)];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, i32>,
    time: f64,
    samples: BTreeMap<String, i32>,
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
            .join("layerstack_conformance/fixtures/internal_arcs")
            .join(&oracle.root),
    );
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    (loaded, stage)
}

/// A layer's file name without its directory.
fn layer_name(loaded: &LoadedStage, layer: layerstack::LayerId) -> String {
    let name = &loaded.layer_names[&layer];
    name.rsplit('/').next().unwrap_or(name).to_string()
}

#[test]
fn prim_stacks_match_openusd() {
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
            .prim_stack(id)
            .expect("composed prim")
            .into_iter()
            .map(|(layer, spec)| {
                (
                    layer_name(&loaded, layer),
                    spec.display(&loaded.store.tokens),
                )
            })
            .collect();
        let divergence = ORDER_DIVERGENCES
            .iter()
            .find(|(path, _)| *path == prim.path);
        if let Some((_, reason)) = divergence {
            let mut sorted = stack.clone();
            sorted.sort();
            let mut expected = prim.prim_stack.clone();
            expected.sort();
            if stack == prim.prim_stack || sorted != expected {
                mismatches.push(format!(
                    "{} is listed in ORDER_DIVERGENCES ({reason})\n    expected the sites {:?} in another order\n    actual   {stack:?}",
                    prim.path, prim.prim_stack
                ));
            }
        } else if stack != prim.prim_stack {
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
    for (attr, expected) in &oracle.samples {
        let property = loaded.store.property_path(attr);
        let value = stage
            .resolve_property_path_at_time(property, oracle.time, InterpolationType::Held)
            .map(|resolved| resolved.value);
        if value != Some(Value::Int(*expected)) {
            mismatches.push(format!(
                "{attr} at {}: expected {expected}, got {value:?}",
                oracle.time
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "values differ from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}
