// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for the children of prims with variant sets.
//!
//! `fixtures/variant_children/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/variant_children/root.usda`
//! (`scripts/variant_children_oracle.py`, which also writes those layers).
//! Layerstack must compose the same prims with the same ordered prim stacks
//! and children, and resolve the same attribute defaults.
//!
//! A prim's children are the names its contributing specs author. A
//! selected variant branch contributes; an unselected one does not, so an
//! `over` inside it neither names a child nor hides one a contributing spec
//! defines:
//!
//! - `/Shape` and `/Scene` keep the child and grandchild their unselected
//!   branches `over`; `/Tree` drops the children only its unselected branch
//!   authors.
//! - `/Lamp` and `/Lantern` reach the base child through a reference and a
//!   payload; `/Boulder` reaches the variant set through a reference.
//! - `/Pine`'s unselected branch is nested in a selected one; `/Pebble`
//!   authors a child in its base and in its selected branch.
//!
//! [`LiveStage`] recomposes a switch between two branches as a full
//! composition does, and as OpenUSD composes the switched stage.
//!
//! Spec: AOUSD Core §10.3.2.5 (variant sets), §11 (stage population);
//! OpenUSD `PcpComposeSiteChildNames` in `pxr/usd/pcp/composeSite.cpp`,
//! `Pcp_ComposeChildNames` in `pxr/usd/pcp/primIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{LiveStage, PathId, Stage, StageOptions, Value};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/variant_children/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, i32>,
    /// The stage after `selections` switch branches.
    switched: Switched,
}

#[derive(Deserialize)]
struct Switched {
    /// `(prim, variant set, variant)` selections the root layer authors.
    selections: Vec<(String, String, String)>,
    prims: Vec<Prim>,
}

#[derive(Deserialize)]
struct Prim {
    path: String,
    prim_stack: Vec<(String, String)>,
    children: Vec<String>,
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

fn options() -> StageOptions {
    StageOptions {
        with_provenance: true,
        ..StageOptions::default()
    }
}

fn load(oracle: &Oracle) -> LoadedStage {
    load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/variant_children")
            .join(&oracle.root),
    )
}

fn compose(oracle: &Oracle) -> (LoadedStage, Stage) {
    let mut loaded = load(oracle);
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options());
    (loaded, stage)
}

/// A layer's file name without its directory.
fn layer_name(loaded: &LoadedStage, layer: layerstack::LayerId) -> String {
    let name = &loaded.layer_names[&layer];
    name.rsplit('/').next().unwrap_or(name).to_string()
}

fn pseudo_root(loaded: &LoadedStage) -> PathId {
    loaded
        .store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("pseudo-root")
}

/// The display paths of the prims `stage` composes, in traversal order.
fn composed_prims(loaded: &LoadedStage, stage: &Stage) -> Vec<String> {
    let root = pseudo_root(loaded);
    stage
        .traverse(root)
        .filter(|prim| *prim != root)
        .map(|prim| loaded.store.paths.display(prim, &loaded.store.tokens))
        .collect()
}

fn children(loaded: &LoadedStage, stage: &Stage, prim: PathId) -> Vec<String> {
    stage
        .children_of(prim)
        .unwrap_or_default()
        .iter()
        .map(|child| {
            let name = loaded.store.paths.resolve(*child).leaf().expect("child");
            loaded.store.tokens.resolve(name).to_string()
        })
        .collect()
}

fn prim_stack(loaded: &LoadedStage, stage: &Stage, prim: PathId) -> Vec<(String, String)> {
    stage
        .explain_prim(prim)
        .expect("composed prim")
        .iter()
        .map(|key| {
            (
                layer_name(loaded, key.layer_id),
                key.spec_path.display(&loaded.store.tokens),
            )
        })
        .collect()
}

#[test]
fn composes_the_prims_openusd_composes() {
    let oracle = oracle();
    let (loaded, stage) = compose(&oracle);
    let expected: Vec<&str> = oracle.prims.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(composed_prims(&loaded, &stage), expected, "composed prims");
}

#[test]
fn prim_stacks_and_children_match_openusd() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let mut mismatches = Vec::new();
    for prim in &oracle.prims {
        let id = loaded.store.path(&prim.path);
        let stack = prim_stack(&loaded, &stage, id);
        if stack != prim.prim_stack {
            mismatches.push(format!(
                "{} prim stack\n    expected {:?}\n    actual   {stack:?}",
                prim.path, prim.prim_stack
            ));
        }
        let children = children(&loaded, &stage, id);
        if children != prim.children {
            mismatches.push(format!(
                "{} children\n    expected {:?}\n    actual   {children:?}",
                prim.path, prim.children
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "prims differ from OpenUSD:\n{}",
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

/// Each prim `stage` composes, with its prim stack and children.
fn snapshot(loaded: &LoadedStage, stage: &Stage) -> Vec<String> {
    let root = pseudo_root(loaded);
    stage
        .traverse(root)
        .filter(|prim| *prim != root)
        .map(|prim| {
            format!(
                "{} {:?} {:?}",
                loaded.store.paths.display(prim, &loaded.store.tokens),
                prim_stack(loaded, stage, prim),
                children(loaded, stage, prim),
            )
        })
        .collect()
}

#[test]
fn switching_branches_recomposes_like_a_full_compose() {
    // Spec: AOUSD Core §10.3.2.5. Switching `/Scene` and `/Tree` between
    // their branches adds and removes the children each branch authors,
    // and leaves the base children in place.
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let root = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root, options());
    let edits = [
        ("/Scene", "look", "Tinted"),
        ("/Tree", "season", "Winter"),
        ("/Scene", "look", "Plain"),
        ("/Tree", "season", "Summer"),
    ];
    for (prim, set, variant) in edits {
        let prim = loaded.store.path(prim);
        let set = loaded.store.tokens.intern(set);
        let variant = loaded.store.tokens.intern(variant);
        loaded
            .store
            .layers
            .get_mut(&root)
            .and_then(|layer| layer.prims.get_mut(&prim))
            .expect("prim spec")
            .variant_selections
            .insert(set, variant);
        live.notify_layer_prim_edits(root, &[prim]);
        live.recompose(&mut loaded.store);
        let fresh = Stage::compose(&mut loaded.store, root, options());
        assert_eq!(snapshot(&loaded, live.stage()), snapshot(&loaded, &fresh));
    }

    // With both switched, the stage matches what OpenUSD composes after the
    // same switch: the unselected branches' children are gone and the
    // selected ones' are composed beside the base children.
    for (prim, set, variant) in &oracle.switched.selections {
        let prim = loaded.store.path(prim);
        let set = loaded.store.tokens.intern(set);
        let variant = loaded.store.tokens.intern(variant);
        loaded
            .store
            .layers
            .get_mut(&root)
            .and_then(|layer| layer.prims.get_mut(&prim))
            .expect("prim spec")
            .variant_selections
            .insert(set, variant);
        live.notify_layer_prim_edits(root, &[prim]);
    }
    live.recompose(&mut loaded.store);
    let expected: Vec<String> = oracle
        .switched
        .prims
        .iter()
        .map(|prim| format!("{} {:?} {:?}", prim.path, prim.prim_stack, prim.children))
        .collect();
    assert_eq!(snapshot(&loaded, live.stage()), expected);
    let fresh = Stage::compose(&mut loaded.store, root, options());
    assert_eq!(snapshot(&loaded, live.stage()), snapshot(&loaded, &fresh));
}
