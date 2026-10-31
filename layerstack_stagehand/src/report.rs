// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a plan did, in counts and notes, so a caller can check the result
//! without reading the triangles.

use alloc::vec::Vec;
use core::fmt;

use exedra_triangulate::TriError;

use crate::input::{Interpolation, Orientation, SubdivisionScheme};
use crate::plan::SmoothNormals;
use crate::triangulate::Triangulation;

/// Why a mesh's topology cannot be drawn: nothing is drawn.
///
/// The same checks as `layerstack_schemas::subset::validate_mesh_topology`
/// (OpenUSD's `UsdGeomMesh::ValidateTopology`, refusing negative counts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopologyProblem {
    /// A face count is negative, or the counts overflow.
    FaceCounts,
    /// The counts do not sum to the number of face-vertex indices.
    SizeMismatch {
        /// The sum of `faceVertexCounts`.
        counts: u64,
        /// The number of `faceVertexIndices`.
        indices: u64,
    },
    /// The mesh has `u32::MAX` or more faces, points or face-vertex
    /// indices: more than 32-bit indices address.
    TooLarge,
    /// A face-vertex index names no point.
    VertexIndex {
        /// The index.
        index: i32,
        /// The number of points.
        points: u64,
    },
}

impl TopologyProblem {
    /// A stable code: `faceCounts`, `sizeMismatch`, `tooLarge` or
    /// `vertexIndex`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::FaceCounts => "faceCounts",
            Self::SizeMismatch { .. } => "sizeMismatch",
            Self::TooLarge => "tooLarge",
            Self::VertexIndex { .. } => "vertexIndex",
        }
    }
}

impl fmt::Display for TopologyProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FaceCounts => {
                f.write_str("a face vertex count is negative or the counts overflow")
            }
            Self::SizeMismatch { counts, indices } => write!(
                f,
                "the face vertex counts sum to {counts}, but there are {indices} face vertex indices"
            ),
            Self::TooLarge => f.write_str(
                "the mesh has more faces, points or indices than 32-bit indices address",
            ),
            Self::VertexIndex { index, points } => {
                write!(
                    f,
                    "the face vertex index {index} is outside the {points} points"
                )
            }
        }
    }
}

/// What a plan made of one face, when it is worth a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceNoteKind {
    /// A face listed in `holeIndices`: not drawn.
    Hole,
    /// A face of fewer than three vertices: not drawn, as Hydra skips it.
    Degenerate,
    /// A concave face fanned, under [`Triangulation::HydraFan`]: its
    /// triangles may cover area outside it.
    ConcaveFanned,
    /// A concave face ear-clipped (under [`Triangulation::Concave`]).
    ConcaveClipped,
    /// A concave face the ear clipper refused, fanned instead; the error
    /// says why (`exedra_triangulate::TriError`).
    ConcaveFellBack(TriError),
}

impl FaceNoteKind {
    /// A stable code: `hole`, `degenerate`, `concaveFanned`,
    /// `concaveClipped` or `concaveFellBack`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Hole => "hole",
            Self::Degenerate => "degenerate",
            Self::ConcaveFanned => "concaveFanned",
            Self::ConcaveClipped => "concaveClipped",
            Self::ConcaveFellBack(_) => "concaveFellBack",
        }
    }
}

/// A note about one authored face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaceNote {
    /// The face's index among `faceVertexCounts`.
    pub face: u32,
    /// What became of it.
    pub kind: FaceNoteKind,
}

/// Why a primvar is not in the buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// Its element size is not 1.
    ElementSize(u32),
    /// It has fewer elements (indices, when indexed) than its
    /// interpolation needs.
    TooFew {
        /// The elements the interpolation needs.
        expected: u64,
        /// The elements it has.
        found: u64,
    },
    /// An index names no value.
    Index(i32),
    /// Its interpolation is not drawn for this kind of prim: `faceVarying`
    /// on curves, `vertex` and `varying` on NURBS curves.
    Interpolation(Interpolation),
}

impl fmt::Display for DropReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ElementSize(size) => write!(f, "element size {size} is not drawn"),
            Self::TooFew { expected, found } => {
                write!(
                    f,
                    "{found} elements where its interpolation needs {expected}"
                )
            }
            Self::Index(index) => write!(f, "the index {index} names no value"),
            Self::Interpolation(interpolation) => {
                write!(f, "{interpolation:?} interpolation is not drawn here")
            }
        }
    }
}

/// What became of one primvar channel (normals, `st`, `displayColor`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelState {
    /// Not given.
    #[default]
    Absent,
    /// Drawn, with this interpolation; `indexed` when it came with
    /// indices.
    Used {
        /// Its interpolation.
        interpolation: Interpolation,
        /// Whether it is indexed.
        indexed: bool,
    },
    /// Given but not drawn.
    Dropped(DropReason),
}

/// Where the buffers' normals come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NormalsSource {
    /// Smooth normals computed from the points, per point: the default
    /// when no usable normals are authored. Under the default
    /// [`SmoothNormals::Hydra`] they are Hydra's
    /// (`pxr/imaging/hd/smoothNormals.cpp`), and the reflex corner of a
    /// lone concave face gets a normal facing away from the face's (its
    /// corner cross product turns the other way);
    /// [`SmoothNormals::FaceOriented`] turns it to the face's side.
    #[default]
    Computed,
    /// The authored normals.
    Authored,
}

/// One face subset's part of the triangle buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubsetReport {
    /// Faces drawn in its range.
    pub faces: u32,
    /// Triangles in its range.
    pub triangles: u32,
    /// Its face indices outside the mesh's faces: ignored.
    pub out_of_range: u32,
    /// Its faces an earlier subset already holds: drawn in the earlier
    /// subset's range only (a `materialBind` family should not overlap).
    pub already_claimed: u32,
}

/// A texture coordinate set's fate ([`crate::MeshInput::uv_sets`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UvSetReport {
    /// The primvar's name.
    pub name: alloc::string::String,
    /// Its channel's fate.
    pub state: ChannelState,
}

/// What a [`crate::MeshPlan`] made of a mesh: counts and notes enough to
/// check the buffers without reading them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshReport {
    /// Why nothing is drawn, when the topology is invalid.
    pub topology_problem: Option<TopologyProblem>,
    /// The triangulation used.
    pub triangulation: Triangulation,
    /// How smooth normals are computed, when [`Self::normals`] is
    /// [`NormalsSource::Computed`].
    pub smooth_normals: SmoothNormals,
    /// The mesh's orientation; left-handed triangles are flipped.
    pub orientation: Orientation,
    /// The subdivision scheme. Every mesh is drawn as its control cage:
    /// a subdivision surface (any scheme but `none`) is not refined.
    pub subdivision_scheme: SubdivisionScheme,
    /// Authored faces.
    pub faces: u32,
    /// Faces drawn.
    pub drawn_faces: u32,
    /// Points.
    pub points: u32,
    /// Triangles in the buffers.
    pub triangles: u32,
    /// Vertices in the buffers: the distinct corners, a corner being a
    /// point with its normal, `st` and color elements.
    pub vertices: u32,
    /// Corners of the drawn triangles (3 per triangle), which
    /// `vertices` dedupes.
    pub triangle_corners: u32,
    /// Faces skipped as holes.
    pub hole_faces: u32,
    /// Faces skipped for having fewer than three vertices.
    pub degenerate_faces: u32,
    /// Concave faces found (of more than three vertices).
    pub concave_faces: u32,
    /// Concave faces ear-clipped.
    pub clipped_faces: u32,
    /// Concave faces the ear clipper refused, fanned instead.
    pub fallback_faces: u32,
    /// `holeIndices` entries that skipped no face. Hydra walks the hole
    /// indices in step with the faces, so an unsorted, repeated or
    /// out-of-range entry skips nothing; so does this plan.
    pub unmatched_hole_indices: u32,
    /// Where the normals come from.
    pub normals: NormalsSource,
    /// The authored normals' fate (absent when computed for lack of them).
    pub normals_channel: ChannelState,
    /// The `st` channel's fate.
    pub st: ChannelState,
    /// The `displayColor` channel's fate.
    pub display_color: ChannelState,
    /// Each texture coordinate set's fate besides `st`, in the input's
    /// order.
    pub uv_sets: Vec<UvSetReport>,
    /// Each face subset's part, in the input's order.
    pub subsets: Vec<SubsetReport>,
    /// Faces drawn in no subset (the last range).
    pub unassigned_faces: u32,
    /// A note for each hole, degenerate and concave face, in face order.
    pub face_notes: Vec<FaceNote>,
}
