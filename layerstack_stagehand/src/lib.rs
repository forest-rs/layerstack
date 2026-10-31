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
//!   normal, `st`, `displayColor` or another texture coordinate set
//!   ([`MeshInput::uv_sets`], each a [`UvSet`] channel of its own) differs,
//!   indexed primvars included, and merged where they agree.
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
//! link collections as data), a `Mesh` with `MeshLightAPI` applied (its
//! faces emit; normalized by their world-space area) and a `GeometryLight`
//! (its target as data). Lights and cameras inside a native instance are
//! listed once per instance, by their instance proxies' paths; lights inside
//! a point instancer's prototype once per unmasked instance ([`LightId`]:
//! the light's path and the instance's [`InstanceId`], its authored `ids`
//! entry, or its position when the instancer authors no `ids`, so
//! reordering the arrays of one that does moves its lights rather than
//! replacing them). A diff lists the cameras and lights added,
//! removed and changed; a change reaches a camera or a light through an
//! input it reads, or its transform or visibility or one above it; a time
//! change reads
//! only those whose inputs or transform might vary. A light input connected
//! to a node graph or another prim's input reads what the connection leads
//! to. Cameras inside a point instancer's prototypes are not listed.
//!
//! # Materials
//!
//! The draw list holds a [`MaterialItem`] for each material its items bind:
//! the `UsdPreviewSurface` the material's surface output resolves to
//! (through [`DrawOptions::render_contexts`], then the universal context),
//! with each input a constant, a primvar or a [`MaterialTexture`] lookup
//! (a texture reading a primvar other than `st`, `st1` say, gets its mesh
//! that primvar as a [`UvSet`] channel, and the mesh is planned again when
//! the sets its materials read change),
//! connections followed through node graphs and material interface inputs,
//! fallbacks from the shader definitions, texture files anchored to the
//! layers authoring them ([`TextureFile`], with UDIM tiles listed by
//! [`TextureFile::udim_tiles`]), the [`AlphaMode`] the opacity inputs come to,
//! and a [`MaterialNote`] for whatever could not be represented. A diff
//! lists the materials added, removed and changed; a material is read
//! again only when a shading property of it, or of a shader or node graph
//! it reads, changes, or when an item comes to bind it, and a time change
//! reads only those whose inputs might vary.
//!
//! # Curves and points
//!
//! A [`CurvesPlan`] lays out `BasisCurves`, `NurbsCurves` and
//! `HermiteCurves` ([`CurvesInput`]) once per topology: each curve's
//! segments by OpenUSD's rules (`vstep`, wrap, pinned phantom points;
//! [`basis_segment_counts`], [`basis_data_sizes`] and
//! [`interpolation_for_size`] answer as `UsdGeomBasisCurves` does), sampled
//! [`CurveOptions::samples`] times per segment, and fills [`CurveBuffers`]:
//! line segments with a width, color and opacity per sample, and with
//! [`CurveStyle::Swept`] tubes of [`CurveOptions::sides`] sides (or
//! ribbons facing authored normals) as triangles. Widths, normals and
//! colors follow their `constant`, `uniform`, `varying` or `vertex`
//! interpolation; a width below zero (a Catmull-Rom chain of widths
//! undershoots) is drawn as 0. A NURBS curve that jumps at an interior knot
//! of full multiplicity is sampled on both sides of the jump, and no line
//! segment or sweep crosses it. The basis matrices, de Boor's algorithm,
//! rotation-minimizing frames and the sweeps are `exedra_curves`'. The
//! [`CurvesReport`] gives segments per curve, the evaluation and any token
//! fallback, samples, triangles, each primvar's fate, clamped widths,
//! notes for curves drawn otherwise than authored, and the `ComputeExtent`
//! extent.
//!
//! [`fill_points`] turns `Points` ([`PointsInput`]) into per-point
//! positions, widths, colors and `ids` ([`PointBuffers`]), for sprites, or
//! for a sphere or (with normals) a disc instanced at each point
//! ([`point_shape`]).
//!
//! In a draw list, curves and points are items like meshes: new points,
//! widths or colors refill their buffers ([`DrawWork::curves_refilled`],
//! [`DrawWork::points_refilled`]); a new `curveVertexCounts`, `type`,
//! `basis` or `wrap`, or a NURBS curve's `order`, `knots`, `ranges` or
//! `pointWeights`, lays curves out again ([`DrawWork::curves_laid_out`]),
//! and a new point count reads points anew ([`DrawWork::points_rebuilt`]).
//!
//! Not yet: shaders other than `UsdPreviewSurface` and its texture,
//! primvar and transform nodes, and light linking evaluation.

#![no_std]
extern crate alloc;

#[cfg(feature = "stage")]
mod camera;
mod curves;
#[cfg(feature = "stage")]
mod draw;
mod input;
#[cfg(feature = "stage")]
mod light;
#[cfg(feature = "stage")]
mod material;
mod plan;
mod points;
mod report;
mod shapes;
#[cfg(feature = "stage")]
mod stage;
#[cfg(feature = "stage")]
mod stage_curves;
mod triangulate;

#[cfg(test)]
mod curve_tests;
#[cfg(test)]
mod tests;

#[cfg(feature = "stage")]
pub use camera::CameraItem;
pub use curves::{
    CurveBasis, CurveBuffers, CurveDataSizes, CurveEvaluation, CurveFallback, CurveForm,
    CurveGeometry, CurveNote, CurveNoteKind, CurveOptions, CurveProblem, CurveStyle, CurveType,
    CurveWrap, CurvesInput, CurvesMismatch, CurvesPlan, CurvesReport, basis_data_sizes,
    basis_segment_counts, curves_extent, interpolation_for_size,
};
#[cfg(feature = "stage")]
pub use draw::{
    BufferKey, DrawBuffers, DrawDiff, DrawItem, DrawList, DrawOptions, DrawWork, ItemId,
    ItemInstance, ItemSource, PointStyle, Purposes,
};
pub use input::{
    FaceSubsets, Interpolation, MeshInput, MeshTopology, Orientation, Primvar, SubdivisionScheme,
    UvSetInput,
};
#[cfg(feature = "stage")]
pub use light::{
    InstanceId, LightEmission, LightGeometry, LightId, LightInstance, LightItem, LightKind,
    LightLink, LightShadow, LightShaping,
};
#[cfg(feature = "stage")]
pub use material::{
    AlphaMode, MaterialItem, MaterialNote, MaterialNoteKind, MaterialTexture, MaterialValue,
    TextureChannel, TextureColorSpace, TextureCoordinates, TextureFile, TextureTransform,
    TextureWrap, UdimTileFile,
};
pub use plan::{DrawRange, MeshBuffers, MeshOptions, MeshPlan, PlanMismatch, SmoothNormals, UvSet};
pub use points::{
    PointBuffers, PointShape, PointsInput, PointsReport, fill_points, point_shape, points_extent,
};
pub use report::{
    ChannelState, DropReason, FaceNote, FaceNoteKind, MeshReport, NormalsSource, SubsetReport,
    TopologyProblem, UvSetReport,
};
pub use shapes::{Axis, Shape, Tessellation, tessellate};
#[cfg(feature = "stage")]
pub use stage::{StageMesh, read_shape};
#[cfg(feature = "stage")]
pub use stage_curves::{StageCurves, StagePoints};
pub use triangulate::Triangulation;

/// Why a NURBS curve cannot be evaluated, carried by [`CurveNoteKind::Nurbs`].
pub use exedra_curves::NurbsError;

/// The ear clipper's error, carried by [`FaceNoteKind::ConcaveFellBack`].
pub use exedra_triangulate::TriError;
