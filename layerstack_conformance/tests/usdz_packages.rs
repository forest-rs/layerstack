// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for composing the layers inside USDZ packages.
//!
//! `fixtures/usdz_packages/oracle.json` records what OpenUSD 26.08
//! composes from each package in `fixtures/usdz_packages`
//! (`scripts/usdz_packages_oracle.py`, which also writes the members).
//! Each test packages the same members in the same order with
//! [`layerstack_usdz::write_usdz`], reads the package back, installs every
//! layer it returns, composes, and must match OpenUSD's prims, prim stacks
//! (as package member paths), children and attribute values:
//!
//! - every layer a member loads reaches the stage, however deep below the
//!   root layer and whether read from `.usda` or `.usdc`;
//! - a member referenced several times, or from members that reference
//!   each other, loads once;
//! - a path naming no member resolves beside the package (the case's
//!   `<case>_outside` directory) through the outer resolver, whose layers
//!   and the members share one ID space, so none is lost on install;
//! - a relative path names the member OpenUSD anchors it to: a path
//!   starting with `.` in the directory of the member authoring it and
//!   nowhere else, a search path there and then beside the root layer,
//!   which may itself sit in a directory.
//!
//! Spec: AOUSD Core §16.4 (USDZ packages), §9.4 (relative asset paths),
//! §9.7 (packaged resource resolution). OpenUSD:
//! `SdfComputeAssetPathRelativeToLayer` in `pxr/usd/sdf/layerUtils.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{Stage, StageOptions, Value};
use layerstack_conformance::{usda_real::LoadedStage, usdz::load_usdz, workspace_root};
use layerstack_usdz::{PackageFile, write_usdz};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/usdz_packages/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    /// Package members in package order, the root layer first.
    members: Vec<String>,
    prims: Vec<Prim>,
    values: BTreeMap<String, Option<f64>>,
    composition_errors: usize,
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

/// Packages the case's members as OpenUSD read them, and loads the package.
fn load(case: &Case) -> LoadedStage {
    let directory = workspace_root()
        .join("layerstack_conformance/fixtures/usdz_packages")
        .join(&case.name);
    let data: Vec<Vec<u8>> = case
        .members
        .iter()
        .map(|member| {
            std::fs::read(directory.join(member))
                .unwrap_or_else(|e| panic!("{}/{member}: {e}", case.name))
        })
        .collect();
    let files: Vec<PackageFile<'_>> = case
        .members
        .iter()
        .zip(&data)
        .map(|(member, data)| PackageFile::new(member, data))
        .collect();
    let package = write_usdz(&files).expect("package");
    // The package lies beside the case's files outside it, if any.
    let beside = directory.with_file_name(format!("{}_outside", case.name));
    load_usdz(&package, &beside).unwrap_or_else(|e| panic!("{}: {e}", case.name))
}

/// Compares one case with OpenUSD, returning each difference.
fn mismatches(case: &Case) -> Vec<String> {
    let mut loaded = load(case);
    let mut out = Vec::new();
    // Every layer the package loads is in the store, each once.
    let mut members: Vec<&str> = loaded.layer_names.values().map(String::as_str).collect();
    members.sort_unstable();
    let before = members.len();
    members.dedup();
    if members.len() != before {
        out.push(format!("a member loaded twice: {:?}", loaded.layer_names));
    }

    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    if stage.composition_errors().is_empty() != (case.composition_errors == 0) {
        out.push(format!(
            "OpenUSD reports {} composition errors, layerstack {:?}",
            case.composition_errors,
            stage.composition_errors()
        ));
    }

    let pseudo_root = loaded.store.path("/");
    let mut composed: Vec<String> = stage
        .traverse(pseudo_root)
        .filter(|prim| *prim != pseudo_root)
        .map(|prim| loaded.store.paths.display(prim, &loaded.store.tokens))
        .collect();
    composed.sort();
    let mut expected: Vec<&str> = case.prims.iter().map(|p| p.path.as_str()).collect();
    expected.sort_unstable();
    if composed != expected {
        out.push(format!(
            "prims: expected {expected:?}, composed {composed:?}"
        ));
    }

    for prim in &case.prims {
        let id = loaded.store.path(&prim.path);
        let stack: Vec<(String, String)> = stage
            .explain_prim(id)
            .unwrap_or_default()
            .iter()
            .map(|key| {
                (
                    loaded
                        .layer_names
                        .get(&key.layer_id)
                        .cloned()
                        .unwrap_or_else(|| format!("{:?}", key.layer_id)),
                    key.spec_path.display(&loaded.store.tokens),
                )
            })
            .collect();
        if stack != prim.prim_stack {
            out.push(format!(
                "{} prim stack\n    expected {:?}\n    actual   {stack:?}",
                prim.path, prim.prim_stack
            ));
        }
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
            out.push(format!(
                "{} children: expected {:?}, got {children:?}",
                prim.path, prim.children
            ));
        }
    }

    for (attr, expected) in &case.values {
        let property = loaded.store.property_path(attr);
        let value = stage
            .resolve_field_path(property)
            .map(|resolved| resolved.value);
        let expected = expected.map(Value::Double);
        if value != expected {
            out.push(format!("{attr}: expected {expected:?}, got {value:?}"));
        }
    }
    out
}

fn check(name: &str) {
    let oracle = oracle();
    let case = oracle
        .cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("no case {name} in oracle.json"));
    let mismatches = mismatches(case);
    assert!(
        mismatches.is_empty(),
        "package `{name}` differs from OpenUSD:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn every_case_is_tested() {
    let tested = [
        "reference",
        "payload",
        "sublayers",
        "mixed",
        "repeated",
        "cycle",
        "external",
        "anchored",
        "same_basename",
        "normalized",
        "root_in_subdirectory",
        "search_fallback",
        "layer_relative_stays",
        "root_back_reference",
    ];
    let recorded: Vec<String> = oracle().cases.into_iter().map(|case| case.name).collect();
    assert_eq!(recorded, tested);
}

/// The root layer references a member.
#[test]
fn a_referenced_member_contributes() {
    check("reference");
}

/// The root layer payloads a member, which loads by default.
#[test]
fn a_payloaded_member_contributes() {
    check("payload");
}

/// A sublayer's sublayer references a member three layers below the root.
#[test]
fn members_below_sublayers_contribute() {
    check("sublayers");
}

/// `.usda` and `.usdc` members reference, payload and sublayer each other.
#[test]
fn text_and_crate_members_load_each_other() {
    check("mixed");
}

/// A member referenced from several prims and layers loads once.
#[test]
fn a_repeated_member_loads_once() {
    check("repeated");
}

/// Members that reference each other each load once.
#[test]
fn members_that_reference_each_other_load_once() {
    check("cycle");
}

/// Members and layers beside the package, loaded interleaved, keep
/// distinct layer IDs, and every one of them contributes.
#[test]
fn members_and_layers_outside_the_package_all_contribute() {
    check("external");
}

/// `./asset.usda` in `models/parent.usdc` is `models/asset.usda`.
#[test]
fn a_layer_relative_path_anchors_to_its_member() {
    check("anchored");
}

/// `asset.usda` and `models/asset.usda` are different members, and a
/// search path in `models/` finds the one beside it.
#[test]
fn members_with_one_file_name_stay_apart() {
    check("same_basename");
}

/// `..` and `.` segments normalize, and one member reached by two paths
/// is one layer.
#[test]
fn member_paths_normalize() {
    check("normalized");
}

/// A root layer in `scene/` anchors its paths there.
#[test]
fn a_root_layer_in_a_directory_anchors_there() {
    check("root_in_subdirectory");
}

/// A search path naming no member beside its layer finds one beside the
/// root layer.
#[test]
fn a_search_path_falls_back_to_the_root_layers_directory() {
    check("search_fallback");
}

/// A path starting with `.` never searches: a missing member is an error,
/// even with a like-named member beside the root layer.
#[test]
fn a_layer_relative_path_never_searches() {
    check("layer_relative_stays");
}

/// A member referencing the root layer finds it.
#[test]
fn a_member_can_reference_the_root_layer() {
    check("root_back_reference");
}
