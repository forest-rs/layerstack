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
//! # Draw lists
//!
//! A [`DrawList`] (with `stage`) is what a renderer draws from a stage,
//! retained: [`DrawItem`]s, each one instanced draw of a retained buffer
//! ([`DrawBuffers`], owned by the list) at one or more world matrices. A
//! gprim outside any instance is an item drawn once; a native instancing
//! prototype's gprims are an item each, drawn at every instance binding
//! the same material; a `PointInstancer`'s prototypes are an item each,
//! drawn at every unmasked instance. Visibility and purpose hide items
//! without dropping their buffers.
//!
//! [`DrawList::apply_changes`] takes the change records an edit produced
//! (`layerstack::Changes`) and reads only what they reach;
//! [`DrawList::set_time`] reads at a new time only the items whose
//! inputs might vary with time (time samples, splines or value clips on
//! their geometry, visibility or transform chain). Each returns a
//! [`DrawDiff`]: the items added, removed, moved, shown or hidden or
//! rebound to new materials, and the buffers added, refilled (by a kept
//! plan, no triangulation), rebuilt or freed, with [`DrawWork`] counting
//! what was read, reused, recomputed and rebuilt.
//!
//! # Cameras and lights
//!
//! The draw list also lists the stage's cameras ([`CameraItem`]: a
//! `layerstack_schemas::camera::ComputedCamera`, with view and projection
//! matrices as `GfCamera` and `GfFrustum` compute them, fitted to a viewport
//! by `conform_window`) and its built-in `UsdLux` lights ([`LightItem`]:
//! world placement and direction, emission as `color * blackbody *
//! intensity * 2^exposure`, divided by the light's world-space size when it
//! normalizes, its shape, `ShapingAPI` and `ShadowAPI` controls, and its
//! link collections as data). A diff lists the cameras and lights added,
//! removed and changed; a change reaches a camera or a light through an
//! input it reads, or its transform or visibility or one above it; a time
//! change reads
//! only those whose inputs or transform might vary. Lights and cameras
//! inside native instances or point instancer prototypes are not listed
//! yet.
//!
//! Not yet: materials beyond their bindings, curves, points and light
//! linking evaluation.

#![no_std]
extern crate alloc;

#[cfg(feature = "stage")]
mod camera;
#[cfg(feature = "stage")]
mod draw;
mod input;
#[cfg(feature = "stage")]
mod light;
mod plan;
mod report;
mod shapes;
#[cfg(feature = "stage")]
mod stage;
mod triangulate;

#[cfg(test)]
mod tests;

#[cfg(feature = "stage")]
pub use camera::CameraItem;
#[cfg(feature = "stage")]
pub use draw::{
    BufferKey, DrawBuffers, DrawDiff, DrawItem, DrawList, DrawOptions, DrawWork, ItemId,
    ItemSource, Purposes,
};
pub use input::{
    FaceSubsets, Interpolation, MeshInput, MeshTopology, Orientation, Primvar, SubdivisionScheme,
};
#[cfg(feature = "stage")]
pub use light::{
    LightEmission, LightGeometry, LightItem, LightKind, LightLink, LightShadow, LightShaping,
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
