// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for variant sets nested in other branches.
//!
//! `fixtures/variant_specs/oracle.json` records what OpenUSD 26.08 composes
//! from `fixtures/variant_specs/root.usda` (`scripts/variant_specs_oracle.py`,
//! which also writes those layers). Layerstack must compose the same prims,
//! in the same child order, with the same ordered prim stacks and variant
//! selections, and resolve the same attribute defaults.
//!
//! Every variant is a spec of its own (`/P{a=x}`, `/P{a=x}{b=y}`), and a
//! variant set is a child of a prim spec or of a variant spec, so nesting is
//! per branch:
//!
//! - `/Stone` nests `grain` in `finish=rough` and another `finish` set in
//!   `grain=coarse`: the outer `{finish=rough}` and the inner one are
//!   separate specs, each with its own node.
//! - `/Oak` and `/Elm` inherit `/_class_Tree`, whose `canopy` set is nested
//!   under both `season` branches with different contents; each takes the
//!   one its `season` selects.
//! - `/River` nests three levels, each branch selecting the set nested in
//!   it.
//! - `/Grove` and `/Copse` reference `model.usda /Model`, whose same-named
//!   inner sets differ by outer branch; `/Grove` overrides the inner
//!   selection.
//! - `/Plain` and `/Twill` select branches of an inherited class that
//!   declare the nested sets `[warp, weft]` and `[weft, warp]`; `/Loop`,
//!   `/Hitch` and `/Bend` three sets in three orders. Each nested set ranks
//!   by its position in the `variantSets` of the branch declaring it, so
//!   each prim's prim stack and value follow its own branch's order.
//!
//! Spec: AOUSD Core §7.3.6 (variant specs may contain variant set specs),
//! §10.3.2.5 (variants). OpenUSD: `SdfVariantSetSpec` and `SdfVariantSpec`
//! (`pxr/usd/sdf/variantSetSpec.h`, `pxr/usd/sdf/variantSpec.h`),
//! `_AddVariantArc` in `pxr/usd/pcp/primIndex.cpp` (the sibling number of a
//! variant arc is its set's index at the node it is added beneath).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{Stage, StageOptions, Value};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/variant_specs/oracle.json");

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
    selections: BTreeMap<String, String>,
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
            .join("layerstack_conformance/fixtures/variant_specs")
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

/// The prims compose in OpenUSD's traversal order: the children of every
/// branch the selections reach, and of no other.
#[test]
fn prims_compose_in_order() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let pseudo_root = loaded.store.path("/");
    let composed: Vec<String> = stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .map(|prim| loaded.store.paths.display(prim, &loaded.store.tokens))
        .collect();
    let expected: Vec<&str> = oracle.prims.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(composed, expected);
}

#[test]
fn prim_stacks_hold_every_nested_variant_spec() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
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
fn variant_selections_match_openusd() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let mut mismatches = Vec::new();
    for prim in &oracle.prims {
        let id = loaded.store.path(&prim.path);
        let selections: BTreeMap<String, String> = stage
            .variant_selections(id, &loaded.store)
            .into_iter()
            .map(|(set, variant)| {
                (
                    loaded.store.tokens.resolve(set).to_string(),
                    loaded.store.tokens.resolve(variant).to_string(),
                )
            })
            .collect();
        if selections != prim.selections {
            mismatches.push(format!(
                "{}: expected {:?}, got {selections:?}",
                prim.path, prim.selections
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "selections differ from OpenUSD:\n{}",
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

/// Two branches declaring the same nested sets in opposite orders do not
/// interfere: each prim takes the opinion of the set its own branch lists
/// first, as OpenUSD 26.08 does (`/Plain.thread = 1`, `/Twill.thread = 4`),
/// and three sets in three orders likewise. The prim stacks are compared
/// exactly by `prim_stacks_hold_every_nested_variant_spec`.
#[test]
fn nested_sets_rank_by_the_branch_declaring_them() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    for (attr, expected) in [
        ("/Plain.thread", 1),
        ("/Twill.thread", 4),
        ("/Loop.strand", 1),
        ("/Hitch.strand", 6),
        ("/Bend.strand", 8),
    ] {
        assert_eq!(oracle.values.get(attr), Some(&expected), "oracle {attr}");
        let property = loaded.store.property_path(attr);
        assert_eq!(
            stage
                .resolve_field_path(property)
                .map(|resolved| resolved.value),
            Some(Value::Int(expected)),
            "{attr}"
        );
    }
}
