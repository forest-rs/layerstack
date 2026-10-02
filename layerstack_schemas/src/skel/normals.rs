// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Normal skinning uses inverse transposes, with explicit corner-to-point maps.
use super::{InfluenceInterpolation, JointInfluences, SkelError, skin_points};
use crate::gf;
use alloc::vec::Vec;
type Matrix = [[f64; 4]; 4];
type NormalMatrix = [[f64; 3]; 3];

fn inverse_transpose(matrix: &Matrix, joint: Option<usize>) -> Result<NormalMatrix, SkelError> {
    // AOUSD Core §6.3 row vectors; OpenUSD SkinningQuery.cpp
    // ComputeSkinnedNormals: inverse transpose of the upper-left 3x3.
    let mut affine = gf::IDENTITY;
    for (out, row) in affine.iter_mut().zip(matrix).take(3) {
        out[..3].copy_from_slice(&row[..3]);
    }
    let (inverse, det) = gf::inverse(&affine);
    if det == 0. || !det.is_finite() {
        return Err(SkelError::SingularNormalTransform { joint });
    }
    Ok(core::array::from_fn(|i| {
        core::array::from_fn(|j| inverse[j][i])
    }))
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD rounds normal transforms to GfVec3f"
)]
fn transform(v: [f32; 3], m: &NormalMatrix) -> [f32; 3] {
    core::array::from_fn(|j| {
        (f64::from(v[0]) * m[0][j] + f64::from(v[1]) * m[1][j] + f64::from(v[2]) * m[2][j]) as f32
    })
}
fn normal_matrices(
    bind: &Matrix,
    joints: &[Matrix],
) -> Result<(NormalMatrix, Vec<NormalMatrix>), SkelError> {
    Ok((
        inverse_transpose(bind, None)?,
        joints
            .iter()
            .enumerate()
            .map(|(i, m)| inverse_transpose(m, Some(i)))
            .collect::<Result<_, _>>()?,
    ))
}
fn deform(
    bind: &NormalMatrix,
    joints: &[NormalMatrix],
    inf: JointInfluences<'_>,
    map: Option<&[i32]>,
    normals: &mut [[f32; 3]],
) {
    for (ni, n) in normals.iter_mut().enumerate() {
        let point = map.map_or(ni, |indices| {
            usize::try_from(indices[ni]).expect("validated corner")
        });
        let block = if inf.interpolation == InfluenceInterpolation::Constant {
            0
        } else {
            point * inf.element_size
        };
        let initial = transform(*n, bind);
        let mut result = [0.; 3];
        for (&index, &weight) in inf.indices[block..block + inf.element_size]
            .iter()
            .zip(&inf.weights[block..block + inf.element_size])
        {
            if weight != 0. {
                let value = transform(
                    initial,
                    &joints[usize::try_from(index).expect("validated influence")],
                );
                for (out, v) in result.iter_mut().zip(value) {
                    *out += v * weight;
                }
            }
        }
        // GfVec3f::Normalize uses a float dot, sqrt and 1e-10 minimum length.
        let length =
            libm::sqrtf(result[0] * result[0] + result[1] * result[1] + result[2] * result[2]);
        let divisor = if length > 1e-10 { length } else { 1e-10 };
        *n = result.map(|v| v / divisor);
    }
}
/// Skins vertex/varying normals with inverse-transpose matrices, then normalizes
/// each result as `UsdSkelSkinNormals`. Inputs are point skinning matrices;
/// this function derives normal matrices itself. Singular matrices error.
pub fn skin_normals(
    bind: &Matrix,
    joints: &[Matrix],
    influences: JointInfluences<'_>,
    normals: &[[f32; 3]],
) -> Result<Vec<[f32; 3]>, SkelError> {
    let mut result = normals.to_vec();
    skin_normals_in_place(bind, joints, influences, &mut result)?;
    Ok(result)
}
/// Skins normals in a reusable buffer. Influence and matrix validation occurs
/// before mutation; errors leave the buffer unchanged.
pub fn skin_normals_in_place(
    bind: &Matrix,
    joints: &[Matrix],
    influences: JointInfluences<'_>,
    normals: &mut [[f32; 3]],
) -> Result<(), SkelError> {
    influences.validate(normals.len(), joints.len())?;
    let (bind, joints) = normal_matrices(bind, joints)?;
    deform(&bind, &joints, influences, None, normals);
    Ok(())
}
/// Skins face-varying normals using one point index per face corner. `point_count`
/// is the geometry point count, not the number of normals; influences address
/// those points. Indices, lengths and inverse transposes validate before output.
pub fn skin_face_varying_normals(
    bind: &Matrix,
    joints: &[Matrix],
    influences: JointInfluences<'_>,
    point_count: usize,
    face_vertex_indices: &[i32],
    normals: &[[f32; 3]],
) -> Result<Vec<[f32; 3]>, SkelError> {
    influences.validate(point_count, joints.len())?;
    if normals.len() != face_vertex_indices.len() {
        return Err(SkelError::InvalidDeformation {
            element: None,
            reason: "face-varying normal count",
        });
    }
    for (element, &index) in face_vertex_indices.iter().enumerate() {
        if usize::try_from(index).ok().is_none_or(|i| i >= point_count) {
            return Err(SkelError::InvalidDeformation {
                element: Some(element),
                reason: "face corner point index",
            });
        }
    }
    let (bind, joints) = normal_matrices(bind, joints)?;
    let mut result = normals.to_vec();
    deform(
        &bind,
        &joints,
        influences,
        Some(face_vertex_indices),
        &mut result,
    );
    Ok(result)
}
/// Computes the skeleton-space transform of a constant influence binding.
/// Matches `UsdSkelSkinTransform`: a unit single-joint binding uses matrix
/// multiplication; other bindings skin a float four-point frame. Weights are
/// authored, unnormalized values. Vertex interpolation is rejected.
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD rigid transform uses a float frame"
)]
pub fn rigid_skinning_transform(
    bind: &Matrix,
    joints: &[Matrix],
    influences: JointInfluences<'_>,
) -> Result<Matrix, SkelError> {
    if influences.interpolation != InfluenceInterpolation::Constant {
        return Err(SkelError::InvalidDeformation {
            element: None,
            reason: "rigid transform needs constant influences",
        });
    }
    influences.validate(0, joints.len())?;
    if influences.element_size == 1 && (influences.weights[0] - 1.).abs() <= 1e-6 {
        return Ok(gf::mul(
            bind,
            &joints[usize::try_from(influences.indices[0]).expect("validated influence")],
        ));
    }
    let pivot: [f32; 3] = core::array::from_fn(|i| bind[3][i] as f32);
    let frame: [[_; 3]; 4] = core::array::from_fn(|i| {
        if i == 3 {
            pivot
        } else {
            core::array::from_fn(|j| pivot[j] + bind[i][j] as f32)
        }
    });
    let frame = skin_points(&gf::IDENTITY, joints, influences, &frame)?;
    let mut result = gf::IDENTITY;
    for i in 0..3 {
        for j in 0..3 {
            result[i][j] = f64::from(frame[i][j] - frame[3][j]);
        }
    }
    result[3][..3].copy_from_slice(&frame[3].map(f64::from));
    Ok(result)
}
