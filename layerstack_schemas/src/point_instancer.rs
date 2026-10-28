// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point-instance transforms and ID masks, without renderer evaluation.
//!
//! OpenUSD: `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`,
//! `ComputeMaskAtTime`, and `usdGeom/samplingUtils.cpp`. Attribute sources,
//! time offsets and interpolation follow AOUSD Core §12.3–12.5.
use crate::{
    Time,
    bounds::{BoundsCache, BoundsError, BoundsOptions, Extent, Range3d},
    gf,
    motion_sampling::{aligned, anchor, rate},
    usd_geom::{ImageablePurpose, PointInstancer},
};
use alloc::{boxed::Box, collections::BTreeSet, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;
use hashbrown::HashSet;
use layerstack::{
    FieldValue, ListOp, MetadataTarget, OpinionValue, PathId, TargetPath, TokenInterner, Value,
};

/// An ID edit encountered incompatible or unreadable authored data.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
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
#[non_exhaustive]
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

// `SdfListOp::_MakeUnique`: deletes and prepends keep each item's first
// occurrence, appends its last.
fn unique_first(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}
fn unique_last(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::with_capacity(ids.len());
    let mut kept: Vec<i64> = ids
        .iter()
        .rev()
        .copied()
        .filter(|id| seen.insert(*id))
        .collect();
    kept.reverse();
    kept
}

// `SdfListOp::ApplyOperations(vector*)`, linear in the list lengths with
// hash sets: an explicit list keeps first occurrences; deletes, then legacy
// added items the list lacks, then prepends (first occurrences, in front)
// and appends (last occurrences, at the back), each moving items it names.
// Reordered items fall back to `ListOp::apply_in_place`, which compares
// item by item; neither this module nor OpenUSD's point instancer authors them.
fn apply_linear(op: &ListOp<i64>, items: &mut Vec<i64>) {
    if !op.reorder.is_empty() {
        op.apply_in_place(items);
        return;
    }
    if let Some(explicit) = &op.explicit {
        *items = unique_first(explicit);
        return;
    }
    if !op.delete.is_empty() {
        let deleted: HashSet<i64> = op.delete.iter().copied().collect();
        items.retain(|id| !deleted.contains(id));
    }
    if !op.add.is_empty() {
        let mut present: HashSet<i64> = items.iter().copied().collect();
        items.extend(op.add.iter().copied().filter(|id| present.insert(*id)));
    }
    if !op.prepend.is_empty() {
        let prepend = unique_first(&op.prepend);
        let moved: HashSet<i64> = prepend.iter().copied().collect();
        items.retain(|id| !moved.contains(id));
        items.splice(0..0, prepend);
    }
    if !op.append.is_empty() {
        let append = unique_last(&op.append);
        let moved: HashSet<i64> = append.iter().copied().collect();
        items.retain(|id| !moved.contains(id));
        items.extend(append);
    }
}

// `_CanonicalizeListOp` in OpenUSD's `pointInstancer.cpp`: an explicit list
// stays as it is; any other becomes its items over an empty list, sorted, as
// prepends, keeping its deletes, so it has no legacy added or reordered items.
fn canonical(op: ListOp<i64>) -> ListOp<i64> {
    if op.explicit.is_some() {
        return op;
    }
    let mut items = Vec::new();
    apply_linear(&op, &mut items);
    items.sort_unstable();
    ListOp::prepended(unique_first(&items)).with_deleted(unique_first(&op.delete))
}

// `SdfListOp::ApplyOperations(inner)` for a `proposed` operation of deletes,
// prepends and appends over a canonical `inner` one: the single operation
// equivalent to applying `inner`, then `proposed`. Hash sets keep each phase
// linear in the list lengths, where OpenUSD erases item by item.
fn compose_over(proposed: &ListOp<i64>, inner: ListOp<i64>) -> ListOp<i64> {
    if let Some(mut items) = inner.explicit {
        apply_linear(proposed, &mut items);
        return ListOp::explicit(unique_first(&items));
    }
    let (mut delete, mut prepend, mut append) = (inner.delete, inner.prepend, inner.append);
    let deleted: HashSet<i64> = proposed.delete.iter().copied().collect();
    prepend.retain(|id| !deleted.contains(id));
    append.retain(|id| !deleted.contains(id));
    let mut present: HashSet<i64> = delete.iter().copied().collect();
    delete.extend(
        proposed
            .delete
            .iter()
            .copied()
            .filter(|id| present.insert(*id)),
    );
    let mut insert = |items: &[i64], front: bool| {
        let inserted: HashSet<i64> = items.iter().copied().collect();
        delete.retain(|id| !inserted.contains(id));
        prepend.retain(|id| !inserted.contains(id));
        append.retain(|id| !inserted.contains(id));
        if front {
            prepend.splice(0..0, items.iter().copied());
        } else {
            append.extend_from_slice(items);
        }
    };
    insert(&proposed.prepend, true);
    insert(&proposed.append, false);
    ListOp::prepended(prepend)
        .with_appended(append)
        .with_deleted(delete)
}

impl crate::usd_geom::PointInstancerEdit {
    /// Reactivates IDs: deletes them from the edit target's `inactiveIds`.
    ///
    /// As OpenUSD's `ActivateIds` (and `ActivateId`): the target's list
    /// operation is canonicalized (its items over an empty list, sorted, as
    /// prepends, keeping its deletes; an explicit list stays explicit), then
    /// the deletes are applied over it. Weaker layers keep participating, and
    /// calls in one edit see the operations collected before them.
    /// AOUSD Core §6.6.3 (list operations), §12.2.6.
    ///
    /// # Errors
    /// [`InstanceIdEditError::InvalidInactiveIds`] when the target authors
    /// `inactiveIds` with another type; nothing is authored.
    #[doc(alias = "UsdGeomPointInstancer::ActivateIds")]
    #[doc(alias = "ActivateIds")]
    #[doc(alias = "ActivateId")]
    pub fn activate_ids<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        self.edit_inactive_ids(edit, ListOp::deleted(unique_first(ids)))?;
        Ok(self)
    }
    /// Deactivates IDs: appends them to the edit target's `inactiveIds`, after
    /// canonicalizing it as [`Self::activate_ids`] does. Repeated IDs keep
    /// their last occurrence, as an appended list does in OpenUSD.
    /// AOUSD Core §6.6.3, §12.2.6.
    ///
    /// # Errors
    /// As [`Self::activate_ids`].
    #[doc(alias = "UsdGeomPointInstancer::DeactivateIds")]
    #[doc(alias = "DeactivateIds")]
    #[doc(alias = "DeactivateId")]
    pub fn deactivate_ids<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        self.edit_inactive_ids(edit, ListOp::appended(unique_last(ids)))?;
        Ok(self)
    }
    // `UsdGeomPointInstancerSetOrMergeOverOp` with OpenUSD's default
    // `USDGEOM_POINTINSTANCER_NEW_APPLYOPS`.
    fn edit_inactive_ids(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        proposed: ListOp<i64>,
    ) -> Result<(), InstanceIdEditError> {
        let current = match edit.local_prim_metadata(self.path(), "inactiveIds") {
            None => ListOp::default(),
            Some(FieldValue::Int64ListOp(op)) => *op,
            Some(_) => return Err(InstanceIdEditError::InvalidInactiveIds),
        };
        let result = compose_over(&proposed, canonical(current));
        edit.set_prim_metadata(
            self.path(),
            "inactiveIds",
            FieldValue::Int64ListOp(Box::new(result)),
        );
        Ok(())
    }
    /// Authors an explicit empty `inactiveIds`, cutting off weaker and future
    /// weaker deactivations. OpenUSD `ActivateAllIds`; AOUSD Core §6.6.3, §12.2.6.
    #[doc(alias = "UsdGeomPointInstancer::ActivateAllIds")]
    #[doc(alias = "ActivateAllIds")]
    pub fn activate_all_ids(&self, edit: &mut crate::SchemaEdit<'_>) -> &Self {
        edit.set_prim_metadata(
            self.path(),
            "inactiveIds",
            FieldValue::Int64ListOp(Box::new(ListOp::explicit(Vec::new()))),
        );
        self
    }
    /// `invisibleIds` at `time` as the edit leaves it; `None` when no value
    /// (not even the fallback) answers, or a block is in effect.
    fn invisible_ids(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        time: Option<f64>,
    ) -> Result<Option<Vec<i64>>, InstanceIdEditError> {
        match edit.value_at(self.path(), "invisibleIds", time) {
            None | Some(Value::Blocked) => Ok(None),
            Some(value) => crate::value::try_read_int64_array_shared(&value, edit.tokens())
                .map_err(InstanceIdEditError::Decode)?
                .map(|ids| Some(Arc::unwrap_or_clone(ids)))
                .ok_or(InstanceIdEditError::InvalidInvisibleIds),
        }
    }
    fn write_invisible_ids(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        time: Option<f64>,
        ids: Vec<i64>,
    ) {
        match time {
            None => self.set_invisible_ids_owned(edit, ids),
            Some(time) => self.set_invisible_ids_owned_at(edit, time, ids),
        };
    }
    /// Makes IDs invisible at the default time: appends each ID that
    /// `invisibleIds` does not already hold to its default, as the edit
    /// leaves it. As in OpenUSD, the IDs are checked against the list as it
    /// was before the call, so an ID repeated in `ids` is appended each time.
    /// Time samples are unchanged; see [`Self::make_invisible_at`].
    /// OpenUSD `InvisIds`; AOUSD Core §12.3.
    ///
    /// # Errors
    /// [`InstanceIdEditError::InvalidInvisibleIds`] when `invisibleIds` has
    /// another type (OpenUSD then authors nothing and reports success), and
    /// [`InstanceIdEditError::Decode`] when retained storage fails to decode;
    /// nothing is authored either way.
    #[doc(alias = "UsdGeomPointInstancer::InvisIds")]
    #[doc(alias = "InvisIds")]
    #[doc(alias = "InvisId")]
    pub fn make_invisible<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        self.invis_ids(edit, None, ids)
    }
    /// [`Self::make_invisible`] at the time code `time`: reads `invisibleIds`
    /// at `time` (held, as the edit leaves it) and authors the result as a
    /// time sample there. AOUSD Core §12.3.2.
    ///
    /// # Errors
    /// As [`Self::make_invisible`].
    #[doc(alias = "UsdGeomPointInstancer::InvisIds")]
    #[doc(alias = "InvisIds")]
    pub fn make_invisible_at<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        time: f64,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        self.invis_ids(edit, Some(time), ids)
    }
    fn invis_ids(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        time: Option<f64>,
        ids: &[i64],
    ) -> Result<&Self, InstanceIdEditError> {
        let Some(mut values) = self.invisible_ids(edit, time)? else {
            return Ok(self);
        };
        let held: HashSet<i64> = values.iter().copied().collect();
        values.extend(ids.iter().copied().filter(|id| !held.contains(id)));
        self.write_invisible_ids(edit, time, values);
        Ok(self)
    }
    /// Makes IDs visible at the default time: removes them from the default of
    /// `invisibleIds`, as the edit leaves it. When any ID is removed the rest
    /// are written sorted and without repeats, as OpenUSD's `VisIds` writes
    /// its set; otherwise the list is written back unchanged. Time samples
    /// are unchanged; see [`Self::make_visible_at`]. AOUSD Core §12.3.
    ///
    /// # Errors
    /// As [`Self::make_invisible`].
    #[doc(alias = "UsdGeomPointInstancer::VisIds")]
    #[doc(alias = "VisIds")]
    #[doc(alias = "VisId")]
    pub fn make_visible<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        self.vis_ids(edit, None, ids)
    }
    /// [`Self::make_visible`] at the time code `time`, reading and authoring
    /// the time sample there. AOUSD Core §12.3.2.
    ///
    /// # Errors
    /// As [`Self::make_invisible`].
    #[doc(alias = "UsdGeomPointInstancer::VisIds")]
    #[doc(alias = "VisIds")]
    pub fn make_visible_at<'a>(
        &'a self,
        edit: &mut crate::SchemaEdit<'_>,
        time: f64,
        ids: &[i64],
    ) -> Result<&'a Self, InstanceIdEditError> {
        self.vis_ids(edit, Some(time), ids)
    }
    fn vis_ids(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        time: Option<f64>,
        ids: &[i64],
    ) -> Result<&Self, InstanceIdEditError> {
        let Some(values) = self.invisible_ids(edit, time)? else {
            return Ok(self);
        };
        let mut set: BTreeSet<i64> = values.iter().copied().collect();
        let removed = ids.iter().filter(|id| set.remove(id)).count();
        let values = if removed == 0 {
            values
        } else {
            set.into_iter().collect()
        };
        self.write_invisible_ids(edit, time, values);
        Ok(self)
    }
    /// Authors an empty default `invisibleIds` when the attribute has any
    /// authored value (default or samples) as the edit leaves it; otherwise
    /// authors nothing. Time samples are unchanged. OpenUSD `VisAllIds`;
    /// AOUSD Core §12.3.
    #[doc(alias = "UsdGeomPointInstancer::VisAllIds")]
    #[doc(alias = "VisAllIds")]
    pub fn make_all_visible(&self, edit: &mut crate::SchemaEdit<'_>) -> &Self {
        self.vis_all_ids(edit, None)
    }
    /// [`Self::make_all_visible`] authoring an empty time sample at `time`.
    #[doc(alias = "UsdGeomPointInstancer::VisAllIds")]
    #[doc(alias = "VisAllIds")]
    pub fn make_all_visible_at(&self, edit: &mut crate::SchemaEdit<'_>, time: f64) -> &Self {
        self.vis_all_ids(edit, Some(time))
    }
    fn vis_all_ids(&self, edit: &mut crate::SchemaEdit<'_>, time: Option<f64>) -> &Self {
        if edit.has_authored_value(self.path(), "invisibleIds") {
            self.write_invisible_ids(edit, time, Vec::new());
        }
        self
    }
}

// `ComputeMaskAtTime`'s loop: one entry per ID, empty when none is masked.
fn mask_from(hidden: &HashSet<i64>, ids: impl ExactSizeIterator<Item = i64>) -> Vec<bool> {
    let mut mask = Vec::with_capacity(ids.len());
    let mut pruned = false;
    for id in ids {
        let keep = !hidden.contains(&id);
        pruned |= !keep;
        mask.push(keep);
    }
    if pruned { mask } else { Vec::new() }
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
mod list_op_tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn appended_ids_keep_their_last_occurrence_and_deletes_their_first() {
        assert_eq!(unique_last(&[40, 10, 40, 7, 10]), vec![40, 7, 10]);
        assert_eq!(unique_first(&[40, 10, 40, 7, 10]), vec![40, 10, 7]);
    }

    #[test]
    fn linear_application_matches_list_op_application() {
        let ops = [
            ListOp::explicit(vec![3, 1, 3, 2]),
            ListOp::deleted(vec![2, 9]),
            ListOp::added(vec![7, 1, 7]),
            ListOp::prepended(vec![5, 1, 5]).with_deleted(vec![4]),
            ListOp::appended(vec![1, 6, 1]).with_prepended(vec![6, 8]),
            ListOp::added(vec![2])
                .with_appended(vec![3])
                .with_prepended(vec![2]),
            ListOp::reordered(vec![3, 1]),
        ];
        for base in [vec![], vec![1, 2, 3, 4]] {
            for op in &ops {
                let mut items = base.clone();
                apply_linear(op, &mut items);
                assert_eq!(items, op.apply_to(&base), "{op:?} over {base:?}");
            }
        }
    }

    #[test]
    fn canonical_operations_prepend_sorted_items_and_keep_deletes() {
        let legacy = ListOp::added(vec![30, 10]).with_deleted(vec![20]);
        assert_eq!(
            canonical(legacy),
            ListOp::prepended(vec![10, 30]).with_deleted(vec![20])
        );
        let explicit = ListOp::explicit(vec![3, 1]);
        assert_eq!(canonical(explicit.clone()), explicit);
    }

    #[test]
    fn proposed_edits_compose_over_canonical_operations() {
        let inner = ListOp::prepended(vec![10, 30]).with_deleted(vec![20]);
        assert_eq!(
            compose_over(&ListOp::appended(vec![10, 40]), inner.clone()),
            ListOp::prepended(vec![30])
                .with_appended(vec![10, 40])
                .with_deleted(vec![20])
        );
        assert_eq!(
            compose_over(&ListOp::deleted(vec![30, 90]), inner),
            ListOp::prepended(vec![10]).with_deleted(vec![20, 30, 90])
        );
        assert_eq!(
            compose_over(&ListOp::deleted(vec![3]), ListOp::explicit(vec![1, 3])),
            ListOp::explicit(vec![1])
        );
    }
}

#[cfg(test)]
mod retained_input_tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn snapshots_are_shareable_and_index_original_positions() {
        fn shareable<T: Send + Sync>() {}
        shareable::<InstanceTransforms>();
        let transforms = InstanceTransforms {
            indices: Arc::new(vec![0, 0, 0]),
            positions: Arc::new(vec![[1., 0., 0.], [2., 0., 0.], [3., 0., 0.]]),
            rotations: Arc::new(Vec::new()),
            scales: Arc::new(vec![[2., 2., 2.]; 3]),
            velocities: Arc::new(Vec::new()),
            accelerations: Arc::new(Vec::new()),
            angular: Arc::new(Vec::new()),
            ids: None,
            mask: Arc::new(vec![true, false, true]),
            prototypes: vec![gf::IDENTITY],
            include_prototype_transform: true,
            velocity_delta: 0.,
            angular_delta: 0.,
            survivors: 2,
        };
        assert!(transforms.get(1).is_none(), "masked");
        assert!(transforms.get(3).is_none(), "past the end");
        let third = transforms.get(2).unwrap();
        assert_eq!((third.index, third.id), (2, 2));
        assert_eq!(third.matrix[3], [3., 0., 0., 1.]);
        assert_eq!(third.matrix[0][0], 2.);
        assert_eq!(
            transforms.iter().collect::<Vec<_>>(),
            vec![transforms.get(0).unwrap(), third]
        );
    }

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
            mask: Arc::new(Vec::new()),
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
#[non_exhaustive]
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
    /// A prototype relationship is empty, or (for bounds and extents) targets
    /// a property.
    InvalidPrototypes,
    /// A prototype index is negative or outside the relationship.
    InvalidPrototypeIndex {
        /// Original instance array position.
        instance: usize,
        /// The invalid index.
        index: i32,
    },
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
        let hidden = self.masked_ids(time)?;
        if hidden.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(ids) = read(self, "ids", time, crate::value::read_int64_array_shared)? {
            return Ok(mask_from(&hidden, ids.iter().copied()));
        }
        let count = read(
            self,
            "protoIndices",
            time,
            crate::value::read_int_array_shared,
        )?
        .map_or(0, |v| v.len());
        Ok(mask_from(
            &hidden,
            (0..count).map(|i| i64::try_from(i).expect("instance index fits int64")),
        ))
    }
    /// The visibility mask of `ids`, in their order, instead of the
    /// instancer's own `ids` (or array positions): one entry per given ID,
    /// `false` where `inactiveIds` or `invisibleIds` at `time` holds it.
    /// Empty when no given ID is masked, as for [`Self::compute_mask`]. Use
    /// it for a subset or a reordering of instances, or for IDs read once and
    /// reused across frames. Decode failures return an empty mask; use
    /// [`Self::try_compute_mask_for_ids`] to distinguish them.
    #[must_use]
    #[doc(alias = "UsdGeomPointInstancer::ComputeMaskAtTime")]
    #[doc(alias = "ComputeMaskAtTime")]
    pub fn compute_mask_for_ids(&self, time: Time, ids: &[i64]) -> Vec<bool> {
        self.try_compute_mask_for_ids(time, ids).unwrap_or_default()
    }
    /// [`Self::compute_mask_for_ids`], preserving retained-array decode
    /// failures of `inactiveIds` and `invisibleIds`. Reads neither `ids` nor
    /// `protoIndices`, as OpenUSD's `ComputeMaskAtTime(time, ids)` does not.
    #[doc(alias = "UsdGeomPointInstancer::ComputeMaskAtTime")]
    #[doc(alias = "ComputeMaskAtTime")]
    pub fn try_compute_mask_for_ids(
        &self,
        time: Time,
        ids: &[i64],
    ) -> Result<Vec<bool>, PointInstancerError> {
        let hidden = self.masked_ids(time)?;
        if hidden.is_empty() {
            return Ok(Vec::new());
        }
        Ok(mask_from(&hidden, ids.iter().copied()))
    }
    /// The composed `inactiveIds` when every opinion is an int64 list
    /// operation: applied weakest first from the strongest explicit list, as
    /// metadata resolution applies them (AOUSD Core §12.2.6), but linearly,
    /// so a large inactive list costs no more than one pass per opinion.
    /// `None` defers to the stage's resolution of other values.
    fn composed_inactive_ids(&self) -> Option<Vec<i64>> {
        let scene = self.scene();
        let key = scene.store().tokens().lookup("inactiveIds")?;
        if !scene
            .stage()
            .schemas()
            .and_then(|schemas| schemas.metadata(key))
            .is_some_and(|definition| definition.applies_to(MetadataTarget::Prim))
        {
            return None;
        }
        let mut chain = Vec::new();
        for opinion in self.prim()?.metadata_opinions(key) {
            let OpinionValue::Field(FieldValue::Int64ListOp(op)) = opinion.value() else {
                return None;
            };
            chain.push(&**op);
            if op.explicit.is_some() {
                break;
            }
        }
        let mut items = Vec::new();
        for op in chain.into_iter().rev() {
            apply_linear(op, &mut items);
        }
        Some(items)
    }
    /// The composed `inactiveIds` and `invisibleIds` at `time`, as a set: one
    /// hash lookup per instance, where OpenUSD searches a `std::set`.
    fn masked_ids(&self, time: Time) -> Result<HashSet<i64>, PointInstancerError> {
        let mut hidden = if let Some(ids) = self.composed_inactive_ids() {
            ids
        } else if let Some(value) = self.metadata_value("inactiveIds") {
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
        if let Some(invisible) = read(
            self,
            "invisibleIds",
            time,
            crate::value::read_int64_array_shared,
        )? {
            hidden.extend_from_slice(&invisible);
        }
        Ok(hidden.into_iter().collect())
    }
    /// Computes ordered shutter samples against one fixed topology/mask base.
    /// Preserves duplicate times and interpolation policies. Any invalid sample
    /// returns an error without a partial batch. All times must have the same
    /// default/numeric kind as `base_time`; empty times return an empty batch.
    ///
    /// Base-time inputs (topology, IDs, the mask, motion arrays) are read once
    /// for the whole batch, as OpenUSD's `ComputeInstanceTransformsAtTimes`
    /// reads them. Without usable motion, positions, scales and orientations
    /// interpolate at each time; a time whose array does not match the
    /// instance count keeps the previous time's array (the base time's for
    /// the first), as OpenUSD carries it from sample to sample.
    #[doc(alias = "UsdGeomPointInstancer::ComputeInstanceTransformsAtTimes")]
    #[doc(alias = "ComputeInstanceTransformsAtTimes")]
    pub fn compute_instance_transforms_at_times(
        &self,
        times: &[Time],
        base_time: Time,
        options: InstanceTransformOptions,
    ) -> Result<Vec<Vec<InstanceTransform>>, PointInstancerError> {
        self.prepare_instance_transforms_at_times(times, base_time, options)
            .map(|batch| batch.iter().map(|sample| sample.iter().collect()).collect())
    }
    /// Computes compacted transforms in instancer space. `base_time` fixes
    /// topology and masks and anchors velocity integration; `time` evaluates
    /// transforms. With no valid aligned motion samples, ordinary interpolation
    /// is used. Velocities and accelerations use seconds, angular velocities
    /// degrees per second. Misaligned motion arrays are ignored as in OpenUSD;
    /// malformed required arrays and prototype indices return explicit errors.
    /// Prototype transforms are local, even when their ancestors are
    /// transformed; a prototype target that is not a prim contributes the
    /// identity, as in OpenUSD.
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
        sample_code(time, base_time)?;
        let mut base = self.base_inputs(base_time, options)?;
        self.sample_inputs(&mut base, time)
    }
    /// [`Self::prepare_instance_transforms`] for each of `times` against one
    /// base, reading base-time inputs once, as
    /// [`Self::compute_instance_transforms_at_times`] does.
    pub fn prepare_instance_transforms_at_times(
        &self,
        times: &[Time],
        base_time: Time,
        options: InstanceTransformOptions,
    ) -> Result<Vec<InstanceTransforms>, PointInstancerError> {
        for &time in times {
            sample_code(time, base_time)?;
        }
        if times.is_empty() {
            return Ok(Vec::new());
        }
        let mut base = self.base_inputs(base_time, options)?;
        times
            .iter()
            .map(|&time| self.sample_inputs(&mut base, time))
            .collect()
    }
    // `_ComputePointInstancerAttributesPreamble` and the base-time reads of
    // `_DoComputeInstanceTransformsAtTimes`.
    fn base_inputs(
        &self,
        base_time: Time,
        options: InstanceTransformOptions,
    ) -> Result<BaseInputs, PointInstancerError> {
        sample_code(base_time, base_time)?;
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
        let positions = vectors(self, "positions", position_anchor.time)?
            .ok_or(PointInstancerError::MissingAttribute("positions"))?;
        if positions.len() != count {
            return Err(PointInstancerError::LengthMismatch("positions"));
        }
        let orientations_f = self.has_authored_value("orientationsf");
        let orientation_name = if orientations_f {
            "orientationsf"
        } else {
            "orientations"
        };
        let orientation_anchor = anchor(self, orientation_name, base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let rotations = self
            .read_orientations(orientations_f, orientation_anchor.time)?
            .filter(|v| v.len() == count)
            .unwrap_or_default();
        let scale_anchor = anchor(self, "scales", base_time)
            .map_err(PointInstancerError::UnsupportedMotionSource)?;
        let scales = vectors(self, "scales", scale_anchor.time)?.unwrap_or_default();
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
        let ids = read(
            self,
            "ids",
            base_time,
            crate::value::read_int64_array_shared,
        )?;
        if ids.as_ref().is_some_and(|v| v.len() != count) {
            return Err(PointInstancerError::LengthMismatch("ids"));
        }
        let mask = Arc::new(if options.apply_mask {
            self.try_compute_mask(base_time)?
        } else {
            Vec::new()
        });
        if !mask.is_empty() && mask.len() != count {
            return Err(PointInstancerError::LengthMismatch("mask"));
        }
        let mut prototypes = Vec::new();
        if options.include_prototype_transform {
            let targets = self.prototypes();
            if targets.is_empty() {
                return Err(PointInstancerError::InvalidPrototypes);
            }
            // `stage->GetPrimAtPath` of a property target, or of a path
            // with no prim, is invalid: its transform is the identity.
            prototypes.extend(targets.into_iter().map(|target| match target {
                TargetPath::Prim(path) => self.scene().prim(path).map(|_| path),
                TargetPath::Property(_) => None,
            }));
        }
        for (i, &index) in indices.iter().enumerate() {
            if usize::try_from(index)
                .ok()
                .filter(|&v| !options.include_prototype_transform || v < prototypes.len())
                .is_none()
            {
                return Err(PointInstancerError::InvalidPrototypeIndex { instance: i, index });
            }
        }
        let rate = rate(&self.scene());
        if !rate.is_finite() || rate <= 0.0 {
            return Err(PointInstancerError::InvalidTime);
        }
        let survivors = if mask.is_empty() {
            count
        } else {
            mask.iter().filter(|&&v| v).count()
        };
        Ok(BaseInputs {
            interpolate: velocities.is_empty() && angular.is_empty(),
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
            orientations_f,
            include_prototype_transform: options.include_prototype_transform,
            velocity_sample: velocity_anchor.sample,
            angular_sample: angular_anchor.sample,
            rate,
            survivors,
        })
    }
    // One sample of `_DoComputeInstanceTransformsAtTimes`: interpolated
    // arrays (carried into later samples), prototype transforms at `time`
    // (`UsdGeomXformCache(time)`) and the motion deltas.
    fn sample_inputs(
        &self,
        base: &mut BaseInputs,
        time: Time,
    ) -> Result<InstanceTransforms, PointInstancerError> {
        let code = sample_code(time, time)?;
        let count = base.indices.len();
        if base.interpolate {
            if let Some(values) = vectors(self, "positions", time)?.filter(|v| v.len() == count) {
                base.positions = values;
            }
            if let Some(values) = vectors(self, "scales", time)?.filter(|v| v.len() == count) {
                base.scales = values;
            }
            if let Some(values) = self
                .read_orientations(base.orientations_f, time)?
                .filter(|v| v.len() == count)
            {
                base.rotations = values;
            }
        }
        let prototypes = base
            .prototypes
            .iter()
            .map(|path| {
                path.and_then(|path| crate::usd_geom::Xformable::new(&self.scene(), path))
                    .map_or(gf::IDENTITY, |prim| prim.local_transform(time).matrix)
            })
            .collect();
        let delta = |sample: Option<f64>| {
            code.zip(sample)
                .map_or(0.0, |(code, sample)| (code - sample) / base.rate)
        };
        Ok(InstanceTransforms {
            indices: base.indices.clone(),
            positions: base.positions.clone(),
            rotations: base.rotations.clone(),
            scales: base.scales.clone(),
            velocities: base.velocities.clone(),
            accelerations: base.accelerations.clone(),
            angular: base.angular.clone(),
            ids: base.ids.clone(),
            mask: base.mask.clone(),
            prototypes,
            include_prototype_transform: base.include_prototype_transform,
            velocity_delta: delta(base.velocity_sample),
            angular_delta: delta(base.angular_sample),
            survivors: base.survivors,
        })
    }
    fn read_orientations(
        &self,
        float: bool,
        time: Time,
    ) -> Result<Option<Arc<Vec<[f32; 4]>>>, PointInstancerError> {
        if float {
            read(
                self,
                "orientationsf",
                time,
                crate::value::read_quatf_array_shared,
            )
        } else {
            read(self, "orientations", time, crate::value::read_quath_array)
                .map(|v| v.map(Arc::new))
        }
    }
}

impl PointInstancer<'_> {
    /// The instancer's extent at `time`: the aligned box, in its own space
    /// (or, with `transform`, in the space `transform` maps it to), around
    /// every unmasked instance's prototype bounds.
    ///
    /// As OpenUSD's `ComputeExtentAtTime`: `protoIndices` and the mask are
    /// read at `base_time`, prototype bounds are the default, proxy and
    /// render purposes' untransformed bounds at `time`, and each instance's
    /// box is carried through its transform (prototype transform included),
    /// then `transform`, before its aligned range joins the extent. With no
    /// instance left, the extent is the empty range (`f32::MAX` minimum,
    /// `-f32::MAX` maximum). `base_time` anchors motion as for
    /// [`Self::compute_instance_transforms`]. AOUSD Core §13 (point
    /// instancing); OpenUSD `UsdGeomBoundable` (extent is local space).
    ///
    /// # Errors
    /// [`BoundsError::InvalidPointInstancer`] for invalid instance data or a
    /// prototype target that is not a prim, and the prototypes' own bounds
    /// errors (a missing prototype prim is [`BoundsError::MissingPrim`]).
    #[doc(alias = "UsdGeomPointInstancer::ComputeExtentAtTime")]
    #[doc(alias = "ComputeExtentAtTime")]
    pub fn compute_extent(
        &self,
        time: Time,
        base_time: Time,
        transform: Option<&[[f64; 4]; 4]>,
    ) -> Result<Extent, BoundsError> {
        self.compute_extent_at_times(&[time], base_time, transform)
            .map(|extents| extents[0])
    }
    /// [`Self::compute_extent`] at each of `times`, in order, against one
    /// base: the instance transforms are one batch, as for
    /// [`Self::compute_instance_transforms_at_times`], and one bounds cache
    /// serves every time. OpenUSD `ComputeExtentAtTimes`.
    ///
    /// # Errors
    /// As [`Self::compute_extent`]; no partial batch is returned.
    #[doc(alias = "UsdGeomPointInstancer::ComputeExtentAtTimes")]
    #[doc(alias = "ComputeExtentAtTimes")]
    pub fn compute_extent_at_times(
        &self,
        times: &[Time],
        base_time: Time,
        transform: Option<&[[f64; 4]; 4]>,
    ) -> Result<Vec<Extent>, BoundsError> {
        let prim = self.path();
        let invalid = |source| BoundsError::InvalidPointInstancer { prim, source };
        // `_ComputeExtentAtTimePreamble`: the indices at the base time itself,
        // the mask, and a nonempty prototype list every index falls in.
        let indices = read(
            self,
            "protoIndices",
            base_time,
            crate::value::read_int_array_shared,
        )
        .map_err(invalid)?
        .ok_or(invalid(PointInstancerError::MissingAttribute(
            "protoIndices",
        )))?;
        let mask = self.try_compute_mask(base_time).map_err(invalid)?;
        if !mask.is_empty() && mask.len() != indices.len() {
            return Err(invalid(PointInstancerError::LengthMismatch("mask")));
        }
        let prototypes = self
            .prototypes()
            .into_iter()
            .map(|target| match target {
                TargetPath::Prim(path) => Ok(path),
                // `UsdGeomBBoxCache::ComputeUntransformedBound` refuses the
                // invalid prim a property target names.
                TargetPath::Property(_) => Err(invalid(PointInstancerError::InvalidPrototypes)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if prototypes.is_empty() {
            return Err(invalid(PointInstancerError::InvalidPrototypes));
        }
        for (instance, &index) in indices.iter().enumerate() {
            if usize::try_from(index).map_or(true, |index| index >= prototypes.len()) {
                return Err(invalid(PointInstancerError::InvalidPrototypeIndex {
                    instance,
                    index,
                }));
            }
        }
        let batch = self
            .prepare_instance_transforms_at_times(
                times,
                base_time,
                InstanceTransformOptions {
                    include_prototype_transform: true,
                    apply_mask: false,
                },
            )
            .map_err(invalid)?;
        let mut cache = BoundsCache::new(
            times.first().copied().unwrap_or(base_time),
            BoundsOptions {
                included_purposes: alloc::vec![
                    ImageablePurpose::Default,
                    ImageablePurpose::Proxy,
                    ImageablePurpose::Render,
                ],
                ..BoundsOptions::default()
            },
        );
        let scene = self.scene();
        let mut bounds = Vec::with_capacity(prototypes.len());
        let mut extents = Vec::with_capacity(times.len());
        for (&time, transforms) in times.iter().zip(&batch) {
            cache.set_time(time);
            bounds.clear();
            for &prototype in &prototypes {
                bounds.push(cache.untransformed_bound(&scene, prototype)?);
            }
            let mut range = Range3d::default();
            for instance in transforms.iter() {
                if !mask.is_empty() && !mask[instance.index] {
                    continue;
                }
                // `GfBBox3d::Transform` twice: instance, then `transform`.
                let mut bound = bounds[instance.prototype_index].transformed(&instance.matrix);
                if let Some(transform) = transform {
                    bound = bound.transformed(transform);
                }
                range.union_with(bound.aligned_range());
            }
            extents.push(range.to_extent());
        }
        Ok(extents)
    }
}

/// The time code of `time`, `None` for the default time, when `time` and
/// `base` are both default or both finite time codes.
fn sample_code(time: Time, base: Time) -> Result<Option<f64>, PointInstancerError> {
    match (time, base) {
        (Time::Default, Time::Default) => Ok(None),
        (Time::At { code, .. }, Time::At { code: base, .. })
            if code.is_finite() && base.is_finite() =>
        {
            Ok(Some(code))
        }
        _ => Err(PointInstancerError::InvalidTime),
    }
}

/// Base-time inputs one batch of sample times shares.
struct BaseInputs {
    indices: Arc<Vec<i32>>,
    positions: Arc<Vec<[f32; 3]>>,
    rotations: Arc<Vec<[f32; 4]>>,
    scales: Arc<Vec<[f32; 3]>>,
    velocities: Arc<Vec<[f32; 3]>>,
    accelerations: Arc<Vec<[f32; 3]>>,
    angular: Arc<Vec<[f32; 3]>>,
    ids: Option<Arc<Vec<i64>>>,
    mask: Arc<Vec<bool>>,
    /// Prototype prims in relationship order; `None` for a target that is
    /// not a prim. Empty when prototype transforms are excluded.
    prototypes: Vec<Option<PathId>>,
    orientations_f: bool,
    include_prototype_transform: bool,
    /// Whether positions, scales and orientations interpolate per sample.
    interpolate: bool,
    velocity_sample: Option<f64>,
    angular_sample: Option<f64>,
    rate: f64,
    survivors: usize,
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
    mask: Arc<Vec<bool>>,
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
    /// The transform of the instance at original array position `index`;
    /// `None` when the captured mask omits it or `index` is past
    /// [`Self::source_len`]. Snapshots are `Send` and `Sync`, so a host can
    /// split `0..source_len()` across its own threads.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<InstanceTransform> {
        (index < self.indices.len() && (self.mask.is_empty() || self.mask[index]))
            .then(|| self.transform(index))
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
        // `GfMatrix4d::SetScale`, then `*=` the orientation's rotation: a
        // diagonal scale times a rotation scales the rotation's rows, which
        // is the full product's value without its additions of zero (signed
        // zeros aside).
        let mut matrix = match (self.scales.is_empty(), self.rotations.is_empty()) {
            (true, true) => gf::IDENTITY,
            (false, true) => gf::scale(self.scales[i].map(f64::from)),
            (true, false) => gf::quaternion(self.rotations[i].map(f64::from)),
            (false, false) => {
                let mut rotation = gf::quaternion(self.rotations[i].map(f64::from));
                for (row, scale) in rotation.iter_mut().zip(self.scales[i]) {
                    for value in &mut row[..3] {
                        *value *= f64::from(scale);
                    }
                }
                rotation
            }
        };
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
        // An identity prototype transform leaves the product's value as it
        // is (signed zeros aside); most prototypes have one.
        if self.include_prototype_transform && self.prototypes[prototype_index] != gf::IDENTITY {
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
