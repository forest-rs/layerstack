// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Skinning with interleaved `[index, weight]` influences against OpenUSD.
//!
//! `scripts/skel_interleaved_oracle.py` calls the `GfVec2f` influence
//! overloads of `UsdSkelSkinPoints(LBS)`, `UsdSkelSkinNormals(LBS)` and
//! `UsdSkelSkinTransform(LBS)` with both skinning methods, including
//! fractional indices (truncated), a unit single-joint transform and
//! refusals. The test replays each case through the skinning functions with
//! [`InterleavedInfluences`], comparing within 1e-5, and checks that the
//! face-varying normal kernel, which Python does not reach, agrees with the
//! same influences split into separate arrays.

use layerstack_schemas::skel::{
    InfluenceInterpolation, InterleavedInfluences, SkinningMethod,
    rigid_skinning_transform_with_method, skin_face_varying_normals_with_method,
    skin_normals_with_method, skin_points_in_place_with_method, skin_points_with_method,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/skel_interleaved/oracle.json");
const TOLERANCE: f64 = 1e-5;

type Matrix = [[f64; 4]; 4];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    bind: Matrix,
    joints: Vec<Matrix>,
    points: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    kernel: String,
    method: String,
    influences: Vec<[f32; 2]>,
    per_point: Option<usize>,
    ok: bool,
    output: Option<Vec<Vec<f64>>>,
}

fn close(got: &[Vec<f64>], want: &[Vec<f64>], label: &str) {
    assert_eq!(got.len(), want.len(), "{label}");
    for (a, b) in got.iter().zip(want) {
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() <= TOLERANCE, "{label}: {got:?} vs {want:?}");
        }
    }
}

fn rows<const N: usize>(values: &[[f32; N]]) -> Vec<Vec<f64>> {
    values
        .iter()
        .map(|row| row.iter().map(|&x| f64::from(x)).collect())
        .collect()
}

#[test]
fn interleaved_skinning_matches_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    for (number, case) in oracle.cases.iter().enumerate() {
        let label = format!("case {number}: {} {}", case.kernel, case.method);
        let method = match case.method.as_str() {
            "classicLinear" => SkinningMethod::ClassicLinear,
            _ => SkinningMethod::DualQuaternion,
        };
        let output = match case.kernel.as_str() {
            "points" | "normals" => {
                let influences = InterleavedInfluences {
                    influences: &case.influences,
                    element_size: case.per_point.unwrap(),
                    interpolation: InfluenceInterpolation::Vertex,
                };
                if case.kernel == "points" {
                    skin_points_with_method(
                        method,
                        &oracle.bind,
                        &oracle.joints,
                        influences,
                        &oracle.points,
                    )
                } else {
                    skin_normals_with_method(
                        method,
                        &oracle.bind,
                        &oracle.joints,
                        influences,
                        &oracle.normals,
                    )
                }
                .map(|values| rows(&values))
            }
            _ => {
                let influences = InterleavedInfluences {
                    influences: &case.influences,
                    element_size: case.influences.len(),
                    interpolation: InfluenceInterpolation::Constant,
                };
                rigid_skinning_transform_with_method(
                    method,
                    &oracle.bind,
                    &oracle.joints,
                    influences,
                )
                .map(|matrix| matrix.iter().map(|row| row.to_vec()).collect())
            }
        };
        assert_eq!(output.is_ok(), case.ok, "{label}: {output:?}");
        if let (Ok(output), Some(expected)) = (&output, &case.output) {
            close(output, expected, &label);
        }
    }
}

/// The face-varying normal kernel and the in-place point kernel read
/// interleaved influences as the same influences split into arrays.
#[test]
fn interleaved_influences_skin_as_their_split_arrays() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let pairs = [
        [0.0, 0.5],
        [1.9, 0.5],
        [2.0, 1.0],
        [0.0, 0.0],
        [1.0, 0.25],
        [2.0, 0.75],
    ];
    let interleaved = InterleavedInfluences {
        influences: &pairs,
        element_size: 2,
        interpolation: InfluenceInterpolation::Vertex,
    };
    let split = interleaved.split();
    assert_eq!(split.indices, [0, 1, 2, 0, 1, 2], "indices truncate");
    let separate = layerstack_schemas::skel::JointInfluences {
        indices: &split.indices,
        weights: &split.weights,
        element_size: 2,
        interpolation: InfluenceInterpolation::Vertex,
    };
    let corners = [0, 1, 2, 2, 1];
    let normals = [[0.0, 0.0, 1.0]; 5];
    for method in [
        SkinningMethod::ClassicLinear,
        SkinningMethod::DualQuaternion,
    ] {
        let skin = |influences| {
            skin_face_varying_normals_with_method(
                method,
                &oracle.bind,
                &oracle.joints,
                influences,
                3,
                &corners,
                &normals,
            )
        };
        assert_eq!(
            skin_face_varying_normals_with_method(
                method,
                &oracle.bind,
                &oracle.joints,
                interleaved,
                3,
                &corners,
                &normals,
            )
            .unwrap(),
            skin(separate).unwrap(),
            "{method:?}"
        );
        let mut points = oracle.points.clone();
        skin_points_in_place_with_method(
            method,
            &oracle.bind,
            &oracle.joints,
            interleaved,
            &mut points,
        )
        .unwrap();
        let expected = skin_points_with_method(
            method,
            &oracle.bind,
            &oracle.joints,
            separate,
            &oracle.points,
        )
        .unwrap();
        assert_eq!(points, expected, "{method:?}");
    }
}
