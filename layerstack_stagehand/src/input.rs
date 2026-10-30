// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a mesh plan is built from: plain slices, borrowed from a stage's
//! arrays ([`crate::StageMesh`]) or from any other source.

/// The winding of a mesh's front faces (`UsdGeomGprim` `orientation`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    /// Counter-clockwise faces are front faces: the fallback.
    #[default]
    RightHanded,
    /// Clockwise faces are front faces; the triangles are flipped so that
    /// they come out counter-clockwise, as Hydra flips them.
    LeftHanded,
}

impl Orientation {
    /// The orientation Hydra draws for `token`: anything but `rightHanded`
    /// flips (`pxr/imaging/hd/meshUtil.cpp`,
    /// `HdMeshUtil::ComputeTriangleIndices`: `flip = orientation !=
    /// rightHanded`).
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        if token == "rightHanded" {
            Self::RightHanded
        } else {
            Self::LeftHanded
        }
    }

    /// The token, `rightHanded` or `leftHanded`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RightHanded => "rightHanded",
            Self::LeftHanded => "leftHanded",
        }
    }
}

/// How a mesh is refined before drawing (`UsdGeomMesh` `subdivisionScheme`).
///
/// Stage 1 draws every mesh as its control cage: the scheme is carried
/// into the [`crate::MeshReport`] so a caller can tell a subdivision
/// surface drawn unrefined from a polygon mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SubdivisionScheme {
    /// `catmullClark`: the schema fallback.
    #[default]
    CatmullClark,
    /// `loop`.
    Loop,
    /// `bilinear`.
    Bilinear,
    /// `none`: a polygon mesh.
    None,
    /// A token the schema does not list.
    Other,
}

impl SubdivisionScheme {
    /// The scheme a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "catmullClark" => Self::CatmullClark,
            "loop" => Self::Loop,
            "bilinear" => Self::Bilinear,
            "none" => Self::None,
            _ => Self::Other,
        }
    }

    /// The token; `other` for [`Self::Other`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CatmullClark => "catmullClark",
            Self::Loop => "loop",
            Self::Bilinear => "bilinear",
            Self::None => "none",
            Self::Other => "other",
        }
    }

    /// Whether a renderer that refines would refine the mesh: every scheme
    /// but `none`.
    #[must_use]
    pub fn is_subdivision_surface(self) -> bool {
        self != Self::None
    }
}

/// How a primvar's elements map onto a mesh (`UsdGeomPrimvar`
/// interpolation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// One element for the whole mesh.
    Constant,
    /// One element per face (holes included).
    Uniform,
    /// One element per point, interpolated linearly.
    Varying,
    /// One element per point.
    Vertex,
    /// One element per face corner (face-vertex), holes included.
    FaceVarying,
}

impl Interpolation {
    /// The interpolation a token names; `None` for an unknown token.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "constant" => Self::Constant,
            "uniform" => Self::Uniform,
            "varying" => Self::Varying,
            "vertex" => Self::Vertex,
            "faceVarying" => Self::FaceVarying,
            _ => return None,
        })
    }

    /// The token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Constant => "constant",
            Self::Uniform => "uniform",
            Self::Varying => "varying",
            Self::Vertex => "vertex",
            Self::FaceVarying => "faceVarying",
        }
    }
}

/// A primvar's data: its values, its optional indices, its interpolation
/// and its element size.
///
/// With `indices`, element `i` is `values[indices[i]]`
/// (`UsdGeomPrimvar::ComputeFlattened`); without, `values[i]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Primvar<'a, T> {
    /// The values.
    pub values: &'a [T],
    /// The indices into `values`, one per element, when the primvar is
    /// indexed.
    pub indices: Option<&'a [i32]>,
    /// How the elements map onto the mesh.
    pub interpolation: Interpolation,
    /// Values per element; only 1 is drawn, any other drops the primvar
    /// with a [`crate::ChannelState::Dropped`] note.
    pub element_size: u32,
}

impl<'a, T> Primvar<'a, T> {
    /// An unindexed primvar of element size 1.
    #[must_use]
    pub fn new(values: &'a [T], interpolation: Interpolation) -> Self {
        Self {
            values,
            indices: None,
            interpolation,
            element_size: 1,
        }
    }

    /// The same primvar, indexed by `indices`.
    #[must_use]
    pub fn with_indices(mut self, indices: &'a [i32]) -> Self {
        self.indices = Some(indices);
        self
    }
}

/// A mesh's topology (`UsdGeomMesh`): what the triangulation depends on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeshTopology<'a> {
    /// `faceVertexCounts`.
    pub face_vertex_counts: &'a [i32],
    /// `faceVertexIndices`.
    pub face_vertex_indices: &'a [i32],
    /// `holeIndices`: faces left out, sorted ascending (Hydra walks them in
    /// step with the faces; see [`crate::MeshReport::unmatched_hole_indices`]).
    pub hole_indices: &'a [i32],
    /// `orientation`.
    pub orientation: Orientation,
    /// `subdivisionScheme`, reported but not applied.
    pub subdivision_scheme: SubdivisionScheme,
}

/// Face subsets (`GeomSubset`s of `elementType = "face"`), flat: subset
/// `i` holds `faces[ends[i - 1]..ends[i]]` (from 0 for the first). The
/// plan draws each subset as one contiguous range of triangles
/// ([`crate::DrawRange`]).
///
/// Flat so that a caller can hand over any number of subsets as two
/// arrays, from a stage or across a WebAssembly boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaceSubsets<'a> {
    /// Every subset's face indices, one subset after another.
    pub faces: &'a [i32],
    /// Where each subset ends in `faces`, non-decreasing.
    pub ends: &'a [u32],
}

impl<'a> FaceSubsets<'a> {
    /// The number of subsets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Subset `index`'s faces; empty when `ends` does not describe a range
    /// of `faces` there.
    #[must_use]
    pub fn get(&self, index: usize) -> &'a [i32] {
        let start = index
            .checked_sub(1)
            .map_or(0, |previous| self.ends[previous] as usize);
        let end = self.ends.get(index).map_or(0, |&end| end as usize);
        self.faces.get(start..end).unwrap_or(&[])
    }
}

/// Everything a mesh plan reads: topology, points, the drawn primvars and
/// the face subsets.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeshInput<'a> {
    /// The topology.
    pub topology: MeshTopology<'a>,
    /// `points`.
    pub points: &'a [[f32; 3]],
    /// The authored normals (`primvars:normals`, else `normals`); without
    /// them the plan computes smooth normals, as Hydra does.
    pub normals: Option<Primvar<'a, [f32; 3]>>,
    /// The texture coordinates (`primvars:st`).
    pub st: Option<Primvar<'a, [f32; 2]>>,
    /// The display color (`primvars:displayColor`).
    pub display_color: Option<Primvar<'a, [f32; 3]>>,
    /// The face subsets drawn as ranges, typically the `materialBind`
    /// family.
    pub subsets: FaceSubsets<'a>,
}
