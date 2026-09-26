// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for relocates.
//!
//! `fixtures/relocates/oracle.json` records what OpenUSD 26.08 composes from
//! `fixtures/relocates/root.usda` (`scripts/relocates_oracle.py`, which also
//! writes those layers). Layerstack must compose the same prims with the
//! same ordered prim stacks, resolve the same attribute defaults and report
//! the same composition errors.
//!
//! A layer's `relocates` metadata moves a prim of its layer stack to a new
//! path. The relocated prim composes at the target from the target's own
//! opinions and the ancestral opinions of its source, and the source no
//! longer exists:
//!
//! - `/Garden/Bed`, which `/Garden` inherits from `/_class_Garden`, composes
//!   at `/Garden/Plot`.
//! - `/Robot/Rig/Controls/Wrist`, which the reference to `arm.usda` brings,
//!   composes at `/Robot/Anim/Wrist` with the class it inherits implied as
//!   `/Robot/_class_Joint`; `root.usda`'s opinions at the source are
//!   ignored and reported, and `/Robot/Rig/Debug`, relocated to nothing, is
//!   gone.
//! - A reference to the source `/Robot/Rig/Controls/Wrist` contributes
//!   nothing and is reported; one to the target composes the relocated prim.
//! - `kit.usda` relocates `/Kit/Parts/Gear`, which its own reference to
//!   `parts.usda` brings, to `/Kit/Gear`; `/Crate` sees it through its
//!   reference to `kit.usda`.
//! - Relationship targets authored in `arm.usda` and `parts.usda` map
//!   through those relocates; the one to the removed `/Robot/Rig/Debug`
//!   keeps its path.
//! - `quarry.usda` relocates `/Stone/Chip` to `/Stone/Flake`: `/Cobble`,
//!   whose selected variant branch references it, has the chip at
//!   `/Cobble/Flake`, and `/Pebble`, whose unselected branch does, keeps
//!   it at `/Pebble/Chip`.
//! - A prim relocated beneath a parent that does not exist otherwise,
//!   `/Nowhere`, is not composed; beneath `/Heap`, which has a spec, and
//!   `/Bin/Tray`, which a reference brings, it is.
//! - `puppet.usda` relocates the thumb and the tips of both hands that its
//!   reference to `strings.usda` brings. Through `/Marionette`, the
//!   relocated prims keep the classes of their sources' namespace, implied
//!   into `root.usda` (OpenUSD's "spooky" inherits): `/_class_Puppet`,
//!   which `/Puppet` inherits, and `/Marionette/Strings/SymHand`, which both
//!   hands and the thumb inherit. That class references `glove.usda`, and
//!   the variant selection `root.usda` authors on it applies to the
//!   relocated thumb and left tip; the right hand selects its own.
//! - `kite.usda` relocates `/Kite/Tail`, which its reference to
//!   `frame.usda` brings, to `/Kite/Streamer`, and `root.usda` relocates
//!   both paths again through `/Flyer`: `/Ribbon` composes the relocated
//!   tail, and `/Knot`, relocated from a relocation source, composes
//!   nothing and is reported. `kite.usda`'s opinion at its source is
//!   reported on `/Ribbon` and on `/Bowline`, which references beneath it,
//!   as `root.usda`'s is on `/SpareWrist`, which references
//!   `/Robot/Anim/Wrist`: each prim index computes the relocated prim's
//!   index afresh.
//! - `/Chain` reaches `/Chain_2/Tail`, a relocation source, through two
//!   internal references, so relocating `/Chain/Tail` too composes nothing
//!   at `/Chain/Tail_1`, not even `/Chain_1/Tail`.
//! - `fleet.usda` relocates `/Fleet/Crew/Sailor`, which its reference to
//!   `crew.usda` brings, to `/Fleet/Deck/Sailor`. `/Deck`'s subroot
//!   reference to `/Fleet/Deck` maps the target but not the source, and
//!   `/Deck/Sailor` still composes the source's ancestral opinions, with
//!   the class the sailor inherits and the class implied from it into
//!   `fleet.usda`.
//! - `/Belltower/Floor/Bell` is relocated to `/Belltower/Bell`: the
//!   variant branch `/Belltower/Floor` selects authors ancestral opinions
//!   of the source, which compose at the target, while the source's own
//!   spec and variant branch are ignored. `/Belltower/Ringer` inherits
//!   beneath the relocated prim. That is the stage's own relocation.
//! - `kiln.usda` relocates `/Kiln/Chamber/Tray`, which its reference to
//!   `clay.usda` brings, to `/Kiln/Shelf`, and authors opinions at the
//!   source inside the variant branch `/Kiln/Chamber` selects, which also
//!   references `glaze.usda`. Every arc reaching the relocated tray
//!   composes both: `/Oven` references `/Kiln`, `/Forge` references
//!   `/Oven`, `/Stove` payloads `/Kiln`, and `/Rack` references the
//!   relocated `/Kiln/Shelf`. Their target paths map through the arcs above
//!   the relocate node only; `/Rack`'s is outside its target and reported.
//!
//! Composition errors are compared by kind and the composed prim whose
//! prim index reports them.
//!
//! The same scene recomposed by a [`LiveStage`] after opinion edits at
//! relocation targets and sources and in the classes implied through them,
//! after edits to the relocates themselves, and after switching variant
//! selections, must match a full composition.
//!
//! Spec: AOUSD Core §10.3.2.6 (relocates), §10.4.2.4 (implied classes);
//! OpenUSD `_EvalNodeRelocations`, `_EvalImpliedRelocations` and
//! `_EvalImpliedClassTree` in `pxr/usd/pcp/primIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::property::get_property_mut;
use layerstack::{
    CompositionError, LayerId, LayerStore, LiveStage, PathId, Relocate, Stage, StageOptions, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/relocates/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
    values: BTreeMap<String, i32>,
    targets: BTreeMap<String, Vec<String>>,
    errors: Vec<ErrorRecord>,
}

#[derive(Deserialize)]
struct Prim {
    path: String,
    prim_stack: Vec<(String, String)>,
}

#[derive(Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ErrorRecord {
    kind: String,
    prim: String,
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
            .join("layerstack_conformance/fixtures/relocates")
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

/// Every composed prim, in traversal order, with its prim stack and
/// children.
fn composed(loaded: &mut LoadedStage, stage: &Stage) -> Vec<(String, Vec<(String, String)>)> {
    let pseudo_root = loaded.store.path("/");
    stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .map(|prim| {
            let children: Vec<String> = stage
                .children_of(prim)
                .unwrap_or_default()
                .iter()
                .map(|child| loaded.store.paths.display(*child, &loaded.store.tokens))
                .collect();
            let path = loaded.store.paths.display(prim, &loaded.store.tokens);
            let mut stack = prim_stack(loaded, stage, prim);
            stack.push(("children".into(), children.join(" ")));
            (path, stack)
        })
        .collect()
}

#[test]
fn prim_stacks_compose_relocated_prims_at_their_targets() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let pseudo_root = loaded.store.path("/");
    let mut paths: Vec<String> = stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .map(|prim| loaded.store.paths.display(prim, &loaded.store.tokens))
        .collect();
    paths.sort();
    let mut expected: Vec<&str> = oracle.prims.iter().map(|p| p.path.as_str()).collect();
    expected.sort_unstable();
    assert_eq!(paths, expected, "composed prims");

    let mut mismatches = Vec::new();
    for prim in &oracle.prims {
        let id = loaded.store.path(&prim.path);
        let stack = prim_stack(&loaded, &stage, id);
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
fn relationship_targets_map_through_relocates() {
    // Spec: AOUSD Core §10.3.2.6.1. A target path authored beneath a
    // relocation source maps to the target, through the relocates of every
    // layer stack above it; a relocate to nothing leaves it as it is.
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let mut mismatches = Vec::new();
    for (rel, expected) in &oracle.targets {
        let property = loaded.store.property_path(rel);
        let targets: Vec<String> = stage
            .resolve_target_list_path(property)
            .map(|resolved| resolved.value)
            .unwrap_or_default()
            .iter()
            .map(|target| target.display(&loaded.store.paths, &loaded.store.tokens))
            .collect();
        if &targets != expected {
            mismatches.push(format!("{rel}: expected {expected:?}, got {targets:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "targets differ from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}

/// The kind of a composition error, as the oracle names OpenUSD's.
fn error_kind(error: &CompositionError) -> &'static str {
    match error {
        CompositionError::OpinionAtRelocationSource(_) => "OpinionAtRelocationSource",
        CompositionError::ArcToProhibitedChild(_) => "ArcToProhibitedChild",
        CompositionError::InvalidAuthoredRelocation(_) => "InvalidAuthoredRelocation",
        CompositionError::InvalidConflictingRelocation(_) => "InvalidConflictingRelocation",
        CompositionError::InvalidSameTargetRelocations(_) => "InvalidSameTargetRelocations",
        CompositionError::InvalidExternalTargetPath(_) => "InvalidExternalTargetPath",
        _ => "Other",
    }
}

#[test]
fn composition_errors_match_openusd() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let mut errors: Vec<ErrorRecord> = stage
        .composition_errors()
        .iter()
        .map(|error| ErrorRecord {
            kind: error_kind(error).into(),
            prim: error.prim().map_or_else(
                || "/".into(),
                |prim| loaded.store.paths.display(prim, &loaded.store.tokens),
            ),
        })
        .collect();
    errors.sort();
    errors.dedup();
    assert_eq!(errors, oracle.errors);

    // The ignored opinion is `root.usda`'s, at the source.
    let root = layer_id(&loaded, "root.usda");
    let source = loaded.store.path("/Robot/Rig/Controls/Wrist");
    assert!(stage.composition_errors().iter().any(|error| matches!(
        error,
        CompositionError::OpinionAtRelocationSource(error)
            if error.layer == root && error.path == source
    )));
}

/// Recomposes `live`, and checks the result against a full composition of
/// the edited store: the same prims, children, prim stacks, values of every
/// attribute the oracle lists and composition errors.
fn assert_matches_full(live: &mut LiveStage, loaded: &mut LoadedStage, oracle: &Oracle) {
    let fresh = Stage::compose(&mut loaded.store, loaded.root_layer, options());
    assert_eq!(
        composed(loaded, live.stage()),
        composed(loaded, &fresh),
        "composed prims"
    );
    for attr in oracle.values.keys() {
        let property = loaded.store.property_path(attr);
        assert_eq!(
            live.stage()
                .resolve_field_path(property)
                .map(|resolved| resolved.value),
            fresh
                .resolve_field_path(property)
                .map(|resolved| resolved.value),
            "{attr}"
        );
    }
    let mut live_errors = live.stage().composition_errors().to_vec();
    let mut fresh_errors = fresh.composition_errors().to_vec();
    let key = |error: &CompositionError| format!("{error:?}");
    live_errors.sort_by_key(key);
    fresh_errors.sort_by_key(key);
    assert_eq!(live_errors, fresh_errors, "composition errors");
}

/// Sets the `int` default of `attr` authored in the layer `layer`.
fn set_int(loaded: &mut LoadedStage, layer: &str, prim: &str, attr: &str, value: i32) -> PathId {
    let layer = layer_id(loaded, layer);
    let prim = loaded.store.path(prim);
    let name = loaded.store.tokens.intern(attr);
    let spec = loaded
        .store
        .layers
        .get_mut(&layer)
        .and_then(|layer| layer.prims.get_mut(&prim))
        .expect("prim spec");
    let property = get_property_mut(&mut spec.properties, name).expect("property");
    property.default = Some(Value::Int(value));
    prim
}

#[test]
fn opinion_edits_at_relocation_targets_and_sources_recompose_in_scope() {
    // Spec: AOUSD Core §10.3.2.6. The relocated prims read opinions at
    // their targets and the ancestral opinions of their sources; editing
    // either recomposes them, and only the prims that read the edited site.
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let root_layer = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root_layer, options());
    let garden = loaded.store.path("/Garden/Plot");

    let edits = [
        (
            "root.usda",
            "/Robot/Anim/Wrist",
            "angle",
            "/Robot/Anim/Wrist",
        ),
        (
            "arm.usda",
            "/Arm/Rig/Controls/Wrist",
            "angle",
            "/Robot/Anim/Wrist",
        ),
        ("kit.usda", "/Kit/Gear", "size", "/Crate/Gear"),
        ("parts.usda", "/Parts/Gear", "pitch", "/Crate/Gear"),
        // Classes implied through the relocation sources.
        (
            "root.usda",
            "/_class_Puppet/Strings/LHand/Tip",
            "bend",
            "/Marionette/Controls/LTip",
        ),
        (
            "root.usda",
            "/Marionette/Strings/SymHand/Tip",
            "bend",
            "/Marionette/Controls/LTip",
        ),
        (
            "root.usda",
            "/Marionette/Strings/SymHand",
            "reach",
            "/Marionette/Controls/Thumb",
        ),
        (
            "puppet.usda",
            "/_class_Puppet/Strings/Thumb",
            "slack",
            "/Marionette/Controls/Thumb",
        ),
        // A source relocated by one layer stack and its target by another,
        // and one relocated through internal references.
        ("frame.usda", "/Frame/Tail", "length", "/Ribbon"),
        ("frame.usda", "/Frame/Tail", "length", "/Chain/Tail_2"),
        ("frame.usda", "/Frame/Tail/Bow", "loops", "/Bowline"),
        // A source outside a subroot reference, and its implied class.
        ("crew.usda", "/Crew/Sailor", "age", "/Deck/Sailor"),
        (
            "fleet.usda",
            "/Fleet/Crew/_class_Sailor",
            "rank",
            "/Deck/Sailor",
        ),
        // Beneath a source whose ancestor's variant branch authors
        // opinions at it, and a class there.
        (
            "tower.usda",
            "/Tower/Floor/Bell/Clapper",
            "weight",
            "/Belltower/Ringer",
        ),
        // Beneath a source a referenced layer stack relocates, through
        // each arc that reaches it.
        (
            "clay.usda",
            "/Clay/Chamber/Tray/Pot",
            "temp",
            "/Forge/Shelf/Pot",
        ),
        ("glaze.usda", "/Glaze/Tray", "coat", "/Rack"),
    ];
    for (layer, prim, attr, relocated) in edits {
        let source = set_int(&mut loaded, layer, prim, attr, 42);
        let layer = layer_id(&loaded, layer);
        live.notify_layer_prim_edits(layer, &[source]);
        let updated = live.recompose(&mut loaded.store);
        let relocated = loaded.store.path(relocated);
        assert!(
            updated.contains(&relocated),
            "{prim} recomposes {relocated:?}"
        );
        assert!(
            !updated.contains(&garden),
            "{prim} leaves /Garden/Plot alone"
        );
        assert_matches_full(&mut live, &mut loaded, &oracle);
    }
    for attr in [
        "/Robot/Anim/Wrist.angle",
        "/Marionette/Controls/LTip.bend",
        "/Marionette/Controls/Thumb.reach",
    ] {
        let attr = loaded.store.property_path(attr);
        assert_eq!(
            live.stage()
                .resolve_field_path(attr)
                .map(|resolved| resolved.value),
            Some(Value::Int(42))
        );
    }
}

#[test]
fn relocates_edits_rebuild_the_stage() {
    // Spec: AOUSD Core §10.3.2.6. Changing a layer's relocates moves prims
    // in every layer stack holding it.
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let root_layer = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root_layer, options());

    // Drop the wrist's relocate from the stage's layer stack: the wrist is
    // back beneath its controls, and `root.usda`'s opinions there count.
    let wrist = loaded.store.path("/Robot/Rig/Controls/Wrist");
    loaded
        .store
        .layers
        .get_mut(&root_layer)
        .expect("root layer")
        .relocates
        .retain(|relocate| relocate.source != wrist);
    live.notify_relocates_edit(root_layer);
    let updated = live.recompose(&mut loaded.store);
    assert!(updated.contains(&wrist));
    assert!(live.stage().has_prim(wrist));
    assert_matches_full(&mut live, &mut loaded, &oracle);

    // Move the kit's gear somewhere else, through the reference.
    let kit = layer_id(&loaded, "kit.usda");
    let (source, target) = (
        loaded.store.path("/Kit/Parts/Gear"),
        loaded.store.path("/Kit/Cog"),
    );
    loaded
        .store
        .layers
        .get_mut(&kit)
        .expect("kit layer")
        .relocates = vec![Relocate {
        source,
        target: Some(target),
    }];
    live.notify_relocates_edit(kit);
    live.recompose(&mut loaded.store);
    let cog = loaded.store.path("/Crate/Cog");
    assert!(live.stage().has_prim(cog));
    assert_matches_full(&mut live, &mut loaded, &oracle);

    // A layer no composed layer stack holds relocates nothing here.
    let unrelated = LayerId(u64::MAX);
    live.notify_relocates_edit(unrelated);
    assert!(live.recompose(&mut loaded.store).is_empty());
}

#[test]
fn the_store_keeps_relocates_as_authored() {
    let oracle = oracle();
    let loaded = load(&oracle);
    let root = loaded.store.layer(loaded.root_layer).expect("root layer");
    assert_eq!(root.relocates.len(), 11);
    assert_eq!(
        root.relocates.iter().filter(|r| r.target.is_none()).count(),
        1
    );
}

#[test]
fn switching_variants_applies_only_the_selected_branch_relocates() {
    // Spec: AOUSD Core §10.3.2.6. Relocates apply through the arcs
    // composition follows: those of a layer stack only an unselected
    // variant branch references move nothing, and selecting that branch
    // moves the prim.
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let root_layer = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root_layer, options());
    let pebble = loaded.store.path("/Pebble");
    let chip = loaded.store.path("/Pebble/Chip");
    let flake = loaded.store.path("/Pebble/Flake");
    let cut = loaded.store.tokens.intern("cut");
    assert!(live.stage().has_prim(chip));
    assert!(!live.stage().has_prim(flake));

    for (selection, present, absent) in [("moved", flake, chip), ("plain", chip, flake)] {
        let selection = loaded.store.tokens.intern(selection);
        loaded
            .store
            .layers
            .get_mut(&root_layer)
            .and_then(|layer| layer.prims.get_mut(&pebble))
            .expect("/Pebble spec")
            .variant_selections
            .insert(cut, selection);
        live.notify_layer_prim_edits(root_layer, &[pebble]);
        live.recompose(&mut loaded.store);
        assert!(live.stage().has_prim(present));
        assert!(!live.stage().has_prim(absent));
        assert_matches_full(&mut live, &mut loaded, &oracle);
    }
}
