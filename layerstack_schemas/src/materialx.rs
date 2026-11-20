// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `MaterialX` node interfaces for USD shading.
//!
//! Generated bindings expose a node's interface through its `DEFINITION`
//! constant. A port retains its `MaterialX` type even when several types share a
//! USD representation. Defaults describe what the node implementation supplies
//! when an input is left unauthored.
//!
//! With the `usd-mtlx` feature, this module also provides bindings for the
//! `MaterialX` 1.39.5 color image and `OpenPBR` Surface nodes. Custom libraries can
//! generate the same interfaces with `layerstack_schemagen --materialx`.
//!
//! ```
//! # #[cfg(feature = "usd-mtlx")]
//! # fn paint(edit: &mut layerstack_schemas::SchemaEdit<'_>, material: layerstack::PathId,
//! #          albedo: layerstack::PathId, surface: layerstack::PathId) -> Result<(), Box<dyn std::error::Error>> {
//! use layerstack_schemas::materialx::{ImageColor3, OpenPbrSurface};
//! use layerstack_schemas::shading::ConnectionModification::Replace;
//! use layerstack_schemas::usd_shade::Material;
//!
//! let paint = Material::define(edit, material)?;
//! let image = ImageColor3::define(edit, albedo)?;
//! image.set_file(edit, "albedo.exr")?;
//! let surface = OpenPbrSurface::define(edit, surface)?;
//!
//! let rgb = image.create_out_output(edit)?;
//! surface.create_base_color_input(edit)?.connect_to_source(edit, rgb, Replace)?;
//! let shader = surface.create_surface_output(edit)?;
//! paint.create_surface_output(edit, "mtlx")?.connect_to_source(edit, shader, Replace)?;
//! # Ok(()) }
//! ```

use crate::shading::PortKind;

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
mod standard;

#[cfg(feature = "usd-mtlx")]
pub use standard::{
    NdImageColor3 as ImageColor3, NdImageColor3Edit as ImageColor3Edit,
    NdOpenPbrSurfaceSurfaceshader as OpenPbrSurface,
    NdOpenPbrSurfaceSurfaceshaderEdit as OpenPbrSurfaceEdit,
};

/// Definitions for the `MaterialX` nodes shipped with this crate.
#[cfg(feature = "usd-mtlx")]
pub const STANDARD_NODES: &[MaterialXNode] = &[ImageColor3::DEFINITION, OpenPbrSurface::DEFINITION];
