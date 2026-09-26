// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for asset paths authored as variable expressions.
//!
//! `fixtures/expression_variables/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/expression_variables/root.usda`
//! (`scripts/expression_variables_oracle.py`, which also writes those
//! layers). Layerstack must gather the same root layer stack, compose the
//! same prims with the same ordered prim stacks, resolve the same attribute
//! defaults and report the same composition errors.
//!
//! The fixture covers a sublayer path, reference and payload paths,
//! variant selections, a referencing layer stack overriding a referenced
//! one's variable (for an asset path, for a variant selection and for the
//! referenced layer stack's own sublayer path), one layer whose selection
//! two referencing contexts evaluate to two variants, one layer that two
//! contexts reach at one destination gathering two sets of sublayers (each
//! bringing its own children, directly and through an inherited class),
//! one class's ancestral reference reached from two such contexts (both
//! sites kept), a
//! variable passing through a layer stack in between, a variable that is
//! not set, an expression that does not parse, one that evaluates to no
//! value and one that evaluates to another type than string, a stronger
//! layer's `delete` of an expression reference and payload a weaker layer
//! adds (each layer's arcs are evaluated and anchored before list ops
//! compose them), a literal delete of an expression arc and an expression
//! delete of a literal arc evaluating to the same asset, for references
//! and payloads, deletes spelling an asset differently from the arc they
//! remove (list editing compares anchored assets), a delete in one
//! directory that does not match an arc anchored in another, the same
//! expression anchored in two directories (two
//! different arcs, which a delete in one does not remove from the other),
//! and invalid selections in a selected and an unselected variant branch
//! (only the one composition reads is an error), for a variant set no
//! spec declares and for one a stronger `delete` removes from `variantSets`
//! (neither is read).
//!
//! Spec: AOUSD Core §7.6.1.7 reserves `expressionVariables`; §10.3.1,
//! §10.3.2.1 and §10.3.2.2 (sublayers, references and payloads). OpenUSD:
//! `SdfVariableExpression`, `PcpExpressionVariables` and
//! `_PcpComposeSiteReferencesOrPayloads` in `pxr/usd/pcp/composeSite.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::{BTreeMap, BTreeSet};

use layerstack::{
    CompositionError, ExpressionContext, LayerStack, LayerStore, LiveStage, Stage, StageOptions,
    Value,
    variable_expression::{ExpressionValue, VariableValue},
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/expression_variables/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    layer_stack: Vec<String>,
    prims: Vec<Prim>,
    values: BTreeMap<String, i32>,
    /// `(kind, prim, context, expression, message)`, as the script records
    /// them.
    errors: Vec<(String, String, String, String, String)>,
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

fn load(oracle: &Oracle) -> LoadedStage {
    load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/expression_variables")
            .join(&oracle.root),
    )
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

/// Every composed prim with its prim stack, by path.
fn prim_stacks(loaded: &mut LoadedStage, stage: &Stage) -> BTreeMap<String, Vec<(String, String)>> {
    let pseudo_root = loaded.store.path("/");
    stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .map(|prim| {
            let stack = stage
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
            (
                loaded.store.paths.display(prim, &loaded.store.tokens),
                stack,
            )
        })
        .collect()
}

#[test]
fn root_layer_stack_evaluates_sublayer_paths() {
    let oracle = oracle();
    let loaded = load(&oracle);
    let stack: Vec<String> = LayerStack::gather(&loaded.store, loaded.root_layer)
        .layers
        .iter()
        .map(|id| layer_name(&loaded, *id))
        .collect();
    assert_eq!(stack, oracle.layer_stack);
}

#[test]
fn prim_stacks_follow_evaluated_asset_paths() {
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options());
    let expected: BTreeMap<String, Vec<(String, String)>> = oracle
        .prims
        .iter()
        .map(|prim| (prim.path.clone(), prim.prim_stack.clone()))
        .collect();
    assert_eq!(prim_stacks(&mut loaded, &stage), expected);
}

#[test]
fn values_match_openusd() {
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options());
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

/// OpenUSD records no prim for an expression error, only the expression
/// and its site, so those compare by context, expression and message.
#[test]
fn errors_match_openusd() {
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options());
    let errors: BTreeSet<(String, String, String, String, String)> = stage
        .composition_errors()
        .iter()
        .map(|error| match error {
            CompositionError::VariableExpressionError(error) => (
                "VariableExpressionError".to_string(),
                String::new(),
                match error.context {
                    ExpressionContext::Sublayer => "sublayer",
                    ExpressionContext::Reference => "reference",
                    ExpressionContext::Payload => "payload",
                    ExpressionContext::VariantSelection => "variant",
                }
                .to_string(),
                error.expression.clone(),
                error.error.clone(),
            ),
            other => {
                let kind = match other {
                    CompositionError::UnresolvedAsset(_) => "InvalidAssetPath",
                    CompositionError::UnresolvedSublayer(_) => "InvalidSublayerPath",
                    _ => "Other",
                };
                let prim = other.prim().map_or_else(String::new, |prim| {
                    loaded.store.paths.display(prim, &loaded.store.tokens)
                });
                (
                    kind.to_string(),
                    prim,
                    String::new(),
                    String::new(),
                    String::new(),
                )
            }
        })
        .collect();
    let expected: BTreeSet<_> = oracle.errors.iter().cloned().collect();
    assert_eq!(errors, expected);
}

/// Editing a layer's `expressionVariables` recomposes what read them: the
/// live stage matches a full composition afterwards, and an edit of a
/// variable composition did not read recomposes nothing.
#[test]
fn live_stage_recomposes_edited_variables() {
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let root = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root, options());
    let field = loaded.store.tokens.intern("expressionVariables");
    let string = |s: &str| Value::String(s.into());

    let set_variables = |loaded: &mut LoadedStage, layer, entries: &[(&str, Value)]| {
        let dictionary = Value::Dictionary(
            entries
                .iter()
                .map(|(name, value)| ((*name).into(), value.clone()))
                .collect(),
        );
        loaded
            .store
            .layer_mut(layer)
            .expect("layer")
            .set_metadata(field, dictionary);
    };

    // A variable no expression reads: nothing to recompose.
    set_variables(
        &mut loaded,
        root,
        &[
            ("ROCK", string("granite")),
            ("LAYER", string("strata")),
            ("DEEP", Value::Bool(true)),
            ("SEASON", string("summer")),
            ("COLOR", string("blue")),
            ("UNUSED", string("x")),
        ],
    );
    live.notify_expression_variables_edit(&loaded.store, root);
    assert!(live.recompose(&mut loaded.store).is_empty());

    // SEASON now names winter: the selections reading it change.
    set_variables(
        &mut loaded,
        root,
        &[
            ("ROCK", string("granite")),
            ("LAYER", string("strata")),
            ("DEEP", Value::Bool(true)),
            ("SEASON", string("winter")),
            ("COLOR", string("blue")),
        ],
    );
    live.notify_expression_variables_edit(&loaded.store, root);
    assert!(!live.recompose(&mut loaded.store).is_empty());
    let meadow = loaded.store.property_path("/Meadow.bloom");
    assert_eq!(
        live.stage()
            .resolve_field_path(meadow)
            .map(|resolved| resolved.value),
        Some(Value::Int(0))
    );

    // ROCK now names basalt: every arc reading it is retargeted.
    set_variables(
        &mut loaded,
        root,
        &[
            ("ROCK", string("basalt")),
            ("LAYER", string("strata")),
            ("DEEP", Value::Bool(true)),
            ("SEASON", string("winter")),
            ("COLOR", string("blue")),
        ],
    );
    assert_eq!(
        loaded
            .store
            .layer(root)
            .expect("root")
            .expression_variables(&loaded.store.tokens)
            .get("ROCK"),
        Some(&VariableValue::Value(ExpressionValue::String(
            "basalt".into()
        )))
    );
    live.notify_expression_variables_edit(&loaded.store, root);
    assert!(!live.recompose(&mut loaded.store).is_empty());
    let full = Stage::compose(&mut loaded.store, root, options());
    let live_stacks = prim_stacks(&mut loaded, live.stage());
    assert_eq!(live_stacks, prim_stacks(&mut loaded, &full));
    assert_eq!(
        live_stacks["/Outcrop"],
        [
            ("root.usda".to_string(), "/Outcrop".to_string()),
            ("strata.usda".into(), "/Outcrop".into()),
            ("basalt.usda".into(), "/Basalt".into()),
        ]
    );
}

/// The expression variable errors of `stage`, as `(expression, error)`.
fn expression_errors(stage: &Stage) -> BTreeSet<(String, String)> {
    stage
        .composition_errors()
        .iter()
        .filter_map(|error| match error {
            CompositionError::VariableExpressionError(error) => {
                Some((error.expression.clone(), error.error.clone()))
            }
            _ => None,
        })
        .collect()
}

/// A malformed selection in an unselected branch is neither an error nor
/// a dependency; selecting its branch makes it one, as a full composition
/// finds it.
#[test]
fn live_stage_reports_a_selection_once_its_branch_is_selected() {
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let root = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root, options());
    let sand = (
        "`${SAND`".to_string(),
        "Missing ending '}' at character 7".to_string(),
    );
    assert!(!expression_errors(live.stage()).contains(&sand));

    // Select `winter`, whose `soil` selection is malformed.
    let orchard = loaded.store.path("/Orchard");
    let season = loaded.store.tokens.intern("season");
    let winter = loaded.store.tokens.intern("winter");
    loaded
        .store
        .layer_mut(root)
        .expect("root")
        .prims
        .get_mut(&orchard)
        .expect("/Orchard")
        .variant_selections
        .insert(season, winter);
    live.notify_structural_change();
    live.recompose(&mut loaded.store);

    let full = Stage::compose(&mut loaded.store, root, options());
    let errors = expression_errors(live.stage());
    assert!(errors.contains(&sand), "{errors:?}");
    assert_eq!(errors, expression_errors(&full));
    assert_eq!(
        prim_stacks(&mut loaded, live.stage()),
        prim_stacks(&mut loaded, &full)
    );
}

/// The layer the loader named `name`.
fn layer_named(loaded: &LoadedStage, name: &str) -> layerstack::LayerId {
    loaded
        .layer_names
        .iter()
        .find_map(|(id, layer)| (layer == name).then_some(*id))
        .unwrap_or_else(|| panic!("no layer {name}"))
}

/// Editing the project's `COLOR` changes the sublayer the referenced
/// `paint.usda` layer stack evaluates, and the live stage recomposes it
/// as a full composition does.
#[test]
fn live_stage_recomposes_referenced_sublayers() {
    let oracle = oracle();
    let mut loaded = load(&oracle);
    let root = loaded.root_layer;
    let mut live = LiveStage::compose(&mut loaded.store, root, options());
    let shade = loaded.store.property_path("/Canvas.shade");
    let value = |stage: &Stage| {
        stage
            .resolve_field_path(shade)
            .map(|resolved| resolved.value)
    };
    assert_eq!(value(live.stage()), Some(Value::Int(2)));

    // The project now sets COLOR to red, as the asset does.
    let field = loaded.store.tokens.intern("expressionVariables");
    let mut variables = match loaded.store.layer(root).and_then(|l| l.metadata(field)) {
        Some(layerstack::FieldValue::Value(Value::Dictionary(entries))) => entries.clone(),
        other => panic!("root expressionVariables: {other:?}"),
    };
    for (name, value) in &mut variables {
        if &**name == "COLOR" {
            *value = Value::String("red".into());
        }
    }
    loaded
        .store
        .layer_mut(root)
        .expect("root")
        .set_metadata(field, Value::Dictionary(variables));

    // A host loads what the new value names: `red.usda`, anchored to the
    // asset's root layer.
    let paint = layer_named(&loaded, "paint.usda");
    let red = layer_named(&loaded, "red.usda");
    let missing = layerstack::expression_asset_paths(&loaded.store, root);
    assert!(
        missing
            .iter()
            .any(|path| path.anchor == paint && path.asset_path == "./red.usda"),
        "{missing:?}"
    );
    loaded.store.insert_asset_layer(paint, "./red.usda", red);

    live.notify_expression_variables_edit(&loaded.store, root);
    assert!(!live.recompose(&mut loaded.store).is_empty());
    let full = Stage::compose(&mut loaded.store, root, options());
    assert_eq!(value(live.stage()), Some(Value::Int(1)));
    assert_eq!(value(&full), Some(Value::Int(1)));
    assert_eq!(
        prim_stacks(&mut loaded, live.stage()),
        prim_stacks(&mut loaded, &full)
    );
}
