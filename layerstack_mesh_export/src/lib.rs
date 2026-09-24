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
//! Serialization goes through [`layerstack_usda::writer`] (deterministic
//! USDA) and packaging through [`layerstack_usdz::write_usdz`]. The
//! composition kernel is not involved.
//!
//! Materials are not exported yet: material bindings and a shader
//! representation (e.g. `UsdPreviewSurface`) are the next scope. Texture
//! files can already be packaged next to the layer with
//! [`Scene::to_usdz`].
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
//! References:
//! [`UsdGeomMesh`](https://openusd.org/dev/api/class_usd_geom_mesh.html),
//! [`UsdGeomPointBased`](https://openusd.org/dev/api/class_usd_geom_point_based.html),
//! [`UsdGeomPrimvar`](https://openusd.org/dev/api/class_usd_geom_primvar.html),
//! [`UsdGeomXformable`](https://openusd.org/dev/api/class_usd_geom_xformable.html),
//! [`UsdGeomBoundable`](https://openusd.org/dev/api/class_usd_geom_boundable.html).

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
