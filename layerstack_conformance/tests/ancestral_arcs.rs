// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for arcs to subroot targets.
//!
//! `fixtures/ancestral_arcs/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/ancestral_arcs/root.usda`
//! (`scripts/ancestral_arcs_oracle.py`, which also writes those layers).
//! Layerstack must compose the same prims with the same ordered prim stacks
//! and resolve the same attribute defaults.
//!
//! An arc to a subroot target reads the target's prim index, which starts
//! from its parent's, so the arcs and variant selections authored on the
//! target's ancestors contribute beneath the arc's node:
//!
//! - `/Orchard` references `grove.usda /Grove/Tree`, whose parent references
//!   `forest.usda /Forest`, and so reaches `/Forest/Tree` and its child;
//!   `/Twig` reaches `/Forest/Tree/Branch` from two levels further down.
//! - `/Meadow` references `field.usda /Field/Patch`, whose parent selects
//!   `season = "summer"`: the branch's opinions for `Patch` and the
//!   reference it authors contribute, whatever `/Meadow` selects.
//! - `/Outcrop` references `cliff.usda /Cliff/Ledge`, whose parent inherits
//!   `/_class_Cliff`; the class `/_class_Cliff/Ledge` is implied into the
//!   root layer stack, whose opinion wins.
//! - `/Crater` has a payload to `basin.usda /Basin/Pool`, whose parent
//!   references `/Lake` of the same layer.
//! - `/Alpha` references `/Zeta/Stone`, which only `/Zeta`'s reference to
//!   `/Quarry` provides, and which comes after `/Alpha` in namespace order.
//! - `/Clock` references, and `/Stopwatch` has a payload to, a subroot
//!   target whose parent references, or has a payload to, a prim of a third
//!   layer stack, each arc with its own offset and scale, and one of the
//!   sampled attributes authored in a sublayer with an offset: every
//!   transform applies once (AOUSD Core §12.3.2.1).
//!
//! Spec: AOUSD Core §10.2 and §10.4; OpenUSD `_AddArc` with
//! `includeAncestralOpinions` and `_BuildInitialPrimIndexFromAncestor` in
//! `pxr/usd/pcp/primIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{InterpolationType, Stage, StageOptions, Value};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/ancestral_arcs/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, i32>,
    /// `(time, value)` probes of each time-sampled attribute.
    samples: BTreeMap<String, Vec<(f64, f64)>>,
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
            .join("layerstack_conformance/fixtures/ancestral_arcs")
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
fn prim_stacks_include_the_ancestral_arcs_of_subroot_targets() {
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

#[test]
fn time_samples_compose_every_enclosing_offset() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    assert!(
        !oracle.samples.is_empty(),
        "the oracle records time samples"
    );
    let mut mismatches = Vec::new();
    for (attr, probes) in &oracle.samples {
        let property = loaded.store.property_path(attr);
        for &(time, expected) in probes {
            let value = stage
                .resolve_property_path_at_time(property, time, InterpolationType::Linear)
                .map(|resolved| resolved.value);
            if value != Some(Value::Double(expected)) {
                mismatches.push(format!(
                    "{attr} at {time}: expected {expected}, got {value:?}"
                ));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "time samples differ from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}
