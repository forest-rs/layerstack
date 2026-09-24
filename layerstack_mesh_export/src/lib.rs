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
//! Materials are not exported yet: direct material bindings, face subsets
//! and a portable shader subset (`UsdPreviewSurface`) are the next scope.
//!
//! # Example
//!
//! ```
//! use layerstack_mesh_export::{
//!     Faces, Mesh, Primvar, Scene, StageSettings, Transform, UpAxis, Xform,
//! };
//!
//! let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
//! let normals = [[0.0, 0.0, 1.0]; 4];
//! let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
//! let quad = Mesh::new("Quad", &points, Faces::Polygons { counts: &[4], indices: &[0, 1, 2, 3] })
//!     .with_normals(Primvar::vertex(&normals))
//!     .with_uvs(Primvar::vertex(&uvs));
//!
//! let root = Xform::new("Root")
//!     .with_transform(Transform::from_translation([0.0, 0.0, 2.0]))
//!     .with_mesh(quad);
//! let scene = Scene::new(StageSettings::new(UpAxis::Z, 1.0), root);
//!
//! let usda = scene.to_usda()?;
//! assert!(usda.contains("uniform token subdivisionScheme = \"none\""));
//! let usdz = scene.to_usdz(&[])?;
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

#![no_std]

extern crate alloc;

mod build;
mod error;
mod mesh;
mod scene;
mod transform;

pub use error::{ExportError, MeshProblem};
pub use layerstack_usda::writer::Value;
pub use layerstack_usdz::PackageFile;
pub use mesh::{
    CustomAttribute, CustomPrimvar, Faces, Interpolation, Mesh, Orientation, Primvar, PrimvarData,
};
pub use scene::{Node, ROOT_LAYER_PATH, Scene, StageSettings, UpAxis, Xform};
pub use transform::Transform;

#[cfg(test)]
mod tests;
