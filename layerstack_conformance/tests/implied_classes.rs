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
//! - `/Shade` references `/Bank/Sprout`, where `/_class_Moss/Sprout` is
//!   implied beneath `/Bank`'s reference from `bed.usda`; the implied class
//!   keeps its origin, so `/Bank`'s authored inherit outranks it. `/Shore`
//!   is the same case with the classes declared in the other order.
//! - `/Branch/SymTwig/Bud` and `/Branch/LeftTwig/Bud` reach
//!   `bough.usda /Bough/_class_Twig/Leaf` beneath the inherit
//!   `/Branch/_class_Twig/Bud` authors and beneath the class implied from
//!   it. OpenUSD adds an implied class after its origin's subtree and skips
//!   the site the second time, so the leaf stays beneath the authored
//!   inherit and ranks after `Bud`.
//! - `/Beacon` references `beacon.usda /Beacon`, which inherits
//!   `/_class_Beacon` and selects `lens = "clear"`. The class implied into
//!   `root.usda` selects `tinted`: the referenced variant set is selected
//!   once the class is implied, so the `tinted` branch and the reference it
//!   authors compose. Edits to the class's selection recompose, through a
//!   [`LiveStage`], as a full composition does.
//!
//! Spec: AOUSD Core §10.4.2.4 (implied class arcs), §10.3.2.5 (variant
//! selection); OpenUSD `_EvalImpliedClasses` and `_EvalNodeVariantSets`
//! in `pxr/usd/pcp/primIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{LiveStage, Stage, StageOptions, Value};
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
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options());
    (loaded, stage)
}

fn options() -> StageOptions {
    StageOptions {
        with_provenance: true,
        ..StageOptions::default()
    }
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

#[test]
fn implied_classes_reached_through_an_arc_keep_their_origins() {
    // `/Shade` and `/Shore` reach the implied class of `/Bank/Sprout` and
    // `/Marsh/Sprout` through a reference: it is implied from the class
    // `bed.usda` authors, and ranks by where that origin sits.
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    for prim in ["/Shade", "/Shore"] {
        let id = loaded.store.path(prim);
        let graph = stage.explain_prim_graph(id).expect("composed prim");
        let implied: Vec<_> = graph
            .nodes()
            .filter(|(_, node)| node.is_implied())
            .collect();
        assert!(!implied.is_empty(), "{prim} has an implied class");
        for (id, node) in implied {
            let origin = node
                .origin()
                .and_then(|origin| graph.node(origin))
                .unwrap_or_else(|| panic!("{prim}: implied node {id:?} has no origin"));
            assert_eq!(
                layer_name(&loaded, origin.layer_stack()),
                "bed.usda",
                "{prim}: implied node {id:?} is implied from `bed.usda`"
            );
        }
    }
}

/// The prim stacks and `int` values of every prim of `stage`, in traversal
/// order.
fn snapshot(loaded: &mut LoadedStage, stage: &Stage) -> Vec<String> {
    let pseudo_root = loaded.store.path("/");
    let mut lines = Vec::new();
    for prim in stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
    {
        let stack: Vec<String> = stage
            .explain_prim(prim)
            .expect("composed prim")
            .iter()
            .map(|key| {
                format!(
                    "{} {}",
                    layer_name(loaded, key.layer_id),
                    key.spec_path.display(&loaded.store.tokens)
                )
            })
            .collect();
        let path = loaded.store.paths.display(prim, &loaded.store.tokens);
        lines.push(format!("{path}: {stack:?}"));
    }
    lines
}

/// Sets the selection of `set` authored on `prim` in the layer `layer` (a
/// file name), and recomposes `live`, which must then match a full
/// composition. A selection that adds children is a structural change
/// (see [`LiveStage::recompose`]).
fn select(
    live: &mut LiveStage,
    loaded: &mut LoadedStage,
    (layer, prim): (&str, &str),
    (set, variant): (&str, &str),
    adds_children: bool,
) {
    let layer = *loaded
        .layer_names
        .iter()
        .find(|(_, name)| name.rsplit('/').next() == Some(layer))
        .expect("layer")
        .0;
    let prim = loaded.store.path(prim);
    let set = loaded.store.tokens.intern(set);
    let variant = loaded.store.tokens.intern(variant);
    loaded
        .store
        .layers
        .get_mut(&layer)
        .and_then(|layer| layer.prims.get_mut(&prim))
        .expect("prim spec")
        .variant_selections
        .insert(set, variant);
    if adds_children {
        live.notify_structural_change();
    } else {
        live.notify_layer_prim_edits(layer, &[prim]);
    }
    live.recompose(&mut loaded.store);
    let fresh = Stage::compose(&mut loaded.store, loaded.root_layer, options());
    assert_eq!(
        snapshot(loaded, live.stage()),
        snapshot(loaded, &fresh),
        "recomposed after selecting {variant:?}"
    );
}

#[test]
fn selection_edits_recompose_like_a_full_composition() {
    // Spec: AOUSD Core §10.3.2.5, §10.4.2.4. `/Beacon` selects its
    // referenced variant set from its complete prim index, the class
    // implied into this layer stack included: editing the class's
    // selection recomposes as a full composition does.
    let oracle = oracle();
    let (mut loaded, _) = compose(&oracle);
    let mut live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options());
    let shade = loaded.store.property_path("/Beacon.shade");
    let filter = loaded.store.path("/Beacon/Filter");
    let resolved = |live: &LiveStage| {
        live.stage()
            .resolve_field_path(shade)
            .map(|resolved| resolved.value)
    };
    assert_eq!(resolved(&live), Some(Value::Int(2)));
    let class = ("root.usda", "/_class_Beacon");
    select(&mut live, &mut loaded, class, ("lens", "clear"), false);
    assert_eq!(resolved(&live), Some(Value::Int(1)));
    assert!(!live.stage().has_prim(filter));
    select(&mut live, &mut loaded, class, ("lens", "tinted"), true);
    assert_eq!(resolved(&live), Some(Value::Int(2)));
    assert!(live.stage().has_prim(filter));
}
