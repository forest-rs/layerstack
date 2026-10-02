// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Backend-independent resolved bindings and retained deformation input views.
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

/// Conservative upload revisions within one [`super::SkelCache`] instance.
/// Compare revisions only from the same cache. They survive `clear`, are never
/// reused, and advance when their component is rebuilt/resampled, even if values
/// happen to compare equal. They are neither content hashes nor scene identities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeformationRevisions {
    /// Shared rig topology, bind/rest transforms and animation mappings.
    pub skeleton_definition: u64,
    /// Geometry binding, joint mapping and captured shape definitions.
    pub binding_definition: u64,
    /// Flattened influences, geometry bind and method; independent of pose.
    pub inputs: u64,
    /// Shared inverse-bind/animated palette, also covering point DQS components.
    pub pose: u64,
    /// Mapped shape weights and inbetween contributions; zero if no shape binding.
    pub blend_weights: u64,
}

/// Borrowed retained deformation inputs at the cache's explicit time.
/// No points/normals are read or deformed. Mesh parts share rig-order palettes;
/// custom binding orders also expose mapped matrices and their joint remapping.
/// Copy or pack required data before mutating the cache. Consumers own GPU
/// formats, uploads, execution and any previous-frame or shutter-sample history.
/// AOUSD Core §6.3, §12.3; OpenUSD `UsdSkel` palette sharing and blend-before-skin.
#[derive(Clone, Copy, Debug)]
pub struct DeformationInputs<'a> {
    pub(super) time: Time,
    pub(super) definition: &'a super::skinning::SkinningDefinition,
    pub(super) binding: &'a SkinningBindingInputs,
    pub(super) shared_transforms: &'a [gf::Matrix4],
    pub(super) transforms: &'a [gf::Matrix4],
    pub(super) dual_quaternions: Option<&'a [super::DualQuaternionJoint]>,
    pub(super) shape_weights: &'a [f32],
    pub(super) contributions: &'a [super::BlendShapeContribution],
    pub(super) revisions: DeformationRevisions,
}
impl DeformationInputs<'_> {
    /// Geometry whose binding supplies these inputs.
    #[must_use]
    pub fn geometry_path(&self) -> layerstack::PathId {
        self.definition.geometry
    }
    /// Skeleton identity in this scene's composed namespace.
    #[must_use]
    pub fn skeleton_path(&self) -> layerstack::PathId {
        self.definition.skeleton
    }
    /// Time and interpolation policy used for sampled values.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }
    /// Resolved influences, geometry-bind transform and method.
    #[must_use]
    pub fn binding(&self) -> &SkinningBindingInputs {
        self.binding
    }
    /// Inverse-bind/animated matrices in the binding order used by influences.
    #[must_use]
    pub fn skinning_transforms(&self) -> &[gf::Matrix4] {
        self.transforms
    }
    /// The same palette in Skeleton order, shared across geometry bindings.
    /// Pair it with `joint_mapping` when influence indices use a custom order.
    #[must_use]
    pub fn shared_skinning_transforms(&self) -> &[gf::Matrix4] {
        self.shared_transforms
    }
    /// Binding-to-Skeleton order; absent means identity order, unmapped means an
    /// identity transform. Matches [`SkinningQuery::joint_mapping`].
    #[must_use]
    pub fn joint_mapping(&self) -> Option<&[Option<usize>]> {
        self.definition.joint_mapping.as_deref()
    }
    /// Prepared point DQS components in Skeleton order, or `None` for LBS.
    /// Apply the binding's joint mapping, including identity for unmapped joints.
    /// Determine residual-scale use from mapped joints, excluding unused rig
    /// joints; `DualQuaternionJoint::has_scale` supplies each joint's flag.
    /// Normal skinning requires its own inverse-transpose decomposition.
    #[must_use]
    pub fn shared_dual_quaternions(&self) -> Option<&[super::DualQuaternionJoint]> {
        self.dual_quaternions
    }
    /// Captured shape arrays, local to this geometry binding.
    #[must_use]
    pub fn blend_shapes(&self) -> Option<&super::BlendShapeQuery> {
        self.definition.blend_shapes.as_ref()
    }
    /// Mapped animation weights in the local shape-name order; empty if unbound.
    #[must_use]
    pub fn blend_shape_weights(&self) -> &[f32] {
        self.shape_weights
    }
    /// Non-null inbetween sample contributions, indexed into `blend_shapes`.
    #[must_use]
    pub fn blend_shape_contributions(&self) -> &[super::BlendShapeContribution] {
        self.contributions
    }
    /// Independent stamps for deciding which retained buffers need an upload.
    #[must_use]
    pub fn revisions(&self) -> DeformationRevisions {
        self.revisions
    }
    /// Validates influences and all shape offset/index cardinalities against an
    /// adapter-owned vertex buffer. This does not inspect or deform that buffer.
    /// Matrix/offset precision conversion and normal topology remain caller policy.
    pub fn validate_point_count(&self, point_count: usize) -> Result<(), SkelError> {
        self.binding.validate(point_count, self.transforms.len())?;
        if let Some(shapes) = self.blend_shapes() {
            shapes.validate_point_count(point_count)?;
        }
        Ok(())
    }
}

/// Borrowed binding-order normal palettes, independently of CPU normal buffers.
/// Matrices use USD row vectors; DQS components come from inverse-transpose
/// matrices, not from the point decomposition. Revisions belong to one cache.
#[derive(Clone, Copy, Debug)]
pub struct NormalSkinningInputs<'a> {
    pub(super) binding: &'a SkinningBindingInputs,
    pub(super) bind: [[f64; 3]; 3],
    pub(super) transforms: &'a [[[f64; 3]; 3]],
    pub(super) dual_quaternions: Option<&'a [super::DualQuaternionJoint]>,
    pub(super) pose_revision: u64,
    pub(super) input_revision: u64,
}
impl NormalSkinningInputs<'_> {
    /// Flattened influences and method shared with point deformation.
    #[must_use]
    pub fn binding(&self) -> &SkinningBindingInputs {
        self.binding
    }
    /// Geometry-bind inverse transpose, without translation.
    #[must_use]
    pub fn geom_bind_normal_transform(&self) -> &[[f64; 3]; 3] {
        &self.bind
    }
    /// Joint inverse transposes in binding order; unmapped entries are identity.
    #[must_use]
    pub fn skinning_transforms(&self) -> &[[[f64; 3]; 3]] {
        self.transforms
    }
    /// Prepared normal DQS components in binding order, or `None` for LBS.
    #[must_use]
    pub fn dual_quaternions(&self) -> Option<&[super::DualQuaternionJoint]> {
        self.dual_quaternions
    }
    /// Shared pose stamp, also covering joint inverse transposes and normal DQS.
    #[must_use]
    pub fn pose_revision(&self) -> u64 {
        self.pose_revision
    }
    /// Binding input stamp, also covering the geometry-bind inverse transpose.
    #[must_use]
    pub fn input_revision(&self) -> u64 {
        self.input_revision
    }
}
