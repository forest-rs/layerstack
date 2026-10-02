// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The little linear algebra transforms need, computed as OpenUSD's `Gf`
//! computes it.
//!
//! Matrices are rows, `m[row][column]`, and transform row vectors
//! (`p' = p * M`), as USD's do: a translation is the last row, and
//! `A * B` applies `A` first. Each operation follows `GfMatrix4d`,
//! `GfMatrix3d` and `GfRotation` step for step (the same products in the
//! same order, the same quaternion round trips), so results agree with
//! OpenUSD's to within the rounding of the platform's `sin`, `cos` and
//! `acos` and of fused multiply-adds.

use core::f64::consts::PI;

/// A 4×4 double matrix, rows first.
pub(crate) type Matrix4 = [[f64; 4]; 4];

type Matrix3 = [[f64; 3]; 3];

/// The identity.
pub(crate) const IDENTITY: Matrix4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// `GF_MIN_VECTOR_LENGTH`.
const MIN_VECTOR_LENGTH: f64 = 1e-10;

/// `a * b`, as `GfMatrix4d::operator*=`.
pub(crate) fn mul(a: &Matrix4, b: &Matrix4) -> Matrix4 {
    let mut out = [[0.0; 4]; 4];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + a[i][3] * b[3][j];
        }
    }
    out
}

/// `a * b`, as `GfMatrix3d::operator*=`.
fn mul3(a: &Matrix3, b: &Matrix3) -> Matrix3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    out
}

/// A translation, as `GfMatrix4d(1).SetTranslate(t)`.
pub(crate) fn translate(t: [f64; 3]) -> Matrix4 {
    let mut m = IDENTITY;
    m[3][0] = t[0];
    m[3][1] = t[1];
    m[3][2] = t[2];
    m
}

/// A scale, as `GfMatrix4d(GfVec4d(s0, s1, s2, 1))`.
pub(crate) fn scale(s: [f64; 3]) -> Matrix4 {
    let mut m = IDENTITY;
    m[0][0] = s[0];
    m[1][1] = s[1];
    m[2][2] = s[2];
    m
}

/// Direct `GfMatrix4d::SetRotate(GfQuatd)`; authored quaternions are not
/// normalized or round-tripped through an axis/angle rotation.
pub(crate) fn quaternion(q: [f64; 4]) -> Matrix4 {
    embed(&quaternion3(q))
}

fn quaternion3([x, y, z, r]: [f64; 4]) -> Matrix3 {
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + z * r),
            2.0 * (z * x - y * r),
        ],
        [
            2.0 * (x * y - z * r),
            1.0 - 2.0 * (z * z + x * x),
            2.0 * (y * z + x * r),
        ],
        [
            2.0 * (z * x + y * r),
            2.0 * (y * z - x * r),
            1.0 - 2.0 * (y * y + x * x),
        ],
    ]
}

/// A 3×3 rotation embedded with no translation, as
/// `GfMatrix4d(GfMatrix3d, GfVec3d(0))`.
fn embed(r: &Matrix3) -> Matrix4 {
    let mut m = IDENTITY;
    for (i, row) in r.iter().enumerate() {
        m[i][..3].copy_from_slice(row);
    }
    m
}

/// A rotation as `GfRotation` holds it: a unit axis and degrees.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rotation {
    axis: [f64; 3],
    degrees: f64,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(v: [f64; 3]) -> f64 {
    libm::sqrt(dot(v, v))
}

impl Rotation {
    /// `GfRotation(axis, degrees)`: the axis is normalized unless it is
    /// already a unit vector to within `1e-10`.
    pub(crate) fn new(axis: [f64; 3], degrees: f64) -> Self {
        let mut axis = axis;
        if (dot(axis, axis) - 1.0).abs() >= 1e-10 {
            let len = length(axis);
            if len >= MIN_VECTOR_LENGTH {
                axis = axis.map(|c| c / len);
            }
        }
        Self { axis, degrees }
    }

    /// `GfRotation(GfQuatd(r, i))` (`GfRotation::SetQuat`): the axis and
    /// angle of the quaternion `[i, j, k, r]`, not normalized first.
    pub(crate) fn from_quat(q: [f64; 4]) -> Self {
        let imaginary = [q[0], q[1], q[2]];
        let len = length(imaginary);
        if len > MIN_VECTOR_LENGTH {
            let x = libm::acos(q[3].clamp(-1.0, 1.0));
            Self::new(imaginary.map(|c| c / len), 2.0 * x * (180.0 / PI))
        } else {
            Self::new([1.0, 0.0, 0.0], 0.0)
        }
    }

    /// `GfRotation::GetInverse`.
    pub(crate) fn inverse(self) -> Self {
        Self {
            axis: self.axis,
            degrees: -self.degrees,
        }
    }

    /// `GfRotation::GetQuat`: `[i, j, k, r]`, normalized.
    fn quat(self) -> [f64; 4] {
        let half = self.degrees * (PI / 180.0) / 2.0;
        let (sin, cos) = libm::sincos(half);
        let i = self.axis.map(|c| c * sin);
        let len = libm::sqrt(cos * cos + dot(i, i));
        if len < MIN_VECTOR_LENGTH {
            return [0.0, 0.0, 0.0, 1.0];
        }
        [i[0] / len, i[1] / len, i[2] / len, cos / len]
    }

    /// `GfMatrix3d(GfRotation)` (`_SetRotateFromQuat`).
    fn matrix3(self) -> Matrix3 {
        quaternion3(self.quat())
    }

    /// `GfMatrix4d(GfRotation, GfVec3d(0))`.
    pub(crate) fn matrix(self) -> Matrix4 {
        embed(&self.matrix3())
    }
}

/// The unit axes.
pub(crate) const AXES: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// The product of three single-axis rotations, in the order `order` names
/// the axes (`[0, 1, 2]` is `X * Y * Z`: X applies first), as
/// `UsdGeomXformOp::GetOpTransform` builds a three-axis rotation from
/// `GfMatrix3d(GfRotation)`s.
pub(crate) fn euler(degrees: [f64; 3], order: [usize; 3]) -> Matrix4 {
    let rotation = |axis: usize| Rotation::new(AXES[axis], degrees[axis]).matrix3();
    let [a, b, c] = order.map(rotation);
    embed(&mul3(&mul3(&a, &b), &c))
}

/// `GfMatrix4d::GetInverse`, with its determinant. A singular matrix
/// (determinant exactly 0) inverts to a scale by `f32::MAX`, as OpenUSD's
/// does.
pub(crate) fn inverse(m: &Matrix4) -> (Matrix4, f64) {
    let [
        [x00, x01, x02, x03],
        [x10, x11, x12, x13],
        [x20, x21, x22, x23],
        [x30, x31, x32, x33],
    ] = *m;
    let mut y01 = x00 * x11 - x10 * x01;
    let mut y02 = x00 * x21 - x20 * x01;
    let mut y03 = x00 * x31 - x30 * x01;
    let mut y12 = x10 * x21 - x20 * x11;
    let mut y13 = x10 * x31 - x30 * x11;
    let mut y23 = x20 * x31 - x30 * x21;

    let z33 = x02 * y12 - x12 * y02 + x22 * y01;
    let z23 = x12 * y03 - x32 * y01 - x02 * y13;
    let z13 = x02 * y23 - x22 * y03 + x32 * y02;
    let z03 = x22 * y13 - x32 * y12 - x12 * y23;
    let z32 = x13 * y02 - x23 * y01 - x03 * y12;
    let z22 = x03 * y13 - x13 * y03 + x33 * y01;
    let z12 = x23 * y03 - x33 * y02 - x03 * y23;
    let z02 = x13 * y23 - x23 * y13 + x33 * y12;

    y01 = x02 * x13 - x12 * x03;
    y02 = x02 * x23 - x22 * x03;
    y03 = x02 * x33 - x32 * x03;
    y12 = x12 * x23 - x22 * x13;
    y13 = x12 * x33 - x32 * x13;
    y23 = x22 * x33 - x32 * x23;

    let z30 = x11 * y02 - x21 * y01 - x01 * y12;
    let z20 = x01 * y13 - x11 * y03 + x31 * y01;
    let z10 = x21 * y03 - x31 * y02 - x01 * y23;
    let z00 = x11 * y23 - x21 * y13 + x31 * y12;
    let z31 = x00 * y12 - x10 * y02 + x20 * y01;
    let z21 = x10 * y03 - x30 * y01 - x00 * y13;
    let z11 = x00 * y23 - x20 * y03 + x30 * y02;
    let z01 = x20 * y13 - x30 * y12 - x10 * y23;

    let det = x30 * z30 + x20 * z20 + x10 * z10 + x00 * z00;
    if det.abs() > 0.0 {
        let rcp = 1.0 / det;
        let inverse = [
            [z00 * rcp, z10 * rcp, z20 * rcp, z30 * rcp],
            [z01 * rcp, z11 * rcp, z21 * rcp, z31 * rcp],
            [z02 * rcp, z12 * rcp, z22 * rcp, z32 * rcp],
            [z03 * rcp, z13 * rcp, z23 * rcp, z33 * rcp],
        ];
        (inverse, det)
    } else {
        let max = f64::from(f32::MAX);
        (scale([max, max, max]), det)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Matrix4, b: &Matrix4) -> bool {
        a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(x, y)| (x - y).abs() < 1e-12)
    }

    fn apply(p: [f64; 3], m: &Matrix4) -> [f64; 3] {
        let row = [p[0], p[1], p[2], 1.0];
        core::array::from_fn(|j| (0..4).map(|i| row[i] * m[i][j]).sum())
    }

    #[test]
    fn rotations_turn_row_vectors_counterclockwise() {
        // 90° about Z takes +X to +Y.
        let m = Rotation::new(AXES[2], 90.0).matrix();
        let p = apply([1.0, 0.0, 0.0], &m);
        assert!((p[0]).abs() < 1e-15 && (p[1] - 1.0).abs() < 1e-15);
        // Translation is the last row; the product applies its left
        // factor first.
        let m = mul(&scale([2.0, 2.0, 2.0]), &translate([1.0, 0.0, 0.0]));
        assert_eq!(apply([1.0, 0.0, 0.0], &m), [3.0, 0.0, 0.0]);
    }

    #[test]
    fn euler_orders_multiply_single_axis_rotations() {
        let degrees = [10.0, 20.0, 30.0];
        let xyz = mul(
            &mul(
                &Rotation::new(AXES[0], 10.0).matrix(),
                &Rotation::new(AXES[1], 20.0).matrix(),
            ),
            &Rotation::new(AXES[2], 30.0).matrix(),
        );
        assert!(close(&euler(degrees, [0, 1, 2]), &xyz));
        assert!(!close(&euler(degrees, [2, 1, 0]), &xyz));
    }

    #[test]
    fn quaternions_and_inverses_round_trip() {
        let half = 45.0_f64.to_radians() / 2.0;
        let q = [0.0, libm::sin(half), 0.0, libm::cos(half)];
        let from_quat = Rotation::from_quat(q).matrix();
        assert!(close(&from_quat, &Rotation::new(AXES[1], 45.0).matrix()));
        let m = mul(&from_quat, &translate([1.0, 2.0, 3.0]));
        let (inv, det) = inverse(&m);
        assert!((det - 1.0).abs() < 1e-12);
        assert!(close(&mul(&m, &inv), &IDENTITY));
        let (singular, det) = inverse(&scale([0.0, 1.0, 1.0]));
        assert_eq!(det, 0.0);
        assert_eq!(singular[0][0], f64::from(f32::MAX));
    }
}
