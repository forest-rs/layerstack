// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Material face subsets combine `UsdGeom` family validation and `UsdShade` binding.
//! Draw ranges, triangulation and renderer grouping belong to the consumer.
use crate::{
    BindingCache, BindingOptions, BoundMaterial, MaterialPurpose, PrimEdit, PrimView, Scene,
    SchemaEdit, Time,
    subset::{
        SubsetAuthoringError, SubsetError, SubsetFamilyType, SubsetProblem, SubsetProblemKind,
        SubsetValidation,
    },
    usd_geom::{GeomSubsetEdit, GeomSubsetElementType, Imageable, ImageableEdit},
    usd_shade::MaterialBindingApiEdit,
};
use alloc::{string::String, sync::Arc, vec::Vec};
use layerstack::{ArrayReadError, PathId, PropertyPath};

/// The family name of subsets that bind materials.
const MATERIAL_BIND: &str = "materialBind";
/// The attribute that declares the `materialBind` family's type.
const FAMILY_TYPE: &str = "subsetFamily:materialBind:familyType";

/// A face subset and its resolved binding, including inherited parent bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialBindingSubset {
    /// Composed subset prim path.
    pub path: PathId,
    /// Face indices at the query time, as authored and retaining native
    /// storage. They are not checked against the mesh: see
    /// [`MaterialBindingSubsets::validation`] before indexing faces with them.
    pub indices: Arc<Vec<i32>>,
    /// Binding resolution for the subset, with winning relationship provenance.
    pub material: BoundMaterial,
}
/// A mesh's material-binding face subsets at one stage time, as a renderer
/// reads them, with the family's validation beside them.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialBindingSubsets {
    /// The authored family type, or `unrestricted` when none is authored
    /// (C++ `GetMaterialBindSubsetsFamilyType`).
    pub family_type: String,
    /// Material for faces not assigned to a subset. Ancestor strength and
    /// collection bindings are resolved by the supplied binding cache.
    pub fallback: BoundMaterial,
    /// The family's active face subsets with readable indices, in composed
    /// child order. Members of another element type are left out, as
    /// `UsdImagingMeshAdapter` leaves them out.
    pub subsets: Vec<MaterialBindingSubset>,
    /// `UsdGeomSubset::ValidateFamily` for the face family at the query time:
    /// overlaps in a restricted family, out-of-range indices, an incomplete
    /// partition, members of another element type or unusable topology.
    /// Problems never remove subsets; a consumer decides what to do with an
    /// invalid family. Empty when the family has no members and is not a
    /// partition (C++ reports a family without indices invalid).
    pub validation: SubsetValidation,
}
/// The mesh's material subsets cannot be read at all.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MaterialSubsetError {
    /// The prim is not a mesh or tetrahedral mesh supporting face subsets.
    InvalidGeometry(PathId),
    /// Retained numeric storage failed to decode.
    Decode {
        /// The failed attribute.
        property: PropertyPath,
        /// The original decoder error.
        error: ArrayReadError,
    },
}
impl core::fmt::Display for MaterialSubsetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid material subsets: {self:?}")
    }
}
impl core::error::Error for MaterialSubsetError {}

impl BindingCache {
    /// Discovers face `materialBind` subsets, resolves each binding plus the
    /// parent fallback using this cache's purpose and legacy-binding policy,
    /// and validates the family at `time`. Native arrays remain shared.
    ///
    /// The read follows C++: `GetMaterialBindSubsets` returns the family's
    /// members whatever its type, and `UsdImagingMeshAdapter` binds every
    /// face member whose indices read. An unauthored (or non-token) family
    /// type reads as `unrestricted`, as `UsdGeomSubset::GetFamilyType` reads
    /// it. Validation is reported in [`MaterialBindingSubsets::validation`]
    /// and never fails the read, so one bad subset does not hide a mesh's
    /// other materials. No face ownership or draw array is allocated.
    /// Missing material targets remain visible in `BoundMaterial::binding`.
    /// Cache invalidation follows the ordinary `BindingCache` contract.
    /// Checked reads preserve a failed retained array's property and error
    /// in [`MaterialSubsetError::Decode`].
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
        let members = geom.geom_subsets(None, Some(MATERIAL_BIND));
        let family_name = FAMILY_TYPE;
        let family_type = geom
            .try_read_value(family_name, Time::Default, crate::value::read_token)
            .map_err(|error| MaterialSubsetError::Decode {
                property: geom.property_path(family_name).expect("authored field"),
                error,
            })?
            .filter(|s| !s.is_empty())
            .unwrap_or("unrestricted");
        let mut subsets = Vec::with_capacity(members.len());
        for subset in &members {
            if subset.element_type() != Some(GeomSubsetElementType::Face) {
                continue;
            }
            let property = subset.property_path("indices").expect("schema indices");
            let Some(indices) = subset
                .try_indices(time)
                .map_err(|error| MaterialSubsetError::Decode { property, error })?
            else {
                continue;
            };
            subsets.push(MaterialBindingSubset {
                path: subset.path(),
                indices,
                material: self.compute_bound_material(scene, subset.path()),
            });
        }
        let validation = if members.is_empty() && family_type != "partition" {
            SubsetValidation::default()
        } else {
            match geom.try_validate_subset_family_at(
                &GeomSubsetElementType::Face,
                MATERIAL_BIND,
                time,
            ) {
                Ok(validation) => validation,
                Err(SubsetError::Decode { property, error }) => {
                    return Err(MaterialSubsetError::Decode { property, error });
                }
                // Checked validation reports topology as problems; keep any
                // other refusal as one rather than failing the read.
                Err(_) => SubsetValidation {
                    problems: alloc::vec![SubsetProblem {
                        path,
                        time,
                        kind: SubsetProblemKind::InvalidTopology,
                    }],
                },
            }
        };
        Ok(MaterialBindingSubsets {
            family_type: family_type.into(),
            subsets,
            fallback: self.compute_bound_material(scene, path),
            validation,
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

impl MaterialBindingApiEdit {
    /// Collects the `materialBind` subset `name` of this prim's geometry, as
    /// [`GeomSubsetEdit::create`] does: an existing subset of that name is
    /// reused, with its indices re-authored. When the family's type, as this
    /// edit leaves it, is unauthored, empty or `unrestricted`, it becomes
    /// `nonOverlapping`, since faces bound to different materials must not
    /// overlap; `partition` and `nonOverlapping` stay.
    ///
    /// Indices are checked against the mesh's default-time topology, where
    /// C++ authors any indices. Rejection collects no edits.
    /// OpenUSD: `UsdShadeMaterialBindingAPI::CreateMaterialBindSubset`.
    /// AOUSD Core §12.3–12.5.
    #[doc(alias = "CreateMaterialBindSubset")]
    #[doc(alias = "UsdShadeMaterialBindingAPI::CreateMaterialBindSubset")]
    pub fn create_material_bind_subset(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        element: GeomSubsetElementType,
        indices: &[i32],
    ) -> Result<GeomSubsetEdit, SubsetAuthoringError> {
        edit.group(|edit| {
            let subset =
                GeomSubsetEdit::create(edit, self.path(), name, element, indices, MATERIAL_BIND)?;
            let current = edit.default_value(self.path(), FAMILY_TYPE);
            let current = current
                .as_ref()
                .and_then(|value| crate::value::read_token(value, edit.tokens()));
            if matches!(current, None | Some("" | "unrestricted")) {
                self.set_material_bind_subsets_family_type(edit, SubsetFamilyType::NonOverlapping)?;
            }
            Ok(subset)
        })
    }

    /// Authors the `materialBind` family's type. `unrestricted` is refused
    /// with [`SubsetAuthoringError::UnrestrictedMaterialBindFamily`], as C++
    /// refuses it; [`ImageableEdit::set_subset_family_type`] authors any
    /// type. Rejection collects no edits.
    /// OpenUSD: `UsdShadeMaterialBindingAPI::SetMaterialBindSubsetsFamilyType`.
    #[doc(alias = "SetMaterialBindSubsetsFamilyType")]
    #[doc(alias = "UsdShadeMaterialBindingAPI::SetMaterialBindSubsetsFamilyType")]
    pub fn set_material_bind_subsets_family_type(
        &self,
        edit: &mut SchemaEdit<'_>,
        family_type: SubsetFamilyType,
    ) -> Result<(), SubsetAuthoringError> {
        if family_type == SubsetFamilyType::Unrestricted {
            return Err(SubsetAuthoringError::UnrestrictedMaterialBindFamily);
        }
        ImageableEdit::from_prim(PrimEdit::from_path_unchecked(self.path())).set_subset_family_type(
            edit,
            MATERIAL_BIND,
            family_type,
        )
    }
}
