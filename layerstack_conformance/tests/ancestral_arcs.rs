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
//!   `/Quarry/Stone` inherits `/Quarry/_class_Rock`, outside the subroot
//!   target; the class maps across `/Zeta`'s reference as authored, and is
//!   implied as `/Zeta/_class_Rock`.
//! - `/Clock` references, and `/Stopwatch` has a payload to, a subroot
//!   target whose parent references, or has a payload to, a prim of a third
//!   layer stack, each arc with its own offset and scale, and one of the
//!   sampled attributes authored in a sublayer with an offset: every
//!   transform applies once (AOUSD Core §12.3.2.1).
//! - `/Vineyard/Cane/Bud` reaches `trellis.usda`'s `/Row/Cane/Bud`,
//!   `/Cane/Bud` and `/Bud` through arcs authored on `vine.usda /Vine` and
//!   its descendants, each site declaring the variant set `bloom` with a
//!   selection of its own. The variant sets are selected once the prim
//!   index holds every site, so `/Bud`, the strongest, selects `open` for
//!   all three. Edits to those selections recompose, through a
//!   [`LiveStage`], as a full composition does.
//! - `/Hut/Loft` references `loft.usda /Hut`, which has the same name as
//!   the stage's `/Hut`. Its target paths map once, into `/Hut/Loft`,
//!   whichever arc brings them: the reference; a class the target inherits,
//!   itself or from the branch of its variant set added once the prim
//!   index is complete; a reference that branch authors; a class a child
//!   specializes; a prim a child references internally. A path beneath
//!   the internal reference's destination does not map back through it,
//!   and is dropped and reported.
//!
//! Spec: AOUSD Core §10.2 and §10.4; OpenUSD `_AddArc` with
//! `includeAncestralOpinions` and `_BuildInitialPrimIndexFromAncestor` in
//! `pxr/usd/pcp/primIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{CompositionError, InterpolationType, LiveStage, Stage, StageOptions, Value};
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
    /// The target paths of each relationship and connected attribute.
    targets: BTreeMap<String, Vec<String>>,
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
fn targets_match_openusd() {
    // Spec: AOUSD Core §12.4 (target paths map through the arcs of their
    // opinions, once).
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    assert!(!oracle.targets.is_empty(), "the oracle records targets");
    let mut mismatches = Vec::new();
    for (property, expected) in &oracle.targets {
        let path = loaded.store.property_path(property);
        let targets: Vec<String> = stage
            .resolve_target_list_path(path)
            .map(|resolved| resolved.value)
            .unwrap_or_default()
            .iter()
            .map(|target| target.display(&loaded.store.paths, &loaded.store.tokens))
            .collect();
        if &targets != expected {
            mismatches.push(format!(
                "{property}: expected {expected:?}, got {targets:?}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "targets differ from OpenUSD:\n{}",
        mismatches.join("\n")
    );

    // The one path OpenUSD drops is reported.
    let shelf = loaded.store.path("/Hut/Loft/Shelf");
    let stray = loaded.store.tokens.intern("stray");
    let dropped: Vec<_> = stage
        .composition_errors()
        .iter()
        .filter_map(|error| match error {
            CompositionError::InvalidExternalTargetPath(error) => {
                Some((error.prim, error.property))
            }
            _ => None,
        })
        .collect();
    assert_eq!(dropped, [(shelf, stray)]);
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
    // Spec: AOUSD Core §10.3.2.5. `/Vineyard/Cane/Bud` selects the variants
    // of the sites its ancestors' arcs reach from its complete prim index:
    // editing the strongest selection, or authoring a stronger one,
    // recomposes as a full composition does.
    let oracle = oracle();
    let (mut loaded, _) = compose(&oracle);
    let mut live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options());
    let petals = loaded.store.property_path("/Vineyard/Cane/Bud.petals");
    let resolved = |live: &LiveStage| {
        live.stage()
            .resolve_field_path(petals)
            .map(|resolved| resolved.value)
    };
    assert_eq!(resolved(&live), Some(Value::Int(5)));
    let petal = loaded.store.path("/Vineyard/Cane/Bud/Petal");
    // A weaker selection changes nothing.
    let cane = ("trellis.usda", "/Cane/Bud");
    select(&mut live, &mut loaded, cane, ("bloom", "open"), false);
    assert_eq!(resolved(&live), Some(Value::Int(5)));
    // The strongest selection picks every site's branch.
    let bud = ("trellis.usda", "/Bud");
    select(&mut live, &mut loaded, bud, ("bloom", "dormant"), false);
    assert_eq!(resolved(&live), Some(Value::Int(0)));
    assert!(!live.stage().has_prim(petal));
    // A stronger site selects over it.
    let vine = ("vine.usda", "/Vine/Cane/Bud");
    select(&mut live, &mut loaded, vine, ("bloom", "open"), true);
    assert_eq!(resolved(&live), Some(Value::Int(5)));
    assert!(live.stage().has_prim(petal));
}
