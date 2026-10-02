// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Optional SIMD skinning; validation and policy remain in the shared kernel.
use super::{InfluenceInterpolation, JointInfluences};
use crate::gf;
use alloc::vec::Vec;
use fearless_simd::{f32x4, f64x4, prelude::*};

#[inline(always)]
fn transform<S: Simd>(simd: S, point: [f32; 3], matrix: &gf::Matrix4, project: bool) -> [f32; 4] {
    // Keep separate operations and GfVec3f rounding; fused arithmetic or a
    // precombined bind/joint matrix would change the USD deformation result.
    let value = f64x4::from_slice(simd, &matrix[0]) * f64::from(point[0])
        + f64x4::from_slice(simd, &matrix[1]) * f64::from(point[1])
        + f64x4::from_slice(simd, &matrix[2]) * f64::from(point[2])
        + f64x4::from_slice(simd, &matrix[3]);
    let mut value: [f64; 4] = value.into();
    if project && value[3] != 0. {
        let inverse_w = 1. / value[3];
        for cell in &mut value {
            *cell *= inverse_w;
        }
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "UsdSkelSkinPoints uses GfVec3f after each transform"
    )]
    value.map(|v| v as f32)
}
#[inline(always)]
fn skin_point<S: Simd>(
    simd: S,
    bind: &gf::Matrix4,
    joints: &[gf::Matrix4],
    indices: &[i32],
    weights: &[f32],
    point: [f32; 3],
    project_bind: bool,
) -> [f32; 3] {
    let initial = transform(simd, point, bind, project_bind);
    let mut result = f32x4::splat(simd, 0.);
    for (&index, &weight) in indices.iter().zip(weights) {
        if weight != 0. {
            let value = transform(
                simd,
                [initial[0], initial[1], initial[2]],
                &joints[usize::try_from(index).expect("validated index")],
                false,
            );
            result += f32x4::from_slice(simd, &value) * weight;
        }
    }
    let result: [f32; 4] = result.into();
    [result[0], result[1], result[2]]
}
#[inline(always)]
pub(super) fn skin_points<S: Simd>(
    simd: S,
    bind: &gf::Matrix4,
    joints: &[gf::Matrix4],
    influences: JointInfluences<'_>,
    points: &[[f32; 3]],
) -> Vec<[f32; 3]> {
    let project = !super::skinning::is_affine(bind);
    if influences.interpolation == InfluenceInterpolation::Constant {
        points
            .iter()
            .map(|&p| {
                skin_point(
                    simd,
                    bind,
                    joints,
                    influences.indices,
                    influences.weights,
                    p,
                    project,
                )
            })
            .collect()
    } else {
        points
            .iter()
            .zip(influences.indices.chunks_exact(influences.element_size))
            .zip(influences.weights.chunks_exact(influences.element_size))
            .map(|((&p, i), w)| skin_point(simd, bind, joints, i, w, p, project))
            .collect()
    }
}
#[inline(always)]
pub(super) fn skin_points_in_place<S: Simd>(
    simd: S,
    bind: &gf::Matrix4,
    joints: &[gf::Matrix4],
    influences: JointInfluences<'_>,
    points: &mut [[f32; 3]],
) {
    let project = !super::skinning::is_affine(bind);
    if influences.interpolation == InfluenceInterpolation::Constant {
        for p in points {
            *p = skin_point(
                simd,
                bind,
                joints,
                influences.indices,
                influences.weights,
                *p,
                project,
            );
        }
    } else {
        for ((p, i), w) in points
            .iter_mut()
            .zip(influences.indices.chunks_exact(influences.element_size))
            .zip(influences.weights.chunks_exact(influences.element_size))
        {
            *p = skin_point(simd, bind, joints, i, w, *p, project);
        }
    }
}
