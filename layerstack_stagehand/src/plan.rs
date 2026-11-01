// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The retained per-mesh plan: triangles, the vertex split and the draw
//! ranges, decided once per topology, and the buffers it fills.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use hashbrown::HashMap;

use crate::input::{Interpolation, MeshInput, MeshTopology, Orientation, Primvar};
use crate::report::{
    ChannelState, DropReason, FaceNote, FaceNoteKind, MeshReport, NormalsSource, SubsetReport,
    TopologyProblem, UvSetReport,
};
use crate::triangulate::{FaceOutcome, Scratch, Triangulation, triangulate_face};

/// No element: a channel that does not split vertices at a corner.
const NONE: u32 = u32::MAX;

/// How a plan is built.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshOptions {
    /// How faces become triangles; [`Triangulation::Concave`] by default.
    pub triangulation: Triangulation,
    /// How smooth normals are computed when none are authored;
    /// [`SmoothNormals::Hydra`] by default.
    pub normals: SmoothNormals,
}

/// How a plan computes smooth normals for a mesh without usable authored
/// ones. Both sum one contribution per face corner into the corner's
/// point and normalize the sums; they differ only at a concave face's
/// reflex corners.
///
/// At each corner the contribution is the cross product of the edges to
/// its two neighbors, `(next - here) × (previous - here)`, which points
/// along the face's normal at a convex corner and against it at a reflex
/// one (the corner turns the other way).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SmoothNormals {
    /// The corner cross products as they are, exactly as Hydra sums them
    /// (`pxr/imaging/hd/smoothNormals.cpp`): the same normals as Hydra
    /// draws. At the reflex corner of a lone concave face the normal then
    /// points away from the face's front, and the corner shades dark. The
    /// default, for parity.
    #[default]
    Hydra,
    /// Each corner's cross product turned to agree with its face's normal
    /// (by Newell's method, which a concave face does not mislead): a
    /// reflex corner contributes along the face's front instead of
    /// against it. The same normals as [`Self::Hydra`] wherever every face
    /// is convex; at a reflex corner, a normal on the face's front side.
    FaceOriented,
}

/// One contiguous run of triangles in [`MeshBuffers::indices`]: a face
/// subset's faces, or the faces in no subset. A renderer draws each with
/// its own material (three.js `BufferGeometry.addGroup(first, count, i)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawRange {
    /// The subset, by its position in [`MeshInput::subsets`]; `None` for
    /// the faces in no subset, which always come last.
    pub subset: Option<u32>,
    /// The first index of the range in [`MeshBuffers::indices`].
    pub first: u32,
    /// The number of indices (three per triangle).
    pub count: u32,
}

/// Render-ready buffers: one vertex per distinct corner, indexed
/// triangles, and draw ranges.
///
/// Triangles wind counter-clockwise seen from their front (left-handed
/// meshes are flipped), in draw-range order: each subset's faces in face
/// order, then the faces in no subset; within a face, in the
/// triangulation's order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshBuffers {
    /// Each vertex's position.
    pub positions: Vec<[f32; 3]>,
    /// Each vertex's normal, unit length (zero for a point no face
    /// bends).
    pub normals: Vec<[f32; 3]>,
    /// Each vertex's texture coordinate, when `st` is drawn.
    pub st: Option<Vec<[f32; 2]>>,
    /// Each vertex's display color (linear RGB), when `displayColor` is
    /// drawn.
    pub colors: Option<Vec<[f32; 3]>>,
    /// Each drawn texture coordinate set besides `st`, in the input's
    /// order ([`MeshInput::uv_sets`]).
    pub uv_sets: Vec<UvSet>,
    /// Three vertex indices per triangle.
    pub indices: Vec<u32>,
    /// The authored face each triangle comes from, for picking and
    /// per-face data.
    pub triangle_faces: Vec<u32>,
    /// The draw ranges: one per subset, in the input's order, then one for
    /// the faces in no subset (empty ranges included).
    pub ranges: Vec<DrawRange>,
}

/// A texture coordinate set's buffer: each vertex's coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UvSet {
    /// The primvar's name.
    pub name: String,
    /// Each vertex's coordinates.
    pub values: Vec<[f32; 2]>,
}

/// Why a plan cannot fill buffers from an input: the input is not the
/// shape the plan was built for, so the caller builds a new plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanMismatch {
    /// The number of points differs.
    PointCount {
        /// The plan's points.
        expected: u32,
        /// The input's points.
        found: u32,
    },
    /// A primvar channel is given where the plan has none, missing where
    /// it has one, or has another interpolation, indexing or too few
    /// elements: `normals`, `st`, `displayColor`, or `uvSets` for a
    /// texture coordinate set besides `st`.
    Channel(&'static str),
}

impl fmt::Display for PlanMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PointCount { expected, found } => {
                write!(f, "the plan has {expected} points, the input {found}")
            }
            Self::Channel(name) => write!(f, "the `{name}` primvar is not laid out as the plan's"),
        }
    }
}

impl core::error::Error for PlanMismatch {}

/// A drawn channel's layout, fixed by the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    interpolation: Interpolation,
    indexed: bool,
}

/// The retained plan of one mesh: its triangles, its vertex split and its
/// draw ranges, decided once from the topology, the subsets and the
/// primvars' layouts.
///
/// [`Self::buffers`] fills [`MeshBuffers`] from an input of that shape;
/// [`Self::refill`] rewrites only the positions and normals, for a change
/// of points (animation), without triangulating again. A topology change
/// needs a new plan ([`Self::same_topology`] tells).
///
/// The triangulation is decided from the points the plan was built with:
/// a concave face stays clipped as it was when the points move. Corners
/// are merged into one vertex when they name the same point and the same
/// normal, `st` and color values (bit for bit) at build time.
#[derive(Clone, Debug)]
pub struct MeshPlan {
    report: MeshReport,
    counts: Vec<i32>,
    face_indices: Vec<i32>,
    holes: Vec<i32>,
    orientation: Orientation,
    smooth: SmoothNormals,
    points: u32,
    faces: u32,
    /// Per vertex: its point, a corner it stands for, and that corner's face.
    vertex_point: Vec<u32>,
    vertex_corner: Vec<u32>,
    vertex_face: Vec<u32>,
    indices: Vec<u32>,
    triangle_faces: Vec<u32>,
    ranges: Vec<DrawRange>,
    normals: Option<Layout>,
    st: Option<Layout>,
    color: Option<Layout>,
    /// The drawn texture coordinate sets besides `st`, by name.
    uv_sets: Vec<(String, Layout)>,
}

impl MeshPlan {
    /// Plans `input`: validates its topology (an invalid one plans
    /// nothing, and [`MeshReport::topology_problem`] says why), skips
    /// holes and faces of fewer than three vertices, triangulates the
    /// rest as `options` says, assigns faces to subsets, and splits the
    /// vertices where a normal, `st` or color differs between corners.
    #[must_use]
    pub fn new(input: &MeshInput<'_>, options: &MeshOptions) -> Self {
        let topology = &input.topology;
        let faces = u32::try_from(topology.face_vertex_counts.len()).unwrap_or(u32::MAX);
        let points = u32::try_from(input.points.len()).unwrap_or(u32::MAX);
        // Counted here with saturation; past validation every count fits.
        let mut report = MeshReport {
            triangulation: options.triangulation,
            smooth_normals: options.normals,
            orientation: topology.orientation,
            subdivision_scheme: topology.subdivision_scheme,
            faces,
            points,
            subsets: vec![SubsetReport::default(); input.subsets.len()],
            ..MeshReport::default()
        };
        let mut plan = Self {
            report: MeshReport::default(),
            counts: topology.face_vertex_counts.to_vec(),
            face_indices: topology.face_vertex_indices.to_vec(),
            holes: topology.hole_indices.to_vec(),
            orientation: topology.orientation,
            smooth: options.normals,
            points,
            faces,
            vertex_point: Vec::new(),
            vertex_corner: Vec::new(),
            vertex_face: Vec::new(),
            indices: Vec::new(),
            triangle_faces: Vec::new(),
            ranges: Vec::new(),
            normals: None,
            st: None,
            color: None,
            uv_sets: Vec::new(),
        };
        if let Err(problem) = validate_topology(topology, input.points.len()) {
            report.topology_problem = Some(problem);
            plan.ranges = empty_ranges(input.subsets.len());
            plan.report = report;
            return plan;
        }
        let corners = topology.face_vertex_indices.len();
        let flip = topology.orientation == Orientation::LeftHanded;

        // The channels, validated; each splits vertices only where it is
        // per face or per corner.
        let normals = checked(input.normals.as_ref(), faces, points, corners);
        report.normals_channel = normals.0;
        report.normals = if normals.1.is_some() {
            NormalsSource::Authored
        } else {
            NormalsSource::Computed
        };
        let st = checked(input.st.as_ref(), faces, points, corners);
        report.st = st.0;
        let color = checked(input.display_color.as_ref(), faces, points, corners);
        report.display_color = color.0;
        plan.normals = normals.1;
        plan.st = st.1;
        plan.color = color.1;
        let normal_keys = normals.1.and(input.normals.as_ref()).map(split_keys);
        let st_keys = st.1.and(input.st.as_ref()).map(split_keys);
        let color_keys = color.1.and(input.display_color.as_ref()).map(split_keys);
        let mut uv_keys = Vec::new();
        for set in input.uv_sets {
            if set.name == "st" || report.uv_sets.iter().any(|seen| seen.name == set.name) {
                continue;
            }
            let (state, layout) = checked(Some(&set.primvar), faces, points, corners);
            report.uv_sets.push(UvSetReport {
                name: set.name.into(),
                state,
            });
            if let Some(layout) = layout {
                plan.uv_sets.push((set.name.into(), layout));
                uv_keys.push(split_keys(&set.primvar));
            }
        }
        // The sets' elements at a corner, interned: one more key.
        let mut uv_elements: HashMap<Vec<u32>, u32> = HashMap::new();

        // Faces to triangles, in face order, skipping holes and degenerate
        // faces as Hydra does (`HdMeshUtil::ComputeTriangleIndices`).
        let mut triangles: Vec<[u32; 3]> = Vec::with_capacity(corners);
        let mut face_triangles = vec![0_u32; faces as usize + 1];
        let mut corner_face = vec![0_u32; corners];
        let mut scratch = Scratch::default();
        let mut hole = 0;
        let mut first = 0_u32;
        for (face, &count) in topology.face_vertex_counts.iter().enumerate() {
            let face = small(face);
            let count = count as u32;
            corner_face[first as usize..(first + count) as usize].fill(face);
            face_triangles[face as usize] = small(triangles.len());
            let note = if count < 3 {
                report.degenerate_faces += 1;
                Some(FaceNoteKind::Degenerate)
            } else if topology.hole_indices.get(hole) == Some(&(face as i32)) {
                hole += 1;
                report.hole_faces += 1;
                Some(FaceNoteKind::Hole)
            } else {
                report.drawn_faces += 1;
                let point = |corner: u32| {
                    input.points[topology.face_vertex_indices[corner as usize] as usize]
                };
                match triangulate_face(
                    first,
                    count,
                    flip,
                    options.triangulation,
                    point,
                    &mut triangles,
                    &mut scratch,
                ) {
                    FaceOutcome::Fanned { concave: false } => None,
                    FaceOutcome::Fanned { concave: true } => {
                        report.concave_faces += 1;
                        Some(FaceNoteKind::ConcaveFanned)
                    }
                    FaceOutcome::Clipped => {
                        report.concave_faces += 1;
                        report.clipped_faces += 1;
                        Some(FaceNoteKind::ConcaveClipped)
                    }
                    FaceOutcome::FellBack(error) => {
                        report.concave_faces += 1;
                        report.fallback_faces += 1;
                        Some(FaceNoteKind::ConcaveFellBack(error))
                    }
                }
            };
            if let Some(kind) = note {
                report.face_notes.push(FaceNote { face, kind });
            }
            first += count;
        }
        face_triangles[faces as usize] = small(triangles.len());
        report.unmatched_hole_indices = small(topology.hole_indices.len() - hole);

        // Each face's range: the first subset that holds it, else the
        // last range.
        let unassigned = small(input.subsets.len());
        let mut owner = vec![unassigned; faces as usize];
        for index in 0..input.subsets.len() {
            let entry = &mut report.subsets[index];
            for &face in input.subsets.get(index) {
                match usize::try_from(face)
                    .ok()
                    .filter(|&face| face < faces as usize)
                {
                    None => entry.out_of_range += 1,
                    Some(face) if owner[face] == unassigned => owner[face] = small(index),
                    Some(face) if owner[face] != small(index) => entry.already_claimed += 1,
                    Some(_) => {}
                }
            }
        }

        // Triangles in range order, each corner a vertex keyed by its point
        // and its channels' elements.
        let mut vertices: HashMap<[u32; 5], u32> = HashMap::with_capacity(corners);
        // The faces in range order, each range in face order (a stable sort).
        let mut order: Vec<u32> = (0..faces).collect();
        order.sort_by_key(|&face| owner[face as usize]);
        let mut cursor = 0;
        for range in 0..=unassigned {
            let start = small(plan.indices.len());
            let end =
                cursor + order[cursor..].partition_point(|&face| owner[face as usize] == range);
            for &face in &order[cursor..end] {
                let span = face_triangles[face as usize] as usize
                    ..face_triangles[face as usize + 1] as usize;
                if span.is_empty() {
                    continue;
                }
                if range < unassigned {
                    report.subsets[range as usize].faces += 1;
                    report.subsets[range as usize].triangles += small(span.len());
                } else {
                    report.unassigned_faces += 1;
                }
                for triangle in &triangles[span] {
                    for &corner in triangle {
                        let point = topology.face_vertex_indices[corner as usize] as u32;
                        let face = corner_face[corner as usize];
                        let uv = if uv_keys.is_empty() {
                            NONE
                        } else {
                            let elements: Vec<u32> = uv_keys
                                .iter()
                                .map(|keys| key_of(Some(keys), corner, face, point))
                                .collect();
                            let next = small(uv_elements.len());
                            *uv_elements.entry(elements).or_insert(next)
                        };
                        let key = [
                            point,
                            key_of(normal_keys.as_ref(), corner, face, point),
                            key_of(st_keys.as_ref(), corner, face, point),
                            key_of(color_keys.as_ref(), corner, face, point),
                            uv,
                        ];
                        let next = small(plan.vertex_point.len());
                        let vertex = *vertices.entry(key).or_insert(next);
                        if vertex == next {
                            plan.vertex_point.push(point);
                            plan.vertex_corner.push(corner);
                            plan.vertex_face.push(face);
                        }
                        plan.indices.push(vertex);
                    }
                    plan.triangle_faces.push(face);
                }
            }
            cursor = end;
            plan.ranges.push(DrawRange {
                subset: (range < unassigned).then_some(range),
                first: start,
                count: small(plan.indices.len()) - start,
            });
        }
        report.triangles = small(plan.triangle_faces.len());
        report.triangle_corners = small(plan.indices.len());
        report.vertices = small(plan.vertex_point.len());
        plan.report = report;
        plan
    }

    /// Each triangle as the points of its corners, for tests.
    #[cfg(test)]
    pub(crate) fn triangle_points(&self) -> Vec<[u32; 3]> {
        self.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| [0, 1, 2].map(|k| self.vertex_point[t[k] as usize]))
            .collect()
    }

    /// Each vertex's source point: the index into the input's points of
    /// the point its corners name, in [`MeshBuffers`] vertex order (empty
    /// when the topology is invalid and nothing is drawn).
    ///
    /// A vertex split for a face-varying normal, `st` or color still
    /// names one point, so per-point data the buffers do not carry (a
    /// skeleton's joint influences, a `vertex` primvar the plan does not
    /// draw) is gathered into vertex order through it: vertex `v` reads
    /// point `vertex_points()[v]`. Fixed by the plan, so a refill keeps it.
    #[must_use]
    pub fn vertex_points(&self) -> &[u32] {
        &self.vertex_point
    }

    /// What the plan made of the mesh.
    #[must_use]
    pub fn report(&self) -> &MeshReport {
        &self.report
    }

    /// Whether `topology` is the topology the plan was built from: then a
    /// change of points only needs [`Self::refill`].
    #[must_use]
    pub fn same_topology(&self, topology: &MeshTopology<'_>) -> bool {
        self.counts == topology.face_vertex_counts
            && self.face_indices == topology.face_vertex_indices
            && self.holes == topology.hole_indices
            && self.orientation == topology.orientation
    }

    /// Fills the buffers from `input`, which must be shaped as the input
    /// the plan was built from: the same number of points, and the same
    /// primvar channels with the same interpolation and indexing.
    ///
    /// # Errors
    ///
    /// [`PlanMismatch`] when `input` is shaped otherwise.
    pub fn buffers(&self, input: &MeshInput<'_>) -> Result<MeshBuffers, PlanMismatch> {
        let st = self.gather("st", self.st, input.st.as_ref())?;
        let colors = self.gather("displayColor", self.color, input.display_color.as_ref())?;
        let mut uv_sets = Vec::with_capacity(self.uv_sets.len());
        for (name, layout) in &self.uv_sets {
            let primvar = input
                .uv_sets
                .iter()
                .find(|set| set.name == name.as_str())
                .map(|set| &set.primvar);
            let values = self
                .gather("uvSets", Some(*layout), primvar)?
                .unwrap_or_default();
            uv_sets.push(UvSet {
                name: name.clone(),
                values,
            });
        }
        let mut buffers = MeshBuffers {
            positions: Vec::new(),
            normals: Vec::new(),
            st,
            colors,
            uv_sets,
            indices: self.indices.clone(),
            triangle_faces: self.triangle_faces.clone(),
            ranges: self.ranges.clone(),
        };
        self.refill(&mut buffers, input.points, input.normals.as_ref())?;
        Ok(buffers)
    }

    /// Rewrites the positions and normals of `buffers` (filled by this
    /// plan) from new `points` and, when the plan draws authored normals,
    /// new `normals` of the same layout; computed normals are computed
    /// again from the points, and `normals` is then not read. Nothing is
    /// triangulated again.
    ///
    /// # Errors
    ///
    /// [`PlanMismatch`] when `points` has another length, or `normals` is
    /// laid out otherwise than the plan's; `buffers` is then unchanged.
    pub fn refill(
        &self,
        buffers: &mut MeshBuffers,
        points: &[[f32; 3]],
        normals: Option<&Primvar<'_, [f32; 3]>>,
    ) -> Result<(), PlanMismatch> {
        if points.len() != self.points as usize {
            return Err(PlanMismatch::PointCount {
                expected: self.points,
                found: u32::try_from(points.len()).unwrap_or(u32::MAX),
            });
        }
        let authored = self.gather("normals", self.normals, normals)?;
        buffers.positions.clear();
        if self.report.topology_problem.is_some() {
            buffers.normals.clear();
            return Ok(());
        }
        buffers.positions.extend(
            self.vertex_point
                .iter()
                .map(|&point| points[point as usize]),
        );
        buffers.normals = match authored {
            Some(normals) => normals.into_iter().map(normalized).collect(),
            None => {
                let smooth = smooth_normals(
                    &self.counts,
                    &self.face_indices,
                    self.orientation,
                    self.smooth,
                    points,
                );
                self.vertex_point
                    .iter()
                    .map(|&point| smooth[point as usize])
                    .collect()
            }
        };
        Ok(())
    }

    /// Each vertex's element of `primvar`, when the plan draws the channel
    /// as `layout`; `None` when it does not draw it.
    fn gather<const N: usize>(
        &self,
        name: &'static str,
        layout: Option<Layout>,
        primvar: Option<&Primvar<'_, [f32; N]>>,
    ) -> Result<Option<Vec<[f32; N]>>, PlanMismatch> {
        let Some(layout) = layout else {
            // A channel the plan dropped is ignored whatever it holds now.
            return Ok(None);
        };
        let primvar = primvar.ok_or(PlanMismatch::Channel(name))?;
        let corners = self.face_indices.len();
        let same = Layout {
            interpolation: primvar.interpolation,
            indexed: primvar.indices.is_some(),
        } == layout;
        if !same || check(primvar, self.faces, self.points, corners).is_err() {
            return Err(PlanMismatch::Channel(name));
        }
        Ok(Some(
            (0..self.vertex_point.len())
                .map(|vertex| {
                    let element = element(
                        primvar.interpolation,
                        self.vertex_corner[vertex],
                        self.vertex_face[vertex],
                        self.vertex_point[vertex],
                    );
                    primvar.values[value_index(primvar, element)]
                })
                .collect(),
        ))
    }
}

/// `n` as a `u32`. Every count a plan keeps is bounded by the mesh's
/// faces, points or face-vertex indices, which [`validate_topology`]
/// keeps under `u32::MAX`, or by a shape's segment counts.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the counts are validated to fit `u32` before planning"
)]
pub(crate) fn small(n: usize) -> u32 {
    n as u32
}

/// Draw ranges with nothing in them: one per subset and the unassigned one.
fn empty_ranges(subsets: usize) -> Vec<DrawRange> {
    (0..=small(subsets))
        .map(|range| DrawRange {
            subset: (range < small(subsets)).then_some(range),
            first: 0,
            count: 0,
        })
        .collect()
}

/// The checks of `layerstack_schemas::subset::validate_mesh_topology`
/// (OpenUSD's `UsdGeomMesh::ValidateTopology`, which, unlike these, lets
/// negative counts through when they sum right), then the limits of
/// 32-bit indices.
///
/// Not a call to it: without the `stage` feature (the WebAssembly module)
/// this crate links neither `layerstack` nor its schemas. A test with
/// `stage` runs both over the same topologies, so they cannot drift
/// apart unnoticed.
fn validate_topology(topology: &MeshTopology<'_>, points: usize) -> Result<(), TopologyProblem> {
    let sum = topology
        .face_vertex_counts
        .iter()
        .try_fold(0_u64, |sum, &count| {
            sum.checked_add(u64::try_from(count).ok()?)
        })
        .ok_or(TopologyProblem::FaceCounts)?;
    let indices = topology.face_vertex_indices.len() as u64;
    if sum != indices {
        return Err(TopologyProblem::SizeMismatch {
            counts: sum,
            indices,
        });
    }
    let limit = u32::MAX as usize;
    if indices >= u64::from(u32::MAX)
        || topology.face_vertex_counts.len() >= limit
        || points >= limit
    {
        return Err(TopologyProblem::TooLarge);
    }
    for &index in topology.face_vertex_indices {
        if usize::try_from(index).map_or(true, |index| index >= points) {
            return Err(TopologyProblem::VertexIndex {
                index,
                points: points as u64,
            });
        }
    }
    Ok(())
}

/// The elements `interpolation` needs.
fn expected(interpolation: Interpolation, faces: u32, points: u32, corners: usize) -> usize {
    match interpolation {
        Interpolation::Constant => 1,
        Interpolation::Uniform => faces as usize,
        Interpolation::Varying | Interpolation::Vertex => points as usize,
        Interpolation::FaceVarying => corners,
    }
}

/// Checks that `primvar` covers the mesh: element size 1, enough elements
/// (indices, when indexed), and every index naming a value.
fn check<T>(
    primvar: &Primvar<'_, T>,
    faces: u32,
    points: u32,
    corners: usize,
) -> Result<(), DropReason> {
    if primvar.element_size != 1 {
        return Err(DropReason::ElementSize(primvar.element_size));
    }
    let needed = expected(primvar.interpolation, faces, points, corners);
    let found = primvar.indices.map_or(primvar.values.len(), <[i32]>::len);
    if found < needed {
        return Err(DropReason::TooFew {
            expected: needed as u64,
            found: found as u64,
        });
    }
    if let Some(&bad) = primvar.indices.and_then(|indices| {
        indices[..needed].iter().find(|&&index| {
            usize::try_from(index).map_or(true, |index| index >= primvar.values.len())
        })
    }) {
        return Err(DropReason::Index(bad));
    }
    Ok(())
}

/// The channel's state and, when drawn, its layout.
fn checked<T>(
    primvar: Option<&Primvar<'_, T>>,
    faces: u32,
    points: u32,
    corners: usize,
) -> (ChannelState, Option<Layout>) {
    let Some(primvar) = primvar else {
        return (ChannelState::Absent, None);
    };
    match check(primvar, faces, points, corners) {
        Err(reason) => (ChannelState::Dropped(reason), None),
        Ok(()) => (
            ChannelState::Used {
                interpolation: primvar.interpolation,
                indexed: primvar.indices.is_some(),
            },
            Some(Layout {
                interpolation: primvar.interpolation,
                indexed: primvar.indices.is_some(),
            }),
        ),
    }
}

/// The element of a corner under `interpolation`.
fn element(interpolation: Interpolation, corner: u32, face: u32, point: u32) -> usize {
    match interpolation {
        Interpolation::Constant => 0,
        Interpolation::Uniform => face as usize,
        Interpolation::Varying | Interpolation::Vertex => point as usize,
        Interpolation::FaceVarying => corner as usize,
    }
}

/// The value of `element`, through the indices when indexed; checked.
fn value_index<T>(primvar: &Primvar<'_, T>, element: usize) -> usize {
    primvar
        .indices
        .map_or(element, |indices| indices[element] as usize)
}

/// How a channel splits vertices: for a per-face or per-corner channel,
/// each value index's first index with the same bits, so equal values do
/// not split; nothing for a per-point or constant one.
struct SplitKeys {
    interpolation: Interpolation,
    indices: Option<Vec<i32>>,
    canonical: Vec<u32>,
}

fn split_keys<const N: usize>(primvar: &Primvar<'_, [f32; N]>) -> SplitKeys {
    let splits = matches!(
        primvar.interpolation,
        Interpolation::Uniform | Interpolation::FaceVarying
    );
    let mut canonical = Vec::new();
    if splits {
        let mut first: HashMap<[u32; N], u32> = HashMap::with_capacity(primvar.values.len());
        canonical.extend(
            primvar.values.iter().enumerate().map(|(index, value)| {
                *first.entry(value.map(f32::to_bits)).or_insert(small(index))
            }),
        );
    }
    SplitKeys {
        interpolation: primvar.interpolation,
        indices: primvar.indices.map(<[i32]>::to_vec),
        canonical,
    }
}

fn key_of(keys: Option<&SplitKeys>, corner: u32, face: u32, point: u32) -> u32 {
    match keys {
        Some(keys) if !keys.canonical.is_empty() => {
            let element = element(keys.interpolation, corner, face, point);
            let value = keys
                .indices
                .as_ref()
                .map_or(element, |indices| indices[element] as usize);
            keys.canonical[value]
        }
        _ => NONE,
    }
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let length = libm::sqrtf(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]);
    if length > 1e-10 {
        v.map(|c| c / length)
    } else {
        v
    }
}

/// Smooth normals per point, as Hydra computes them: at each corner of
/// every face (holes included), `(next - here) × (previous - here)`,
/// summed per point and normalized; a left-handed face swaps its
/// neighbors. `pxr/imaging/hd/smoothNormals.cpp`,
/// `_SmoothNormalsWorker::Compute`, over the adjacency of
/// `pxr/imaging/hd/vertexAdjacency.cpp`, `BuildAdjacencyTable`.
///
/// Under [`SmoothNormals::FaceOriented`], a corner whose cross product
/// points against its face's Newell normal (oriented the same way, so
/// flipped for a left-handed face) contributes its negation.
fn smooth_normals(
    counts: &[i32],
    indices: &[i32],
    orientation: Orientation,
    mode: SmoothNormals,
    points: &[[f32; 3]],
) -> Vec<[f32; 3]> {
    let flip = orientation == Orientation::LeftHanded;
    let mut normals = vec![[0.0_f32; 3]; points.len()];
    let mut first = 0;
    for &count in counts {
        let count = count as usize;
        let face = &indices[first..first + count];
        let front = match mode {
            SmoothNormals::Hydra => None,
            SmoothNormals::FaceOriented => {
                let n = newell(face, points);
                Some(if flip { n.map(|c| -c) } else { n })
            }
        };
        for j in 0..count {
            let mut previous = face[(j + count - 1) % count] as usize;
            let here = face[j] as usize;
            let mut next = face[(j + 1) % count] as usize;
            if flip {
                core::mem::swap(&mut previous, &mut next);
            }
            let p = points[here];
            let a = sub3(points[next], p);
            let b = sub3(points[previous], p);
            let mut c = [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ];
            if let Some(front) = front
                && c[0] * front[0] + c[1] * front[1] + c[2] * front[2] < 0.0
            {
                c = c.map(|v| -v);
            }
            let n = &mut normals[here];
            n[0] += c[0];
            n[1] += c[1];
            n[2] += c[2];
        }
        first += count;
    }
    for normal in &mut normals {
        *normal = normalized(*normal);
    }
    normals
}

/// The face's normal by Newell's method: twice its vector area, along the
/// normal its corners wind counter-clockwise about, for any simple
/// polygon, concave or not.
fn newell(face: &[i32], points: &[[f32; 3]]) -> [f32; 3] {
    let mut n = [0.0_f32; 3];
    for (j, &here) in face.iter().enumerate() {
        let p = points[here as usize];
        let q = points[face[(j + 1) % face.len()] as usize];
        n[0] += (p[1] - q[1]) * (p[2] + q[2]);
        n[1] += (p[2] - q[2]) * (p[0] + q[0]);
        n[2] += (p[0] - q[0]) * (p[1] + q[1]);
    }
    n
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[cfg(all(test, feature = "stage"))]
mod drift {
    use layerstack_schemas::subset::{MeshTopologyError, validate_mesh_topology};

    use super::validate_topology;
    use crate::input::MeshTopology;
    use crate::report::TopologyProblem;

    /// Each topology the schemas' validation and this crate's judge alike:
    /// the same verdict, the same reason.
    #[test]
    fn the_plans_validation_agrees_with_the_schemas() {
        let cases: [(&[i32], &[i32], usize); 8] = [
            (&[3], &[0, 1, 2], 3),
            (&[4, 3], &[0, 1, 2, 3, 1, 4, 2], 5),
            (&[], &[], 0),
            (&[3], &[0, 1], 3),
            (&[2, 2], &[0, 1, 2], 3),
            (&[-1, 4], &[0, 1, 2], 3),
            (&[3], &[0, 1, 3], 3),
            (&[3], &[0, -1, 2], 3),
        ];
        for (counts, indices, points) in cases {
            let topology = MeshTopology {
                face_vertex_counts: counts,
                face_vertex_indices: indices,
                ..MeshTopology::default()
            };
            let ours = validate_topology(&topology, points);
            let theirs = validate_mesh_topology(indices, counts, points);
            let same = match (&ours, &theirs) {
                (Ok(()), Ok(())) => true,
                (Err(TopologyProblem::FaceCounts), Err(MeshTopologyError::InvalidFaceCounts))
                | (
                    Err(TopologyProblem::SizeMismatch { .. }),
                    Err(MeshTopologyError::SizeMismatch),
                ) => true,
                (
                    Err(TopologyProblem::VertexIndex { index, .. }),
                    Err(MeshTopologyError::InvalidVertexIndex(theirs)),
                ) => index == theirs,
                _ => false,
            };
            assert!(
                same,
                "{counts:?} {indices:?} {points}: {ours:?} and {theirs:?}"
            );
        }
    }
}
