// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `MaterialX` node interfaces for USD shading.
//!
//! Generated bindings expose a node's interface through its `DEFINITION`
//! constant. A port retains its `MaterialX` type even when several types share a
//! USD representation. Defaults describe what the node implementation supplies
//! when an input is left unauthored.
//!
//! With `usd-mtlx`, this module ships selected `MaterialX` 1.39.5 bindings for
//! image and tiled-image sampling, texture coordinates and placement, normal
//! and bump mapping, channel operations, math and color adjustment, and
//! `OpenPBR` Surface, Standard Surface and `glTF` PBR. Their descriptors are
//! available in `STANDARD_NODES`. With `usd-shade`, custom libraries can use
//! `layerstack_schemagen --materialx` to generate their own bindings.
//!
//! Pass the selected definitions to
//! [`PortEdit::connect_to_materialx_source`](crate::shading::PortEdit::connect_to_materialx_source)
//! to check a wire between two shader nodes before authoring it.
//! Material terminals use the ordinary USD connection API.
//!
//! ```
//! # #[cfg(feature = "usd-mtlx")]
//! # fn paint(edit: &mut layerstack_schemas::SchemaEdit<'_>, material: layerstack::PathId,
//! #          albedo: layerstack::PathId, surface: layerstack::PathId) -> Result<(), Box<dyn std::error::Error>> {
//! use layerstack_schemas::materialx::{ImageColor3, OpenPbrSurface, STANDARD_NODES};
//! use layerstack_schemas::shading::ConnectionModification::Replace;
//! use layerstack_schemas::usd_shade::Material;
//!
//! let paint = Material::define(edit, material)?;
//! let image = ImageColor3::define(edit, albedo)?;
//! image.set_file(edit, "albedo.exr")?;
//! let surface = OpenPbrSurface::define(edit, surface)?;
//!
//! let rgb = image.create_out_output(edit)?;
//! surface.create_base_color_input(edit)?.connect_to_materialx_source(edit, rgb, STANDARD_NODES, Replace)?;
//! let shader = surface.create_surface_output(edit)?;
//! paint.create_surface_output(edit, "mtlx")?.connect_to_source(edit, shader, Replace)?;
//! # Ok(()) }
//! ```

use crate::shading::PortKind;

mod connections;
pub use connections::MaterialXConnectionError;

/// A `MaterialX` `<nodedef>` selected when generating bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaterialXNode {
    /// Node definition name authored on the USD Shader's `info:id`.
    pub id: &'static str,
    /// The node's inputs and outputs, including inherited ports.
    pub ports: &'static [MaterialXPort],
}

/// A `MaterialX` port and the USD shading attribute used to author it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaterialXPort {
    /// Local port name in the `MaterialX` interface.
    pub name: &'static str,
    /// Whether the port supplies or consumes a value.
    pub direction: PortKind,
    /// `MaterialX` type name, such as `color3` or `surfaceshader`.
    pub type_name: &'static str,
    /// Full USD attribute name, including `inputs:` or `outputs:`.
    pub usd_name: &'static str,
    /// USD value type name, such as `color3f` or `token`.
    pub usd_type_name: &'static str,
    /// Default declared by the node definition, if any.
    pub default: Option<MaterialXDefault>,
}

/// How a `MaterialX` definition describes a port's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialXDefault {
    /// A constant in the `MaterialX` type's lexical representation.
    Literal(&'static str),
    /// An input obtains its value from a named geometry property.
    GeomProp(&'static str),
    /// An output takes its default from a named input.
    Input {
        /// The input named by the output's `defaultinput`.
        name: &'static str,
        /// The output's optional literal `default`, in `MaterialX` syntax.
        fallback: Option<&'static str>,
    },
}

#[cfg(feature = "usd-mtlx")]
mod pbr;
#[cfg(feature = "usd-mtlx")]
mod standard;
#[cfg(feature = "usd-mtlx")]
pub use pbr::{PbrMaterialEdit, PbrMaterialError, PbrMaterialInputs, define_pbr_material};

#[cfg(feature = "usd-mtlx")]
pub use standard::{
    NODE_DEFINITIONS as STANDARD_NODES, NdAddColor3 as AddColor3, NdAddColor3Edit as AddColor3Edit,
    NdAddFloat as AddFloat, NdAddFloatEdit as AddFloatEdit, NdBumpVector3 as Bump,
    NdBumpVector3Edit as BumpEdit, NdClampFloat as ClampFloat, NdClampFloatEdit as ClampFloatEdit,
    NdColorcorrectColor3 as ColorCorrectColor3, NdColorcorrectColor3Edit as ColorCorrectColor3Edit,
    NdCombine3Color3 as Combine3Color3, NdCombine3Color3Edit as Combine3Color3Edit,
    NdExtractColor3 as ExtractColor3, NdExtractColor3Edit as ExtractColor3Edit,
    NdExtractColor4 as ExtractColor4, NdExtractColor4Edit as ExtractColor4Edit,
    NdExtractVector3 as ExtractVector3, NdExtractVector3Edit as ExtractVector3Edit,
    NdGltfPbrSurfaceshader as GltfPbr, NdGltfPbrSurfaceshaderEdit as GltfPbrEdit,
    NdImageColor3 as ImageColor3, NdImageColor3Edit as ImageColor3Edit,
    NdImageColor4 as ImageColor4, NdImageColor4Edit as ImageColor4Edit, NdImageFloat as ImageFloat,
    NdImageFloatEdit as ImageFloatEdit, NdImageVector3 as ImageVector3,
    NdImageVector3Edit as ImageVector3Edit, NdInvertFloat as InvertFloat,
    NdInvertFloatEdit as InvertFloatEdit, NdMixColor3 as MixColor3,
    NdMixColor3Edit as MixColor3Edit, NdMixFloat as MixFloat, NdMixFloatEdit as MixFloatEdit,
    NdMultiplyColor3 as MultiplyColor3, NdMultiplyColor3Edit as MultiplyColor3Edit,
    NdMultiplyColor3Fa as MultiplyColor3ByFloat,
    NdMultiplyColor3FaEdit as MultiplyColor3ByFloatEdit, NdMultiplyFloat as MultiplyFloat,
    NdMultiplyFloatEdit as MultiplyFloatEdit, NdNormalmapFloat as NormalMap,
    NdNormalmapFloatEdit as NormalMapEdit, NdOpenPbrSurfaceSurfaceshader as OpenPbrSurface,
    NdOpenPbrSurfaceSurfaceshaderEdit as OpenPbrSurfaceEdit, NdPlace2dVector2 as Place2dVector2,
    NdPlace2dVector2Edit as Place2dVector2Edit, NdRemapFloat as RemapFloat,
    NdRemapFloatEdit as RemapFloatEdit, NdSeparate3Vector3 as Separate3Vector3,
    NdSeparate3Vector3Edit as Separate3Vector3Edit,
    NdStandardSurfaceSurfaceshader as StandardSurface,
    NdStandardSurfaceSurfaceshaderEdit as StandardSurfaceEdit,
    NdTexcoordVector2 as TexcoordVector2, NdTexcoordVector2Edit as TexcoordVector2Edit,
    NdTiledimageColor3 as TiledImageColor3, NdTiledimageColor3Edit as TiledImageColor3Edit,
    NdTiledimageFloat as TiledImageFloat, NdTiledimageFloatEdit as TiledImageFloatEdit,
    NdTiledimageVector3 as TiledImageVector3, NdTiledimageVector3Edit as TiledImageVector3Edit,
};
