// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Joint influence utilities against OpenUSD, call by call.
//!
//! `scripts/skel_influences_oracle.py` runs `UsdSkel.NormalizeWeights`,
//! `UsdSkel.SortInfluences` and `UsdSkel.ResizeInfluences` on each case and
//! records whether OpenUSD accepted it and the arrays it left. Every case
//! must be accepted or refused here as there, an accepted one must leave
//! the same arrays (integers exactly, weights within 1e-6), and a refused
//! one must leave the inputs unchanged.
//!
//! OpenUSD: `pxr/usd/usdSkel/utils.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use layerstack_schemas::skel::{
    normalize_joint_weights, normalize_joint_weights_with_epsilon, resize_joint_indices,
    resize_joint_weights, sort_joint_influences,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/skel_influences/oracle.json");
const TOLERANCE: f32 = 1e-6;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    op: String,
    #[serde(default)]
    indices: Vec<i32>,
    #[serde(default)]
    weights: Vec<f32>,
    per_component: Option<usize>,
    epsilon: Option<f32>,
    from: Option<usize>,
    to: Option<usize>,
    ok: bool,
    result_indices: Option<Vec<i32>>,
    result_weights: Option<Vec<f32>>,
}

#[test]
fn influence_utilities_match_openusd_call_by_call() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    for (number, case) in oracle.cases.iter().enumerate() {
        let mut indices = case.indices.clone();
        let mut weights = case.weights.clone();
        let accepted = match case.op.as_str() {
            "normalize" => {
                let n = case.per_component.unwrap();
                match case.epsilon {
                    Some(epsilon) => normalize_joint_weights_with_epsilon(&mut weights, n, epsilon),
                    None => normalize_joint_weights(&mut weights, n),
                }
            }
            "sort" => {
                sort_joint_influences(&mut indices, &mut weights, case.per_component.unwrap())
            }
            "resize_indices" => {
                resize_joint_indices(&mut indices, case.from.unwrap(), case.to.unwrap())
            }
            "resize_weights" => {
                resize_joint_weights(&mut weights, case.from.unwrap(), case.to.unwrap())
            }
            op => panic!("unknown op {op}"),
        }
        .is_ok();
        assert_eq!(accepted, case.ok, "case {number} ({})", case.op);
        if !accepted {
            assert_eq!(indices, case.indices, "case {number} refused unchanged");
            assert_eq!(weights, case.weights, "case {number} refused unchanged");
            continue;
        }
        if let Some(expected) = &case.result_indices {
            assert_eq!(&indices, expected, "case {number} indices");
        }
        if let Some(expected) = &case.result_weights {
            assert_eq!(weights.len(), expected.len(), "case {number} weights");
            for (got, want) in weights.iter().zip(expected) {
                assert!(
                    (got - want).abs() <= TOLERANCE,
                    "case {number}: {weights:?} vs {expected:?}"
                );
            }
        }
    }
}

#[test]
fn pinned_influence_oracle_matches_configured_openusd() {
    let Ok(python) = std::env::var("LAYERSTACK_USD_PYTHON") else {
        return;
    };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/skel_influences_oracle.py"
        ))
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
