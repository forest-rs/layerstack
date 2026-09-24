// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Polygon mesh export to USDA and USDZ.
//!
//! This crate is the adapter between a mesh kernel's buffers and the
//! `UsdGeom` schemas. The kernel describes its data with plain slices —
//! positions, face topology, normals, UVs, extra primvars, transforms — and
//! the adapter applies the USD conventions explicitly:
//!
//! - positions become `points` (`point3f[]`), topology becomes
//!   `faceVertexCounts` / `faceVertexIndices`, and every mesh authors
//!   `subdivisionScheme = "none"` so polygonal data is not subdivided by
//!   consumers (the schema's fallback is `catmullClark`);
//! - normals keep their interpolation. Unindexed normals are written to
//!   `normals`; indexed normals are written as the `primvars:normals`
//!   primvar, since only primvars can be indexed;
//! - UVs become the `primvars:st` primvar (`texCoord2f[]`), with optional
//!   `primvars:st:indices`;
//! - each prim's local transform is one `xformOp:transform` in
//!   `xformOpOrder`; `extent` is computed from the points;
//! - stage metadata `upAxis` and `metersPerUnit` are always authored, and
//!   the root prim is the layer's `defaultPrim`.
//!
//! Face-varying indices are written exactly as given. They encode the
//! primvar's own topology (seams and hard edges), so equal values at
//! distinct indices are never merged.
//!
//! # Outputs and profile
//!
//! [`Scene::to_usda`] writes one *authored* layer through
//! [`layerstack_usda::writer`]; nothing is composed or flattened, and the
//! composition kernel is not involved. [`Scene::to_usdz`] is a separate
//! packaging step ([`layerstack_usdz::write_usdz`]): the layer becomes the
//! package's first file, followed by the caller's files (e.g. textures),
//! and every asset path the scene authors must name one of those files.
//! Paths are used as given; the packager does not discover or rewrite
//! dependencies.
//!
//! The package targets the **generic USDZ profile** (OpenUSD
//! `docs/spec_usdz.rst`): a USDA default layer plus media. It is not the
//! `ARKit` / AR Quick Look profile, which expects a single USDC layer
//! (`spec_usdz.rst:210`, `pxr/usd/usdUtils/usdzPackage.h:71`); passing
//! `usdchecker --arkit` does not by itself establish compatibility with
//! those viewers.
//!
//! # Materials
//!
//! A [`Material`] is written under `/<root>/Materials` as a `Material` prim
//! whose surface is a `UsdPreviewSurface` shader in the metallic workflow.
//! Each input is a constant or a texture: textures become `UsdUVTexture`
//! shaders reading `primvars:st` (or another UV set) through a
//! `UsdPrimvarReader_float2`, with `sourceColorSpace` fixed by the input
//! (`sRGB` for base and emissive color, `raw` for data), explicit wrap
//! modes, and the `scale`/`bias` remap the specification prescribes for
//! normal maps. Channels of one image (e.g. packed occlusion, roughness
//! and metallic) share one texture shader. See [`Material`] for the
//! inputs and their fallbacks.
//!
//! A mesh binds a material by name ([`Mesh::with_material`]): the mesh gets
//! the `MaterialBindingAPI` schema and a direct `material:binding`
//! relationship. Bindings are checked: the material must exist and the
//! mesh must author every UV set its textures read.
//!
//! Known limits, by design of this profile: bindings are direct, of
//! default strength and for all purposes (no collection-based bindings,
//! `bindMaterialAs`, or purpose-specific `material:binding:<purpose>`);
//! the only shading model is `UsdPreviewSurface` (no `MaterialX` or other
//! render contexts, no specular workflow, clearcoat, `ior` or
//! displacement); and texture coordinates are not transformed
//! (`UsdTransform2d`).
//!
//! # Example
//!
//! ```
//! use layerstack_mesh_export::{
//!     Channel, ColorInput, Faces, FloatInput, Material, Mesh, PackageFile, Primvar, Scene,
//!     StageSettings, Texture, Transform, UpAxis, Xform,
//! };
//!
//! let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
//! let normals = [[0.0, 0.0, 1.0]; 4];
//! let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
//! let quad = Mesh::new("Quad", &points, Faces::Polygons { counts: &[4], indices: &[0, 1, 2, 3] })
//!     .with_normals(Primvar::vertex(&normals))
//!     .with_uvs(Primvar::vertex(&uvs))
//!     .with_material("Painted");
//!
//! // A glTF-style material: base color texture, packed metal/roughness.
//! let base = Texture::new("textures/base.png");
//! let metal_rough = Texture::new("textures/metal_rough.png");
//! let painted = Material::new("Painted")
//!     .with_diffuse_color(ColorInput::texture(base))
//!     .with_roughness(FloatInput::texture(metal_rough, Channel::G))
//!     .with_metallic(FloatInput::texture(metal_rough, Channel::B));
//!
//! let root = Xform::new("Root")
//!     .with_transform(Transform::from_translation([0.0, 0.0, 2.0]))
//!     .with_mesh(quad);
//! let scene = Scene::new(StageSettings::new(UpAxis::Z, 1.0), root).with_material(painted);
//!
//! let usda = scene.to_usda()?;
//! assert!(usda.contains("uniform token subdivisionScheme = \"none\""));
//! assert!(usda.contains("rel material:binding = </Root/Materials/Painted>"));
//! # let png: &[u8] = b"\x89PNG\r\n\x1a\n";
//! let usdz = scene.to_usdz(&[
//!     PackageFile::new("textures/base.png", png),
//!     PackageFile::new("textures/metal_rough.png", png),
//! ])?;
//! assert_eq!(&usdz[..4], b"PK\x03\x04");
//! # Ok::<(), layerstack_mesh_export::ExportError>(())
//! ```
//!
//! # References
//!
//! Conventions follow the OpenUSD reference implementation (v26.08 headers;
//! `pxr/usd/usdGeom/...`), which documents the `UsdGeom` domain schemas
//! that AOUSD Core (§2.1) leaves out of scope:
//! [`mesh.h`](https://openusd.org/dev/api/class_usd_geom_mesh.html) (topology,
//! explicit polygonal `subdivisionScheme`, primvar element counts),
//! [`pointBased.h`](https://openusd.org/dev/api/class_usd_geom_point_based.html)
//! (`normals` vs. `primvars:normals`),
//! [`primvar.h`](https://openusd.org/dev/api/class_usd_geom_primvar.html)
//! (interpolation, indexed primvars),
//! [`metrics.h`](https://openusd.org/dev/api/group___usd_geom_up_axis__group.html)
//! (`upAxis`, `metersPerUnit`),
//! [`xformable.h`](https://openusd.org/dev/api/class_usd_geom_xformable.html)
//! (`xformOpOrder`),
//! [`gprim.h`](https://openusd.org/dev/api/class_usd_geom_gprim.html)
//! (`orientation`, `doubleSided`), and
//! [`boundable.h`](https://openusd.org/dev/api/class_usd_geom_boundable.html)
//! (`extent`).
//!
//! Materials follow the `UsdPreviewSurface` specification (OpenUSD
//! `docs/spec_usdpreviewsurface.rst`: inputs, fallbacks, color spaces,
//! normal-map remapping, texture orientation), the node definitions in
//! `pxr/usd/plugin/usdShaders/shaders/shaderDefs.usda` (input and output
//! types), and the `UsdShade` headers:
//! [`material.h`](https://openusd.org/dev/api/class_usd_shade_material.html)
//! (`outputs:surface`) and
//! [`materialBindingAPI.h`](https://openusd.org/dev/api/class_usd_shade_material_binding_a_p_i.html)
//! (direct bindings).

#![no_std]

extern crate alloc;

mod build;
mod error;
mod material;
mod mesh;
mod scene;
mod shading;
mod transform;

pub use error::{ExportError, MaterialProblem, MeshProblem};
pub use layerstack_usda::writer::Value;
pub use layerstack_usdz::PackageFile;
pub use material::{Channel, ColorInput, FloatInput, MATERIALS_SCOPE, Material, Texture, Wrap};
pub use mesh::{
    CustomAttribute, CustomPrimvar, Faces, Interpolation, Mesh, Orientation, Primvar, PrimvarData,
};
pub use scene::{Node, ROOT_LAYER_PATH, Scene, StageSettings, UpAxis, Xform};
pub use transform::Transform;

#[cfg(test)]
mod material_tests;
#[cfg(test)]
mod tests;
