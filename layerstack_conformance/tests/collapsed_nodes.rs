// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for sites reached more than once.
//!
//! `fixtures/collapsed_nodes/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/collapsed_nodes/root.usda`
//! (`scripts/collapsed_nodes_oracle.py`, which also writes those layers).
//! Layerstack must compose the same prims with the same ordered prim stacks,
//! repeats included, and resolve the same attribute values.
//!
//! A prim index has one node per arc occurrence, so a site reached twice
//! contributes twice, each time with the offset of its own arc path:
//!
//! - `/Junction` references `left.usda /Left` and `right.usda /Right`,
//!   which both reference `source.usda /Source` with different offsets;
//!   `/Delta` reaches the same diamond through payloads.
//! - `/Echo` references `pulse.usda /Pulse` three times with different
//!   offsets: three nodes, the first strongest.
//! - `/Orbit/Moon`'s own payload to `moon.usda /Moon` is stronger than the
//!   same payload reached beneath `/Orbit`'s payload, so `depth` resolves
//!   from `moon.usda`.
//!
//! A [`LiveStage`] recomposes every occurrence of an edited site, and its
//! scoped recomposition matches a full composition.
//!
//! Spec: AOUSD Core §10.4 (LIVERPS strength ordering, applied recursively
//! within each arc's target) and §12.3.2.1 (layer offsets); OpenUSD
//! `_AddArc` in `pxr/usd/pcp/primIndex.cpp`, which skips a site already in
//! the graph only for class-based arcs (`skipDuplicateNodes`).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::property::get_property_mut;
use layerstack::{
    InterpolationType, LayerId, ListOp, LiveStage, PathId, Stage, StageOptions, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/collapsed_nodes/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, i32>,
    /// `(time, value)` probes of each time-sampled attribute.
    samples: BTreeMap<String, Vec<(f64, f64)>>,
    /// `(layer, spec path, offset, scale)` of each spec of each
    /// time-sampled attribute, strongest first.
    property_stacks: BTreeMap<String, Vec<(String, String, f64, f64)>>,
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

fn options() -> StageOptions {
    StageOptions {
        with_provenance: true,
        ..StageOptions::default()
    }
}

fn load(oracle: &Oracle) -> LoadedStage {
    load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/collapsed_nodes")
            .join(&oracle.root),
    )
}

fn compose(oracle: &Oracle) -> (LoadedStage, Stage) {
    let mut loaded = load(oracle);
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options());
    (loaded, stage)
}

/// A layer's file name without its directory.
fn layer_name(loaded: &LoadedStage, layer: LayerId) -> String {
    let name = &loaded.layer_names[&layer];
    name.rsplit('/').next().unwrap_or(name).to_string()
}

/// The layer named `name` (a file name).
fn layer_id(loaded: &LoadedStage, name: &str) -> LayerId {
    *loaded
        .layer_names
        .iter()
        .find(|(_, layer)| layer.rsplit('/').next() == Some(name))
        .expect("layer")
        .0
}

#[test]
fn prim_stacks_repeat_each_occurrence_of_a_site() {
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
fn each_occurrence_keeps_its_own_layer_offset() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    assert!(
        !oracle.property_stacks.is_empty(),
        "the oracle records property stacks"
    );
    let mut mismatches = Vec::new();
    for (attr, expected) in &oracle.property_stacks {
        let property = loaded.store.property_path(attr);
        let stack: Vec<(String, String, f64, f64)> = stage
            .explain_property_path(property)
            .unwrap_or_default()
            .iter()
            .map(|opinion| {
                (
                    layer_name(&loaded, opinion.key.layer_id),
                    opinion.key.spec_path.display(&loaded.store.tokens),
                    opinion.layer_offset.offset,
                    opinion.layer_offset.scale,
                )
            })
            .collect();
        if &stack != expected {
            mismatches.push(format!(
                "{attr}\n    expected {expected:?}\n    actual   {stack:?}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "property stacks differ from OpenUSD:\n{}",
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
fn time_samples_use_the_strongest_occurrence_offset() {
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

/// Every prim `stage` composes, in traversal order, with its children and
/// its prim stack and property stacks, each opinion with its layer offset.
fn snapshot(loaded: &mut LoadedStage, stage: &Stage) -> Vec<String> {
    let pseudo_root = loaded.store.path("/");
    let prims: Vec<PathId> = stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .collect();
    let mut lines = Vec::new();
    for prim in prims {
        let path = loaded.store.paths.display(prim, &loaded.store.tokens);
        let children: Vec<String> = stage
            .children_of(prim)
            .unwrap_or_default()
            .iter()
            .map(|child| loaded.store.paths.display(*child, &loaded.store.tokens))
            .collect();
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
        lines.push(format!("{path} {stack:?} {children:?}"));
    }
    for attr in ["/Junction/Tap.flow", "/Delta/Tap.flow", "/Junction.width"] {
        let property = loaded.store.property_path(attr);
        let stack: Vec<String> = stage
            .explain_property_path(property)
            .unwrap_or_default()
            .iter()
            .map(|opinion| {
                format!(
                    "{} {:?}",
                    layer_name(loaded, opinion.key.layer_id),
                    opinion.layer_offset
                )
            })
            .collect();
        let value = stage
            .resolve_field_path(property)
            .map(|resolved| resolved.value);
        lines.push(format!("{attr} {stack:?} {value:?}"));
    }
    lines
}

#[test]
fn edits_to_a_shared_site_recompose_every_occurrence() {
    // Spec: AOUSD Core §10.4. `source.usda /Source` is two nodes of both
    // `/Junction` and `/Delta`; an edit there reaches every prim reading
    // either node, and the scoped recomposition matches a full one.
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let root = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root, options());
    let source = layer_id(&loaded, "source.usda");

    for (prim, attr, value, expected) in [
        ("/Source/Tap", "flow", 42, ["/Junction/Tap", "/Delta/Tap"]),
        ("/Source", "width", 7, ["/Junction", "/Delta"]),
    ] {
        let prim = loaded.store.path(prim);
        let name = loaded.store.tokens.intern(attr);
        let spec = loaded
            .store
            .layers
            .get_mut(&source)
            .and_then(|layer| layer.prims.get_mut(&prim))
            .expect("prim spec");
        get_property_mut(&mut spec.properties, name)
            .expect("property")
            .default = Some(Value::Int(value));
        live.notify_layer_prim_edits(source, &[prim]);
        let updated = live.recompose(&mut loaded.store);
        for path in expected {
            let path = loaded.store.path(path);
            assert!(updated.contains(&path), "the edit recomposes {path:?}");
        }
        let fresh = Stage::compose(&mut loaded.store, root, options());
        assert_eq!(
            snapshot(&mut loaded, live.stage()),
            snapshot(&mut loaded, &fresh)
        );
    }
    for attr in ["/Junction/Tap.flow", "/Delta/Tap.flow"] {
        let property = loaded.store.property_path(attr);
        assert_eq!(
            live.stage()
                .resolve_field_path(property)
                .map(|resolved| resolved.value),
            Some(Value::Int(42)),
            "{attr}"
        );
    }

    // Dropping `/Right`'s reference leaves one occurrence of `/Source`.
    let right = layer_id(&loaded, "right.usda");
    let right_prim = loaded.store.path("/Right");
    loaded
        .store
        .layers
        .get_mut(&right)
        .and_then(|layer| layer.prims.get_mut(&right_prim))
        .expect("prim spec")
        .references = ListOp::default();
    live.notify_layer_prim_edits(right, &[right_prim]);
    live.recompose(&mut loaded.store);
    let fresh = Stage::compose(&mut loaded.store, root, options());
    assert_eq!(
        snapshot(&mut loaded, live.stage()),
        snapshot(&mut loaded, &fresh)
    );
    let tap = loaded.store.path("/Junction/Tap");
    assert_eq!(live.stage().explain_prim(tap).map(<[_]>::len), Some(1));
}
