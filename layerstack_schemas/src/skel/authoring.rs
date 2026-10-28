// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authoring joint influences and joint animation from matrices, as a
//! generator builds a rig: OpenUSD's `UsdSkelBindingAPI` influence
//! creators, `UsdSkelAnimation::SetTransforms` and the `utils.cpp`
//! decomposition it uses.
use super::{InfluenceInterpolation, JointTopology, JointTransformComponents};
use crate::{
    SchemaEdit, gf,
    gf::decomposition,
    primvar::{PrimvarEdit, PrimvarError},
    usd_skel::{SkelAnimationEdit, SkelBindingApiEdit},
};
use alloc::vec::Vec;
use layerstack::{PropertyType, half};

/// A joint transform that cannot be decomposed into translation, rotation
/// and scale, so nothing is authored.
///
/// OpenUSD: `UsdSkelDecomposeTransforms` ("Failed decomposing transform
/// %zu. The source transform may be singular.").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecomposeError {
    /// The offending transform's index.
    pub joint: usize,
}

impl core::fmt::Display for DecomposeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "joint transform {} is singular or has no rotation",
            self.joint
        )
    }
}

impl core::error::Error for DecomposeError {}

/// Why [`SkelBindingApiEdit::set_rigid_joint_influence`] authored nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RigidInfluenceError {
    /// The joint index is negative (OpenUSD: "Invalid jointIndex").
    NegativeJointIndex(i32),
    /// An influence primvar cannot be created, such as when the prim has
    /// `primvars:skel:jointIndices` of another type.
    Primvar(PrimvarError),
}

impl core::fmt::Display for RigidInfluenceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NegativeJointIndex(joint) => write!(f, "joint index {joint} is negative"),
            Self::Primvar(error) => error.fmt(f),
        }
    }
}

impl core::error::Error for RigidInfluenceError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Primvar(error) => Some(error),
            Self::NegativeJointIndex(_) => None,
        }
    }
}

impl From<PrimvarError> for RigidInfluenceError {
    fn from(error: PrimvarError) -> Self {
        Self::Primvar(error)
    }
}

const JOINT_INDICES: &str = "primvars:skel:jointIndices";
const JOINT_WEIGHTS: &str = "primvars:skel:jointWeights";

impl SkelBindingApiEdit {
    /// Creates `primvars:skel:jointIndices` (`int[]`) with `interpolation`
    /// (`constant` for a rigid binding, else `vertex`) and, when given and
    /// positive, `element_size` influences per point; an existing `int[]`
    /// attribute is kept and its metadata authored. `None`, or a size of zero
    /// or less, authors no `elementSize`, as OpenUSD's default of `-1` does.
    /// Values are authored with the returned handle.
    ///
    /// OpenUSD: `UsdSkelBindingAPI::CreateJointIndicesPrimvar(constant,
    /// elementSize)`, through `UsdGeomPrimvarsAPI::CreatePrimvar`.
    ///
    /// # Errors
    /// [`PrimvarError::TypeMismatch`] when the prim has the attribute with
    /// another type, and [`PrimvarError::MissingPrim`] without a prim;
    /// nothing is authored. OpenUSD instead returns the existing attribute,
    /// authors the metadata on it, and fails when a value is set.
    #[doc(alias = "UsdSkelBindingAPI::CreateJointIndicesPrimvar")]
    #[doc(alias = "CreateJointIndicesPrimvar")]
    pub fn create_joint_indices_primvar(
        &self,
        edit: &mut SchemaEdit<'_>,
        interpolation: InfluenceInterpolation,
        element_size: Option<i32>,
    ) -> Result<PrimvarEdit, PrimvarError> {
        self.create_influence_primvar(edit, JOINT_INDICES, "int[]", interpolation, element_size)
    }

    /// Creates `primvars:skel:jointWeights` (`float[]`), as
    /// [`Self::create_joint_indices_primvar`] creates the indices.
    ///
    /// OpenUSD: `UsdSkelBindingAPI::CreateJointWeightsPrimvar(constant,
    /// elementSize)`.
    ///
    /// # Errors
    /// As [`Self::create_joint_indices_primvar`].
    #[doc(alias = "UsdSkelBindingAPI::CreateJointWeightsPrimvar")]
    #[doc(alias = "CreateJointWeightsPrimvar")]
    pub fn create_joint_weights_primvar(
        &self,
        edit: &mut SchemaEdit<'_>,
        interpolation: InfluenceInterpolation,
        element_size: Option<i32>,
    ) -> Result<PrimvarEdit, PrimvarError> {
        self.create_influence_primvar(edit, JOINT_WEIGHTS, "float[]", interpolation, element_size)
    }

    fn create_influence_primvar(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        ty: &str,
        interpolation: InfluenceInterpolation,
        element_size: Option<i32>,
    ) -> Result<PrimvarEdit, PrimvarError> {
        edit.group(|edit| {
            let ty = PropertyType::parse(ty).expect("a registered value type");
            let primvar = self.create_primvar(edit, name, ty)?;
            primvar.set_interpolation(
                edit,
                match interpolation {
                    InfluenceInterpolation::Constant => "constant",
                    InfluenceInterpolation::Vertex => "vertex",
                },
            )?;
            if let Some(size) = element_size.filter(|size| *size > 0) {
                primvar.set_element_size(edit, size)?;
            }
            Ok(primvar)
        })
    }

    /// Binds the whole prim rigidly to one joint: constant
    /// `primvars:skel:jointIndices = [joint]` and `jointWeights = [weight]`,
    /// each with an `elementSize` of 1, in one group.
    ///
    /// OpenUSD: `UsdSkelBindingAPI::SetRigidJointInfluence(jointIndex,
    /// weight)`.
    ///
    /// # Errors
    /// [`RigidInfluenceError::NegativeJointIndex`], checked first, and the
    /// primvar errors of [`Self::create_joint_indices_primvar`]; nothing is
    /// authored. OpenUSD creates both primvars before it refuses a negative
    /// index.
    #[doc(alias = "UsdSkelBindingAPI::SetRigidJointInfluence")]
    #[doc(alias = "SetRigidJointInfluence")]
    pub fn set_rigid_joint_influence(
        &self,
        edit: &mut SchemaEdit<'_>,
        joint: i32,
        weight: f32,
    ) -> Result<&Self, RigidInfluenceError> {
        if joint < 0 {
            return Err(RigidInfluenceError::NegativeJointIndex(joint));
        }
        edit.group(|edit| {
            let constant = InfluenceInterpolation::Constant;
            let indices = self.create_joint_indices_primvar(edit, constant, Some(1))?;
            let weights = self.create_joint_weights_primvar(edit, constant, Some(1))?;
            let value = crate::value::write_int_array(&[joint], edit.tokens());
            indices.set(edit, value)?;
            let value = crate::value::write_float_array(&[weight], edit.tokens());
            weights.set(edit, value)?;
            Ok::<_, RigidInfluenceError>(())
        })?;
        Ok(self)
    }
}

impl SkelAnimationEdit {
    /// Authors `translations`, `rotations` and `scales` as the default
    /// decomposition of joint-local `transforms`, in the animation's joint
    /// order ([`decompose_joint_transforms`]).
    ///
    /// OpenUSD: `UsdSkelAnimation::SetTransforms` at the default time.
    ///
    /// # Errors
    /// [`DecomposeError`] for the first transform that cannot be
    /// decomposed; nothing is authored, as OpenUSD sets nothing then.
    #[doc(alias = "UsdSkelAnimation::SetTransforms")]
    #[doc(alias = "SetTransforms")]
    pub fn set_transforms(
        &self,
        edit: &mut SchemaEdit<'_>,
        transforms: &[[[f64; 4]; 4]],
    ) -> Result<&Self, DecomposeError> {
        let components = decompose_joint_transforms(transforms)?;
        self.set_translations_owned(edit, components.translations)
            .set_rotations_owned(edit, components.rotations)
            .set_scales(edit, &components.scales);
        Ok(self)
    }

    /// [`Self::set_transforms`] as time samples at the time code `time`.
    ///
    /// # Errors
    /// As [`Self::set_transforms`].
    #[doc(alias = "UsdSkelAnimation::SetTransforms")]
    #[doc(alias = "SetTransforms")]
    pub fn set_transforms_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        transforms: &[[[f64; 4]; 4]],
    ) -> Result<&Self, DecomposeError> {
        let components = decompose_joint_transforms(transforms)?;
        self.set_translations_owned_at(edit, time, components.translations)
            .set_rotations_owned_at(edit, time, components.rotations)
            .set_scales_at(edit, time, &components.scales);
        Ok(self)
    }
}

/// Decomposes joint-local transforms into the float translations, float
/// quaternion rotations (`[i, j, k, r]`) and half scales a `SkelAnimation`
/// stores, rounded as they are stored. Shear is dropped: the rotation is the
/// orthonormalized factor of `GfMatrix4d::Factor` and the scale its
/// principal stretches.
///
/// OpenUSD: `UsdSkelDecomposeTransforms`.
///
/// # Errors
/// [`DecomposeError`] for the first transform whose linear part is
/// singular (`|det| < 1e-10`) or whose rotation does not orthonormalize.
#[doc(alias = "UsdSkelDecomposeTransforms")]
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD stores float translations and rotations"
)]
pub fn decompose_joint_transforms(
    transforms: &[[[f64; 4]; 4]],
) -> Result<JointTransformComponents, DecomposeError> {
    let mut components = JointTransformComponents {
        translations: Vec::with_capacity(transforms.len()),
        rotations: Vec::with_capacity(transforms.len()),
        scales: Vec::with_capacity(transforms.len()),
    };
    for (joint, matrix) in transforms.iter().enumerate() {
        // OpenUSD `_DecomposeTransform`: Factor, then Orthonormalize the
        // rotation and extract its quaternion.
        let linear: decomposition::Matrix3 =
            core::array::from_fn(|i| core::array::from_fn(|j| matrix[i][j]));
        let (_, scale, rotation, singular) = decomposition::factor(&linear, 1e-10);
        let (rotation, converged) = decomposition::orthonormalize_checked(rotation);
        if singular || !converged {
            return Err(DecomposeError { joint });
        }
        let [r, i, j, k] = decomposition::quaternion(&rotation);
        components
            .translations
            .push(core::array::from_fn(|axis| matrix[3][axis] as f32));
        components
            .rotations
            .push([i as f32, j as f32, k as f32, r as f32]);
        components
            .scales
            .push(scale.map(|s| half::to_f32(half::from_f64(s))));
    }
    Ok(components)
}

impl JointTopology {
    /// Joint-local transforms from skeleton-space ones, the inverse of
    /// [`Self::concatenate`]: each joint's transform times its parent's
    /// inverse, and a root's times `root_inverse` when given (to make
    /// world-space transforms skeleton-relative). `None` when the count is
    /// not the topology's. A singular parent inverts as OpenUSD's
    /// `GfMatrix4d::GetInverse` does, to a scale by `f32::MAX`.
    ///
    /// OpenUSD: `UsdSkelComputeJointLocalTransforms`.
    #[doc(alias = "UsdSkelComputeJointLocalTransforms")]
    #[must_use]
    pub fn compute_local_transforms(
        &self,
        skeleton_space: &[[[f64; 4]; 4]],
        root_inverse: Option<&[[f64; 4]; 4]>,
    ) -> Option<Vec<[[f64; 4]; 4]>> {
        if skeleton_space.len() != self.parents().len() {
            return None;
        }
        let inverses: Vec<_> = skeleton_space.iter().map(|m| gf::inverse(m).0).collect();
        Some(
            skeleton_space
                .iter()
                .zip(self.parents())
                .map(|(matrix, parent)| match (parent, root_inverse) {
                    (Some(parent), _) => gf::mul(matrix, &inverses[*parent]),
                    (None, Some(root)) => gf::mul(matrix, root),
                    (None, None) => *matrix,
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decomposition_recovers_scale_rotation_translation() {
        // Scale (2, 1, 0.5), then 90° about Z, then translate (1, 2, 3).
        let m = [
            [0.0, 2.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.5, 0.0],
            [1.0, 2.0, 3.0, 1.0],
        ];
        let c = decompose_joint_transforms(&[m]).unwrap();
        assert_eq!(c.translations, [[1.0, 2.0, 3.0]]);
        assert_eq!(c.scales, [[2.0, 1.0, 0.5]]);
        let [i, j, k, r] = c.rotations[0];
        let half = core::f32::consts::FRAC_1_SQRT_2;
        assert!(i.abs() < 1e-7 && j.abs() < 1e-7 && (k - half).abs() < 1e-7);
        assert!((r - half).abs() < 1e-7);
        let zero = [[0.0; 4]; 4];
        assert_eq!(
            decompose_joint_transforms(&[m, zero]),
            Err(DecomposeError { joint: 1 })
        );
    }

    #[test]
    fn local_transforms_undo_concatenation() {
        let topology = JointTopology::new(&["A", "A/B", "A/B/C"]).unwrap();
        let local = [
            gf::translate([1.0, 0.0, 0.0]),
            gf::mul(&gf::scale([2.0, 2.0, 2.0]), &gf::translate([0.0, 1.0, 0.0])),
            gf::translate([0.0, 0.0, 3.0]),
        ];
        let skel = topology.concatenate(&local).unwrap();
        let back = topology.compute_local_transforms(&skel, None).unwrap();
        for (a, b) in back.iter().zip(&local) {
            for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
                assert!((x - y).abs() < 1e-12);
            }
        }
        let offset = gf::translate([0.0, -5.0, 0.0]);
        let rooted = topology
            .compute_local_transforms(&skel, Some(&offset))
            .unwrap();
        assert_eq!(rooted[0], gf::mul(&skel[0], &offset));
        assert!(
            topology
                .compute_local_transforms(&skel[..2], None)
                .is_none()
        );
    }
}
