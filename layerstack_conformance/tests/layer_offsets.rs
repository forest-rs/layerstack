// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for arcs authored in sublayers with offsets.
//!
//! `fixtures/layer_offsets/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/layer_offsets/root.usda`
//! (`scripts/layer_offsets_oracle.py`, which also writes those layers).
//! Layerstack must compose the same prims with the same prim stacks,
//! resolve the same attribute defaults and time samples, and explain each
//! sampled attribute's strongest opinion with the same offset.
//!
//! A reference or payload to another layer stack is read on the timeline
//! of the layer that authors it: the authoring layer's offset within its
//! layer stack applies first, then the arc's own offset.
//!
//! - `/Comet` references `orbit.usda /Orbit` from `shot.usda`, a sublayer
//!   with offset 10, with offset 5 of its own.
//! - `/Satellite` has a payload authored in `timing.usda`, a sublayer with
//!   offset 4 and scale 2, with offset 1 and scale 3 of its own.
//! - `/Station` references `hub.usda /Hub`, whose sublayer, with an offset
//!   and scale, authors a nested reference.
//! - `/Probe` references `system.usda /System/Planet`; `/System`'s
//!   reference is authored in a sublayer with an offset and scale, and
//!   reaches the target as an ancestral arc.
//! - `/Echo`'s internal reference and `/Flare`'s inherits, both authored in
//!   `shot.usda`, read their targets in the stage's layer stack, whose
//!   layers keep their own offsets: `shot.usda`'s offset does not apply.
//!
//! Spec: AOUSD Core §12.3.2.1 (layer offsets on sublayers, references and
//! payloads), §10.3.1.1 (offsets compose along a chain); OpenUSD
//! `_EvalRefOrPayloadArcs` in `pxr/usd/pcp/primIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{InterpolationType, LayerOffset, Stage, StageOptions, Value};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/layer_offsets/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, i32>,
    /// `(time, value)` probes of each time-sampled attribute.
    samples: BTreeMap<String, Vec<(f64, f64)>>,
    /// `(offset, scale)` from the layer of each time-sampled attribute's
    /// strongest opinion to the stage.
    offsets: BTreeMap<String, (f64, f64)>,
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
            .join("layerstack_conformance/fixtures/layer_offsets")
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
fn prim_stacks_and_values_match_openusd() {
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
        "composition differs from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn time_samples_take_the_authoring_sublayer_offset() {
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

/// The explanation of a sampled value names the offset its strongest
/// opinion is read with: every arc's offset and every sublayer's, the
/// authoring sublayer's included.
#[test]
fn explanations_report_the_combined_offset() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let mut mismatches = Vec::new();
    for (attr, &(offset, scale)) in &oracle.offsets {
        let property = loaded.store.property_path(attr);
        let (time, _) = oracle.samples[attr][0];
        let explanation = stage
            .explain_property_value_at_time(property, time, InterpolationType::Linear)
            .expect("explained");
        let strongest = explanation.contributors().next().expect("a contributor");
        let expected = LayerOffset { offset, scale };
        if strongest.layer_offset() != expected {
            mismatches.push(format!(
                "{attr}: expected {expected:?}, got {:?}",
                strongest.layer_offset()
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "explained offsets differ from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}
