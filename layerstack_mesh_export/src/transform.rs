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
}

#[cfg(test)]
mod tests {
    use super::Transform;

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
