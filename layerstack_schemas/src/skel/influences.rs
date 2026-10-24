// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Normalizing, sorting and resizing flat joint influence arrays, as a
//! generator prepares `primvars:skel:jointIndices` and `jointWeights`.
//!
//! Influences are stored in blocks of `influences_per_component` entries,
//! one block per point (vertex interpolation) or one for the whole prim
//! (constant). OpenUSD: `UsdSkelNormalizeWeights`, `UsdSkelSortInfluences`
//! and `UsdSkelResizeInfluences` (`pxr/usd/usdSkel/utils.cpp`).
use alloc::vec::Vec;

/// Why an influence utility refused its arrays; they are left unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfluenceShapeError {
    /// The number of influences per component is zero (OpenUSD: "number of
    /// influences must be greater than zero").
    NoInfluencesPerComponent,
    /// The array length is not a multiple of the influences per component.
    Length {
        /// The array's length.
        len: usize,
        /// The influences per component it was split by.
        influences_per_component: usize,
    },
    /// The index and weight arrays differ in length.
    Mismatch {
        /// The number of joint indices.
        indices: usize,
        /// The number of joint weights.
        weights: usize,
    },
}

impl core::fmt::Display for InfluenceShapeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoInfluencesPerComponent => {
                f.write_str("the number of influences per component must be positive")
            }
            Self::Length {
                len,
                influences_per_component,
            } => write!(
                f,
                "{len} influences are not a multiple of {influences_per_component} per component"
            ),
            Self::Mismatch { indices, weights } => {
                write!(f, "{indices} joint indices but {weights} joint weights")
            }
        }
    }
}

impl core::error::Error for InfluenceShapeError {}

// OpenUSD `_ValidateArrayShape`.
fn validate(len: usize, influences_per_component: usize) -> Result<(), InfluenceShapeError> {
    if influences_per_component == 0 {
        return Err(InfluenceShapeError::NoInfluencesPerComponent);
    }
    if !len.is_multiple_of(influences_per_component) {
        return Err(InfluenceShapeError::Length {
            len,
            influences_per_component,
        });
    }
    Ok(())
}

/// Scales each block of `influences_per_component` weights to sum to one;
/// a block whose sum is within `f32::EPSILON` of zero becomes all zeros.
///
/// OpenUSD: `UsdSkelNormalizeWeights` with its default epsilon.
///
/// # Errors
/// [`InfluenceShapeError`] when `influences_per_component` is zero or does not
/// divide the length; `weights` is unchanged.
#[doc(alias = "UsdSkelNormalizeWeights")]
pub fn normalize_joint_weights(
    weights: &mut [f32],
    influences_per_component: usize,
) -> Result<(), InfluenceShapeError> {
    normalize_joint_weights_with_epsilon(weights, influences_per_component, f32::EPSILON)
}

/// [`normalize_joint_weights`] with an explicit `epsilon`: a block whose sum
/// has an absolute value of at most `epsilon` becomes all zeros.
///
/// # Errors
/// As [`normalize_joint_weights`].
#[doc(alias = "UsdSkelNormalizeWeights")]
pub fn normalize_joint_weights_with_epsilon(
    weights: &mut [f32],
    influences_per_component: usize,
    epsilon: f32,
) -> Result<(), InfluenceShapeError> {
    validate(weights.len(), influences_per_component)?;
    for block in weights.chunks_exact_mut(influences_per_component) {
        // Sum in order, as the C++ loop does, so rounding agrees.
        let sum: f32 = block.iter().fold(0.0, |sum, weight| sum + weight);
        if sum.abs() > epsilon {
            for weight in block {
                *weight /= sum;
            }
        } else {
            block.fill(0.0);
        }
    }
    Ok(())
}

/// Sorts each block of influences by descending weight, keeping each index
/// with its weight; equal weights order by descending joint index, as
/// OpenUSD's `std::greater` over `(weight, index)` pairs does. NaN weights
/// sort as [`f32::total_cmp`] orders them.
///
/// OpenUSD: `UsdSkelSortInfluences`.
///
/// # Errors
/// [`InfluenceShapeError::Mismatch`] for arrays of different lengths, else
/// as [`normalize_joint_weights`]; both arrays are unchanged.
#[doc(alias = "UsdSkelSortInfluences")]
pub fn sort_joint_influences(
    indices: &mut [i32],
    weights: &mut [f32],
    influences_per_component: usize,
) -> Result<(), InfluenceShapeError> {
    if indices.len() != weights.len() {
        return Err(InfluenceShapeError::Mismatch {
            indices: indices.len(),
            weights: weights.len(),
        });
    }
    validate(indices.len(), influences_per_component)?;
    if influences_per_component < 2 {
        return Ok(());
    }
    let mut block = Vec::with_capacity(influences_per_component);
    for (indices, weights) in indices
        .chunks_exact_mut(influences_per_component)
        .zip(weights.chunks_exact_mut(influences_per_component))
    {
        block.clear();
        block.extend(weights.iter().copied().zip(indices.iter().copied()));
        block.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
        for ((weight, index), (w, i)) in block.iter().zip(weights.iter_mut().zip(indices)) {
            *w = *weight;
            *i = *index;
        }
    }
    Ok(())
}

/// Changes each block of joint indices from `from` to `to` influences:
/// truncating keeps each block's first `to`, and growing pads each block
/// with joint `0`.
///
/// OpenUSD: `UsdSkelResizeInfluences` for a `VtIntArray`: equal sizes do
/// nothing, an empty array stays empty, and `to == 0` empties the array.
///
/// # Errors
/// As [`normalize_joint_weights`] for `from`; `indices` is unchanged.
#[doc(alias = "UsdSkelResizeInfluences")]
pub fn resize_joint_indices(
    indices: &mut Vec<i32>,
    from: usize,
    to: usize,
) -> Result<(), InfluenceShapeError> {
    resize(indices, from, to, 0)
}

/// Changes each block of joint weights from `from` to `to` influences:
/// truncating keeps each block's first `to` and renormalizes them (as
/// [`normalize_joint_weights`]), and growing pads each block with zeros.
///
/// OpenUSD: `UsdSkelResizeInfluences` for a `VtFloatArray`.
///
/// # Errors
/// As [`normalize_joint_weights`] for `from`, and
/// [`InfluenceShapeError::NoInfluencesPerComponent`] for `to == 0`, even
/// for no weights (OpenUSD empties the array, then fails to renormalize
/// it); `weights` is unchanged.
#[doc(alias = "UsdSkelResizeInfluences")]
pub fn resize_joint_weights(
    weights: &mut Vec<f32>,
    from: usize,
    to: usize,
) -> Result<(), InfluenceShapeError> {
    if to == 0 && from != 0 {
        validate(weights.len(), from)?;
        return Err(InfluenceShapeError::NoInfluencesPerComponent);
    }
    resize(weights, from, to, 0.0)?;
    if to < from {
        normalize_joint_weights(weights, to)?;
    }
    Ok(())
}

fn resize<T: Copy>(
    values: &mut Vec<T>,
    from: usize,
    to: usize,
    fill: T,
) -> Result<(), InfluenceShapeError> {
    if from == to {
        return Ok(());
    }
    validate(values.len(), from)?;
    let components = values.len() / from;
    if components == 0 {
        return Ok(());
    }
    if to < from {
        for component in 1..components {
            values.copy_within(component * from..component * from + to, component * to);
        }
        values.truncate(components * to);
    } else {
        values.resize(components * to, fill);
        // Move blocks back to front so no block overwrites one not yet moved.
        for component in (0..components).rev() {
            values.copy_within(component * from..(component + 1) * from, component * to);
            values[component * to + from..(component + 1) * to].fill(fill);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn normalizing_scales_blocks_and_zeroes_empty_ones() {
        let mut weights = [1.0, 3.0, 0.0, 0.0, -2.0, 2.0];
        normalize_joint_weights(&mut weights, 2).unwrap();
        assert_eq!(weights, [0.25, 0.75, 0.0, 0.0, 0.0, 0.0]);
        let mut odd = [1.0, 1.0, 1.0];
        assert_eq!(
            normalize_joint_weights(&mut odd, 2),
            Err(InfluenceShapeError::Length {
                len: 3,
                influences_per_component: 2
            })
        );
        assert_eq!(odd, [1.0; 3], "refusals leave weights unchanged");
        assert_eq!(
            normalize_joint_weights(&mut odd, 0),
            Err(InfluenceShapeError::NoInfluencesPerComponent)
        );
    }

    #[test]
    fn sorting_orders_by_weight_then_index_descending() {
        let mut indices = [1, 2, 3, 4, 7, 5, 6, 8];
        let mut weights = [0.1, 0.9, 0.5, 0.5, 0.2, 0.2, 0.6, 0.0];
        sort_joint_influences(&mut indices, &mut weights, 4).unwrap();
        assert_eq!(indices, [2, 4, 3, 1, 6, 7, 5, 8]);
        assert_eq!(weights, [0.9, 0.5, 0.5, 0.1, 0.6, 0.2, 0.2, 0.0]);
        assert_eq!(
            sort_joint_influences(&mut [0; 2], &mut [0.0; 3], 1),
            Err(InfluenceShapeError::Mismatch {
                indices: 2,
                weights: 3
            })
        );
    }

    #[test]
    fn resizing_truncates_pads_and_renormalizes() {
        let mut indices = vec![1, 2, 3, 4, 5, 6];
        resize_joint_indices(&mut indices, 3, 4).unwrap();
        assert_eq!(indices, [1, 2, 3, 0, 4, 5, 6, 0]);
        resize_joint_indices(&mut indices, 4, 1).unwrap();
        assert_eq!(indices, [1, 4]);
        resize_joint_indices(&mut indices, 1, 0).unwrap();
        assert!(indices.is_empty());
        let mut weights = vec![0.5, 0.3, 0.2, 0.1, 0.1, 0.8];
        resize_joint_weights(&mut weights, 3, 2).unwrap();
        assert_eq!(weights, [0.625, 0.375, 0.5, 0.5]);
        resize_joint_weights(&mut weights, 2, 3).unwrap();
        assert_eq!(weights, [0.625, 0.375, 0.0, 0.5, 0.5, 0.0]);
        assert_eq!(
            resize_joint_weights(&mut weights, 3, 0),
            Err(InfluenceShapeError::NoInfluencesPerComponent)
        );
        assert_eq!(weights.len(), 6, "refusals leave weights unchanged");
        let mut odd = vec![1.0; 5];
        assert!(resize_joint_weights(&mut odd, 2, 1).is_err());
        assert!(
            resize_joint_weights(&mut odd, 2, 2).is_ok(),
            "equal sizes skip checks"
        );
    }
}
