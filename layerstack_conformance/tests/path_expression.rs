// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Path expressions and collection predicates against OpenUSD.
//!
//! `fixtures/path_expression/scene.usda` has prims of every kind in the
//! built-in hierarchy (in and out of the model hierarchy, and an unknown
//! kind), concrete, abstract and unknown types, single and multiple-apply
//! schemas, variant selections (one naming no variant, one that is not an
//! identifier), `def`, `over` and `class` specifiers, and properties. Its
//! `/Expressions` prim has a collection per expression, once with the
//! default expansion rule and once with `expandPrimsAndProperties`: every
//! pattern feature (globs, classes, `//`, property patterns, predicates
//! with arguments), every collection predicate with its argument quirks,
//! every set operator, the complement of `//`, and references (nested, a
//! cycle, missing collections, a prim without the collection applied, and a
//! collection across a reference). `weak.usda`, a sublayer, and
//! `asset.usda`, referenced by `/Referencing`, give `%_` weaker opinions to
//! compose over across layers and through a reference.
//!
//! `scripts/path_expression_oracle.py` records what OpenUSD 26.08
//! computes in `oracle.json`:
//!
//! - `SdfPathExpression::GetText` of every text in its list, or that it
//!   does not parse;
//! - for every collection, its composed expression,
//!   `ResolveCompleteMembershipExpression`, which paths (the pseudo-root,
//!   every prim, every property outside `/Expressions`)
//!   `IsPathIncluded` includes, and which
//!   `ComputeIncludedPathsFromCollection` computes; or that the expression
//!   does not link (`u*`: an unknown predicate, arguments one does not
//!   bind, a malformed glob), which OpenUSD's Python raises for and its
//!   evaluator (`Sdf_MakePathExpressionEvalImpl`) makes match nothing.
//!
//! Every value must agree.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use layerstack::path_expression::PathExpression;
use layerstack::{PathId, PropertyPath, Stage, StageOptions, TargetPath, Value};
use layerstack_conformance::usda_real::{LoadedStage, load_entry_usda};
use layerstack_conformance::workspace_root;
use layerstack_schemas::usd::CollectionApi;
use layerstack_schemas::{ExpressionEvaluator, MembershipProblem, PrimView, Scene};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/path_expression/oracle.json");
const SELECTION_GLOBS: &str = include_str!("../fixtures/path_expression/selection_globs.json");
const DIVERGENCES: &str =
    include_str!("../fixtures/path_expression/selection_glob_divergences.json");

#[derive(Deserialize)]
struct SelectionGlobs {
    openusd_version: String,
    globs: BTreeMap<String, Option<Vec<String>>>,
}

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    texts: BTreeMap<String, Option<String>>,
    paths: Vec<String>,
    collections: BTreeMap<String, CollectionRecord>,
}

#[derive(Deserialize)]
struct CollectionRecord {
    expression: Option<String>,
    resolved: String,
    #[serde(default)]
    unlinkable: bool,
    #[serde(default)]
    included: Vec<usize>,
    #[serde(default)]
    computed: Vec<usize>,
}

fn load() -> (LoadedStage, Stage) {
    load_fixture("scene.usda")
}

fn load_fixture(name: &str) -> (LoadedStage, Stage) {
    let mut loaded = load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/path_expression")
            .join(name),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    (loaded, stage)
}

#[test]
fn text_matches_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let mut failures = Vec::new();
    for (input, expected) in &oracle.texts {
        let got = PathExpression::parse(input).ok().map(|e| e.text());
        if got != *expected {
            failures.push(format!("{input:?}: {got:?}, OpenUSD {expected:?}"));
        }
    }
    eprintln!(
        "{} texts against OpenUSD {}",
        oracle.texts.len(),
        oracle.openusd_version
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn collection_expressions_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    let (loaded, stage) = load();
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);
    let root = store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("the pseudo-root");
    let prims: Vec<PathId> = stage.traverse(root).skip(1).collect();
    let display = |path: TargetPath| path.display(&store.paths, &store.tokens);

    // The same paths as OpenUSD's, by their text.
    let mut universe = vec![TargetPath::Prim(root)];
    for prim in &prims {
        universe.push(TargetPath::Prim(*prim));
        if display(TargetPath::Prim(*prim)) == "/Expressions" {
            continue;
        }
        for name in stage.property_names(*prim, store) {
            universe.push(TargetPath::Property(PropertyPath::new(*prim, name)));
        }
    }
    let by_text: HashMap<String, TargetPath> = universe
        .iter()
        .map(|path| (display(*path), *path))
        .collect();
    let ours: BTreeSet<&String> = by_text.keys().collect();
    let theirs: BTreeSet<&String> = oracle.paths.iter().collect();
    assert_eq!(ours, theirs, "the same prims and properties");
    let index: HashMap<TargetPath, usize> = oracle
        .paths
        .iter()
        .enumerate()
        .map(|(i, text)| (by_text[text], i))
        .collect();

    let mut failures = Vec::new();
    let (mut checks, mut seen) = (0, 0);
    for prim in &prims {
        for collection in CollectionApi::instances(&scene, *prim) {
            seen += 1;
            let name = format!(
                "{}.collection:{}",
                display(TargetPath::Prim(*prim)),
                collection.instance()
            );
            let Some(expected) = oracle.collections.get(&name) else {
                failures.push(format!("{name}: OpenUSD has no such collection"));
                continue;
            };
            // The composed value: anchored, mapped and `%_` spliced in,
            // compared as OpenUSD writes it.
            let composed = PrimView::new(scene, *prim)
                .property_path(&format!(
                    "collection:{}:membershipExpression",
                    collection.instance()
                ))
                .and_then(|property| {
                    stage.resolve_value_with_schema(*prim, property.property(), store)
                })
                .and_then(|resolved| match resolved.value {
                    layerstack::ResolvedValue::Scalar(Value::PathExpression(text)) => {
                        PathExpression::parse(&text).ok().map(|e| e.text())
                    }
                    _ => None,
                });
            if composed != expected.expression {
                failures.push(format!(
                    "{name}: composed {composed:?}, OpenUSD {:?}",
                    expected.expression
                ));
            }
            let query = collection.membership_query();
            let resolved = query.expression().map(PathExpression::text);
            if resolved.as_deref() != Some(expected.resolved.as_str()) {
                failures.push(format!(
                    "{name}: resolved {resolved:?}, OpenUSD {:?}",
                    expected.resolved
                ));
            }
            // An expression that does not link matches nothing, and says why.
            let unevaluable = query
                .problems()
                .iter()
                .any(|p| matches!(p, MembershipProblem::UnevaluableExpression { .. }));
            if unevaluable != expected.unlinkable {
                failures.push(format!(
                    "{name}: unevaluable {unevaluable}, OpenUSD {}: {:?}",
                    expected.unlinkable,
                    query.problems()
                ));
            }
            let included: BTreeSet<usize> = expected.included.iter().copied().collect();
            for path in &universe {
                checks += 1;
                let got = query.is_included(&scene, *path).is_included();
                if got != included.contains(&index[path]) {
                    failures.push(format!(
                        "{name} {}: included {got}, OpenUSD {}",
                        display(*path),
                        !got
                    ));
                }
            }
            checks += 1;
            let computed: BTreeSet<String> = query
                .included_paths(&scene)
                .into_iter()
                .filter(|path| index.contains_key(path))
                .map(display)
                .collect();
            let expected_computed: BTreeSet<String> = expected
                .computed
                .iter()
                .map(|i| oracle.paths[*i].clone())
                .collect();
            if computed != expected_computed {
                failures.push(format!(
                    "{name}: computes {:?} more, {:?} fewer than OpenUSD",
                    computed.difference(&expected_computed).collect::<Vec<_>>(),
                    expected_computed.difference(&computed).collect::<Vec<_>>()
                ));
            }
        }
    }
    assert_eq!(seen, oracle.collections.len(), "every collection");
    eprintln!(
        "{checks} memberships of {seen} collections against OpenUSD {}",
        oracle.openusd_version
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A glob the `variant` predicate is allowed to reject though OpenUSD
/// accepts it, and why.
#[derive(Deserialize)]
struct Divergence {
    /// `undefined` (syntax POSIX leaves undefined, which platforms read
    /// differently) or `unsupported` (a POSIX feature not implemented).
    tag: String,
    reason: String,
}

/// Why the `variant` predicate rejected a glob: the tag and reason
/// `SelectionGlob` gives (`… has undefined: an empty branch at …`).
fn rejection(error: &str) -> (String, String) {
    let text = error.split_once(" has ").map_or(error, |(_, rest)| rest);
    let (tag, reason) = text.split_once(": ").unwrap_or(("", text));
    let reason = reason.split(" at character").next().unwrap_or(reason);
    (tag.to_owned(), reason.to_owned())
}

/// The `variant` predicate's selection globs against OpenUSD's
/// `ArchRegex` (`scripts/selection_glob_oracle.py`: hand-picked globs and
/// 400 random ones). Every glob must behave as in OpenUSD (rejected, or
/// matching the same selections) except those
/// `selection_glob_divergences.json` lists, each `undefined` or
/// `unsupported` with its reason; the list must be exact, so a listed glob
/// that no longer diverges fails too.
#[test]
fn selection_globs_match_openusd() {
    let oracle: SelectionGlobs = serde_json::from_str(SELECTION_GLOBS).expect("the oracle parses");
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let permitted: BTreeMap<String, Divergence> =
        serde_json::from_str(DIVERGENCES).expect("the divergences parse");
    let (loaded, stage) = load_fixture("selections.usda");
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);
    let root = store.paths.lookup(&layerstack::Path::root()).expect("root");
    let prims: Vec<PathId> = stage.traverse(root).skip(1).collect();
    let display = |path: PathId| store.paths.display(path, &store.tokens);

    let mut failures = Vec::new();
    let mut agreed = 0;
    let mut diverged: BTreeMap<String, usize> = BTreeMap::new();
    for (glob, expected) in &oracle.globs {
        let quoted = glob.replace('\\', "\\\\").replace('"', "\\\"");
        let text = format!("//{{variant(color=\"{quoted}\")}}");
        let expression = PathExpression::parse(&text).expect("parses");
        let evaluator = ExpressionEvaluator::new(&scene, &expression);
        match (evaluator, expected) {
            (Err(_), None) => agreed += 1,
            (Ok(_), None) => failures.push(format!("{glob:?}: accepted, OpenUSD rejects it")),
            (Err(error), Some(_)) => {
                let (tag, reason) = rejection(&error.to_string());
                match permitted.get(glob) {
                    Some(listed) if listed.tag == tag && listed.reason == reason => {
                        *diverged.entry(tag).or_default() += 1;
                    }
                    Some(listed) => failures.push(format!(
                        "{glob:?}: rejected as {tag}: {reason}, listed as {}: {}",
                        listed.tag, listed.reason
                    )),
                    None => failures.push(format!(
                        "{glob:?}: rejected as {tag}: {reason}, OpenUSD accepts it, not listed"
                    )),
                }
            }
            (Ok(evaluator), Some(expected)) => {
                let got: Vec<String> = prims
                    .iter()
                    .filter(|prim| evaluator.matches(&scene, TargetPath::Prim(**prim)).value)
                    .map(|prim| display(*prim))
                    .collect();
                if got == *expected {
                    agreed += 1;
                } else {
                    failures.push(format!("{glob:?}: {got:?}, OpenUSD {expected:?}"));
                }
                if permitted.contains_key(glob) {
                    failures.push(format!("{glob:?}: listed, but no longer diverges"));
                }
            }
        }
        if expected.is_none() && permitted.contains_key(glob) {
            failures.push(format!("{glob:?}: listed, but OpenUSD rejects it too"));
        }
    }
    for glob in permitted.keys() {
        if !oracle.globs.contains_key(glob) {
            failures.push(format!("{glob:?}: listed, but not in the oracle"));
        }
    }
    eprintln!(
        "{agreed} of {} selection globs agree with OpenUSD {}; permitted divergences: {diverged:?}",
        oracle.globs.len(),
        oracle.openusd_version,
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
