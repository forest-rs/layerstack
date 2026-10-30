// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render-ready buffers from a composed stage: the layer between
//! Layerstack and a renderer.
//!
//! Stagehand turns what a stage composes into what a GPU draws: indexed
//! triangles with per-vertex positions, normals, texture coordinates and
//! colors, and draw ranges for material subsets. It is not a renderer and
//! knows no graphics API; a 3D engine, a procedural generator or the
//! browser demo hands the buffers to its own.
//!
//! # Meshes
//!
//! A [`MeshPlan`] is built once per mesh topology from a [`MeshInput`]
//! (plain slices: topology, points, primvars, face subsets) and kept:
//!
//! - **Triangulation.** Faces are fanned from their first vertex exactly
//!   as Hydra does (`HdMeshUtil::ComputeTriangleIndices`): the same
//!   triangles in the same order, holes and faces of fewer than three
//!   vertices skipped, left-handed faces flipped. Under the default
//!   [`Triangulation::Concave`], a concave face (found in the projection to
//!   its best-fit plane) is ear-clipped by `exedra_triangulate` instead,
//!   and fanned only if the ear clipper refuses it, which the report
//!   notes. [`Triangulation::HydraFan`] fans everything, for parity.
//! - **Vertices.** Corners are split where a face-varying or uniform
//!   normal, `st` or `displayColor` differs, indexed primvars included,
//!   and merged where they agree.
//! - **Normals.** Authored normals (`vertex`, `varying`, `faceVarying`,
//!   `uniform` or `constant`) are used; without them, smooth normals are
//!   computed per point as Hydra computes them, or, with
//!   [`SmoothNormals::FaceOriented`], with each reflex corner of a concave
//!   face turned to the face's front.
//! - **Subsets.** Each face subset's triangles form one [`DrawRange`],
//!   then the faces in no subset.
//! - **Animation.** [`MeshPlan::refill`] rewrites positions and normals for
//!   new points without triangulating again; a topology change needs a new
//!   plan ([`MeshPlan::same_topology`]).
//! - **Subdivision surfaces** are drawn as their control cage; the report
//!   carries the scheme.
//!
//! The [`MeshReport`] counts what happened (faces, triangles, vertices,
//! holes, degenerate and concave faces, fallbacks, dropped primvars, subset
//! overlaps) and notes each skipped or concave face, so a caller can check
//! the buffers without reading them.
//!
//! # Implicit shapes
//!
//! [`tessellate`] turns a `Cube`, `Sphere`, `Cylinder`, `Cone`, `Capsule`
//! or `Plane`, or a `Cylinder_1` or `Capsule_1` with a radius per end
//! ([`Shape`]), into buffers at an explicit [`Tessellation`].
//!
//! # From a stage
//!
//! With the `stage` feature (on by default), [`StageMesh::read`] reads a
//! mesh off a `layerstack_schemas::Scene` at a time and [`read_shape`] an
//! implicit shape. Without it the crate depends on neither `layerstack`
//! nor its schemas: the WebAssembly module of
//! `@forest-rs/layerstack_stagehand` is built that way, and takes its
//! input as typed arrays.
//!
//! ```
//! use layerstack_stagehand::{MeshInput, MeshOptions, MeshPlan, MeshTopology};
//!
//! // A unit square, one quad.
//! let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
//! let input = MeshInput {
//!     topology: MeshTopology {
//!         face_vertex_counts: &[4],
//!         face_vertex_indices: &[0, 1, 2, 3],
//!         ..MeshTopology::default()
//!     },
//!     points: &points,
//!     ..MeshInput::default()
//! };
//! let plan = MeshPlan::new(&input, &MeshOptions::default());
//! let buffers = plan.buffers(&input).expect("the input the plan was built from");
//! assert_eq!(buffers.indices, [0, 1, 2, 0, 2, 3]);
//! assert_eq!(plan.report().triangles, 2);
//! assert_eq!(buffers.normals[0], [0.0, 0.0, 1.0]);
//! ```
//!
//! Not yet: instancing, draw-list diffs, cameras, lights, materials,
//! curves and points.

#![no_std]
extern crate alloc;

mod input;
mod plan;
mod report;
mod shapes;
#[cfg(feature = "stage")]
mod stage;
mod triangulate;

#[cfg(test)]
mod tests;

pub use input::{
    FaceSubsets, Interpolation, MeshInput, MeshTopology, Orientation, Primvar, SubdivisionScheme,
};
pub use plan::{DrawRange, MeshBuffers, MeshOptions, MeshPlan, PlanMismatch, SmoothNormals};
pub use report::{
    ChannelState, DropReason, FaceNote, FaceNoteKind, MeshReport, NormalsSource, SubsetReport,
    TopologyProblem,
};
pub use shapes::{Axis, Shape, Tessellation, tessellate};
#[cfg(feature = "stage")]
pub use stage::{StageMesh, read_shape};
pub use triangulate::Triangulation;

/// The ear clipper's error, carried by [`FaceNoteKind::ConcaveFellBack`].
pub use exedra_triangulate::TriError;
