// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point-instance transforms and ID masks, without renderer evaluation.
//!
//! OpenUSD: `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`,
//! `ComputeMaskAtTime`, and `usdGeom/samplingUtils.cpp`. Attribute sources,
//! time offsets and interpolation follow AOUSD Core §12.3–12.5.
use crate::{
    Time, gf,
    motion_sampling::{aligned, anchor, rate, read, vectors},
    usd_geom::PointInstancer,
};
use alloc::vec::Vec;
use layerstack::{PathId, TargetPath};

/// Policies for computing per-instance transforms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstanceTransformOptions {
    /// Prepend each prototype's local transform; never its ancestor transforms.
    pub include_prototype_transform: bool,
    /// Omit inactive and invisible IDs from the result.
    pub apply_mask: bool,
}
impl Default for InstanceTransformOptions {
    fn default() -> Self {
        Self {
            include_prototype_transform: true,
            apply_mask: true,
        }
    }
}
/// One surviving instance, retaining its original position in authored arrays.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceTransform {
    /// Original array position, before mask compaction.
    pub index: usize,
    /// Stable authored ID, or the array position when IDs are unauthored.
    pub id: i64,
    /// Index into the prototypes relationship.
    pub prototype_index: usize,
    /// Row-vector matrix in the instancer's coordinate space.
    pub matrix: [[f64; 4]; 4],
}
/// Invalid instance data; computations never return partial transforms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointInstancerError {
    /// A required array cannot be read with its declared type.
    MissingAttribute(&'static str),
    /// An authored array length differs from the instance count.
    LengthMismatch(&'static str),
    /// A prototype relationship is empty or targets a property.
    InvalidPrototypes,
    /// A prototype index is negative or outside the relationship.
    InvalidPrototypeIndex {
        /// Original instance array position.
        instance: usize,
        /// The invalid index.
        index: i32,
    },
    /// A referenced prototype is absent or has invalid transform operations.
    InvalidPrototype(PathId),
    /// Numeric times must be finite, both times must have the same kind, and
    /// the stage's time-code rate must be finite and positive.
    InvalidTime,
    /// Motion anchoring requires array samples; scalar splines are unsupported.
    UnsupportedMotionSource(&'static str),
}
impl core::fmt::Display for PointInstancerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid point instancer: {self:?}")
    }
}
impl core::error::Error for PointInstancerError {}

impl PointInstancer<'_> {
    /// Visibility mask in original array order. Empty means every instance
    /// passes, matching OpenUSD. `inactiveIds` and time-varying `invisibleIds`
    /// match stable `ids`, falling back to array positions when IDs are absent.
    #[must_use]
    pub fn compute_mask(&self, time: Time) -> Vec<bool> {
        let mut hidden = self.inactive_ids().unwrap_or_default();
        hidden.extend(
            read(self, "invisibleIds", time, crate::value::read_int64_array).unwrap_or_default(),
        );
        if hidden.is_empty() {
            return Vec::new();
        }
        hidden.sort_unstable();
        hidden.dedup();
        let ids = read(self, "ids", time, crate::value::read_int64_array).unwrap_or_else(|| {
            (0..read(self, "protoIndices", time, crate::value::read_int_array)
                .map_or(0, |v| v.len()))
                .map(|i| i64::try_from(i).expect("instance index fits int64"))
                .collect()
        });
        let mask: Vec<_> = ids
            .iter()
            .map(|id| hidden.binary_search(id).is_err())
            .collect();
        if mask.iter().all(|&v| v) {
            Vec::new()
        } else {
            mask
        }
    }
    /// Computes ordered shutter samples against one fixed topology/mask base.
    /// Preserves duplicate times and interpolation policies. Any invalid sample
    /// returns an error without a partial batch. All times must have the same
    /// default/numeric kind as `base_time`; empty times return an empty batch.
    /// Matches `UsdGeomPointInstancer::ComputeInstanceTransformsAtTimes` ordering.
    pub fn compute_instance_transforms_at_times(
        &self,
        times: &[Time],
        base_time: Time,
        options: InstanceTransformOptions,
    ) -> Result<Vec<Vec<InstanceTransform>>, PointInstancerError> {
        times
            .iter()
            .map(|&time| self.compute_instance_transforms(time, base_time, options))
            .collect()
    }
    /// Computes compacted transforms in instancer space. `base_time` fixes
    /// topology and masks and anchors velocity integration; `time` evaluates
    /// transforms. With no valid aligned motion samples, ordinary interpolation
    /// is used. Velocities and accelerations use seconds, angular velocities
    /// degrees per second. Misaligned motion arrays are ignored as in OpenUSD;
    /// malformed required arrays and prototype indices return explicit errors.
    /// Prototype transforms are local, even when their ancestors are transformed.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "GfVec3f motion arithmetic rounds each vector result to float32"
    )]
    pub fn compute_instance_transforms(
        &self,
        time: Time,
        base_time: Time,
        options: InstanceTransformOptions,
    ) -> Result<Vec<InstanceTransform>, PointInstancerError> {
        let code = match (time, base_time) {
            (Time::Default, Time::Default) => None,
            (Time::At { code, .. }, Time::At { code: base, .. })
                if code.is_finite() && base.is_finite() =>
            {
                Some(code)
            }
            _ => return Err(PointInstancerError::InvalidTime),
        };
        let indices_anchor = anchor(self, "protoIndices", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let indices = read(
            self,
            "protoIndices",
            indices_anchor.time,
            crate::value::read_int_array,
        )
        .ok_or(PointInstancerError::MissingAttribute("protoIndices"))?;
        let count = indices.len();
        let position_anchor = anchor(self, "positions", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let mut positions = vectors(self, "positions", position_anchor.time)
            .ok_or(PointInstancerError::MissingAttribute("positions"))?;
        if positions.len() != count {
            return Err(PointInstancerError::LengthMismatch("positions"));
        }
        let orientation_name = if self.has_authored_value("orientationsf") {
            "orientationsf"
        } else {
            "orientations"
        };
        let orientation_anchor = anchor(self, orientation_name, base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let orientations = |at| {
            if orientation_name == "orientationsf" {
                read(self, orientation_name, at, crate::value::read_quatf_array)
            } else {
                read(self, orientation_name, at, crate::value::read_quath_array)
            }
        };
        let mut rotations = orientations(orientation_anchor.time)
            .filter(|v| v.len() == count)
            .unwrap_or_default();
        let scale_anchor = anchor(self, "scales", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let mut scales = vectors(self, "scales", scale_anchor.time).unwrap_or_default();
        if !scales.is_empty() && scales.len() != count {
            return Err(PointInstancerError::LengthMismatch("scales"));
        }
        let velocity_anchor = anchor(self, "velocities", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let velocities = vectors(self, "velocities", velocity_anchor.time)
            .filter(|v| v.len() == count && aligned(position_anchor, velocity_anchor))
            .unwrap_or_default();
        let acceleration_anchor = anchor(self, "accelerations", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let accelerations = vectors(self, "accelerations", acceleration_anchor.time)
            .filter(|v| {
                v.len() == count
                    && !velocities.is_empty()
                    && aligned(velocity_anchor, acceleration_anchor)
            })
            .unwrap_or_default();
        let angular_anchor = anchor(self, "angularVelocities", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let angular = vectors(self, "angularVelocities", angular_anchor.time)
            .filter(|v| {
                v.len() == count
                    && !rotations.is_empty()
                    && aligned(orientation_anchor, angular_anchor)
            })
            .unwrap_or_default();
        if velocities.is_empty() && angular.is_empty() {
            if let Some(values) = vectors(self, "positions", time).filter(|v| v.len() == count) {
                positions = values;
            }
            if let Some(values) = vectors(self, "scales", time).filter(|v| v.len() == count) {
                scales = values;
            }
            if let Some(values) = orientations(time).filter(|v| v.len() == count) {
                rotations = values;
            }
        }
        let ids = read(self, "ids", base_time, crate::value::read_int64_array);
        if ids.as_ref().is_some_and(|v| v.len() != count) {
            return Err(PointInstancerError::LengthMismatch("ids"));
        }
        let mask = if options.apply_mask {
            self.compute_mask(base_time)
        } else {
            Vec::new()
        };
        if !mask.is_empty() && mask.len() != count {
            return Err(PointInstancerError::LengthMismatch("mask"));
        }
        let mut prototypes = Vec::new();
        if options.include_prototype_transform {
            let targets = self.prototypes();
            if targets.is_empty() {
                return Err(PointInstancerError::InvalidPrototypes);
            }
            for target in targets {
                let TargetPath::Prim(path) = target else {
                    return Err(PointInstancerError::InvalidPrototypes);
                };
                if !self.scene().stage().has_prim(path) {
                    return Err(PointInstancerError::InvalidPrototype(path));
                }
                let transform = crate::usd_geom::Xformable::new(&self.scene(), path)
                    .map_or_else(crate::LocalTransform::identity, |prim| {
                        prim.local_transform(time)
                    });
                if !transform.problems.is_empty() {
                    return Err(PointInstancerError::InvalidPrototype(path));
                }
                prototypes.push(transform.matrix);
            }
        }
        let rate = rate(&self.scene());
        if !rate.is_finite() || rate <= 0.0 {
            return Err(PointInstancerError::InvalidTime);
        }
        let delta = |sample: Option<f64>| {
            code.zip(sample)
                .map_or(0.0, |(code, sample)| (code - sample) / rate)
        };
        let velocity_delta = delta(velocity_anchor.sample);
        let angular_delta = delta(angular_anchor.sample);
        let mut result = Vec::new();
        for (i, &index) in indices.iter().enumerate() {
            let prototype_index = usize::try_from(index)
                .ok()
                .filter(|&v| !options.include_prototype_transform || v < prototypes.len())
                .ok_or(PointInstancerError::InvalidPrototypeIndex { instance: i, index })?;
            if !mask.is_empty() && !mask[i] {
                continue;
            }
            let mut matrix = if scales.is_empty() {
                gf::IDENTITY
            } else {
                gf::scale(scales[i].map(f64::from))
            };
            if !rotations.is_empty() {
                matrix = gf::mul(&matrix, &gf::quaternion(rotations[i].map(f64::from)));
            }
            if !angular.is_empty() {
                let [x, y, z] = angular[i];
                // GfVec3f::GetLength rounds both the dot product and speed to
                // float32 before the double-precision rotation is constructed.
                let length = f64::from(libm::sqrtf(x * x + y * y + z * z));
                let axis = angular[i].map(f64::from);
                matrix = gf::mul(
                    &matrix,
                    &gf::Rotation::new(axis, angular_delta * length).matrix(),
                );
            }
            let mut position = positions[i];
            if !velocities.is_empty() {
                for (axis, coordinate) in position.iter_mut().enumerate() {
                    let mut velocity = velocities[i][axis];
                    if !accelerations.is_empty() {
                        velocity +=
                            ((velocity_delta * f64::from(accelerations[i][axis])) as f32) * 0.5;
                    }
                    *coordinate += (velocity_delta * f64::from(velocity)) as f32;
                }
            }
            matrix[3][..3].copy_from_slice(&position.map(f64::from));
            if options.include_prototype_transform {
                matrix = gf::mul(&prototypes[prototype_index], &matrix);
            }
            result.push(InstanceTransform {
                index: i,
                id: ids.as_ref().map_or_else(
                    || i64::try_from(i).expect("instance index fits int64"),
                    |v| v[i],
                ),
                prototype_index,
                matrix,
            });
        }
        Ok(result)
    }
}
