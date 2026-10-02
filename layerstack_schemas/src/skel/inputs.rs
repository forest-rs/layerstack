// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Backend-independent binding inputs, without pose or vertex evaluation.
use super::{
    InfluenceInterpolation, JointInfluences, SkelError, SkinningMethod, SkinningQuery, invalid,
    read,
};
use crate::{PrimView, Time, gf, primvar::Primvar};
use alloc::vec::Vec;

/// Owned resolved geometry-binding values at one explicit time.
/// Indices are flattened and address the binding's joint order, not necessarily
/// the Skeleton's order. Weights remain unnormalized. This snapshot does not
/// evaluate joint poses or read geometry points; rebuild it after relevant edits.
/// GPU precision, layout and uploads belong to the consuming application.
/// AOUSD Core §6.3, §12.3; OpenUSD `UsdSkelSkinningQuery` binding inputs.
#[derive(Clone, Debug)]
pub struct SkinningBindingInputs {
    pub(super) method: SkinningMethod,
    pub(super) bind: gf::Matrix4,
    pub(super) indices: Vec<i32>,
    pub(super) weights: Vec<f32>,
    pub(super) element_size: usize,
    pub(super) interpolation: InfluenceInterpolation,
}
impl SkinningBindingInputs {
    /// Authored deformation method, or the `classicLinear` fallback.
    #[must_use]
    pub fn skinning_method(&self) -> SkinningMethod {
        self.method
    }
    /// Row-vector transform from geometry-local coordinates to bind space.
    /// Joint inverse-bind/animated transforms follow this transform; the
    /// Skeleton's scene transform is applied after skinning.
    #[must_use]
    pub fn geom_bind_transform(&self) -> &gf::Matrix4 {
        &self.bind
    }
    /// Flattened influence blocks, in the binding's joint order.
    #[must_use]
    pub fn influences(&self) -> JointInfluences<'_> {
        JointInfluences {
            indices: &self.indices,
            weights: &self.weights,
            element_size: self.element_size,
            interpolation: self.interpolation,
        }
    }
    /// Validates influence counts, finite weights and indices against a caller's
    /// geometry and binding-order joint counts, without reading vertex buffers.
    pub fn validate(&self, point_count: usize, joint_count: usize) -> Result<(), SkelError> {
        self.influences().validate(point_count, joint_count)
    }
}
impl SkinningQuery<'_> {
    /// Resolves method, geometry bind and flattened influence arrays at `time`.
    /// This does not evaluate the Skeleton, read points/normals or apply shapes.
    /// Definitions are captured by the query; sampled influences and bind values
    /// resolve at the supplied time, including its interpolation policy.
    pub fn binding_inputs(&self, time: Time) -> Result<SkinningBindingInputs, SkelError> {
        let method = self.skinning_method()?;
        let geom_bind = self
            .definition
            .geom_bind
            .and_then(|p| {
                read(
                    &PrimView::new(self.scene, p),
                    "primvars:skel:geomBindTransform",
                    time,
                    crate::value::read_matrix4d,
                )
            })
            .unwrap_or(gf::IDENTITY);
        let flattened = |path, name, property| {
            Primvar::new(&self.scene, path, name)
                .ok_or_else(|| invalid(path, property))?
                .compute_flattened(time)
                .map_err(|source| SkelError::Primvar { prim: path, source })?
                .ok_or_else(|| invalid(path, property))
        };
        let indices = crate::value::read_int_array(
            &flattened(
                self.definition.indices,
                "skel:jointIndices",
                "primvars:skel:jointIndices",
            )?,
            self.scene.store().tokens(),
        )
        .ok_or_else(|| invalid(self.definition.indices, "primvars:skel:jointIndices"))?;
        let weights = crate::value::read_float_array(
            &flattened(
                self.definition.weights,
                "skel:jointWeights",
                "primvars:skel:jointWeights",
            )?,
            self.scene.store().tokens(),
        )
        .ok_or_else(|| invalid(self.definition.weights, "primvars:skel:jointWeights"))?;
        Ok(SkinningBindingInputs {
            method,
            bind: geom_bind,
            indices,
            weights,
            element_size: self.definition.element_size,
            interpolation: self.definition.interpolation,
        })
    }
    /// Binding-order joint index to Skeleton-order joint index. `None` means
    /// identical orders; an unmapped entry uses an identity transform, as USD's
    /// joint mapper does. Influence indices address this mapping when present.
    #[must_use]
    pub fn joint_mapping(&self) -> Option<&[Option<usize>]> {
        self.definition.joint_mapping.as_deref()
    }
}
