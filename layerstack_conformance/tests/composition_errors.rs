// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for composition errors and what they drop.
//!
//! `fixtures/composition_errors/oracle.json` records what OpenUSD 26.08
//! composes from each entry layer in `fixtures/composition_errors`, with
//! its variant fallbacks, and the layers its text parser rejects
//! (`scripts/composition_errors_oracle.py`, which also writes those
//! layers). Layerstack must report the same errors, as their
//! `PcpErrorType` name and composed prim, compose the same prims with the
//! same ordered prim and property stacks, and resolve the same target
//! paths and attribute defaults:
//!
//! - `/Stone`, `/Brook` and `/River` reach a reference or payload to a prim
//!   path without specs ([`CompositionError::UnresolvedPrimPath`]), and
//!   `/Nest/Egg/Shell` one whose only specs lie across a cycle.
//! - `/Lantern` authors an attribute over a referenced relationship and a
//!   relationship over a referenced attribute; the weaker spec of the other
//!   kind is dropped ([`CompositionError::InconsistentPropertyType`]).
//! - `/Yard`, `/Shed`, `/Orchard` and `/Grove/Oak` drop target paths that
//!   cannot be mapped across the arc that brings them in
//!   ([`CompositionError::InvalidExternalTargetPath`]), and `/Grove/Elm`
//!   one that a class authors to an instance of itself
//!   ([`CompositionError::InvalidInstanceTargetPath`]). `/Hedge` and
//!   `/Tracing` report nothing: their explicit targets replace the
//!   referenced ones, from another layer or the same one. `/Sketch`
//!   reports the path in the explicit list that wins. `/Bench` maps a
//!   class's connections through the relocations of the layer stack it
//!   references: each instance's own relocated part is outside the class's
//!   scope, and the other's targets an instance of the class.
//! - `fallbacks.usda` is composed with variant fallbacks
//!   ([`StageOptions::variant_fallbacks`]): the first fallback naming a
//!   variant of the set is selected wherever no selection is authored, in
//!   the order the prim declares its variant sets: `/Mast` and `/Boom`
//!   declare the same sets in both orders, and `/Hull`'s fallback branch
//!   declares a set that falls back in turn.
//! - `selection.usda` evaluates variant sets one at a time, in the order
//!   each prim declares them, from the sites composed so far: `/Mast`'s
//!   `shape=cube`, selected by a weaker reference, selects `size` over that
//!   reference, and `/Boom` declares the sets the other way around;
//!   `/Hull` and `/Keel` take a set declared in that branch from the
//!   reference or the branch, `/Spar` chains three sets, and `/Deck`
//!   references `/Mast`.
//!   `selection_fallbacks.usda` adds fallbacks: `/Mast`'s fallback branch
//!   selects `size` after the referenced selection has, and `/Boom`'s
//!   referenced selection selects a branch whose selection outranks the
//!   fallback. [`LiveStage`] recomposes edits of `/Mast`'s selections as a
//!   full composition does.
//! - Each `invalid_*.usda` layer authors an arc, relocates or target path
//!   with a variant selection, which the text parser rejects;
//!   `variant_connection.usda` authors the relative connection inside a
//!   variant branch that it accepts.
//!
//! Spec: AOUSD Core §10.3.2 (arcs), §10.3.2.5 (variants), §10.6
//! (composition errors), §16.2.16.9 (target paths); OpenUSD `pxr/usd/pcp/errors.h`, `primIndex.cpp`,
//! `propertyIndex.cpp` and `targetIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::{BTreeMap, BTreeSet};

use layerstack::{
    CompositionError, LayerId, LiveStage, PathId, PropertyKind, PropertyPath, Stage, StageOptions,
    TargetPath, TokenId, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/composition_errors/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    compositions: Vec<Composition>,
    rejected: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Composition {
    root: String,
    fallbacks: BTreeMap<String, Vec<String>>,
    prims: Vec<Prim>,
    errors: BTreeSet<(String, String)>,
}

#[derive(Deserialize)]
struct Prim {
    path: String,
    prim_stack: Vec<(String, String)>,
    properties: Vec<Property>,
}

#[derive(Deserialize)]
struct Property {
    name: String,
    kind: String,
    stack: Vec<(String, String)>,
    targets: Vec<String>,
    value: Option<i32>,
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

fn load(name: &str) -> LoadedStage {
    load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/composition_errors")
            .join(name),
    )
}

fn compose(composition: &Composition) -> (LoadedStage, Stage) {
    let mut loaded = load(&composition.root);
    assert!(
        loaded.invalid.is_empty(),
        "{} was rejected: {:?}",
        composition.root,
        loaded.invalid
    );
    let variant_fallbacks = composition
        .fallbacks
        .iter()
        .map(|(set, names)| {
            let names: Vec<_> = names
                .iter()
                .map(|n| loaded.store.tokens.intern(n))
                .collect();
            (loaded.store.tokens.intern(set), names)
        })
        .collect();
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            with_provenance: true,
            variant_fallbacks,
            ..StageOptions::default()
        },
    );
    (loaded, stage)
}

/// A layer's file name without its directory.
fn layer_name(loaded: &LoadedStage, layer: LayerId) -> String {
    let name = &loaded.layer_names[&layer];
    name.rsplit('/').next().unwrap_or(name).to_string()
}

/// The `PcpErrorType` name OpenUSD reports for `error`.
fn error_kind(error: &CompositionError) -> &'static str {
    match error {
        CompositionError::SublayerCycle(_) => "SublayerCycle",
        CompositionError::ArcCycle(_) => "ArcCycle",
        CompositionError::UnresolvedDefaultPrim(_) | CompositionError::UnresolvedPrimPath(_) => {
            "UnresolvedPrimPath"
        }
        CompositionError::UnresolvedAsset(_) => "InvalidAssetPath",
        CompositionError::UnresolvedSublayer(_) => "InvalidSublayerPath",
        CompositionError::InconsistentPropertyType(_) => "InconsistentPropertyType",
        CompositionError::InvalidExternalTargetPath(_) => "InvalidExternalTargetPath",
        CompositionError::InvalidInstanceTargetPath(_) => "InvalidInstanceTargetPath",
        _ => "Other",
    }
}

#[test]
fn errors_match_openusd() {
    for composition in &oracle().compositions {
        let (loaded, stage) = compose(composition);
        let errors: BTreeSet<(String, String)> = stage
            .composition_errors()
            .iter()
            .map(|error| {
                let prim = error.prim().map_or_else(String::new, |prim| {
                    loaded.store.paths.display(prim, &loaded.store.tokens)
                });
                (error_kind(error).to_string(), prim)
            })
            .collect();
        assert_eq!(
            errors,
            composition.errors,
            "errors composing {}: {:?}",
            composition.root,
            stage.composition_errors()
        );
    }
}

#[test]
fn stacks_targets_and_values_match_openusd() {
    for composition in &oracle().compositions {
        let (mut loaded, stage) = compose(composition);
        let pseudo_root = loaded.store.path("/");
        let mut composed: Vec<String> = stage
            .traverse(pseudo_root)
            .filter(|prim| *prim != pseudo_root)
            .map(|prim| loaded.store.paths.display(prim, &loaded.store.tokens))
            .collect();
        composed.sort();
        let mut expected: Vec<&str> = composition.prims.iter().map(|p| p.path.as_str()).collect();
        expected.sort_unstable();
        assert_eq!(
            composed, expected,
            "prims composed from {}",
            composition.root
        );

        let mut mismatches = Vec::new();
        for prim in &composition.prims {
            let id = loaded.store.path(&prim.path);
            let site = |loaded: &LoadedStage, layer, spec: &layerstack::SpecPath| {
                (
                    layer_name(loaded, layer),
                    spec.display(&loaded.store.tokens),
                )
            };
            let stack: Vec<(String, String)> = stage
                .explain_prim(id)
                .expect("composed prim")
                .iter()
                .map(|key| site(&loaded, key.layer_id, &key.spec_path))
                .collect();
            if stack != prim.prim_stack {
                mismatches.push(format!(
                    "{}\n    expected {:?}\n    actual   {stack:?}",
                    prim.path, prim.prim_stack
                ));
            }
            for property in &prim.properties {
                let name = loaded.store.tokens.intern(&property.name);
                let path = PropertyPath::new(id, name);
                let label = format!("{}.{}", prim.path, property.name);
                let stack: Vec<(String, String)> = stage
                    .explain_property_path(path)
                    .unwrap_or_default()
                    .iter()
                    .map(|opinion| site(&loaded, opinion.key.layer_id, &opinion.key.spec_path))
                    .collect();
                if stack != property.stack {
                    mismatches.push(format!(
                        "{label} stack\n    expected {:?}\n    actual   {stack:?}",
                        property.stack
                    ));
                }
                let kind = match stage.resolve_property_declaration(id, name).map(|d| d.kind) {
                    Some(PropertyKind::Relationship) => "relationship",
                    Some(PropertyKind::Attribute) => "attribute",
                    None => "none",
                };
                if kind != property.kind {
                    mismatches.push(format!("{label}: kind {kind}, expected {}", property.kind));
                }
                let targets: Vec<String> = stage
                    .resolve_target_list_path(path)
                    .map(|resolved| resolved.value)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|target: TargetPath| {
                        target.display(&loaded.store.paths, &loaded.store.tokens)
                    })
                    .collect();
                if targets != property.targets {
                    mismatches.push(format!(
                        "{label} targets\n    expected {:?}\n    actual   {targets:?}",
                        property.targets
                    ));
                }
                if let Some(expected) = property.value {
                    let value = stage
                        .resolve_field_path(path)
                        .map(|resolved| resolved.value);
                    if value != Some(Value::Int(expected)) {
                        mismatches.push(format!("{label}: expected {expected}, got {value:?}"));
                    }
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} differs from OpenUSD:\n{}",
            composition.root,
            mismatches.join("\n")
        );
    }
}

#[test]
fn layers_openusd_rejects_are_rejected() {
    for (layer, message) in &oracle().rejected {
        let loaded = load(layer);
        assert!(
            loaded
                .invalid
                .iter()
                .any(|reason| reason.contains(message.as_str())),
            "{layer}: expected rejection {message:?}, got {:?}",
            loaded.invalid
        );
    }
}

/// Each prim `stage` composes, with its prim stack and the values of the
/// `scale` and `hue` attributes `selection.usda` authors.
fn selection_snapshot(loaded: &LoadedStage, stage: &Stage) -> Vec<String> {
    let pseudo_root = loaded
        .store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("pseudo-root");
    let attributes: Vec<TokenId> = ["scale", "hue"]
        .into_iter()
        .filter_map(|name| loaded.store.tokens.lookup(name))
        .collect();
    stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .map(|prim| {
            let stack: Vec<(String, String)> = stage
                .explain_prim(prim)
                .expect("composed prim")
                .iter()
                .map(|key| {
                    (
                        layer_name(loaded, key.layer_id),
                        key.spec_path.display(&loaded.store.tokens),
                    )
                })
                .collect();
            let values: Vec<Option<Value>> = attributes
                .iter()
                .map(|name| {
                    stage
                        .resolve_field_path(PropertyPath::new(prim, *name))
                        .map(|resolved| resolved.value)
                })
                .collect();
            format!(
                "{} {stack:?} {values:?}",
                loaded.store.paths.display(prim, &loaded.store.tokens)
            )
        })
        .collect()
}

/// Sets (`Some`) or removes (`None`) the selection of `set` authored on the
/// prim spec at `prim` in `layer`, and recomposes the prims `live` draws
/// from that spec; returns them.
fn edit_selection(
    live: &mut LiveStage,
    loaded: &mut LoadedStage,
    layer: LayerId,
    prim: PathId,
    set: TokenId,
    variant: Option<TokenId>,
) -> Vec<PathId> {
    let selections = &mut loaded
        .store
        .layers
        .get_mut(&layer)
        .and_then(|layer| layer.prims.get_mut(&prim))
        .expect("prim spec")
        .variant_selections;
    match variant {
        Some(variant) => selections.insert(set, variant),
        None => selections.remove(&set),
    };
    live.notify_layer_prim_edits(layer, &[prim]);
    live.recompose(&mut loaded.store)
}

#[test]
fn selection_edits_recompose_like_a_full_compose() {
    // Spec: AOUSD Core §10.3.2.5.1. `/Mast` evaluates `size` after the
    // referenced `shape=cube` selects the branch that selects `size=small`.
    // Editing either selection recomposes the prims drawing on the edited
    // spec, and they must compose what a full composition does.
    let mut loaded = load("selection.usda");
    let root = loaded.root_layer;
    let options = || StageOptions {
        with_provenance: true,
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut loaded.store, root, options());
    let reference = *loaded
        .layer_names
        .iter()
        .find(|(_, name)| name.ends_with("selection_ref.usda"))
        .expect("selection_ref.usda")
        .0;
    let (mast, hull, deck) = (
        loaded.store.path("/Mast"),
        loaded.store.path("/Hull"),
        loaded.store.path("/Deck"),
    );
    let tokens = &mut loaded.store.tokens;
    let (shape, size) = (tokens.intern("shape"), tokens.intern("size"));
    let (cube, large, scale) = (
        tokens.intern("cube"),
        tokens.intern("large"),
        tokens.intern("scale"),
    );

    // Without the referenced `shape=cube`, the referenced `size=large`
    // holds; with it again, `size=small`; then a local `size=large` is
    // stronger than the branch's selection.
    let edits = [
        (reference, shape, None, 2),
        (reference, shape, Some(cube), 1),
        (root, size, Some(large), 2),
    ];
    for (layer, set, variant, expected) in edits {
        let updated = edit_selection(&mut live, &mut loaded, layer, mast, set, variant);
        assert!(
            updated.contains(&mast) && updated.contains(&deck),
            "{updated:?}"
        );
        assert!(!updated.contains(&hull), "{updated:?}");
        assert_eq!(
            live.stage()
                .resolve_field_path(PropertyPath::new(mast, scale))
                .map(|resolved| resolved.value),
            Some(Value::Int(expected))
        );
        let fresh = Stage::compose(&mut loaded.store, root, options());
        assert_eq!(
            selection_snapshot(&loaded, live.stage()),
            selection_snapshot(&loaded, &fresh)
        );
    }
}
