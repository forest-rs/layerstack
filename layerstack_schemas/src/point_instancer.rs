// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point-instance transforms and ID masks, without renderer evaluation.
//!
//! OpenUSD: `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`,
//! `ComputeMaskAtTime`, and `usdGeom/samplingUtils.cpp`. Attribute sources,
//! time offsets and interpolation follow AOUSD Core §12.3–12.5.
use crate::{
    Time, gf,
    motion_sampling::{aligned, anchor, rate},
    usd_geom::PointInstancer,
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;
use layerstack::{PathId, TargetPath, TokenInterner, Value};

/// An ID edit encountered incompatible or unreadable authored data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstanceIdEditError {
    /// Target-local `inactiveIds` is not an int64 list operation.
    InvalidInactiveIds,
    /// The composed default of `invisibleIds` is not an int64 array.
    InvalidInvisibleIds,
    /// Retained invisible ID storage failed to decode.
    Decode(layerstack::ArrayReadError),
}
impl core::fmt::Display for InstanceIdEditError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "cannot edit instance IDs: {self:?}")
    }
}
impl core::error::Error for InstanceIdEditError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Decode(error) => Some(error),
            _ => None,
        }
    }
}

/// A visibility mask does not match the array's grouped element layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstanceMaskError {
    /// Number of values in the array before compaction.
    pub array_len: usize,
    /// Number of instance entries in the nonempty mask.
    pub mask_len: usize,
    /// Number of consecutive values belonging to one instance.
    pub element_size: usize,
}
impl core::fmt::Display for InstanceMaskError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} values do not match {} mask entries of size {}",
            self.array_len, self.mask_len, self.element_size
        )
    }
}
impl core::error::Error for InstanceMaskError {}

impl PointInstancer<'_> {
    /// Applies a computed mask to consecutive groups of `element_size` values.
    /// Empty masks mean every instance survives and leave the array unchanged.
    /// A nonempty mask must describe the complete array; mismatch is reported
    /// before mutation. Compaction preserves order and reuses the allocation,
    /// without cloning values. AOUSD Core §13 (point instancing);
    /// OpenUSD `UsdGeomPointInstancer::ApplyMaskToArray`.
    #[doc(alias = "ApplyMaskToArray")]
    pub fn apply_mask_to_array<T>(
        mask: &[bool],
        values: &mut Vec<T>,
        element_size: NonZeroUsize,
    ) -> Result<(), InstanceMaskError> {
        if mask.is_empty() {
            return Ok(());
        }
        let size = element_size.get();
        if !values.len().is_multiple_of(size) || values.len() / size != mask.len() {
            return Err(InstanceMaskError {
                array_len: values.len(),
                mask_len: mask.len(),
                element_size: size,
            });
        }
        let mut at = 0;
        values.retain(|_| {
            let keep = mask[at / size];
            at += 1;
            keep
        });
        Ok(())
    }
}

fn unique_ids(ids: &[i64]) -> Vec<i64> {
    let mut seen = alloc::collections::BTreeSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

impl crate::usd_geom::PointInstancerEdit {
    /// Reactivates IDs with target-local deletions from `inactiveIds`.
    /// Preserves weaker-layer participation; repeated calls in one batch see
    /// earlier collected operations. No composed list is flattened into this
    /// target. AOUSD Core §6.6.3, §12.2.6; OpenUSD `UsdGeomPointInstancer::ActivateIds`.
    #[doc(alias = "ActivateIds")]
    pub fn activate_ids<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        self.edit_inactive_ids(edit, ids, true)?;
        Ok(self)
    }
    /// Deactivates IDs using a canonical target-local list operation.
    /// Incoming duplicates retain their first occurrence. Existing legacy
    /// added/reordered lists are normalized as sets before the new append.
    /// AOUSD Core §6.6.3, §12.2.6; OpenUSD `UsdGeomPointInstancer::DeactivateIds`.
    #[doc(alias = "DeactivateIds")]
    pub fn deactivate_ids<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        self.edit_inactive_ids(edit, ids, false)?;
        Ok(self)
    }
    fn edit_inactive_ids(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
        activate: bool,
    ) -> Result<(), InstanceIdEditError> {
        edit.group(|edit| {
            let current = match edit.local_prim_metadata(self.path(), "inactiveIds") {
                None => layerstack::ListOp::default(),
                Some(layerstack::FieldValue::Int64ListOp(op)) => *op,
                Some(_) => return Err(InstanceIdEditError::InvalidInactiveIds),
            };
            let ids = unique_ids(ids);
            let requested: alloc::collections::BTreeSet<_> = ids.iter().copied().collect();
            let result = if current.explicit.is_some() {
                let change = if activate {
                    layerstack::ListOp::default().with_deleted(ids)
                } else {
                    layerstack::ListOp::appended(ids)
                };
                layerstack::ListOp::explicit(change.apply_to(&current.apply_to(&[])))
            } else {
                let mut added = current.apply_to(&[]);
                added.sort_unstable();
                added.dedup();
                added.retain(|id| !requested.contains(id));
                let deleted = if activate {
                    unique_ids(
                        &current
                            .delete
                            .iter()
                            .chain(&ids)
                            .copied()
                            .collect::<Vec<_>>(),
                    )
                } else {
                    unique_ids(
                        &current
                            .delete
                            .iter()
                            .copied()
                            .filter(|id| !requested.contains(id))
                            .collect::<Vec<_>>(),
                    )
                };
                let mut result = layerstack::ListOp::prepended(added).with_deleted(deleted);
                if !activate {
                    result.append = ids;
                }
                result
            };
            edit.set_prim_metadata(
                self.path(),
                "inactiveIds",
                layerstack::FieldValue::Int64ListOp(Box::new(result)),
            );
            Ok(())
        })
    }
    /// Authors an explicit empty inactive list, including a cutoff of weaker
    /// and future weaker deactivations. OpenUSD `ActivateAllIds`; AOUSD Core §6.6.3, §12.2.6.
    #[doc(alias = "ActivateAllIds")]
    pub fn activate_all_ids(&self, edit: &mut crate::SchemaEdit<'_>) -> &Self {
        edit.set_prim_metadata(
            self.path(),
            "inactiveIds",
            layerstack::FieldValue::Int64ListOp(Box::new(layerstack::ListOp::explicit(Vec::new()))),
        );
        self
    }
    fn invisible_ids(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
    ) -> Result<Vec<i64>, InstanceIdEditError> {
        match edit.default_value(self.path(), "invisibleIds") {
            None => Ok(Vec::new()),
            Some(value) => {
                crate::value::try_read_array(&value, edit.tokens(), crate::value::read_int64)
                    .map_err(InstanceIdEditError::Decode)?
                    .ok_or(InstanceIdEditError::InvalidInvisibleIds)
            }
        }
    }
    /// Adds IDs to the default-time invisible list, retaining first-occurrence
    /// order and removing duplicates. Numeric samples are unchanged; use the
    /// generated sample setters for time-varying visibility. Decode failures
    /// collect no edits. OpenUSD `InvisIds`; AOUSD Core §12.3–12.5.
    #[doc(alias = "InvisIds")]
    pub fn make_invisible<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        edit.group(|edit| {
            let mut values = self.invisible_ids(edit)?;
            values.extend_from_slice(ids);
            let values = unique_ids(&values);
            self.set_invisible_ids(edit, &values);
            Ok(())
        })?;
        Ok(self)
    }
    /// Removes IDs from default-time invisibility. When any ID is removed the
    /// remaining set is sorted, matching OpenUSD `VisIds`. Incoming and retained
    /// duplicates are normalized. Numeric samples are unchanged. AOUSD §12.3–12.5.
    #[doc(alias = "VisIds")]
    pub fn make_visible<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        edit.group(|edit| {
            let mut values = self.invisible_ids(edit)?;
            let before = values.len();
            let requested: alloc::collections::BTreeSet<_> = ids.iter().copied().collect();
            values.retain(|id| !requested.contains(id));
            if values.len() != before {
                values.sort_unstable();
            }
            let values = unique_ids(&values);
            self.set_invisible_ids(edit, &values);
            Ok(())
        })?;
        Ok(self)
    }
    /// Authors an empty default-time invisible array. Numeric samples remain.
    /// OpenUSD `VisAllIds`; AOUSD Core §12.3–12.5.
    #[doc(alias = "VisAllIds")]
    pub fn make_all_visible(&self, edit: &mut crate::SchemaEdit<'_>) -> &Self {
        self.set_invisible_ids(edit, &[]);
        self
    }
}

fn read<'a, T>(
    prim: &crate::PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Result<Option<T>, PointInstancerError> {
    crate::motion_sampling::read(prim, name, time, decode).map_err(|error| {
        PointInstancerError::Decode {
            property: prim.property_path(name).expect("failed attribute exists"),
            error,
        }
    })
}

#[cfg(test)]
mod retained_input_tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn independent_inputs_preserve_float_bits_and_detect_motion_or_identity_changes() {
        let a = InstanceTransforms {
            indices: Arc::new(vec![0]),
            positions: Arc::new(vec![[f32::NAN, 0., 0.]]),
            rotations: Arc::new(Vec::new()),
            scales: Arc::new(Vec::new()),
            velocities: Arc::new(Vec::new()),
            accelerations: Arc::new(Vec::new()),
            angular: Arc::new(Vec::new()),
            ids: Some(Arc::new(vec![7])),
            mask: Vec::new(),
            prototypes: vec![gf::IDENTITY],
            include_prototype_transform: true,
            velocity_delta: 0.,
            angular_delta: 0.,
            survivors: 1,
        };
        let mut b = a.clone();
        assert!(a.same_inputs(&b));
        b.positions = Arc::new(a.positions.as_ref().clone());
        assert!(
            a.same_inputs(&b),
            "independent equal NaNs retain the same input representation"
        );
        Arc::make_mut(&mut b.positions)[0][1] = -0.;
        assert!(!a.same_inputs(&b), "signed zero is a representation change");
        b = a.clone();
        b.velocity_delta = 1.;
        assert!(!a.same_inputs(&b));
        b = a.clone();
        b.prototypes[0][3][0] = 2.;
        assert!(!a.same_inputs(&b));
        b = a.clone();
        b.ids = Some(Arc::new(vec![8]));
        assert!(!a.same_inputs(&b));
    }
}
fn vectors(
    prim: &crate::PrimView<'_>,
    name: &str,
    time: Time,
) -> Result<Option<Arc<Vec<[f32; 3]>>>, PointInstancerError> {
    read(prim, name, time, crate::value::read_float3_array_shared)
}

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PointInstancerError {
    /// A required array cannot be read with its declared type.
    MissingAttribute(&'static str),
    /// Retained numeric storage failed to decode; no partial transforms are returned.
    Decode {
        /// The failed attribute in stage namespace.
        property: layerstack::PropertyPath,
        /// The original decoder error.
        error: layerstack::ArrayReadError,
    },
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
impl core::error::Error for PointInstancerError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Decode { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl PointInstancer<'_> {
    /// Visibility mask in original array order. Empty means every instance
    /// passes, matching OpenUSD. Decode failures also return an empty mask; use
    /// [`Self::try_compute_mask`] to distinguish corrupt storage. `inactiveIds` and time-varying `invisibleIds`
    /// match stable `ids`, falling back to array positions when IDs are absent.
    #[must_use]
    #[doc(alias = "UsdGeomPointInstancer::ComputeMaskAtTime")]
    #[doc(alias = "ComputeMaskAtTime")]
    pub fn compute_mask(&self, time: Time) -> Vec<bool> {
        self.try_compute_mask(time).unwrap_or_default()
    }
    /// Computes the visibility mask while preserving retained-array decode failures.
    /// Missing and incompatible optional arrays keep the ordinary USD fallback behavior.
    /// Prefer this checked form when consuming deferred numeric data.
    #[doc(alias = "UsdGeomPointInstancer::ComputeMaskAtTime")]
    #[doc(alias = "ComputeMaskAtTime")]
    pub fn try_compute_mask(&self, time: Time) -> Result<Vec<bool>, PointInstancerError> {
        let mut hidden = if let Some(value) = self.metadata_value("inactiveIds") {
            crate::value::try_read_array(
                &value,
                self.scene().store().tokens(),
                crate::value::read_int64,
            )
            .map_err(|error| PointInstancerError::Decode {
                property: self
                    .property_path("inactiveIds")
                    .expect("failed field exists"),
                error,
            })?
            .unwrap_or_default()
        } else {
            Vec::new()
        };
        hidden.extend(
            read(self, "invisibleIds", time, crate::value::read_int64_array)?.unwrap_or_default(),
        );
        if hidden.is_empty() {
            return Ok(Vec::new());
        }
        hidden.sort_unstable();
        hidden.dedup();
        let ids = read(self, "ids", time, crate::value::read_int64_array_shared)?;
        let count = if let Some(ids) = &ids {
            ids.len()
        } else {
            read(
                self,
                "protoIndices",
                time,
                crate::value::read_int_array_shared,
            )?
            .map_or(0, |v| v.len())
        };
        let mask: Vec<_> = (0..count)
            .map(|i| {
                let id = ids.as_ref().map_or_else(
                    || i64::try_from(i).expect("instance index fits int64"),
                    |ids| ids[i],
                );
                hidden.binary_search(&id).is_err()
            })
            .collect();
        Ok(if mask.iter().all(|&v| v) {
            Vec::new()
        } else {
            mask
        })
    }
    /// Computes ordered shutter samples against one fixed topology/mask base.
    /// Preserves duplicate times and interpolation policies. Any invalid sample
    /// returns an error without a partial batch. All times must have the same
    /// default/numeric kind as `base_time`; empty times return an empty batch.
    /// Matches `UsdGeomPointInstancer::ComputeInstanceTransformsAtTimes` ordering.
    #[doc(alias = "UsdGeomPointInstancer::ComputeInstanceTransformsAtTimes")]
    #[doc(alias = "ComputeInstanceTransformsAtTimes")]
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
    #[doc(alias = "UsdGeomPointInstancer::ComputeInstanceTransformsAtTime")]
    #[doc(alias = "ComputeInstanceTransformsAtTime")]
    pub fn compute_instance_transforms(
        &self,
        time: Time,
        base_time: Time,
        options: InstanceTransformOptions,
    ) -> Result<Vec<InstanceTransform>, PointInstancerError> {
        Ok(self
            .prepare_instance_transforms(time, base_time, options)?
            .iter()
            .collect())
    }
    /// Reuses a caller-owned output allocation. Required input validation happens
    /// before changing `output`; failures leave its previous contents intact.
    pub fn compute_instance_transforms_into(
        &self,
        time: Time,
        base_time: Time,
        options: InstanceTransformOptions,
        output: &mut Vec<InstanceTransform>,
    ) -> Result<(), PointInstancerError> {
        self.prepare_instance_transforms(time, base_time, options)?
            .write_into(output);
        Ok(())
    }
    /// Captures and validates inputs without allocating per-instance matrices.
    /// Iterate the immutable snapshot directly, reuse a vector, or emit bounded
    /// chunks. Numeric inputs retain native buffers where representations match;
    /// half rotations, legacy values and interpolation can materialize inputs.
    /// Captured data remains valid after stage edits and carries no hidden cache.
    /// Motion anchoring, masks and prototype transforms match the ordinary
    /// computation (AOUSD Core §12.3–12.5; `UsdGeom::samplingUtils`).
    pub fn prepare_instance_transforms(
        &self,
        time: Time,
        base_time: Time,
        options: InstanceTransformOptions,
    ) -> Result<InstanceTransforms, PointInstancerError> {
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
            crate::value::read_int_array_shared,
        )?
        .ok_or(PointInstancerError::MissingAttribute("protoIndices"))?;
        let count = indices.len();
        let position_anchor = anchor(self, "positions", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let mut positions = vectors(self, "positions", position_anchor.time)?
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
                read(
                    self,
                    orientation_name,
                    at,
                    crate::value::read_quatf_array_shared,
                )
            } else {
                read(self, orientation_name, at, crate::value::read_quath_array)
                    .map(|v| v.map(Arc::new))
            }
        };
        let mut rotations = orientations(orientation_anchor.time)?
            .filter(|v| v.len() == count)
            .unwrap_or_default();
        let scale_anchor = anchor(self, "scales", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let mut scales = vectors(self, "scales", scale_anchor.time)?.unwrap_or_default();
        if !scales.is_empty() && scales.len() != count {
            return Err(PointInstancerError::LengthMismatch("scales"));
        }
        let velocity_anchor = anchor(self, "velocities", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let velocities = vectors(self, "velocities", velocity_anchor.time)?
            .filter(|v| v.len() == count && aligned(position_anchor, velocity_anchor))
            .unwrap_or_default();
        let acceleration_anchor = anchor(self, "accelerations", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let accelerations = vectors(self, "accelerations", acceleration_anchor.time)?
            .filter(|v| {
                v.len() == count
                    && !velocities.is_empty()
                    && aligned(velocity_anchor, acceleration_anchor)
            })
            .unwrap_or_default();
        let angular_anchor = anchor(self, "angularVelocities", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let angular = vectors(self, "angularVelocities", angular_anchor.time)?
            .filter(|v| {
                v.len() == count
                    && !rotations.is_empty()
                    && aligned(orientation_anchor, angular_anchor)
            })
            .unwrap_or_default();
        if velocities.is_empty() && angular.is_empty() {
            if let Some(values) = vectors(self, "positions", time)?.filter(|v| v.len() == count) {
                positions = values;
            }
            if let Some(values) = vectors(self, "scales", time)?.filter(|v| v.len() == count) {
                scales = values;
            }
            if let Some(values) = orientations(time)?.filter(|v| v.len() == count) {
                rotations = values;
            }
        }
        let ids = read(
            self,
            "ids",
            base_time,
            crate::value::read_int64_array_shared,
        )?;
        if ids.as_ref().is_some_and(|v| v.len() != count) {
            return Err(PointInstancerError::LengthMismatch("ids"));
        }
        let mask = if options.apply_mask {
            self.try_compute_mask(base_time)?
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
                if self.scene().prim(path).is_none() {
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
        for (i, &index) in indices.iter().enumerate() {
            if usize::try_from(index)
                .ok()
                .filter(|&v| !options.include_prototype_transform || v < prototypes.len())
                .is_none()
            {
                return Err(PointInstancerError::InvalidPrototypeIndex { instance: i, index });
            }
        }
        let survivors = if mask.is_empty() {
            count
        } else {
            mask.iter().filter(|&&v| v).count()
        };
        Ok(InstanceTransforms {
            indices,
            positions,
            rotations,
            scales,
            velocities,
            accelerations,
            angular,
            ids,
            mask,
            prototypes,
            include_prototype_transform: options.include_prototype_transform,
            velocity_delta,
            angular_delta,
            survivors,
        })
    }
}

/// Validated immutable inputs for streaming point-instance transforms.
/// Repeated iteration computes matrices without creating a complete output array.
#[derive(Clone, Debug)]
pub struct InstanceTransforms {
    indices: Arc<Vec<i32>>,
    positions: Arc<Vec<[f32; 3]>>,
    rotations: Arc<Vec<[f32; 4]>>,
    scales: Arc<Vec<[f32; 3]>>,
    velocities: Arc<Vec<[f32; 3]>>,
    accelerations: Arc<Vec<[f32; 3]>>,
    angular: Arc<Vec<[f32; 3]>>,
    ids: Option<Arc<Vec<i64>>>,
    mask: Vec<bool>,
    prototypes: Vec<[[f64; 4]; 4]>,
    include_prototype_transform: bool,
    velocity_delta: f64,
    angular_delta: f64,
    survivors: usize,
}
impl InstanceTransforms {
    /// Whether both snapshots retain identical transform inputs and policies.
    ///
    /// This compares input representations, not computed matrices. Shared array
    /// owners skip element comparison; independent owners compare their contents.
    /// Float bits distinguish signed zero and preserve identical NaNs. No
    /// matrices are evaluated and no output buffer is allocated. Hosts can use
    /// this to suppress unchanged input handoffs after conservative invalidation.
    #[must_use]
    pub fn same_inputs(&self, other: &Self) -> bool {
        fn shared<T>(a: &Arc<Vec<T>>, b: &Arc<Vec<T>>, same: impl Fn(&T, &T) -> bool) -> bool {
            Arc::ptr_eq(a, b)
                || (a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same(a, b)))
        }
        fn bits<const N: usize>(a: &[f32; N], b: &[f32; N]) -> bool {
            a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
        }
        shared(&self.indices, &other.indices, PartialEq::eq)
            && shared(&self.positions, &other.positions, bits)
            && shared(&self.rotations, &other.rotations, bits)
            && shared(&self.scales, &other.scales, bits)
            && shared(&self.velocities, &other.velocities, bits)
            && shared(&self.accelerations, &other.accelerations, bits)
            && shared(&self.angular, &other.angular, bits)
            && match (&self.ids, &other.ids) {
                (None, None) => true,
                (Some(a), Some(b)) => shared(a, b, PartialEq::eq),
                _ => false,
            }
            && self.mask == other.mask
            && self.prototypes.len() == other.prototypes.len()
            && self
                .prototypes
                .iter()
                .flatten()
                .flatten()
                .zip(other.prototypes.iter().flatten().flatten())
                .all(|(a, b)| a.to_bits() == b.to_bits())
            && self.include_prototype_transform == other.include_prototype_transform
            && self.velocity_delta.to_bits() == other.velocity_delta.to_bits()
            && self.angular_delta.to_bits() == other.angular_delta.to_bits()
            && self.survivors == other.survivors
    }

    /// Number of transforms after mask compaction.
    #[must_use]
    pub fn len(&self) -> usize {
        self.survivors
    }
    /// Whether no instance passes the captured mask.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Original instance count before masking.
    #[must_use]
    pub fn source_len(&self) -> usize {
        self.indices.len()
    }
    /// Computes each surviving transform in original array order.
    pub fn iter(&self) -> impl Iterator<Item = InstanceTransform> + '_ {
        (0..self.indices.len())
            .filter(|&i| self.mask.is_empty() || self.mask[i])
            .map(|i| self.transform(i))
    }
    /// Replaces output contents while reusing its allocation and capacity.
    pub fn write_into(&self, output: &mut Vec<InstanceTransform>) {
        output.clear();
        output.reserve(self.len());
        output.extend(self.iter());
    }
    /// Visits bounded chunks using one caller-owned scratch allocation. The
    /// final chunk may be shorter; no callback runs for an empty snapshot.
    /// Scratch is cleared on entry; on exit it contains the final partial chunk
    /// or is empty when no partial chunk remained.
    pub fn for_each_chunk(
        &self,
        chunk_size: NonZeroUsize,
        scratch: &mut Vec<InstanceTransform>,
        mut visit: impl FnMut(&[InstanceTransform]),
    ) {
        scratch.clear();
        for transform in self.iter() {
            scratch.push(transform);
            if scratch.len() == chunk_size.get() {
                visit(scratch);
                scratch.clear();
            }
        }
        if !scratch.is_empty() {
            visit(scratch);
        }
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "GfVec3f motion arithmetic rounds to float32"
    )]
    fn transform(&self, i: usize) -> InstanceTransform {
        let prototype_index = usize::try_from(self.indices[i]).expect("validated prototype index");
        let mut matrix = if self.scales.is_empty() {
            gf::IDENTITY
        } else {
            gf::scale(self.scales[i].map(f64::from))
        };
        if !self.rotations.is_empty() {
            matrix = gf::mul(&matrix, &gf::quaternion(self.rotations[i].map(f64::from)));
        }
        if !self.angular.is_empty() {
            let [x, y, z] = self.angular[i];
            // GfVec3f::GetLength rounds both the dot product and speed to
            // float32 before the double-precision rotation is constructed.
            let length = f64::from(libm::sqrtf(x * x + y * y + z * z));
            let axis = self.angular[i].map(f64::from);
            matrix = gf::mul(
                &matrix,
                &gf::Rotation::new(axis, self.angular_delta * length).matrix(),
            );
        }
        let mut position = self.positions[i];
        if !self.velocities.is_empty() {
            for (axis, coordinate) in position.iter_mut().enumerate() {
                let mut velocity = self.velocities[i][axis];
                if !self.accelerations.is_empty() {
                    velocity += ((self.velocity_delta * f64::from(self.accelerations[i][axis]))
                        as f32)
                        * 0.5;
                }
                *coordinate += (self.velocity_delta * f64::from(velocity)) as f32;
            }
        }
        matrix[3][..3].copy_from_slice(&position.map(f64::from));
        if self.include_prototype_transform {
            matrix = gf::mul(&self.prototypes[prototype_index], &matrix);
        }
        InstanceTransform {
            index: i,
            id: self.ids.as_ref().map_or_else(
                || i64::try_from(i).expect("instance index fits int64"),
                |v| v[i],
            ),
            prototype_index,
            matrix,
        }
    }
}
