// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Joint influence utilities against OpenUSD, call by call.
//!
//! `scripts/skel_influences_oracle.py` runs `UsdSkel.NormalizeWeights`,
//! `UsdSkel.SortInfluences`, `UsdSkel.ResizeInfluences`,
//! `UsdSkel.ExpandConstantInfluencesToVarying` and
//! `UsdSkel.InterleaveInfluences` on each case and records whether OpenUSD
//! accepted it and the arrays it left. Every case must be accepted or
//! refused here as there, an accepted one must leave the same arrays
//! (integers exactly, weights within 1e-6), and a refused one must leave
//! the inputs unchanged. It also records each skinning query's
//! `ComputeJointInfluences` and `ComputeVaryingJointInfluences` on
//! `fixtures/skel_influences/scene.usda`.
//!
//! OpenUSD: `pxr/usd/usdSkel/utils.cpp`, `skinningQuery.cpp`.

use std::collections::BTreeMap;

use layerstack::Time;
use layerstack_schemas::Scene;
use layerstack_schemas::skel::{
    InfluenceShapeError, JointInfluenceArrays, SkelError, SkinningQuery,
    expand_constant_influences, interleave_joint_influences, normalize_joint_weights,
    normalize_joint_weights_with_epsilon, resize_joint_indices, resize_joint_weights,
    sort_joint_influences,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/skel_influences/oracle.json");
const SCENE: &str = include_str!("../../fixtures/skel_influences/scene.usda");
const TOLERANCE: f32 = 1e-6;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    cases: Vec<Case>,
    queries: BTreeMap<String, Query>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Query {
    rigid: bool,
    per_component: usize,
    influences: Option<Arrays>,
    varying: BTreeMap<String, Option<Arrays>>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Arrays {
    indices: Vec<i32>,
    weights: Vec<f32>,
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
    size: Option<usize>,
    ok: bool,
    result_indices: Option<Vec<i32>>,
    result_weights: Option<Vec<f32>>,
    result_interleaved: Option<Vec<[f32; 2]>>,
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
            "expand_indices" => {
                expand_constant_influences(&mut indices, case.size.unwrap());
                Ok(())
            }
            "expand_weights" => {
                expand_constant_influences(&mut weights, case.size.unwrap());
                Ok(())
            }
            "interleave" => interleave_joint_influences(&indices, &weights).map(|pairs| {
                assert_eq!(
                    Some(&pairs),
                    case.result_interleaved.as_ref(),
                    "case {number}"
                );
            }),
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

fn arrays(found: Result<JointInfluenceArrays, SkelError>) -> Option<Arrays> {
    found.ok().map(|found| Arrays {
        indices: found.indices,
        weights: found.weights,
    })
}

#[test]
fn skinning_query_influences_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (store, live) = crate::support::schema_scene::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for (name, expected) in &oracle.queries {
        let prim = store.path(&format!("/Root/{name}"));
        let query = SkinningQuery::new(&scene, prim)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .unwrap_or_else(|| panic!("{name}: bound"));
        let actual = Query {
            rigid: query.is_rigidly_deformed(),
            per_component: query.element_size(),
            influences: arrays(query.compute_joint_influences(Time::Default)),
            varying: expected
                .varying
                .keys()
                .map(|count| {
                    let found = query
                        .compute_varying_joint_influences(count.parse().unwrap(), Time::Default);
                    (count.clone(), arrays(found))
                })
                .collect(),
        };
        assert_eq!(&actual, expected, "{name}");
    }
}

/// A refusal names the geometry and how its arrays are misshapen.
#[test]
fn misshapen_influences_name_their_shape() {
    let (store, live) = crate::support::schema_scene::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    let query = |name: &str| {
        let prim = store.path(&format!("/Root/{name}"));
        (prim, SkinningQuery::new(&scene, prim).unwrap().unwrap())
    };
    let (prim, rigid) = query("RigidTooMany");
    assert_eq!(
        rigid.compute_joint_influences(Time::Default),
        Err(SkelError::Influences {
            prim,
            error: InfluenceShapeError::Count {
                len: 2,
                expected: 1
            }
        })
    );
    let (prim, vertex) = query("Vertex");
    assert_eq!(
        vertex.compute_varying_joint_influences(2, Time::Default),
        Err(SkelError::Influences {
            prim,
            error: InfluenceShapeError::Count {
                len: 6,
                expected: 4
            }
        })
    );
    let interleaved = vertex
        .compute_joint_influences(Time::Default)
        .unwrap()
        .interleaved();
    assert_eq!(interleaved[..2], [[0.0, 0.75], [1.0, 0.25]]);
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
