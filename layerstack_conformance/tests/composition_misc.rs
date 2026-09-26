// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for instance descendants, child order and path
//! expressions.
//!
//! `fixtures/composition_misc/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/composition_misc/root.usda`
//! (`scripts/composition_misc_oracle.py`, which also writes those layers).
//! Layerstack must compose the same prims with the same ordered prim stacks
//! and children, and resolve the same attribute values, at the default
//! time and at numeric times, through resolution and explanation alike:
//!
//! - `/Grove` is an instance of `/Seedling`. The reference, inherit, variant
//!   set and value its local `over "Leaf"` authors never contribute: beneath
//!   an instance only the prototype's arcs do.
//! - `/Row` authors children and `reorder nameChildren` in two layers. Each
//!   layer appends its new children, then reorders the names gathered so
//!   far; `/Column` keeps the children before the first reordered name in
//!   front.
//! - `/Kiln`, and `/Studio`, which references a copy of it, author explicit
//!   inherits, specializes, references and payloads on the prim and on its
//!   selected variant branch, and on a child and on its spec in that
//!   branch. Each node composes the list ops of its own sites, so no
//!   node's explicit list replaces another's.
//! - `/Loom/Frame`, and `/Mill/Frame` through a reference to a copy,
//!   author the same reference, inherit, specializes and payload on the
//!   prim and on its spec in its parent's selected branch. Each node's
//!   reference and payload is an arc of its own, so the referenced sites
//!   compose twice; the class arcs reach a site the prim index already
//!   uses the second time, which OpenUSD adds once.
//! - `/Press`'s selected branch adds a reference, an inherit and a payload
//!   in a sublayer, and deletes them in the root layer: one node's list
//!   ops chain across its layers, deletes included, so none remains there,
//!   while the same reference `/Press` itself adds stays.
//! - `/Punch`, and `/Stamp`'s selected branch, add references, payloads and
//!   inherits in a sublayer, which the root layer only reorders: each list
//!   takes the order its reorder gives, the items it names moved with the
//!   items that follow them.
//! - `/Die`'s references, payloads and inherits from a sublayer are edited
//!   in the root layer with the legacy `add`, of new items and of items the
//!   weaker list holds, and a reorder: an added item the list holds stays
//!   where it is, where an `append` would move it.
//! - `/Sets`, `/Part`, `/Copy` and `/Heir` author path expressions: `%_`
//!   splices in the next weaker expression, and each expression is anchored
//!   at the prim authoring it and mapped through the arcs to the stage
//!   namespace, where a pattern outside a reference's domain drops out.
//!   Character classes, root globs, leading stretches and relative
//!   references each map as OpenUSD maps them; `%_` also composes over
//!   time samples, and over a block.
//!
//! Spec: AOUSD Core §11.3.3 (scene graph instancing), §11 (stage
//! population), §10 (composition arcs map namespace), §10.3.2.5 (a variant
//! branch is a site of its own). OpenUSD:
//! `_ConvertNodeForChild` in `pxr/usd/pcp/primIndex.cpp`,
//! `PcpComposeSiteChildNames` and `PcpComposeSiteInherits` in
//! `pxr/usd/pcp/composeSite.cpp`,
//! `SdfPathExpression::ComposeOver` and `PcpMapFunction::MapSourceToTarget`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{
    Contribution, InterpolationType, OpinionRole, ResolvedValue, Stage, StageOptions, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/composition_misc/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, Option<OracleValue>>,
    /// Values at numeric times, keyed by the time's text.
    values_at_time: BTreeMap<String, BTreeMap<String, Option<OracleValue>>>,
}

#[derive(Deserialize)]
struct Prim {
    path: String,
    prim_stack: Vec<(String, String)>,
    children: Vec<String>,
}

/// A resolved value: a `double`, or a path expression's text.
#[derive(Deserialize)]
#[serde(untagged)]
enum OracleValue {
    Double(f64),
    PathExpression(String),
}

impl OracleValue {
    fn to_value(&self) -> Value {
        match self {
            Self::Double(value) => Value::Double(*value),
            Self::PathExpression(text) => Value::PathExpression(text.as_str().into()),
        }
    }
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
            .join("layerstack_conformance/fixtures/composition_misc")
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
fn prims_and_prim_stacks_match_openusd() {
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
fn children_are_ordered_as_in_openusd() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let mut mismatches = Vec::new();
    for prim in &oracle.prims {
        let id = loaded.store.path(&prim.path);
        let children: Vec<String> = stage
            .children_of(id)
            .unwrap_or_default()
            .iter()
            .map(|child| {
                let name = loaded.store.paths.resolve(*child).leaf().expect("child");
                loaded.store.tokens.resolve(name).to_string()
            })
            .collect();
        if children != prim.children {
            mismatches.push(format!(
                "{}\n    expected {:?}\n    actual   {children:?}",
                prim.path, prim.children
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "child order differs from OpenUSD:\n{}",
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
        let expected = expected.as_ref().map(OracleValue::to_value);
        let value = stage
            .resolve_field_path(property)
            .map(|resolved| resolved.value);
        if value != expected {
            mismatches.push(format!("{attr}: expected {expected:?}, got {value:?}"));
        }
        let explained = stage
            .explain_property_value(property)
            .and_then(|explanation| explanation.value);
        if explained != expected.clone().map(ResolvedValue::Scalar) {
            mismatches.push(format!(
                "{attr}: explained {explained:?}, expected {expected:?}"
            ));
        }
    }
    for (time, values) in &oracle.values_at_time {
        let time: f64 = time.parse().expect("time");
        for (attr, expected) in values {
            let property = loaded.store.property_path(attr);
            let expected = expected.as_ref().map(OracleValue::to_value);
            let value = stage
                .resolve_property_path_at_time(property, time, InterpolationType::Held)
                .map(|resolved| resolved.value);
            if value != expected {
                mismatches.push(format!(
                    "{attr} at {time}: expected {expected:?}, got {value:?}"
                ));
            }
            let explained = stage
                .explain_property_value_at_time(property, time, InterpolationType::Held)
                .and_then(|explanation| explanation.value);
            if explained != expected {
                mismatches.push(format!(
                    "{attr} at {time}: explained {explained:?}, expected {expected:?}"
                ));
            }
        }
    }
    // Only the discarded local arcs of the instance descendant author these.
    for attr in ["/Grove/Leaf.vein", "/Grove/Leaf.moisture"] {
        let property = loaded.store.property_path(attr);
        if stage.has_property_path(property) {
            mismatches.push(format!("{attr}: composed, but OpenUSD discards it"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "values differ from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}

/// The weaker opinion a `%_` splices in contributes to the explained value,
/// at the default time and at a numeric time; a block that ends the fold is
/// explained as the block.
#[test]
fn explanations_credit_the_weaker_expressions() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let contributing = |explained: &[layerstack::ExplainedOpinion<'_>]| {
        explained
            .iter()
            .filter(|opinion| opinion.role == OpinionRole::Contributed(Contribution::Value))
            .count()
    };

    let plus = loaded.store.property_path("/Part.plus");
    let explained = stage.explain_property_value(plus).expect("authored");
    assert_eq!(contributing(&explained.opinions), 2, "{explained:?}");
    let explained = stage
        .explain_property_value_at_time(plus, 1.0, InterpolationType::Held)
        .expect("authored");
    assert_eq!(contributing(&explained.opinions), 2, "{explained:?}");

    let sampled = loaded.store.property_path("/Sets.sampled");
    let explained = stage
        .explain_property_value_at_time(sampled, 5.0, InterpolationType::Held)
        .expect("authored");
    assert_eq!(contributing(&explained.opinions), 2, "{explained:?}");

    let blocked = loaded.store.property_path("/Sets.blocked");
    let explained = stage.explain_property_value(blocked).expect("authored");
    assert_eq!(explained.value, None);
    assert!(
        explained
            .opinions
            .iter()
            .any(|opinion| opinion.role == OpinionRole::Block),
        "{explained:?}"
    );
}
