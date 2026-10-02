// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! OpenUSD DQS: linear residual scale plus hemisphere-aligned rotation/translation.
use super::{InfluenceInterpolation, JointInfluences};
use crate::gf;
use crate::gf::decomposition as math;
use alloc::vec::Vec;
use math::Matrix3;
/// One prepared skinning joint: real/dual quaternion and residual scale.
/// Quaternion components are scalar-first `[w, x, y, z]`; residual matrices use
/// USD row vectors; point palettes store `f32`-rounded scale/shear as `f64`.
/// Singular factorization uses a zero real/dual quaternion and identity residual.
/// Normal palettes use a separate inverse-transpose decomposition. The input
/// view identifies which palette supplied these values. No GPU memory layout or
/// precision conversion is implied.
#[derive(Clone, Copy, Debug)]
pub struct DualQuaternionJoint {
    real: [f64; 4],
    dual: [f64; 4],
    scale: Matrix3,
    scaled: bool,
}
impl DualQuaternionJoint {
    /// Rotation quaternion, scalar-first. A singular factorization yields zero.
    #[must_use]
    pub fn real(&self) -> [f64; 4] {
        self.real
    }
    /// Dual quaternion encoding translation, scalar-first.
    #[must_use]
    pub fn dual(&self) -> [f64; 4] {
        self.dual
    }
    /// Linear residual scale/shear in row-vector convention.
    #[must_use]
    pub fn residual_scale(&self) -> &Matrix3 {
        &self.scale
    }
    /// Whether the residual differs from identity at OpenUSD's `1e-6` threshold.
    #[must_use]
    pub fn has_scale(&self) -> bool {
        self.scaled
    }
    /// Identity rotation/translation with identity residual scale.
    pub const IDENTITY: Self = Self {
        real: [1., 0., 0., 0.],
        dual: [0.; 4],
        scale: math::IDENTITY,
        scaled: false,
    };
}
pub(super) struct Palette {
    joints: Vec<DualQuaternionJoint>,
    float_scales: bool,
}
fn dot(a: [f64; 4], b: [f64; 4]) -> f64 {
    a[1] * b[1] + a[2] * b[2] + a[3] * b[3] + a[0] * b[0]
}
fn multiply(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0] * b[0] - (a[1] * b[1] + a[2] * b[2] + a[3] * b[3]),
        a[0] * b[1] + b[0] * a[1] + (a[2] * b[3] - a[3] * b[2]),
        a[0] * b[2] + b[0] * a[2] + (a[3] * b[1] - a[1] * b[3]),
        a[0] * b[3] + b[0] * a[3] + (a[1] * b[2] - a[2] * b[1]),
    ]
}
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let imaginary_dot = q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
    let real_square = q[0] * q[0];
    let product = q[1] * v[0] + q[2] * v[1] + q[3] * v[2];
    let cross = [
        q[2] * v[2] - q[3] * v[1],
        q[3] * v[0] - q[1] * v[2],
        q[1] * v[1] - q[2] * v[0],
    ];
    core::array::from_fn(|i| {
        (2. * product * q[i + 1] + (real_square - imaginary_dot) * v[i] + 2. * q[0] * cross[i])
            / (real_square + imaginary_dot)
    })
}
fn normalize(real: &mut [f64; 4], dual: &mut [f64; 4]) {
    let length = libm::sqrt(dot(*real, *real));
    if length < 1e-10 {
        *real = DualQuaternionJoint::IDENTITY.real;
        *dual = [0.; 4];
    } else {
        let inverse = 1. / length;
        *real = real.map(|v| v * inverse);
        *dual = dual.map(|v| v * inverse);
        let projection = dot(*real, *dual);
        for i in 0..4 {
            dual[i] -= projection * real[i];
        }
    }
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD point residual scales are GfMatrix3f"
)]
fn joint(
    matrix: Matrix3,
    rotation: Matrix3,
    translation: [f64; 3],
    float_scales: bool,
) -> DualQuaternionJoint {
    let real = math::quaternion(&rotation);
    let dual = multiply(
        [
            0.,
            0.5 * translation[0],
            0.5 * translation[1],
            0.5 * translation[2],
        ],
        real,
    );
    let mut scale = math::mul(&matrix, &math::inverse(&rotation));
    if float_scales {
        scale = scale.map(|row| row.map(|v| f64::from(v as f32)));
    }
    let scaled = (0..3).any(|i| {
        (0..3)
            .any(|j| (scale[i][j] - math::IDENTITY[i][j]).abs() >= 1e-6 || !scale[i][j].is_finite())
    });
    DualQuaternionJoint {
        real,
        dual,
        scale,
        scaled,
    }
}
impl Palette {
    // AOUSD Core §12.4; OpenUSD utils.cpp _ConvertToDualQuaternions.
    // Singular factorization produces a zero DQ and identity residual scale.
    pub(super) fn points(matrices: &[gf::Matrix4]) -> Self {
        let joints = matrices
            .iter()
            .map(|m| {
                let matrix = core::array::from_fn(|i| core::array::from_fn(|j| m[i][j]));
                math::factored_rotation(&matrix).map_or(
                    DualQuaternionJoint {
                        real: [0.; 4],
                        ..DualQuaternionJoint::IDENTITY
                    },
                    |rotation| joint(matrix, rotation, [m[3][0], m[3][1], m[3][2]], true),
                )
            })
            .collect();
        Self {
            joints,
            float_scales: true,
        }
    }
    // Normal matrices already contain inverse-transpose scale. OpenUSD's
    // normal path orthonormalizes their rows directly, without Factor().
    pub(super) fn normals(matrices: impl IntoIterator<Item = Matrix3>) -> Self {
        let joints = matrices
            .into_iter()
            .map(|m| joint(m, math::orthonormalize(m), [0.; 3], false))
            .collect();
        Self {
            joints,
            float_scales: false,
        }
    }
    pub(super) fn joints(&self) -> &[DualQuaternionJoint] {
        &self.joints
    }
    pub(super) fn occupancy(&self) -> (usize, usize) {
        (
            self.joints.len() * size_of::<DualQuaternionJoint>(),
            self.joints.capacity() * size_of::<DualQuaternionJoint>(),
        )
    }
    pub(super) fn view<'a>(&'a self, mapping: Option<&'a [Option<usize>]>) -> View<'a> {
        let mut view = View {
            palette: self,
            mapping,
            scaled: false,
        };
        view.scaled = (0..view.len()).any(|i| view.joint(i).scaled);
        view
    }
}
pub(super) struct View<'a> {
    palette: &'a Palette,
    mapping: Option<&'a [Option<usize>]>,
    scaled: bool,
}
impl View<'_> {
    fn len(&self) -> usize {
        self.mapping
            .map_or(self.palette.joints.len(), <[Option<usize>]>::len)
    }
    fn joint(&self, index: usize) -> DualQuaternionJoint {
        self.mapping.map_or_else(
            || self.palette.joints[index],
            |m| m[index].map_or(DualQuaternionJoint::IDENTITY, |i| self.palette.joints[i]),
        )
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "USD rounds intermediate residual-scale and point values to floats"
    )]
    fn deform(
        &self,
        initial: [f32; 3],
        inf: JointInfluences<'_>,
        point: usize,
        normal: bool,
    ) -> [f32; 3] {
        let block = if inf.interpolation == InfluenceInterpolation::Constant {
            0
        } else {
            point * inf.element_size
        };
        let ids = &inf.indices[block..block + inf.element_size];
        let weights = &inf.weights[block..block + inf.element_size];
        let mut pivot = 0;
        for i in 1..weights.len() {
            if weights[i] > weights[pivot] {
                pivot = i;
            }
        }
        let pivot = self
            .joint(usize::try_from(ids[pivot]).expect("validated influence"))
            .real;
        let mut real = [0.; 4];
        let mut dual = [0.; 4];
        let mut scaled = [0.; 3];
        for (&id, &weight) in ids.iter().zip(weights) {
            if weight == 0. {
                continue;
            }
            let joint = self.joint(usize::try_from(id).expect("validated influence"));
            if self.scaled {
                let p: [f32; 3] = core::array::from_fn(|j| {
                    if self.palette.float_scales {
                        initial[0] * joint.scale[0][j] as f32
                            + initial[1] * joint.scale[1][j] as f32
                            + initial[2] * joint.scale[2][j] as f32
                    } else {
                        (f64::from(initial[0]) * joint.scale[0][j]
                            + f64::from(initial[1]) * joint.scale[1][j]
                            + f64::from(initial[2]) * joint.scale[2][j])
                            as f32
                    }
                });
                for i in 0..3 {
                    scaled[i] += p[i] * weight;
                }
            }
            let weight = f64::from(if dot(joint.real, pivot) < 0. {
                -weight
            } else {
                weight
            });
            for i in 0..4 {
                real[i] += joint.real[i] * weight;
                dual[i] += joint.dual[i] * weight;
            }
        }
        normalize(&mut real, &mut dual);
        let value = if self.scaled { scaled } else { initial };
        let mut result = rotate(real, value.map(f64::from));
        if normal {
            let length =
                libm::sqrt(result[0] * result[0] + result[1] * result[1] + result[2] * result[2])
                    .max(1e-10);
            result = result.map(|v| v / length);
        } else {
            let translation = multiply(dual, [real[0], -real[1], -real[2], -real[3]]);
            for i in 0..3 {
                result[i] += 2. * translation[i + 1];
            }
        }
        result.map(|v| v as f32)
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "OpenUSD geometry-bind transformation rounds to GfVec3f"
    )]
    pub(super) fn points(
        &self,
        bind: &gf::Matrix4,
        inf: JointInfluences<'_>,
        points: &mut [[f32; 3]],
    ) {
        for (i, p) in points.iter_mut().enumerate() {
            let initial =
                super::skinning::transform_point(*p, bind, !super::skinning::is_affine(bind));
            *p = self.deform(initial, inf, i, false);
        }
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "OpenUSD normal-bind transformation rounds to GfVec3f"
    )]
    pub(super) fn normals(
        &self,
        bind: &Matrix3,
        inf: JointInfluences<'_>,
        map: Option<&[i32]>,
        normals: &mut [[f32; 3]],
    ) {
        for (i, n) in normals.iter_mut().enumerate() {
            let initial = core::array::from_fn(|j| {
                (f64::from(n[0]) * bind[0][j]
                    + f64::from(n[1]) * bind[1][j]
                    + f64::from(n[2]) * bind[2][j]) as f32
            });
            let point = map.map_or(i, |indices| {
                usize::try_from(indices[i]).expect("validated face corner")
            });
            *n = self.deform(initial, inf, point, true);
        }
    }
}
