// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Renderer-neutral affine inspection using OpenUSD `GfMatrix4d::Factor`.
//!
//! Matrices transform row vectors. Full factors preserve shear and reflection;
//! checked TRS conversion is an optional consumer operation with explicit error tolerance.
use crate::gf::decomposition::{self as math, Matrix3};

/// Why a matrix cannot be factored as finite affine data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AffineError {
    /// The matrix contains NaN or infinity, or factoring overflowed.
    NonFinite,
    /// The last column is not `[0, 0, 0, 1]`; perspective is not affine.
    NonAffine,
    /// Epsilon must be positive and finite.
    InvalidEpsilon,
}
impl core::fmt::Display for AffineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "affine factorization: {self:?}")
    }
}
impl core::error::Error for AffineError {}
/// Complete affine factors. The row-vector matrix is `R * S * Rᵀ * U * T`.
///
/// Principal stretch axes R can rotate relative to local axes. Their presence
/// is essential for shear; dropping them is not an equivalent TRS conversion.
/// Reflections use Gf's convention of negative signs on all three scales.
/// AOUSD Core §12.3 (transform attribute values); OpenUSD `GfMatrix4d::Factor`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffineFactors {
    /// Translation in the original coordinate system and length units.
    pub translation: [f64; 3],
    /// Principal stretches S, in Gf's eigenvalue order (not sorted).
    pub scale: [f64; 3],
    /// Principal stretch orientation R, as a row-vector 3×3 matrix.
    pub scale_orientation: [[f64; 3]; 3],
    /// Residual rotation U. Singular factors need not be orthogonal; no
    /// orthonormalization or loss of the original linear transform is hidden.
    pub rotation: [[f64; 3]; 3],
    /// Determinant of the original linear transform; negative means reflection.
    pub determinant: f64,
    /// Gf's singular classification `abs(determinant) < epsilon`. Singular
    /// stretches are floored using epsilon, while residual U retains the input.
    pub singular: bool,
}
impl AffineFactors {
    /// Factors finite affine data with Gf's default epsilon `1e-10`.
    pub fn compute(matrix: &[[f64; 4]; 4]) -> Result<Self, AffineError> {
        Self::with_epsilon(matrix, 1e-10)
    }
    /// Factors finite affine data with an explicit positive epsilon. Matches
    /// Gf's eigenvalue flooring and singular flag. Perspective is rejected
    /// explicitly instead of being discarded as Gf's affine Factor does.
    pub fn with_epsilon(matrix: &[[f64; 4]; 4], epsilon: f64) -> Result<Self, AffineError> {
        if !epsilon.is_finite() || epsilon <= 0. {
            return Err(AffineError::InvalidEpsilon);
        }
        if !matrix.iter().flatten().all(|v| v.is_finite()) {
            return Err(AffineError::NonFinite);
        }
        if [matrix[0][3], matrix[1][3], matrix[2][3], matrix[3][3]] != [0., 0., 0., 1.] {
            return Err(AffineError::NonAffine);
        }
        let linear: Matrix3 = core::array::from_fn(|i| core::array::from_fn(|j| matrix[i][j]));
        let (scale_orientation, scale, rotation, singular) = math::factor(&linear, epsilon);
        let determinant = math::determinant(&linear);
        if !determinant.is_finite()
            || !scale
                .iter()
                .chain(rotation.iter().flatten())
                .chain(scale_orientation.iter().flatten())
                .all(|v| v.is_finite())
        {
            return Err(AffineError::NonFinite);
        }
        Ok(Self {
            translation: [matrix[3][0], matrix[3][1], matrix[3][2]],
            scale,
            scale_orientation,
            rotation,
            determinant,
            singular,
        })
    }
    /// Symmetric stretch `R * S * Rᵀ`. Off-diagonal terms represent shear
    /// relative to the original local axes, rather than principal stretch axes.
    #[must_use]
    pub fn stretch(&self) -> [[f64; 3]; 3] {
        let mut s = math::IDENTITY;
        for (i, value) in self.scale.iter().enumerate() {
            s[i][i] = *value;
        }
        math::mul(
            &math::mul(&self.scale_orientation, &s),
            &math::transpose(&self.scale_orientation),
        )
    }
    /// Reconstructs all affine factors, including shear and reflection.
    #[must_use]
    pub fn recompose(&self) -> [[f64; 4]; 4] {
        let linear = math::mul(&self.stretch(), &self.rotation);
        let mut matrix = crate::gf::IDENTITY;
        for i in 0..3 {
            matrix[i][..3].copy_from_slice(&linear[i]);
        }
        matrix[3][..3].copy_from_slice(&self.translation);
        matrix
    }
    /// Converts to local-axis scale, rotation and translation only when the
    /// reconstructed linear matrix agrees within `relative_tolerance`.
    ///
    /// Tolerance is the maximum element error divided by the largest absolute
    /// input linear element (or 1 for the zero matrix). Translation is unchanged.
    /// No quaternion, coordinate conversion or precision narrowing is imposed.
    /// Singular matrices are rejected because their rotation is not unique.
    pub fn to_trs(&self, relative_tolerance: f64) -> Result<AffineTrs, TrsError> {
        if !relative_tolerance.is_finite() || relative_tolerance < 0. {
            return Err(TrsError::InvalidTolerance);
        }
        if self.singular {
            return Err(TrsError::Singular);
        }
        if !self
            .translation
            .iter()
            .chain(self.scale.iter())
            .chain(self.scale_orientation.iter().flatten())
            .chain(self.rotation.iter().flatten())
            .all(|v| v.is_finite())
        {
            return Err(TrsError::NonFinite);
        }
        let stretch = self.stretch();
        let rotation = math::orthonormalize(self.rotation);
        let scale = [stretch[0][0], stretch[1][1], stretch[2][2]];
        let original = math::mul(&stretch, &self.rotation);
        let candidate: Matrix3 = core::array::from_fn(|i| rotation[i].map(|v| v * scale[i]));
        if !original
            .iter()
            .flatten()
            .chain(candidate.iter().flatten())
            .chain(rotation.iter().flatten())
            .chain(scale.iter())
            .all(|v| v.is_finite())
        {
            return Err(TrsError::NonFinite);
        }
        // Reconstruction tolerance does not relax the requirement that U be
        // a proper rotation. Public factors can be constructed by callers;
        // Gf's iterative orthonormalization may leave degenerate rows unchanged.
        let gram = math::mul(&rotation, &math::transpose(&rotation));
        if (math::determinant(&rotation) - 1.).abs() > 1e-6
            || (0..3)
                .any(|i| (0..3).any(|j| (gram[i][j] - if i == j { 1. } else { 0. }).abs() > 1e-6))
        {
            return Err(TrsError::InvalidRotation);
        }
        let largest = original
            .iter()
            .flatten()
            .map(|v| v.abs())
            .fold(0., f64::max);
        let max_error = original
            .iter()
            .flatten()
            .zip(candidate.iter().flatten())
            .map(|(a, b)| (a - b).abs())
            .fold(0., f64::max);
        let relative_error = max_error / if largest > 0. { largest } else { 1. };
        if !relative_error.is_finite() {
            return Err(TrsError::NonFinite);
        }
        if relative_error > relative_tolerance {
            return Err(TrsError::Residual { relative_error });
        }
        Ok(AffineTrs {
            translation: self.translation,
            scale,
            rotation,
            relative_error,
        })
    }
}
/// Checked TRS in the original coordinate system, with row-vector rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffineTrs {
    /// Translation, unchanged from the affine factors.
    pub translation: [f64; 3],
    /// Scale along local X, Y and Z; reflection signs are retained.
    pub scale: [f64; 3],
    /// Rotation matrix, without a consumer-specific quaternion convention.
    pub rotation: [[f64; 3]; 3],
    /// Relative matrix error admitted by the caller's tolerance.
    pub relative_error: f64,
}
/// Why a complete affine factorization cannot be represented as checked TRS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrsError {
    /// A finite nonnegative tolerance is required.
    InvalidTolerance,
    /// A singular transform does not have a unique rotation.
    Singular,
    /// Factors or reconstructed matrices contain nonfinite data.
    NonFinite,
    /// The factors cannot produce a proper orthonormal rotation. This check is
    /// independent of the caller's reconstruction tolerance; rows must be
    /// orthonormal and determinant must be +1 within `1e-6`.
    InvalidRotation,
    /// A TRS reconstruction would exceed the tolerance, for example due to shear
    /// or Gf's epsilon flooring of tiny principal stretches.
    Residual {
        /// Maximum relative linear matrix element error.
        relative_error: f64,
    },
}
impl core::fmt::Display for TrsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "affine TRS conversion: {self:?}")
    }
}
impl core::error::Error for TrsError {}
