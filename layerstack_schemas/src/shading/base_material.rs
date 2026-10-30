// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A material's base material: the material it specializes, whose opinions
//! it overrides and whose updates it follows.
//!
//! OpenUSD `UsdShadeMaterial::GetBaseMaterialPath` and its siblings
//! (`material.cpp`): the base material is found in the prim's composition
//! graph, not in its authored `specializes` list, so a material inherits
//! its base through any arc that keeps it in the stage's namespace.
//! Spec: AOUSD Core §10.4.1 (specializes), §10.4.2.4 (implied arcs).

use layerstack::{ArcKind, ListOp, PathId};

use crate::usd_shade::{Material, MaterialEdit};
use crate::{PrimView, SchemaEdit};

impl Material<'_> {
    /// The path of the material this one specializes: among the
    /// specializes arcs directly beneath the prim's own site, in strength
    /// order, the first whose target is a `Material` on the stage. An arc
    /// authored in another layer stack (inside a referenced or payloaded
    /// asset) is skipped, since its target is not a stage path; one such
    /// arc whose target lies within the referenced prim is implied into the
    /// stage's layer stack with its path mapped, and that counts. An
    /// internal reference keeps the stage's namespace, so specializes
    /// reached through one count.
    ///
    /// OpenUSD: `UsdShadeMaterial::GetBaseMaterialPath`
    /// (`FindBaseMaterialPathInPrimIndex`). For a base material inside an
    /// instance, C++ answers the path of the prim in the instance's
    /// prototype (`/__Prototype_1/…`); here it is the instance proxy's
    /// path, as the stage names no prototype prims.
    #[doc(alias = "UsdShadeMaterial::GetBaseMaterialPath")]
    #[doc(alias = "GetBaseMaterialPath")]
    #[must_use]
    pub fn base_material_path(&self) -> Option<PathId> {
        let prim: &PrimView<'_> = self;
        let scene = prim.scene();
        let graph = prim.prim()?.composition_graph();
        let root = graph.node(graph.root()?)?;
        let stage_stack = root.layer_stack_identifier();
        graph
            .nodes()
            .filter(|(_, node)| {
                node.arc_kind() == ArcKind::Specializes
                    && node.parent() == graph.root()
                    // C++ skips a node whose map to the root does not map
                    // `/`: one reached through an external reference or
                    // payload, whose layer stack is not the stage's.
                    && node.layer_stack_identifier() == stage_stack
            })
            .map(|(_, node)| node.site().prim_path())
            .find(|&path| scene.is_a(path, Material::SCHEMA))
    }

    /// The material [`Self::base_material_path`] names.
    ///
    /// OpenUSD: `UsdShadeMaterial::GetBaseMaterial`.
    #[doc(alias = "UsdShadeMaterial::GetBaseMaterial")]
    #[doc(alias = "GetBaseMaterial")]
    #[must_use]
    pub fn base_material(&self) -> Option<Self> {
        let prim: &PrimView<'_> = self;
        Material::get(&prim.scene(), self.base_material_path()?)
    }

    /// Whether the material specializes a base material
    /// ([`Self::base_material_path`]).
    ///
    /// OpenUSD: `UsdShadeMaterial::HasBaseMaterial`.
    #[doc(alias = "UsdShadeMaterial::HasBaseMaterial")]
    #[doc(alias = "HasBaseMaterial")]
    #[must_use]
    pub fn has_base_material(&self) -> bool {
        self.base_material_path().is_some()
    }
}

impl MaterialEdit {
    /// Makes `base` the material's base material: authors it as the only,
    /// explicit entry of the prim's `specializes` in the edit target,
    /// replacing whatever the edit target authors there (only one base is
    /// allowed). `base` is not checked to be a material, as in C++;
    /// [`Material::base_material_path`] only reports one that is. `None`
    /// clears the edit target's `specializes`, as
    /// [`Self::clear_base_material`] does.
    ///
    /// OpenUSD: `UsdShadeMaterial::SetBaseMaterialPath` (and
    /// `SetBaseMaterial`, which passes the material's path, or none for an
    /// invalid one).
    #[doc(alias = "UsdShadeMaterial::SetBaseMaterialPath")]
    #[doc(alias = "SetBaseMaterialPath")]
    #[doc(alias = "UsdShadeMaterial::SetBaseMaterial")]
    #[doc(alias = "SetBaseMaterial")]
    pub fn set_base_material(&self, edit: &mut SchemaEdit<'_>, base: Option<PathId>) -> &Self {
        let specializes = match base {
            Some(base) => ListOp::explicit(alloc::vec![base]),
            None => ListOp::default(),
        };
        edit.set_specializes(self.path(), specializes);
        self
    }

    /// Clears the `specializes` the edit target authors on the material, so
    /// it has no base material unless a weaker layer gives it one.
    ///
    /// OpenUSD: `UsdShadeMaterial::ClearBaseMaterial`
    /// (`UsdSpecializes::ClearSpecializes`).
    #[doc(alias = "UsdShadeMaterial::ClearBaseMaterial")]
    #[doc(alias = "ClearBaseMaterial")]
    pub fn clear_base_material(&self, edit: &mut SchemaEdit<'_>) -> &Self {
        self.set_base_material(edit, None)
    }
}
