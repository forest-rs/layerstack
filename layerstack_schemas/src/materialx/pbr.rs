// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{OpenPbrSurface, OpenPbrSurfaceEdit};
use crate::shading::nodes::{PreviewSurface, PreviewSurfaceEdit};
use crate::shading::{ConnectionModification::Replace, PortError};
use crate::usd_mtlx::MaterialXConfigApi;
use crate::usd_shade::{Material, MaterialEdit};
use crate::{ApplySchemaError, CannotEdit, SchemaEdit, value};
use core::fmt;
use layerstack::{PathId, PropertyType, Value};

/// Shared material values for a constant metallic-workflow surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PbrMaterialInputs {
    /// Linear RGB in the stage's working color space. Convert source color
    /// values to that space before authoring.
    pub base_color: [f32; 3],
    /// Perceptual specular roughness, from smooth (0) to rough (1).
    pub roughness: f32,
    /// Metalness, from dielectric (0) to metallic (1).
    pub metallic: f32,
    /// Surface presence, from absent (0) to opaque (1).
    pub opacity: f32,
}

/// Handles to extend or bind the material authored by [`define_pbr_material`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PbrMaterialEdit {
    /// Material prim and owner of the shared inputs.
    pub material: MaterialEdit,
    /// Shader for the `MaterialX` render context.
    pub open_pbr: OpenPbrSurfaceEdit,
    /// Shader for the universal render context.
    pub preview: PreviewSurfaceEdit,
}

/// A failure to queue a dual-context material.
#[derive(Debug)]
#[non_exhaustive]
pub enum PbrMaterialError {
    /// The prim path could not be edited.
    Prim(CannotEdit),
    /// `MaterialX` version metadata could not be applied.
    Configuration(ApplySchemaError),
    /// A material or shader port could not be authored.
    Port(PortError),
    /// A base-color component is infinite or NaN.
    NonFiniteBaseColor,
    /// A normalized control is non-finite or outside the range 0 to 1.
    InvalidFactor {
        /// Material input name without its namespace.
        input: &'static str,
        /// Rejected value.
        value: f32,
    },
}

impl fmt::Display for PbrMaterialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prim(error) => error.fmt(f),
            Self::Configuration(error) => error.fmt(f),
            Self::Port(error) => error.fmt(f),
            Self::NonFiniteBaseColor => f.write_str("PBR base color must have finite components"),
            Self::InvalidFactor { input, value } => {
                write!(f, "PBR {input} must be between 0 and 1, got {value}")
            }
        }
    }
}

impl core::error::Error for PbrMaterialError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Prim(error) => Some(error),
            Self::Configuration(error) => Some(error),
            Self::Port(error) => Some(error),
            Self::NonFiniteBaseColor | Self::InvalidFactor { .. } => None,
        }
    }
}

impl From<CannotEdit> for PbrMaterialError {
    fn from(error: CannotEdit) -> Self {
        Self::Prim(error)
    }
}

impl From<PortError> for PbrMaterialError {
    fn from(error: PortError) -> Self {
        Self::Port(error)
    }
}

/// Queues an `OpenPBR` material with a universal `UsdPreviewSurface` fallback.
///
/// The material's `inputs:baseColor`, `inputs:roughness`, `inputs:metallic`,
/// and `inputs:opacity` drive both shaders. Validated values are authored as given.
/// The fallback uses the metallic workflow and `opacityMode = "presence"`.
/// Appearance depends on the shading model and renderer support for its ports.
/// The `OpenPBR` node interface is from `MaterialX` 1.39.5.
///
/// Use a new material path for a new graph. The helper defines the material
/// and its `OpenPBR` and `PreviewSurface` shader children, replacing connections
/// to the two surface terminals and the eight shader inputs it wires. For
/// subsequent value edits, set the material inputs directly through
/// [`crate::PrimEdit::create_input`] and [`crate::shading::PortEdit::set`].
///
/// # Errors
///
/// Rejects non-finite color components and normalized controls outside
/// their specified ranges, including non-finite values. Invalid edit
/// locations, unavailable `MaterialX` configuration schema and incompatible
/// ports also fail. On error the existing edit stays intact.
pub fn define_pbr_material(
    edit: &mut SchemaEdit<'_>,
    path: PathId,
    inputs: PbrMaterialInputs,
) -> Result<PbrMaterialEdit, PbrMaterialError> {
    if !inputs.base_color.into_iter().all(f32::is_finite) {
        return Err(PbrMaterialError::NonFiniteBaseColor);
    }
    for (input, value) in [
        ("roughness", inputs.roughness),
        ("metallic", inputs.metallic),
        ("opacity", inputs.opacity),
    ] {
        if !(0.0..=1.0).contains(&value) {
            return Err(PbrMaterialError::InvalidFactor { input, value });
        }
    }

    edit.group(|edit| {
        let material = Material::define(edit, path)?;
        MaterialXConfigApi::apply(edit, path)
            .map_err(PbrMaterialError::Configuration)?
            .set_config_mtlx_version(edit, "1.39.5");

        let open_pbr_name = edit.tokens().intern("OpenPBR");
        let preview_name = edit.tokens().intern("PreviewSurface");
        let open_pbr_path = edit.child_path(path, open_pbr_name);
        let preview_path = edit.child_path(path, preview_name);
        let open_pbr = OpenPbrSurface::define(edit, open_pbr_path)?;
        let preview = PreviewSurface::define(edit, preview_path)?;
        preview.set_use_specular_workflow(edit, 0)?;
        preview.set_opacity_mode(edit, "presence")?;

        // AOUSD Core §7.6.4.2.3 and §12.4. UsdShade propagates values
        // from a material input to each connected shader input.
        let color = value::write_float3(inputs.base_color, edit.tokens());
        let interface = [
            (
                "baseColor",
                "color3f",
                color,
                open_pbr.create_base_color_input(edit)?,
                preview.create_diffuse_color_input(edit)?,
            ),
            (
                "roughness",
                "float",
                Value::from(inputs.roughness),
                open_pbr.create_specular_roughness_input(edit)?,
                preview.create_roughness_input(edit)?,
            ),
            (
                "metallic",
                "float",
                Value::from(inputs.metallic),
                open_pbr.create_base_metalness_input(edit)?,
                preview.create_metallic_input(edit)?,
            ),
            (
                "opacity",
                "float",
                Value::from(inputs.opacity),
                open_pbr.create_geometry_opacity_input(edit)?,
                preview.create_opacity_input(edit)?,
            ),
        ];
        for (name, ty, value, open_pbr_input, preview_input) in interface {
            let ty = PropertyType::parse(ty).expect("registered USD type");
            let source = material.create_input(edit, name, ty)?;
            source.set(edit, value)?;
            open_pbr_input.connect_to_source(edit, &source, Replace)?;
            preview_input.connect_to_source(edit, &source, Replace)?;
        }

        let output = open_pbr.create_surface_output(edit)?;
        material
            .create_surface_output(edit, "mtlx")?
            .connect_to_source(edit, output, Replace)?;
        let output = preview.create_surface_output(edit)?;
        material
            .create_surface_output(edit, "")?
            .connect_to_source(edit, output, Replace)?;
        Ok(PbrMaterialEdit {
            material,
            open_pbr,
            preview,
        })
    })
}

#[cfg(test)]
mod tests;
