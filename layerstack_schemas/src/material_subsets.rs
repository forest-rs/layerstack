// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Material face subsets combine `UsdGeom` family validation and `UsdShade` binding.
//! Draw ranges, triangulation and renderer grouping belong to the consumer.
use crate::{
    BindingCache, BindingOptions, BoundMaterial, MaterialPurpose, PrimView, Scene, Time,
    subset::SubsetValidation,
    usd_geom::{GeomSubsetElementType, Imageable},
};
use alloc::{string::String, sync::Arc, vec::Vec};
use layerstack::{ArrayReadError, PathId, PropertyPath};

/// A face subset and its resolved binding, including inherited parent bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialBindingSubset {
    /// Composed subset prim path.
    pub path: PathId,
    /// Validated face indices at the query time, retaining native storage.
    pub indices: Arc<Vec<i32>>,
    /// Binding resolution for the subset, with winning relationship provenance.
    pub material: BoundMaterial,
}
/// A validated material-binding family at one stage time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialBindingSubsets {
    /// `nonOverlapping`, `partition`, or `unrestricted` when no subsets exist.
    pub family_type: String,
    /// Material for faces not assigned to a subset. Ancestor strength and
    /// collection bindings are resolved by the supplied binding cache.
    pub fallback: BoundMaterial,
    /// Direct active subset children, in composed child order.
    pub subsets: Vec<MaterialBindingSubset>,
}
/// A material family cannot be consumed as a validated face selection.
#[derive(Clone, Debug, PartialEq)]
pub enum MaterialSubsetError {
    /// The prim is not a mesh or tetrahedral mesh supporting face subsets.
    InvalidGeometry(PathId),
    /// A nonempty material-binding family is neither partition nor nonoverlapping.
    InvalidFamilyType(String),
    /// A required topology, family token or indices value is absent/incompatible.
    MissingAttribute(PropertyPath),
    /// A family member selects something other than faces.
    InvalidElementType(PathId),
    /// Retained numeric storage failed to decode.
    Decode {
        /// The failed attribute.
        property: PropertyPath,
        /// The original decoder error.
        error: ArrayReadError,
    },
    /// Bounds, overlap or partition coverage failed at the requested time.
    InvalidFamily(SubsetValidation),
}
impl core::fmt::Display for MaterialSubsetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid material subsets: {self:?}")
    }
}
impl core::error::Error for MaterialSubsetError {}

impl BindingCache {
    /// Discovers face `materialBind` subsets, validates the family at `time`,
    /// and resolves each binding plus the parent fallback using this cache's
    /// purpose and legacy-binding policy. Native arrays remain shared.
    ///
    /// An unauthored family type is `unrestricted`, matching C++'s getter;
    /// `CreateMaterialBindSubset` authors `nonOverlapping` during creation.
    /// Nonempty unrestricted families are rejected instead of silently adopting
    /// a renderer-specific default. No face ownership or draw array is allocated.
    /// Missing material targets remain visible in `BoundMaterial::binding`.
    /// Cache invalidation follows the ordinary `BindingCache` contract.
    /// OpenUSD: `UsdShadeMaterialBindingAPI::GetMaterialBindSubsets`,
    /// `UsdGeomSubset::ValidateFamily`, `ComputeBoundMaterial`.
    /// AOUSD Core §12.2–12.5 (composed subset attributes and relationships).
    pub fn material_binding_subsets(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
        time: Time,
    ) -> Result<MaterialBindingSubsets, MaterialSubsetError> {
        let geom = Imageable::new(scene, path)
            .filter(|_| scene.is_a(path, "Mesh") || scene.is_a(path, "TetMesh"))
            .ok_or(MaterialSubsetError::InvalidGeometry(path))?;
        let members = geom.geom_subsets(None, Some("materialBind"));
        let family_name = "subsetFamily:materialBind:familyType";
        let family_type = geom
            .try_read_value(family_name, Time::Default, crate::value::read_token)
            .map_err(|error| MaterialSubsetError::Decode {
                property: geom.property_path(family_name).expect("authored field"),
                error,
            })?;
        if family_type.is_none() && geom.has_authored_value(family_name) {
            return Err(MaterialSubsetError::MissingAttribute(
                geom.property_path(family_name).expect("authored field"),
            ));
        }
        let family_type = family_type
            .filter(|s| !s.is_empty())
            .unwrap_or("unrestricted");
        if !members.is_empty() && !matches!(family_type, "partition" | "nonOverlapping") {
            return Err(MaterialSubsetError::InvalidFamilyType(family_type.into()));
        }
        let mut subsets = Vec::with_capacity(members.len());
        for subset in members {
            if subset.element_type() != Some(GeomSubsetElementType::Face) {
                return Err(MaterialSubsetError::InvalidElementType(subset.path()));
            }
            let property = subset.property_path("indices").expect("schema indices");
            let indices = subset
                .try_indices(time)
                .map_err(|error| MaterialSubsetError::Decode { property, error })?
                .ok_or(MaterialSubsetError::MissingAttribute(property))?;
            subsets.push(MaterialBindingSubset {
                path: subset.path(),
                indices,
                material: self.compute_bound_material(scene, subset.path()),
            });
        }
        if !subsets.is_empty() || family_type == "partition" {
            let topology = if scene.is_a(path, "Mesh") {
                "faceVertexCounts"
            } else {
                "surfaceFaceVertexIndices"
            };
            let property = geom.property_path(topology).expect("schema topology");
            geom.try_read_value(topology, time, |v, t| {
                if topology == "faceVertexCounts" {
                    crate::value::read_int_array_shared(v, t).map(|a| a.len())
                } else {
                    crate::value::read_int3_array_shared(v, t).map(|a| a.len())
                }
            })
            .map_err(|error| MaterialSubsetError::Decode { property, error })?
            .ok_or(MaterialSubsetError::MissingAttribute(property))?;
            let validation =
                geom.validate_subset_family_at(&GeomSubsetElementType::Face, "materialBind", time);
            if !validation.is_valid() {
                return Err(MaterialSubsetError::InvalidFamily(validation));
            }
        }
        Ok(MaterialBindingSubsets {
            family_type: family_type.into(),
            subsets,
            fallback: self.compute_bound_material(scene, path),
        })
    }
}
impl PrimView<'_> {
    /// One-shot material subset query. Use `BindingCache::material_binding_subsets`
    /// to reuse binding inputs and collection membership across many meshes.
    pub fn material_binding_subsets(
        &self,
        time: Time,
        purpose: &MaterialPurpose,
        options: BindingOptions,
    ) -> Result<MaterialBindingSubsets, MaterialSubsetError> {
        BindingCache::new(purpose.clone(), options).material_binding_subsets(
            &self.scene(),
            self.path(),
            time,
        )
    }
}
