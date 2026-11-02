// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Cluster level-of-detail hierarchies for triangle meshes: a mesh split
//! into small clusters, then simplified group by group into coarser
//! clusters, level after level, as a DAG a renderer cuts through at a
//! screen-space error.
//!
//! [`ClusterSet::build`] takes a triangulated mesh ([`ClusterInput`]:
//! positions, indices, optional normals and texture coordinates as
//! simplification attributes, optional per-triangle subset ids) and a
//! [`ClusterConfig`], and returns one [`ClusterSet`]:
//!
//! - **One vertex stream.** Every level indexes the caller's vertices; the
//!   build makes no new vertices and does not copy them.
//! - **Clusters** ([`ClusterTable`]): each at most
//!   [`ClusterConfig::max_triangles`] triangles over at most
//!   [`ClusterConfig::max_vertices`] vertices, with its indices in
//!   [`ClusterSet::indices`], its bounding sphere, the group it belongs to
//!   and the group whose simplification made it ([`ClusterTable::refined`],
//!   [`NO_GROUP`] for the original triangles).
//! - **Groups** ([`GroupTable`]): clusters simplified together, with the
//!   bounds and error of their simplified replacement. The error never
//!   decreases from a group to the group its simplification lands in, and
//!   the bounds contain the bounds below them, so a projected error is
//!   monotonic up the DAG and every threshold cuts it exactly once.
//! - **A hierarchy** ([`NodeTable`]): one bounding tree per DAG level over
//!   the groups, so a cut visits only the groups that might be drawn.
//! - **A report** ([`ClusterReport`]): clusters, groups and triangles per
//!   level, groups that could not simplify further, and groups that needed
//!   the sloppy fallback.
//!
//! [`ClusterFile`] encodes a set and the vertices it indexes as one
//! versioned little-endian file of plain columns ([`mod@file`]), for sets built
//! ahead of time and loaded by a renderer that cannot build them.
//!
//! [`ClusterSet::select_cut`] picks the clusters to draw for a
//! [`CutCamera`] and a pixel error: a cluster is drawn when its own group's
//! simplified error projects above the threshold (its replacement would be
//! visibly coarser) and the group it was simplified from projects at or
//! below it (it is fine enough itself), as meshoptimizer's `clusterlod`
//! selects them. The selected clusters cover the surface once, with no
//! cracks: groups are simplified with the vertices they share with other
//! groups locked.
//!
//! # Subsets
//!
//! With [`ClusterInput::triangle_subsets`], a cluster holds the triangles of
//! one subset only ([`ClusterTable::subset`]), and the vertices where two
//! subsets meet are locked at every level, so each subset's triangles in a
//! cut keep their material and meet their neighbours without gaps.
//!
//! # Seams
//!
//! A vertex split for a face-varying normal or texture coordinate (two
//! vertices at one position with different attributes) is protected:
//! simplification may move other vertices across the seam but never this
//! one, so texture seams stay where they are.
//!
//! # Threads
//!
//! The build is single threaded and deterministic: the same input and
//! configuration give the same set, bit for bit. Within one level, each
//! group is simplified and split again from read-only state, then appended
//! in group order, so a host could simplify a level's groups on its own
//! threads and append the results in order without changing the output.
//!
//! The geometry primitives (cluster building, partitioning, simplification
//! and bounds) are meshoptimizer's, through the `meshopt` crate; the
//! orchestration is a port of meshoptimizer's `clusterlod.h`.
//!
//! ```
//! use layerstack_clusters::{ClusterConfig, ClusterInput, ClusterSet, CutCamera};
//!
//! // A 16 x 16 grid of quads, gently curved.
//! let n = 16_u32;
//! let mut positions = Vec::new();
//! for y in 0..=n {
//!     for x in 0..=n {
//!         let (u, v) = (x as f32 / n as f32, y as f32 / n as f32);
//!         positions.push([u, v, 0.1 * (u * 6.0).sin() * (v * 5.0).cos()]);
//!     }
//! }
//! let mut indices = Vec::new();
//! for y in 0..n {
//!     for x in 0..n {
//!         let i = y * (n + 1) + x;
//!         indices.extend([i, i + 1, i + n + 2, i, i + n + 2, i + n + 1]);
//!     }
//! }
//! let input = ClusterInput::new(&positions, &indices);
//! let set = ClusterSet::build(&input, &ClusterConfig::default()).expect("a valid mesh");
//! assert!(set.report().levels.len() > 1);
//!
//! // Far away, a few coarse clusters stand in for the whole grid.
//! let camera = CutCamera {
//!     position: [0.5, 0.5, 100.0],
//!     projection_y: 1.0,
//!     viewport_height: 1080.0,
//!     z_near: 0.01,
//! };
//! let mut cut = Vec::new();
//! set.select_cut(&camera, 1.0, &mut cut);
//! let triangles: u32 = cut.iter().map(|&c| set.clusters().triangle_count[c as usize]).sum();
//! assert!(triangles < 512);
//! ```

mod build;
mod cut;
pub mod file;
mod hierarchy;
#[cfg(test)]
mod tests;

use core::fmt;

pub use cut::CutCamera;
pub use file::{ClusterFile, DecodeError};

/// The group id of "no group": the [`ClusterTable::refined`] of a cluster
/// of the original triangles, and the [`NodeTable::group`] of an internal
/// hierarchy node.
pub const NO_GROUP: u32 = u32::MAX;

/// How a [`ClusterSet`] is built.
///
/// The defaults are meshoptimizer's `clodDefaultConfig(128)`, tuned for
/// rasterization: clusters of up to 128 triangles and 128 vertices, groups
/// of about 16 clusters, each level aiming at half the triangles; plus a
/// normal weight of 4 (see [`Self::normal_weight`]), a texture coordinate
/// weight of 0.5 and a hierarchy 8 wide.
#[derive(Clone, Debug, PartialEq)]
pub struct ClusterConfig {
    /// The most vertices a cluster references; 3 to 256.
    pub max_vertices: u32,
    /// The fewest triangles a cluster is split down to when its bounds are
    /// large (see [`Self::split_factor`]); 1 to [`Self::max_triangles`].
    pub min_triangles: u32,
    /// The most triangles in a cluster; 4 to 256.
    pub max_triangles: u32,
    /// How readily a cluster with large bounds is split before it is full
    /// (`meshopt_buildMeshletsFlex`'s `split_factor`); 0 never splits.
    pub split_factor: f32,
    /// The number of clusters a group aims at. Partitions may come out up
    /// to a third larger.
    pub partition_size: u32,
    /// The fraction of a group's triangles its simplification aims at.
    pub simplify_ratio: f32,
    /// A group whose simplification keeps more than this fraction of its
    /// triangles is stuck: it ends its branch of the DAG.
    pub simplify_threshold: f32,
    /// A group's error is `max(previous * this, own) + own *`
    /// [`Self::error_merge_additive`], where `previous` is the largest
    /// error of its clusters and `own` the error of its simplification.
    pub error_merge_previous: f32,
    /// See [`Self::error_merge_previous`].
    pub error_merge_additive: f32,
    /// Fall back to sloppy simplification for a group regular
    /// simplification cannot reduce enough. Sloppy simplification ignores
    /// topology: it keeps locked vertices, so the group still meets its
    /// neighbours without cracks, but it may leave holes or folds inside
    /// the group (its error is scaled by [`Self::sloppy_error_factor`] for
    /// that). [`LevelReport::sloppy_groups`] counts where it ran.
    pub sloppy_fallback: bool,
    /// The factor a sloppy simplification's error is multiplied by, for
    /// the appearance it loses.
    pub sloppy_error_factor: f32,
    /// When above zero, a group's error is capped at its longest edge
    /// (the larger of the shortest edge and a quarter of the longest, per
    /// triangle) times this, so subpixel triangles go even where the
    /// attribute error is high.
    pub edge_error_limit: f32,
    /// Let simplification collapse across attribute discontinuities other
    /// than the protected seams (meshoptimizer's permissive mode).
    pub permissive: bool,
    /// Clamp the attribute error to the scale of the position error.
    pub clamp_attribute_error: bool,
    /// Keep simplifying when one cluster is left, until simplification is
    /// stuck, so the coarsest level is as few triangles as the mesh
    /// simplifies to rather than up to a full cluster. Off by default, as
    /// upstream `clusterlod` stops at one cluster.
    ///
    /// It matters for small meshes drawn many times, which otherwise never
    /// drop below a cluster per instance: on a production element's 594 k
    /// instances of 300 to 700-triangle meshes, it cuts the triangles
    /// drawn at 1 px from 90% of the full count to 7%. But those instances
    /// are two or three pixels across there, and a pixel of error on each,
    /// mostly inward, takes a visible share of their coverage: the mean
    /// grey difference against the full render rises from 0.5 to 39 of
    /// 255. Meshes a few pixels across or larger are unaffected.
    pub simplify_root: bool,
    /// The level `meshopt_optimizeMeshletLevel` reorders each cluster's
    /// triangles at for locality, 0 to 9.
    pub optimize_level: u8,
    /// The weight of each normal component in the simplification error.
    ///
    /// A geometric error under a pixel does not bound the shading error:
    /// smooth normals interpolated over larger triangles shade
    /// differently. Rendered with interpolated normals and compared pixel
    /// by pixel against the full meshes, 1 px cuts of a production
    /// element's meshes differ by a mean of 5.1 grey levels (of 255) at
    /// the upstream demo's weight of 0.5, 2.1 at 2 and 1.2 at 4, drawing
    /// 7%, 13% and 17% of the triangles; 4 is the default.
    pub normal_weight: f32,
    /// The weight of each texture coordinate component in the
    /// simplification error.
    pub uv_weight: f32,
    /// The most children of a [`NodeTable`] node; at least 2.
    pub hierarchy_width: u32,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            max_vertices: 128,
            min_triangles: 128 / 3,
            max_triangles: 128,
            split_factor: 2.0,
            partition_size: 16,
            simplify_ratio: 0.5,
            simplify_threshold: 0.85,
            error_merge_previous: 1.0,
            error_merge_additive: 0.0,
            sloppy_fallback: true,
            sloppy_error_factor: 2.0,
            edge_error_limit: 0.0,
            permissive: true,
            clamp_attribute_error: true,
            simplify_root: false,
            optimize_level: 1,
            normal_weight: 4.0,
            uv_weight: 0.5,
            hierarchy_width: 8,
        }
    }
}

/// A triangulated mesh to build a [`ClusterSet`] from.
#[derive(Clone, Copy, Debug)]
pub struct ClusterInput<'a> {
    /// Each vertex's position.
    pub positions: &'a [[f32; 3]],
    /// Three vertex indices per triangle.
    pub indices: &'a [u32],
    /// Each vertex's normal, weighed by [`ClusterConfig::normal_weight`]
    /// in the simplification error; a seam where it differs at one
    /// position is protected.
    pub normals: Option<&'a [[f32; 3]]>,
    /// Each vertex's texture coordinate, weighed by
    /// [`ClusterConfig::uv_weight`]; a seam is protected.
    pub uvs: Option<&'a [[f32; 2]]>,
    /// Each triangle's subset (a material's draw range, say): clusters hold
    /// one subset's triangles, and the vertices where subsets meet are
    /// locked. `None` puts every triangle in subset 0.
    pub triangle_subsets: Option<&'a [u32]>,
}

impl<'a> ClusterInput<'a> {
    /// A mesh of positions and triangles alone.
    #[must_use]
    pub fn new(positions: &'a [[f32; 3]], indices: &'a [u32]) -> Self {
        Self {
            positions,
            indices,
            normals: None,
            uvs: None,
            triangle_subsets: None,
        }
    }
}

/// Why [`ClusterSet::build`] refused its input.
#[derive(Clone, Debug, PartialEq)]
pub enum BuildError {
    /// The index count is not a multiple of three.
    PartialTriangle {
        /// The number of indices.
        indices: usize,
    },
    /// An index names no vertex.
    IndexOutOfRange {
        /// The position of the index in [`ClusterInput::indices`].
        at: usize,
        /// The index.
        index: u32,
        /// The number of vertices.
        vertices: usize,
    },
    /// An attribute or subset array is not one per vertex or per triangle.
    LengthMismatch {
        /// The array: `normals`, `uvs` or `triangle_subsets`.
        array: &'static str,
        /// The length it has.
        len: usize,
        /// The length it should have.
        expected: usize,
    },
    /// A position, normal or texture coordinate is not finite.
    NotFinite {
        /// The array: `positions`, `normals` or `uvs`.
        array: &'static str,
        /// The vertex.
        vertex: usize,
    },
    /// The mesh has more vertices or indices than 32-bit indices reach.
    TooLarge,
    /// A [`ClusterConfig`] field is out of its range.
    Config {
        /// The field.
        field: &'static str,
        /// The range it must be in.
        expected: &'static str,
    },
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PartialTriangle { indices } => {
                write!(f, "{indices} indices is not a whole number of triangles")
            }
            Self::IndexOutOfRange {
                at,
                index,
                vertices,
            } => write!(
                f,
                "index {index} at {at} is out of range for {vertices} vertices"
            ),
            Self::LengthMismatch {
                array,
                len,
                expected,
            } => write!(f, "`{array}` has {len} entries, expected {expected}"),
            Self::NotFinite { array, vertex } => {
                write!(f, "`{array}` of vertex {vertex} is not finite")
            }
            Self::TooLarge => f.write_str("the mesh is too large for 32-bit indices"),
            Self::Config { field, expected } => {
                write!(f, "`ClusterConfig::{field}` must be {expected}")
            }
        }
    }
}

impl core::error::Error for BuildError {}

/// The clusters of a [`ClusterSet`], one entry per cluster in each column.
///
/// A group's clusters are contiguous ([`GroupTable::cluster_start`]), and
/// groups come in build order: deeper levels after shallower ones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClusterTable {
    /// The first of the cluster's indices in [`ClusterSet::indices`].
    pub index_start: Vec<u32>,
    /// The cluster's triangle count (three indices each).
    pub triangle_count: Vec<u32>,
    /// The number of distinct vertices the cluster references.
    pub vertex_count: Vec<u32>,
    /// The group the cluster belongs to.
    pub group: Vec<u32>,
    /// The group whose simplification produced the cluster, or [`NO_GROUP`]
    /// for a cluster of the original triangles.
    pub refined: Vec<u32>,
    /// The subset of the cluster's triangles ([`ClusterInput::triangle_subsets`]).
    pub subset: Vec<u32>,
    /// The bounding sphere of the cluster's triangles: center and radius,
    /// in the mesh's space, for culling.
    pub bounds: Vec<[f32; 4]>,
}

impl ClusterTable {
    /// The number of clusters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.group.len()
    }

    /// Whether there are no clusters.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.group.is_empty()
    }
}

/// The groups of a [`ClusterSet`], one entry per group in each column.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroupTable {
    /// The DAG level the group was made at: 0 for groups of original
    /// clusters.
    pub depth: Vec<u32>,
    /// The bounding sphere of the group's simplified replacement (center
    /// and radius, in the mesh's space). It contains the bounds of every
    /// group the group's clusters were simplified from.
    pub bounds: Vec<[f32; 4]>,
    /// The error of the group's simplified replacement, in the mesh's
    /// units: never less than the error of a group below it.
    /// `f32::INFINITY` for a group that is not simplified further (the
    /// root, or a stuck group).
    pub error: Vec<f32>,
    /// The group's first cluster in the [`ClusterTable`].
    pub cluster_start: Vec<u32>,
    /// The group's number of clusters.
    pub cluster_count: Vec<u32>,
}

impl GroupTable {
    /// The number of groups.
    #[must_use]
    pub fn len(&self) -> usize {
        self.depth.len()
    }

    /// Whether there are no groups.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.depth.is_empty()
    }
}

/// A bounding hierarchy over the groups of each DAG level, one entry per
/// node in each column (meshoptimizer's `clodBuildHierarchy`).
///
/// Nodes `0..levels` are the roots of the levels' trees, level by level. A
/// node's bounds contain its subtree's group bounds and its error is the
/// largest of their errors, so a cut descends into a node only while its
/// projected error is above the threshold.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeTable {
    /// The node's bounding sphere: center and radius.
    pub bounds: Vec<[f32; 4]>,
    /// The largest group error in the node's subtree.
    pub error: Vec<f32>,
    /// The group of a leaf, or [`NO_GROUP`] for an internal node.
    pub group: Vec<u32>,
    /// An internal node's first child.
    pub child_start: Vec<u32>,
    /// An internal node's number of children.
    pub child_count: Vec<u32>,
}

impl NodeTable {
    /// The number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.group.len()
    }

    /// Whether there are no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.group.is_empty()
    }
}

/// One DAG level of a [`ClusterReport`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LevelReport {
    /// The level's groups.
    pub groups: u32,
    /// The clusters in the level's groups.
    pub clusters: u32,
    /// The triangles of those clusters.
    pub triangles: u64,
    /// Groups whose simplification kept more than
    /// [`ClusterConfig::simplify_threshold`] of their triangles: their
    /// clusters are not simplified further.
    pub stuck_groups: u32,
    /// Groups simplified by the sloppy fallback.
    pub sloppy_groups: u32,
}

/// What [`ClusterSet::build`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClusterReport {
    /// The input's vertices.
    pub vertices: u32,
    /// The input's triangles.
    pub triangles: u64,
    /// The input's subsets (distinct [`ClusterInput::triangle_subsets`]).
    pub subsets: u32,
    /// Vertices locked where subsets meet, at every level.
    pub subset_border_vertices: u32,
    /// Vertices protected as attribute seams.
    pub seam_vertices: u32,
    /// Each DAG level, from the original clusters (level 0) up.
    pub levels: Vec<LevelReport>,
}

impl ClusterReport {
    /// The clusters of every level.
    #[must_use]
    pub fn clusters(&self) -> u64 {
        self.levels.iter().map(|l| u64::from(l.clusters)).sum()
    }

    /// The triangles of every level.
    #[must_use]
    pub fn total_triangles(&self) -> u64 {
        self.levels.iter().map(|l| l.triangles).sum()
    }
}

/// A mesh's cluster level-of-detail DAG: clusters, groups, a hierarchy over
/// the groups and a report, in plain columns.
///
/// Every index refers to the vertices of the [`ClusterInput`] the set was
/// built from, unchanged; the set holds no vertex data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClusterSet {
    vertex_count: u32,
    indices: Vec<u32>,
    clusters: ClusterTable,
    groups: GroupTable,
    nodes: NodeTable,
    levels: u32,
    report: ClusterReport,
}

impl ClusterSet {
    /// Builds the DAG of `input` under `config`.
    ///
    /// # Errors
    ///
    /// A [`BuildError`] for an index out of range, a partial triangle, an
    /// attribute or subset array of the wrong length, a value that is not
    /// finite, a mesh beyond 32-bit indices, or a configuration field out
    /// of range.
    pub fn build(input: &ClusterInput<'_>, config: &ClusterConfig) -> Result<Self, BuildError> {
        build::build(input, config)
    }

    /// The number of vertices the set's indices refer to: the input's.
    #[must_use]
    pub fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    /// Every cluster's indices, cluster after cluster
    /// ([`ClusterTable::index_start`]).
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// The indices of cluster `cluster`: three per triangle.
    ///
    /// # Panics
    ///
    /// When `cluster` is not a cluster of the set.
    #[must_use]
    pub fn cluster_indices(&self, cluster: usize) -> &[u32] {
        let start = self.clusters.index_start[cluster] as usize;
        let len = self.clusters.triangle_count[cluster] as usize * 3;
        &self.indices[start..start + len]
    }

    /// The clusters.
    #[must_use]
    pub fn clusters(&self) -> &ClusterTable {
        &self.clusters
    }

    /// The groups.
    #[must_use]
    pub fn groups(&self) -> &GroupTable {
        &self.groups
    }

    /// The bounding hierarchy over each level's groups.
    #[must_use]
    pub fn nodes(&self) -> &NodeTable {
        &self.nodes
    }

    /// The number of DAG levels (one more than the deepest group's depth).
    #[must_use]
    pub fn levels(&self) -> u32 {
        self.levels
    }

    /// What the build did.
    #[must_use]
    pub fn report(&self) -> &ClusterReport {
        &self.report
    }

    /// The bytes the set holds in its columns.
    #[must_use]
    pub fn byte_size(&self) -> usize {
        use core::mem::size_of_val;
        let c = &self.clusters;
        let g = &self.groups;
        let n = &self.nodes;
        size_of_val(self.indices.as_slice())
            + size_of_val(c.index_start.as_slice())
            + size_of_val(c.triangle_count.as_slice())
            + size_of_val(c.vertex_count.as_slice())
            + size_of_val(c.group.as_slice())
            + size_of_val(c.refined.as_slice())
            + size_of_val(c.subset.as_slice())
            + size_of_val(c.bounds.as_slice())
            + size_of_val(g.depth.as_slice())
            + size_of_val(g.bounds.as_slice())
            + size_of_val(g.error.as_slice())
            + size_of_val(g.cluster_start.as_slice())
            + size_of_val(g.cluster_count.as_slice())
            + size_of_val(n.bounds.as_slice())
            + size_of_val(n.error.as_slice())
            + size_of_val(n.group.as_slice())
            + size_of_val(n.child_start.as_slice())
            + size_of_val(n.child_count.as_slice())
    }
}
