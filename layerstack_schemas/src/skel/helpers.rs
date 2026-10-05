// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composed animation introspection and rigid deformation helpers.
use super::{SkelError, compose, invalid, length, read, tokens};
use crate::{PrimView, Time, usd_skel::SkelAnimation};
use alloc::vec::Vec;
use layerstack::PropertyPath;
const TRS: [&str; 3] = ["translations", "rotations", "scales"];

/// Sampled joint-local transform components in the animation's joint order.
#[derive(Clone, Debug, PartialEq)]
pub struct JointTransformComponents {
    /// Joint-local translations.
    pub translations: Vec<[f32; 3]>,
    /// USD quaternions in `[x, y, z, real]` order.
    pub rotations: Vec<[f32; 4]>,
    /// Authored half scales widened to floats.
    pub scales: Vec<[f32; 3]>,
}
fn sample_info(prim: &PrimView<'_>, names: &[&str]) -> (Vec<f64>, bool) {
    let mut result = Vec::new();
    let mut varying = false;
    for name in names {
        if let Some(p) = prim.property_path(name) {
            result.extend(
                prim.scene()
                    .stage()
                    .property_sample_times(p.prim_path(), p.property()),
            );
            varying |= prim
                .scene()
                .stage()
                .property_might_be_time_varying(p.prim_path(), p.property());
        }
    }
    result.sort_by(f64::total_cmp);
    result.dedup_by(|a, b| *a == *b);
    (result, varying)
}
fn samples(prim: &PrimView<'_>, names: &[&str]) -> Vec<f64> {
    sample_info(prim, names).0
}
fn varying(prim: &PrimView<'_>, names: &[&str]) -> bool {
    sample_info(prim, names).1
}
fn interval(mut samples: Vec<f64>, start: f64, end: f64) -> Vec<f64> {
    if start.is_nan() || end.is_nan() || start > end {
        return Vec::new();
    }
    samples.retain(|t| *t >= start && *t <= end);
    samples
}
impl SkelAnimation<'_> {
    /// Existing transform attribute paths, in translation/rotation/scale order.
    #[must_use]
    pub fn joint_transform_attributes(&self) -> Vec<PropertyPath> {
        TRS.iter()
            .filter_map(|name| self.property_path(name))
            .collect()
    }
    /// Sorted unique stage-time samples of effective TRS sources. Layer offsets
    /// are applied. Sparse edits preserve contributing weaker grids; dense
    /// defaults/blocks and dense held regions mask weaker samples. Splines have
    /// no discrete sample times. OpenUSD `UsdSkelAnimQuery::GetJointTransformTimeSamples`.
    #[must_use]
    pub fn joint_transform_time_samples(&self) -> Vec<f64> {
        samples(self, &TRS)
    }
    /// Joint transform sample times within an inclusive interval. Reversed or
    /// NaN endpoints return no samples.
    #[must_use]
    pub fn joint_transform_time_samples_in_interval(&self, start: f64, end: f64) -> Vec<f64> {
        interval(self.joint_transform_time_samples(), start, end)
    }
    /// Whether effective TRS sources have multiple samples or a spline.
    /// One sample is constant across numeric times, but may differ at default
    /// time. This helper alone is insufficient for cache time invalidation.
    #[must_use]
    pub fn joint_transforms_might_be_time_varying(&self) -> bool {
        varying(self, &TRS)
    }
    /// Sorted unique stage-time samples of the effective weight source.
    #[must_use]
    pub fn blend_shape_weight_time_samples(&self) -> Vec<f64> {
        samples(self, &["blendShapeWeights"])
    }
    /// Blend-shape weight samples within an inclusive interval.
    #[must_use]
    pub fn blend_shape_weight_time_samples_in_interval(&self, start: f64, end: f64) -> Vec<f64> {
        interval(self.blend_shape_weight_time_samples(), start, end)
    }
    /// Whether the effective weight source has multiple samples or a spline.
    /// A single numeric sample may still differ from the default-time value.
    #[must_use]
    pub fn blend_shape_weights_might_be_time_varying(&self) -> bool {
        varying(self, &["blendShapeWeights"])
    }
    /// Reads TRS in animation joint order, validating each array's length.
    /// Absent or incompatible components return `None`, as OpenUSD's animation
    /// query does. Retained numeric decode failures return `SkelError::Decode`;
    /// this standalone query does not substitute a skeleton's rest pose.
    pub fn compute_joint_local_transform_components(
        &self,
        time: Time,
    ) -> Result<Option<JointTransformComponents>, SkelError> {
        let Some(translations) = read(self, "translations", time, crate::value::read_float3_array)?
        else {
            return Ok(None);
        };
        let Some(rotations) = read(self, "rotations", time, crate::value::read_quatf_array)? else {
            return Ok(None);
        };
        let Some(scales) = read(self, "scales", time, crate::value::read_half3_array)? else {
            return Ok(None);
        };
        let count = tokens(self, "joints")
            .ok_or_else(|| invalid(self.path(), "joints"))?
            .len();
        for (name, size) in [
            ("translations", translations.len()),
            ("rotations", rotations.len()),
            ("scales", scales.len()),
        ] {
            length(self.path(), name, count, size)?;
        }
        Ok(Some(JointTransformComponents {
            translations,
            rotations,
            scales,
        }))
    }
    /// Composes sampled TRS into joint-local matrices in animation joint order.
    /// Absent or incompatible components return `None`; malformed readable
    /// arrays and retained decode failures error.
    pub fn compute_joint_local_transforms(
        &self,
        time: Time,
    ) -> Result<Option<Vec<[[f64; 4]; 4]>>, SkelError> {
        Ok(self
            .compute_joint_local_transform_components(time)?
            .map(|c| {
                c.translations
                    .into_iter()
                    .zip(c.rotations)
                    .zip(c.scales)
                    .map(|((t, r), s)| compose(t, r, s))
                    .collect()
            }))
    }
    /// Reads sampled weights in the animation's own blend-shape order.
    /// Absent or incompatible weights return `None`; malformed readable lengths
    /// and retained decode failures error.
    pub fn compute_blend_shape_weights(&self, time: Time) -> Result<Option<Vec<f32>>, SkelError> {
        let Some(weights) = read(
            self,
            "blendShapeWeights",
            time,
            crate::value::read_float_array,
        )?
        else {
            return Ok(None);
        };
        let names = tokens(self, "blendShapes").unwrap_or_default();
        length(self.path(), "blendShapeWeights", names.len(), weights.len())?;
        Ok(Some(weights))
    }
}
