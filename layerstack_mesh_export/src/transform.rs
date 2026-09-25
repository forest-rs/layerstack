// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Local transforms.

/// A local-to-parent affine transform, stored as USD authors a `matrix4d`.
///
/// USD uses row vectors: a point transforms as `p' = p · M`, so the
/// translation lives in the last row (`m[3][0..3]`). Most mesh kernels use
/// column vectors instead (`p' = M · p`, translation in the last column);
/// [`Self::from_affine_3x4`] converts from that form.
///
/// Spec: `UsdGeomXformable`, `xformOp:transform`
/// (<https://openusd.org/dev/api/class_usd_geom_xformable.html>); matrices
/// are row-major with row-vector semantics (AOUSD Core §6.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    rows: [[f64; 4]; 4],
}

impl Transform {
    /// The identity transform.
    pub const IDENTITY: Self = Self {
        rows: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };

    /// Uses a matrix already in USD's row-vector layout.
    pub fn from_usd_rows(rows: [[f64; 4]; 4]) -> Self {
        Self { rows }
    }

    /// Converts a column-vector affine transform given as its top three
    /// rows, `[[r00, r01, r02, tx], [r10, r11, r12, ty], [r20, r21, r22, tz]]`
    /// (so `p' = R · p + t`), into USD's row-vector layout.
    pub fn from_affine_3x4(rows: [[f64; 4]; 3]) -> Self {
        let [a, b, c] = rows;
        Self {
            rows: [
                [a[0], b[0], c[0], 0.0],
                [a[1], b[1], c[1], 0.0],
                [a[2], b[2], c[2], 0.0],
                [a[3], b[3], c[3], 1.0],
            ],
        }
    }

    /// A pure translation.
    pub fn from_translation(t: [f64; 3]) -> Self {
        let mut m = Self::IDENTITY;
        m.rows[3] = [t[0], t[1], t[2], 1.0];
        m
    }

    /// The matrix in USD's row-vector layout.
    pub fn usd_rows(&self) -> [[f64; 4]; 4] {
        self.rows
    }

    /// Splits the transform into scale, rotation and translation, applied
    /// in that order, as `UsdGeomPointInstancer` composes an instance
    /// (`pxr/usd/usdGeom/pointInstancer.h`, "Computing an Instance
    /// Transform").
    ///
    /// Each scale is the length of the image of one axis; a mirroring
    /// transform (negative determinant) negates the X scale so the rest is
    /// a proper rotation. The axes' images must be orthogonal to within
    /// [`SHEAR_TOLERANCE`].
    pub(crate) fn decompose(&self) -> Result<Decomposed, NotRigid> {
        let m = &self.rows;
        if !m.iter().flatten().all(|c| c.is_finite()) {
            return Err(NotRigid::NonFinite);
        }
        if (0..3).any(|i| m[i][3].abs() > SHEAR_TOLERANCE)
            || (m[3][3] - 1.0).abs() > SHEAR_TOLERANCE
        {
            return Err(NotRigid::Projective);
        }
        // Row `i` is the image of axis `i`: `scale[i] * rotation[i]`.
        let axes: [[f64; 3]; 3] = core::array::from_fn(|i| [m[i][0], m[i][1], m[i][2]]);
        let mut scale: [f64; 3] = axes.map(|a| libm::sqrt(dot(a, a)));
        if scale.iter().any(|&s| !(s > 0.0 && s.is_finite())) {
            return Err(NotRigid::Singular);
        }
        let [x, y, z] = axes;
        if dot(cross(x, y), z) < 0.0 {
            scale[0] = -scale[0];
        }
        let rows: [[f64; 3]; 3] = core::array::from_fn(|i| axes[i].map(|c| c / scale[i]));
        let deviation = [(0, 1), (0, 2), (1, 2)]
            .iter()
            .map(|&(a, b)| dot(rows[a], rows[b]).abs())
            .fold(0.0, f64::max);
        if deviation > SHEAR_TOLERANCE {
            return Err(NotRigid::Sheared { deviation });
        }
        Ok(Decomposed {
            scale,
            orientation: quaternion(&rows),
            translation: [m[3][0], m[3][1], m[3][2]],
        })
    }
}

/// The largest cosine allowed between the images of two axes for a
/// transform to count as scale, rotation and translation (about 0.0006°
/// from square), and the largest projective component allowed. It absorbs
/// the rounding of `f32` and `f64` inputs; genuinely sheared transforms
/// are far outside it.
pub const SHEAR_TOLERANCE: f64 = 1e-5;

/// Why a transform cannot be written as a scale, a rotation and a
/// translation, the only transforms a `PointInstancer` instance can have.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NotRigid {
    /// An element is NaN or infinite.
    NonFinite,
    /// The matrix has a projective part (its last column is not
    /// `(0, 0, 0, 1)` in USD's row-vector layout).
    Projective,
    /// An axis collapses to zero length.
    Singular,
    /// The axes' images are not orthogonal: the transform shears.
    Sheared {
        /// The largest cosine between two of them, above
        /// [`SHEAR_TOLERANCE`].
        deviation: f64,
    },
}

impl core::fmt::Display for NotRigid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NonFinite => write!(f, "the transform is not finite"),
            Self::Projective => write!(f, "the transform is projective"),
            Self::Singular => write!(f, "the transform collapses an axis"),
            Self::Sheared { deviation } => write!(
                f,
                "the transform shears (axes {deviation:e} from orthogonal)"
            ),
        }
    }
}

impl core::error::Error for NotRigid {}

/// A transform split as `UsdGeomPointInstancer` applies it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Decomposed {
    /// Scale along the prototype's axes, applied first; X is negative for
    /// a mirroring transform.
    pub(crate) scale: [f64; 3],
    /// Unit quaternion `[x, y, z, w]` with `w >= 0`.
    pub(crate) orientation: [f64; 4],
    /// Translation, applied last.
    pub(crate) translation: [f64; 3],
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// The unit quaternion `[x, y, z, w]` (with `w >= 0`) of a rotation given
/// in row-vector form, the inverse of `GfMatrix4d::SetRotate`
/// (`pxr/base/gf/matrix4d.cpp`). It solves for the largest of the four
/// components first and divides by it, so no small square root dominates.
fn quaternion(r: &[[f64; 3]; 3]) -> [f64; 4] {
    let trace = r[0][0] + r[1][1] + r[2][2];
    let q = if trace > 0.0 {
        let s = 2.0 * libm::sqrt(1.0 + trace);
        [
            (r[1][2] - r[2][1]) / s,
            (r[2][0] - r[0][2]) / s,
            (r[0][1] - r[1][0]) / s,
            s / 4.0,
        ]
    } else if r[0][0] > r[1][1] && r[0][0] > r[2][2] {
        let s = 2.0 * libm::sqrt(1.0 + r[0][0] - r[1][1] - r[2][2]);
        [
            s / 4.0,
            (r[0][1] + r[1][0]) / s,
            (r[2][0] + r[0][2]) / s,
            (r[1][2] - r[2][1]) / s,
        ]
    } else if r[1][1] > r[2][2] {
        let s = 2.0 * libm::sqrt(1.0 + r[1][1] - r[0][0] - r[2][2]);
        [
            (r[0][1] + r[1][0]) / s,
            s / 4.0,
            (r[1][2] + r[2][1]) / s,
            (r[2][0] - r[0][2]) / s,
        ]
    } else {
        let s = 2.0 * libm::sqrt(1.0 + r[2][2] - r[0][0] - r[1][1]);
        [
            (r[2][0] + r[0][2]) / s,
            (r[1][2] + r[2][1]) / s,
            s / 4.0,
            (r[0][1] - r[1][0]) / s,
        ]
    };
    let norm = libm::sqrt(q.iter().map(|c| c * c).sum());
    let sign = if q[3] < 0.0 { -1.0 } else { 1.0 };
    q.map(|c| sign * c / norm)
}

#[cfg(test)]
mod tests {
    use super::{NotRigid, SHEAR_TOLERANCE, Transform};
    use crate::instancer::rotation;

    /// `S · R · T` in row-vector form, as `UsdGeomPointInstancer` builds
    /// an instance.
    fn compose(scale: [f64; 3], q: [f64; 4], t: [f64; 3]) -> [[f64; 4]; 4] {
        let r = rotation(q);
        let mut m = [[0.0; 4]; 4];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = scale[i] * r[i][j];
            }
        }
        m[3] = [t[0], t[1], t[2], 1.0];
        m
    }

    fn assert_close(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) {
        for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
            assert!((x - y).abs() < 1e-12, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn decomposition_recomposes() {
        let half = core::f64::consts::FRAC_1_SQRT_2;
        let n = libm::sqrt(0.1 * 0.1 + 0.7 * 0.7 + 0.2 * 0.2 + 0.5 * 0.5);
        for (scale, q) in [
            ([1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]),
            ([2.0, 0.5, 3.0], [0.0, 0.0, half, half]),
            ([1.0, 1.0, 1.0], [1.0, 0.0, 0.0, 0.0]),
            ([1.0, 1.0, 1.0], [0.0, 1.0, 0.0, 0.0]),
            ([1.0, 1.0, 1.0], [0.0, 0.0, 1.0, 0.0]),
            ([0.2, 0.3, 4.0], [0.1 / n, -0.7 / n, 0.2 / n, 0.5 / n]),
            // Mirrored in X, then turned.
            ([-1.5, 1.0, 2.0], [0.0, half, 0.0, half]),
        ] {
            let t = [1.0, -2.0, 30.0];
            let m = compose(scale, q, t);
            let d = Transform::from_usd_rows(m).decompose().unwrap();
            assert_close(&compose(d.scale, d.orientation, d.translation), &m);
            assert!(d.orientation[3] >= 0.0, "w >= 0: {:?}", d.orientation);
            assert_eq!(d.scale[0] < 0.0, scale[0] < 0.0, "mirroring: {scale:?}");
        }
    }

    #[test]
    fn mirroring_in_any_axis_becomes_a_negative_x_scale() {
        // Mirror in Y: the same as mirroring in X, then half a turn about Z.
        let mirror_y = Transform::from_affine_3x4([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, -1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ]);
        let d = mirror_y.decompose().unwrap();
        assert_eq!(d.scale, [-1.0, 1.0, 1.0]);
        assert_close(
            &compose(d.scale, d.orientation, d.translation),
            &mirror_y.usd_rows(),
        );
    }

    #[test]
    fn rejects_what_an_instance_cannot_carry() {
        let shear = Transform::from_affine_3x4([
            [1.0, 0.1, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ]);
        assert!(matches!(
            shear.decompose(),
            Err(NotRigid::Sheared { deviation }) if deviation > SHEAR_TOLERANCE
        ));
        let flat = Transform::from_affine_3x4([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.0],
        ]);
        assert_eq!(flat.decompose(), Err(NotRigid::Singular));
        let mut rows = Transform::IDENTITY.usd_rows();
        rows[0][3] = 0.5;
        assert_eq!(
            Transform::from_usd_rows(rows).decompose(),
            Err(NotRigid::Projective)
        );
        rows[0][3] = 0.0;
        rows[3][0] = f64::NAN;
        assert_eq!(
            Transform::from_usd_rows(rows).decompose(),
            Err(NotRigid::NonFinite)
        );
        // Rounding noise well inside the tolerance is accepted.
        let noisy = Transform::from_affine_3x4([
            [1.0, 1e-7, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ]);
        assert!(noisy.decompose().is_ok());
    }

    /// `p · M` for an affine `M` (row-vector convention).
    fn apply(t: &Transform, p: [f64; 3]) -> [f64; 3] {
        let m = t.usd_rows();
        core::array::from_fn(|j| p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j] + m[3][j])
    }

    #[test]
    fn affine_3x4_matches_column_vector_semantics() {
        // Rotate 90° about Z, then translate by (10, 20, 30).
        let t = Transform::from_affine_3x4([
            [0.0, -1.0, 0.0, 10.0],
            [1.0, 0.0, 0.0, 20.0],
            [0.0, 0.0, 1.0, 30.0],
        ]);
        assert_eq!(apply(&t, [1.0, 0.0, 0.0]), [10.0, 21.0, 30.0], "x axis");
        assert_eq!(t.usd_rows()[3], [10.0, 20.0, 30.0, 1.0], "translation row");
        assert_eq!(
            apply(&Transform::from_translation([1.0, 2.0, 3.0]), [0.0; 3]),
            [1.0, 2.0, 3.0],
            "translation"
        );
    }
}
