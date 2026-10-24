// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Skel authoring helpers against OpenUSD, call by call.
//!
//! `scripts/skel_authoring_oracle.py` makes a sequence of
//! `CreateJointIndicesPrimvar`, `CreateJointWeightsPrimvar`,
//! `SetRigidJointInfluence` and `UsdSkelAnimation::SetTransforms` calls on
//! an in-memory stage and records, after each, whether it was accepted and
//! every attribute spec its prim then has in the root layer. Here each call
//! is made through its own `SchemaEdit` and applied; it must be accepted or
//! refused as in OpenUSD, an accepted call must leave the same specs (names,
//! types and metadata exactly, numbers within 1e-6), and a refused call
//! must author nothing. OpenUSD authors part of two refused calls (it
//! creates the primvars before refusing a negative joint index, and authors
//! metadata on an attribute of the wrong type); `changed_when_refused`
//! marks them.
//!
//! The oracle also records `UsdSkel.ComputeJointLocalTransforms`.
//!
//! OpenUSD: `pxr/usd/usdSkel/bindingAPI.cpp`, `animation.cpp`, `utils.cpp`.

use layerstack::{EditTarget, LayerId};
use layerstack_schemas::SchemaEdit;
use layerstack_schemas::skel::{InfluenceInterpolation, JointTopology, RigidInfluenceError};
use layerstack_schemas::usd_skel::{SkelAnimationEdit, SkelBindingApiEdit};
use serde::Deserialize;
use serde_json::Value as Json;

use crate::support::layer_json;
use crate::support::schema_scene as support;

const ORACLE: &str = include_str!("../../fixtures/skel_authoring/oracle.json");
const TOLERANCE: f64 = 1e-6;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    calls: Vec<Call>,
    local_transforms: LocalTransforms,
}

#[derive(Deserialize)]
struct Call {
    prim: String,
    op: String,
    args: Args,
    accepted: bool,
    changed_when_refused: bool,
    prim_after: Json,
}

#[derive(Deserialize)]
struct Args {
    joint: Option<i32>,
    weight: Option<f32>,
    constant: Option<bool>,
    element_size: Option<i32>,
    time: Option<f64>,
    transforms: Option<Vec<[[f64; 4]; 4]>>,
}

#[derive(Deserialize)]
struct LocalTransforms {
    joints: Vec<String>,
    skeleton_space: Vec<[[f64; 4]; 4]>,
    root_inverse: [[f64; 4]; 4],
    local: Vec<[[f64; 4]; 4]>,
    rooted: Vec<[[f64; 4]; 4]>,
}

/// The stage the oracle builds before its calls.
const SCENE: &str = r#"#usda 1.0
def Mesh "Rigid" (prepend apiSchemas = ["SkelBindingAPI"]) {}
def Mesh "Fresh" {}
def Mesh "Skinned" (prepend apiSchemas = ["SkelBindingAPI"]) {}
def Mesh "Typed"
{
    double primvars:skel:jointWeights
}
def SkelAnimation "Anim" {}
"#;

#[test]
fn skel_authoring_matches_openusd_call_by_call() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, mut live) = support::scene(SCENE);
    for (number, call) in oracle.calls.iter().enumerate() {
        let path = store.path(&call.prim);
        let before = layer_json::prim_attributes(&store, LayerId(1), path);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let accepted = match call.op.as_str() {
            "set_rigid_joint_influence" => {
                let binding = SkelBindingApiEdit::new(&edit, path).unwrap().unwrap();
                let joint = call.args.joint.unwrap();
                let result =
                    binding.set_rigid_joint_influence(&mut edit, joint, call.args.weight.unwrap());
                if joint < 0 {
                    assert!(matches!(
                        result,
                        Err(RigidInfluenceError::NegativeJointIndex(j)) if j == joint
                    ));
                }
                result.is_ok()
            }
            op @ ("create_joint_indices_primvar" | "create_joint_weights_primvar") => {
                let binding = SkelBindingApiEdit::new(&edit, path).unwrap().unwrap();
                let interpolation = if call.args.constant.unwrap() {
                    InfluenceInterpolation::Constant
                } else {
                    InfluenceInterpolation::Vertex
                };
                let size = call.args.element_size;
                if op == "create_joint_indices_primvar" {
                    binding.create_joint_indices_primvar(&mut edit, interpolation, size)
                } else {
                    binding.create_joint_weights_primvar(&mut edit, interpolation, size)
                }
                .is_ok()
            }
            "set_transforms" => {
                let animation = SkelAnimationEdit::new(&edit, path).unwrap().unwrap();
                let transforms = call.args.transforms.as_deref().unwrap();
                match call.args.time {
                    Some(time) => animation.set_transforms_at(&mut edit, time, transforms),
                    None => animation.set_transforms(&mut edit, transforms),
                }
                .is_ok()
            }
            op => panic!("unknown op {op}"),
        };
        let transaction = edit.finish();
        assert_eq!(
            accepted, call.accepted,
            "call {number}: {} on {}",
            call.op, call.prim
        );
        if !accepted {
            assert!(
                transaction.is_empty(),
                "call {number} refused authors nothing"
            );
            if !call.changed_when_refused {
                assert_eq!(
                    call.prim_after, before,
                    "call {number} unchanged in OpenUSD too"
                );
            }
            continue;
        }
        live.apply(&mut store, &transaction).unwrap();
        let after = layer_json::prim_attributes(&store, LayerId(1), path);
        assert!(after.as_object().is_some_and(|specs| !specs.is_empty()));
        if let Some(difference) = layer_json::difference(&after, &call.prim_after, TOLERANCE) {
            panic!("call {number}: {} on {}: {difference}", call.op, call.prim);
        }
    }
}

#[test]
fn local_transforms_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let local = oracle.local_transforms;
    let joints: Vec<&str> = local.joints.iter().map(String::as_str).collect();
    let topology = JointTopology::new(&joints).unwrap();
    let check = |got: Vec<[[f64; 4]; 4]>, want: &[[[f64; 4]; 4]]| {
        assert_eq!(got.len(), want.len());
        for (got, want) in got.iter().zip(want) {
            for (a, b) in got.iter().flatten().zip(want.iter().flatten()) {
                assert!(
                    (a - b).abs() <= TOLERANCE * a.abs().max(1.0),
                    "{got:?} vs {want:?}"
                );
            }
        }
    };
    check(
        topology
            .compute_local_transforms(&local.skeleton_space, None)
            .unwrap(),
        &local.local,
    );
    check(
        topology
            .compute_local_transforms(&local.skeleton_space, Some(&local.root_inverse))
            .unwrap(),
        &local.rooted,
    );
}

#[test]
fn pinned_authoring_oracle_matches_configured_openusd() {
    let Ok(python) = std::env::var("LAYERSTACK_USD_PYTHON") else {
        return;
    };
    let output = std::process::Command::new(python)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/skel_authoring_oracle.py"
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
