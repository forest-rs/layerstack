// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The DAG build: a port of `clodBuild` from meshoptimizer's
//! `demo/clusterlod.h`, version 1.3
//! (<https://github.com/zeux/meshoptimizer/blob/v1.3/demo/clusterlod.h>),
//! over meshoptimizer's primitives through the `meshopt` crate.
//!
//! Changes from upstream: clusters never mix triangle subsets (each group
//! is simplified subset by subset, with subset borders locked); the
//! boundary locks are recomputed only for the vertices the level still
//! references; every cluster gets precise bounds; border dilation and
//! spatial partition sorting are left out.
//!
//! The upstream notice, which covers the ported code:
//!
//! Copyright (c) 2016-2026 Arseny Kapoulkine
//!
//! Permission is hereby granted, free of charge, to any person
//! obtaining a copy of this software and associated documentation
//! files (the "Software"), to deal in the Software without
//! restriction, including without limitation the rights to use,
//! copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the
//! Software is furnished to do so, subject to the following
//! conditions:
//!
//! The above copyright notice and this permission notice shall be
//! included in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//! EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES
//! OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
//! HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
//! WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//! FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
//! OTHER DEALINGS IN THE SOFTWARE.

use meshopt::{
    PositionDataAdapter, RadiusDataAdapter, SimplifyOptions, SimplifyVertex, VertexDataAdapter,
};

use crate::{
    BuildError, ClusterConfig, ClusterInput, ClusterReport, ClusterSet, ClusterTable, GroupTable,
    LevelReport, NO_GROUP, hierarchy,
};

/// A sphere and the error of the geometry it bounds (`clodBounds`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LodBounds {
    sphere: [f32; 4],
    error: f32,
}

/// A cluster being processed (`clod::Cluster`): its indices in the level's
/// index buffer.
#[derive(Clone, Copy, Debug)]
struct Work {
    index_start: usize,
    index_count: usize,
    vertex_count: u32,
    refined: u32,
    subset: u32,
    /// Precise bounds for an original cluster; the bounds of the group it
    /// was simplified from otherwise, which keeps the bounds of the group it
    /// joins conservative.
    bounds: LodBounds,
}

/// The mesh and the per-vertex state every level reads.
struct Mesh<'a> {
    positions: &'a [[f32; 3]],
    vertices: VertexDataAdapter<'a>,
    /// Interleaved simplification attributes: normal, then uv.
    attributes: Vec<f32>,
    /// Attribute stride in bytes.
    attribute_stride: usize,
    weights: Vec<f32>,
    /// Each vertex's canonical vertex: the first at its position.
    remap: Vec<u32>,
    /// Locks every level keeps: subset borders, protected seams.
    fixed_locks: Vec<SimplifyVertex>,
}

/// The output tables being filled.
#[derive(Default)]
struct Output {
    indices: Vec<u32>,
    clusters: ClusterTable,
    groups: GroupTable,
    levels: Vec<LevelReport>,
}

/// Checks `input` and `config`, then builds the set.
pub(crate) fn build(
    input: &ClusterInput<'_>,
    config: &ClusterConfig,
) -> Result<ClusterSet, BuildError> {
    check_config(config)?;
    check_input(input)?;
    let vertex_count = to_u32(input.positions.len());
    let mut report = ClusterReport {
        vertices: vertex_count,
        triangles: (input.indices.len() / 3) as u64,
        ..ClusterReport::default()
    };
    let mesh = prepare(input, config, &mut report);
    let output = build_levels(&mesh, input, config);
    let levels = to_u32(output.levels.len());
    let nodes = hierarchy::build(&output.groups, levels, config.hierarchy_width as usize);
    report.levels = output.levels;
    Ok(ClusterSet {
        vertex_count,
        indices: output.indices,
        clusters: output.clusters,
        groups: output.groups,
        nodes,
        levels,
        report,
    })
}

fn check_config(config: &ClusterConfig) -> Result<(), BuildError> {
    let fail = |field, expected| Err(BuildError::Config { field, expected });
    if !(3..=256).contains(&config.max_vertices) {
        return fail("max_vertices", "in 3..=256");
    }
    if !(4..=256).contains(&config.max_triangles) {
        return fail("max_triangles", "in 4..=256");
    }
    if config.min_triangles < 1 || config.min_triangles > config.max_triangles {
        return fail("min_triangles", "in 1..=max_triangles");
    }
    if config.partition_size < 1 {
        return fail("partition_size", "at least 1");
    }
    if !(config.simplify_ratio >= 0.0 && config.simplify_ratio < 1.0) {
        return fail("simplify_ratio", "in 0.0..1.0");
    }
    if !(config.simplify_threshold > 0.0 && config.simplify_threshold < 1.0) {
        return fail("simplify_threshold", "in 0.0..1.0, exclusive");
    }
    if config.split_factor.is_nan() || config.split_factor < 0.0 {
        return fail("split_factor", "non-negative");
    }
    let factors = [
        ("error_merge_previous", config.error_merge_previous),
        ("error_merge_additive", config.error_merge_additive),
        ("sloppy_error_factor", config.sloppy_error_factor),
        ("edge_error_limit", config.edge_error_limit),
        ("normal_weight", config.normal_weight),
        ("uv_weight", config.uv_weight),
    ];
    for (field, value) in factors {
        if !(value >= 0.0 && value.is_finite()) {
            return fail(field, "finite and non-negative");
        }
    }
    if config.optimize_level > 9 {
        return fail("optimize_level", "in 0..=9");
    }
    if config.hierarchy_width < 2 {
        return fail("hierarchy_width", "at least 2");
    }
    Ok(())
}

fn check_input(input: &ClusterInput<'_>) -> Result<(), BuildError> {
    let vertices = input.positions.len();
    if vertices >= u32::MAX as usize || input.indices.len() >= u32::MAX as usize {
        return Err(BuildError::TooLarge);
    }
    if !input.indices.len().is_multiple_of(3) {
        return Err(BuildError::PartialTriangle {
            indices: input.indices.len(),
        });
    }
    if let Some(at) = input.indices.iter().position(|&i| i as usize >= vertices) {
        return Err(BuildError::IndexOutOfRange {
            at,
            index: input.indices[at],
            vertices,
        });
    }
    finite("positions", input.positions)?;
    let mismatch = |array, len, expected| {
        if len == expected {
            Ok(())
        } else {
            Err(BuildError::LengthMismatch {
                array,
                len,
                expected,
            })
        }
    };
    if let Some(normals) = input.normals {
        mismatch("normals", normals.len(), vertices)?;
    }
    if let Some(uvs) = input.uvs {
        mismatch("uvs", uvs.len(), vertices)?;
    }
    if let Some(normals) = input.normals {
        finite("normals", normals)?;
    }
    if let Some(uvs) = input.uvs {
        finite("uvs", uvs)?;
    }
    if let Some(subsets) = input.triangle_subsets {
        mismatch("triangle_subsets", subsets.len(), input.indices.len() / 3)?;
    }
    Ok(())
}

/// Refuses the first vertex of `values` with a component that is not
/// finite.
fn finite<const N: usize>(array: &'static str, values: &[[f32; N]]) -> Result<(), BuildError> {
    match values.iter().position(|v| !v.iter().all(|x| x.is_finite())) {
        Some(vertex) => Err(BuildError::NotFinite { array, vertex }),
        None => Ok(()),
    }
}

/// Lays out the attributes, finds each vertex's canonical vertex, and
/// fixes the locks that hold at every level: seams and subset borders.
fn prepare<'a>(
    input: &ClusterInput<'a>,
    config: &ClusterConfig,
    report: &mut ClusterReport,
) -> Mesh<'a> {
    let positions = input.positions;
    let vertices = VertexDataAdapter::new(meshopt::typed_to_bytes(positions), 12, 0)
        .expect("positions are whole float triples");
    let remap = meshopt::generate_position_remap(&vertices);

    let mut stride = 0;
    let mut weights = Vec::new();
    if input.normals.is_some() {
        stride += 3;
        weights.extend([config.normal_weight; 3]);
    }
    if input.uvs.is_some() {
        stride += 2;
        weights.extend([config.uv_weight; 2]);
    }
    let mut attributes = Vec::with_capacity(stride * positions.len());
    if stride > 0 {
        for v in 0..positions.len() {
            if let Some(normals) = input.normals {
                attributes.extend(normals[v]);
            }
            if let Some(uvs) = input.uvs {
                attributes.extend(uvs[v]);
            }
        }
    }

    // A vertex whose attributes differ from its position's canonical
    // vertex lies on a seam; permissive simplification keeps it.
    let mut fixed_locks = vec![SimplifyVertex::None; positions.len()];
    if stride > 0 {
        for (v, lock) in fixed_locks.iter_mut().enumerate() {
            let r = remap[v] as usize;
            if r != v
                && attributes[v * stride..(v + 1) * stride]
                    != attributes[r * stride..(r + 1) * stride]
            {
                *lock |= SimplifyVertex::Protect;
                report.seam_vertices += 1;
            }
        }
    }

    // A position used by triangles of two subsets is locked, every wedge
    // of it.
    if let Some(subsets) = input.triangle_subsets {
        const UNSEEN: u32 = u32::MAX;
        const MIXED: u32 = u32::MAX - 1;
        let mut owner = vec![UNSEEN; positions.len()];
        for (triangle, &subset) in subsets.iter().enumerate() {
            for &v in &input.indices[triangle * 3..triangle * 3 + 3] {
                let r = remap[v as usize] as usize;
                owner[r] = match owner[r] {
                    UNSEEN => subset,
                    same if same == subset => same,
                    _ => MIXED,
                };
            }
        }
        for (v, lock) in fixed_locks.iter_mut().enumerate() {
            if owner[remap[v] as usize] == MIXED {
                *lock |= SimplifyVertex::Lock;
                report.subset_border_vertices += 1;
            }
        }
        let mut distinct: Vec<u32> = subsets.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        report.subsets = to_u32(distinct.len());
    } else {
        report.subsets = u32::from(!input.indices.is_empty());
    }

    Mesh {
        positions,
        vertices,
        attribute_stride: stride * 4,
        attributes,
        weights,
        remap,
        fixed_locks,
    }
}

/// The level loop of `clodBuild`.
fn build_levels(mesh: &Mesh<'_>, input: &ClusterInput<'_>, config: &ClusterConfig) -> Output {
    let mut output = Output::default();
    let mut clusters: Vec<Work> = Vec::new();
    let mut cluster_indices: Vec<u32> = Vec::new();

    // The original clusters, subset by subset.
    match input.triangle_subsets {
        None => clusterize(
            &mut clusters,
            &mut cluster_indices,
            config,
            mesh,
            input.indices,
            NO_GROUP,
            0,
        ),
        Some(subsets) => {
            for (subset, indices) in split_by_subset(input.indices, subsets) {
                clusterize(
                    &mut clusters,
                    &mut cluster_indices,
                    config,
                    mesh,
                    &indices,
                    NO_GROUP,
                    subset,
                );
            }
        }
    }
    for cluster in &mut clusters {
        let indices =
            &cluster_indices[cluster.index_start..cluster.index_start + cluster.index_count];
        cluster.bounds = LodBounds {
            sphere: cluster_sphere(mesh, indices),
            error: 0.0,
        };
    }

    let mut locks = vec![SimplifyVertex::None; mesh.positions.len()];
    let mut seen = vec![0_u8; mesh.positions.len()];
    let mut pending: Vec<Work> = Vec::new();
    let mut pending_indices: Vec<u32> = Vec::new();
    let mut scratch = Scratch::default();
    let mut depth = 0_u32;

    // Merge and simplify until one cluster is left, or with
    // `simplify_root` until nothing simplifies any more.
    while clusters.len() > 1 || (config.simplify_root && !clusters.is_empty()) {
        let group_offsets = partition(&mut clusters, &cluster_indices, mesh, config);
        lock_boundary(
            &mut locks,
            &mut seen,
            &clusters,
            &group_offsets,
            &cluster_indices,
            mesh,
        );
        pending.clear();
        pending_indices.clear();
        let mut level = LevelReport::default();

        for range in group_offsets.windows(2) {
            let group = &clusters[range[0] as usize..range[1] as usize];
            // `simplify_group` reads only state shared by the level: groups
            // of one level could be simplified on separate threads, then
            // output and split again in group order here.
            let outcome =
                simplify_group(group, &cluster_indices, &locks, mesh, config, &mut scratch);
            level.sloppy_groups += u32::from(outcome.sloppy);
            match outcome.parts {
                None => {
                    level.stuck_groups += 1;
                    let mut bounds = outcome.bounds;
                    bounds.error = f32::INFINITY;
                    output_group(
                        &mut output,
                        &mut level,
                        group,
                        &cluster_indices,
                        bounds,
                        depth,
                        mesh,
                    );
                }
                Some(parts) => {
                    let refined = output_group(
                        &mut output,
                        &mut level,
                        group,
                        &cluster_indices,
                        outcome.bounds,
                        depth,
                        mesh,
                    );
                    for part in parts {
                        let start = pending.len();
                        clusterize(
                            &mut pending,
                            &mut pending_indices,
                            config,
                            mesh,
                            &part.indices,
                            refined,
                            part.subset,
                        );
                        for cluster in &mut pending[start..] {
                            cluster.bounds = outcome.bounds;
                        }
                    }
                }
            }
        }
        output.levels.push(level);
        core::mem::swap(&mut clusters, &mut pending);
        core::mem::swap(&mut cluster_indices, &mut pending_indices);
        depth += 1;
    }

    if let [cluster] = clusters.as_slice() {
        let mut level = LevelReport::default();
        let mut bounds = cluster.bounds;
        bounds.error = f32::INFINITY;
        output_group(
            &mut output,
            &mut level,
            &clusters,
            &cluster_indices,
            bounds,
            depth,
            mesh,
        );
        output.levels.push(level);
    }
    output
}

/// The input's triangles grouped by subset, subsets in ascending order,
/// triangles in input order within each.
fn split_by_subset(indices: &[u32], subsets: &[u32]) -> Vec<(u32, Vec<u32>)> {
    let mut order: Vec<u32> = (0..to_u32(subsets.len())).collect();
    order.sort_by_key(|&t| subsets[t as usize]);
    let mut parts: Vec<(u32, Vec<u32>)> = Vec::new();
    for t in order {
        let subset = subsets[t as usize];
        if parts.last().is_none_or(|(s, _)| *s != subset) {
            parts.push((subset, Vec::new()));
        }
        let t = t as usize;
        parts
            .last_mut()
            .expect("pushed above")
            .1
            .extend_from_slice(&indices[t * 3..t * 3 + 3]);
    }
    parts
}

/// Splits `indices` into clusters appended to `clusters` and
/// `cluster_indices` (`clod::clusterize`).
fn clusterize(
    clusters: &mut Vec<Work>,
    cluster_indices: &mut Vec<u32>,
    config: &ClusterConfig,
    mesh: &Mesh<'_>,
    indices: &[u32],
    refined: u32,
    subset: u32,
) {
    if indices.is_empty() {
        return;
    }
    let meshopt::Meshlets {
        meshlets,
        mut vertices,
        mut triangles,
    } = meshopt::build_meshlets_flex(
        indices,
        &mesh.vertices,
        config.max_vertices as usize,
        config.min_triangles as usize,
        config.max_triangles as usize,
        0.0,
        config.split_factor,
    );
    cluster_indices.reserve(indices.len());
    for meshlet in &meshlets {
        let vertex_range =
            meshlet.vertex_offset as usize..(meshlet.vertex_offset + meshlet.vertex_count) as usize;
        let triangle_range = meshlet.triangle_offset as usize
            ..(meshlet.triangle_offset + meshlet.triangle_count * 3) as usize;
        if config.optimize_level > 0 {
            meshopt::optimize_meshlet_level(
                &mut vertices[vertex_range.clone()],
                &mut triangles[triangle_range.clone()],
                config.optimize_level,
            );
        }
        let local = &vertices[vertex_range];
        let index_start = cluster_indices.len();
        cluster_indices.extend(triangles[triangle_range].iter().map(|&t| local[t as usize]));
        clusters.push(Work {
            index_start,
            index_count: meshlet.triangle_count as usize * 3,
            vertex_count: meshlet.vertex_count,
            refined,
            subset,
            bounds: LodBounds::default(),
        });
    }
}

/// Partitions `clusters` into groups, reordering them so each group is
/// contiguous; returns the groups' offsets (`clod::partition`).
fn partition(
    clusters: &mut Vec<Work>,
    cluster_indices: &[u32],
    mesh: &Mesh<'_>,
    config: &ClusterConfig,
) -> Vec<u32> {
    let count = to_u32(clusters.len());
    if clusters.len() <= config.partition_size as usize {
        return vec![0, count];
    }
    let mut counts = Vec::with_capacity(clusters.len());
    let mut remapped = Vec::with_capacity(cluster_indices.len());
    for cluster in clusters.iter() {
        counts.push(to_u32(cluster.index_count));
        remapped.extend(
            cluster_indices[cluster.index_start..cluster.index_start + cluster.index_count]
                .iter()
                .map(|&v| mesh.remap[v as usize]),
        );
    }
    let mut part = vec![0_u32; clusters.len()];
    let partitions = meshopt::partition_clusters_with_positions(
        &mut part,
        &remapped,
        &counts,
        &mesh.vertices,
        config.partition_size as usize,
    );
    drop(remapped);

    // Counting sort into contiguous groups, stable within each.
    let mut offsets = vec![0_u32; partitions + 1];
    for &p in &part {
        offsets[p as usize + 1] += 1;
    }
    for i in 1..offsets.len() {
        offsets[i] += offsets[i - 1];
    }
    let mut write = offsets.clone();
    let mut sorted = vec![clusters[0]; clusters.len()];
    for (cluster, &p) in clusters.iter().zip(&part) {
        sorted[write[p as usize] as usize] = *cluster;
        write[p as usize] += 1;
    }
    *clusters = sorted;
    offsets
}

const SEEN: u8 = 0x80;
const LOCKED: u8 = 0x01;

/// Locks every vertex two groups share, by position, so simplifying one
/// group leaves no crack against another (`clod::lockBoundary`). Touches
/// only the vertices the level references.
fn lock_boundary(
    locks: &mut [SimplifyVertex],
    seen: &mut [u8],
    clusters: &[Work],
    group_offsets: &[u32],
    cluster_indices: &[u32],
    mesh: &Mesh<'_>,
) {
    for range in group_offsets.windows(2) {
        let group = &clusters[range[0] as usize..range[1] as usize];
        // Locked if a prior group saw it.
        for &v in indices_of(group, cluster_indices) {
            let r = mesh.remap[v as usize] as usize;
            seen[r] |= seen[r] >> 7;
        }
        for &v in indices_of(group, cluster_indices) {
            seen[mesh.remap[v as usize] as usize] |= SEEN;
        }
    }
    for &v in indices_of(clusters, cluster_indices) {
        let r = mesh.remap[v as usize] as usize;
        let mut lock = mesh.fixed_locks[v as usize];
        if seen[r] & LOCKED != 0 {
            lock |= SimplifyVertex::Lock;
        }
        locks[v as usize] = lock;
    }
    for &v in indices_of(clusters, cluster_indices) {
        seen[mesh.remap[v as usize] as usize] = 0;
    }
}

/// The indices of `clusters`, cluster after cluster.
fn indices_of<'a>(
    clusters: &'a [Work],
    cluster_indices: &'a [u32],
) -> impl Iterator<Item = &'a u32> + 'a {
    clusters
        .iter()
        .flat_map(|c| &cluster_indices[c.index_start..c.index_start + c.index_count])
}

/// Buffers reused from group to group.
#[derive(Default)]
struct Scratch {
    merged: Vec<u32>,
    spheres: Vec<[f32; 4]>,
}

/// One subset's share of a simplified group.
struct Part {
    subset: u32,
    indices: Vec<u32>,
}

/// What simplifying a group came to.
struct Outcome {
    /// The group's merged bounds, with the simplified error.
    bounds: LodBounds,
    /// The simplified triangles, subset by subset; `None` when the group is
    /// stuck.
    parts: Option<Vec<Part>>,
    /// Whether a part needed the sloppy fallback.
    sloppy: bool,
}

/// Simplifies one group, subset by subset, reading only shared state.
fn simplify_group(
    group: &[Work],
    cluster_indices: &[u32],
    locks: &[SimplifyVertex],
    mesh: &Mesh<'_>,
    config: &ClusterConfig,
    scratch: &mut Scratch,
) -> Outcome {
    // Error and bounds monotonicity: merge the clusters' bounds rather than
    // bounding the merged or simplified triangles.
    let mut bounds = merge_bounds(group.iter().map(|c| c.bounds), &mut scratch.spheres);

    let mut subsets: Vec<u32> = group.iter().map(|c| c.subset).collect();
    subsets.sort_unstable();
    subsets.dedup();

    let mut parts = Vec::with_capacity(subsets.len());
    let mut merged_total = 0;
    let mut simplified_total = 0;
    let mut error = 0.0_f32;
    let mut sloppy = false;
    for subset in subsets {
        scratch.merged.clear();
        for cluster in group.iter().filter(|c| c.subset == subset) {
            scratch.merged.extend_from_slice(
                &cluster_indices[cluster.index_start..cluster.index_start + cluster.index_count],
            );
        }
        let target = target_indices(scratch.merged.len(), config.simplify_ratio);
        let (indices, part_error, part_sloppy) =
            simplify(mesh, config, &scratch.merged, locks, target);
        merged_total += scratch.merged.len();
        simplified_total += indices.len();
        error = error.max(part_error);
        sloppy |= part_sloppy;
        parts.push(Part { subset, indices });
    }

    if simplified_total as f32 > merged_total as f32 * config.simplify_threshold {
        return Outcome {
            bounds,
            parts: None,
            sloppy,
        };
    }
    bounds.error = (bounds.error * config.error_merge_previous).max(error)
        + error * config.error_merge_additive;
    Outcome {
        bounds,
        parts: Some(parts),
        sloppy,
    }
}

/// Simplifies one subset of a group toward `target` indices; returns the
/// indices, their absolute error, and whether the sloppy fallback ran
/// (`clod::simplify`).
fn simplify(
    mesh: &Mesh<'_>,
    config: &ClusterConfig,
    indices: &[u32],
    locks: &[SimplifyVertex],
    target: usize,
) -> (Vec<u32>, f32, bool) {
    let mut options = SimplifyOptions::Sparse | SimplifyOptions::ErrorAbsolute;
    if config.clamp_attribute_error {
        options |= SimplifyOptions::ErrorClamped;
    }
    if config.permissive {
        options |= SimplifyOptions::Permissive;
    }
    let mut error = 0.0_f32;
    let mut lod = meshopt::simplify_with_attributes_and_locks(
        indices,
        &mesh.vertices,
        &mesh.attributes,
        &mesh.weights,
        mesh.attribute_stride,
        locks,
        target,
        f32::MAX,
        options,
        Some(&mut error),
    );
    let mut sloppy = false;
    if lod.len() > target && config.sloppy_fallback {
        let (sloppy_lod, sloppy_error) = simplify_sloppy(mesh, indices, locks, target);
        lod = sloppy_lod;
        error = sloppy_error * config.sloppy_error_factor;
        sloppy = true;
    }
    // A degenerate result keeps the first triangle, so no cluster is empty.
    if lod.is_empty() && !indices.is_empty() {
        lod.extend_from_slice(&indices[..3]);
    }
    if config.edge_error_limit > 0.0 {
        let mut max_edge_sq = 0.0_f32;
        for triangle in indices.as_chunks::<3>().0 {
            let [a, b, c] = triangle.map(|v| mesh.positions[v as usize]);
            let edges = [distance_sq(a, b), distance_sq(a, c), distance_sq(b, c)];
            let emax = edges[0].max(edges[1]).max(edges[2]);
            let emin = edges[0].min(edges[1]).min(edges[2]);
            // The shortest edge, so sub-pixel slivers go; a quarter of the
            // longest, so long thin triangles (wires) stay.
            max_edge_sq = max_edge_sq.max(emin.max(emax / 4.0));
        }
        error = error.min(max_edge_sq.sqrt() * config.edge_error_limit);
    }
    (lod, error, sloppy)
}

/// Sloppy simplification of a group subset, deindexed so it does not walk
/// the whole vertex buffer (`clod::simplifyFallback`).
fn simplify_sloppy(
    mesh: &Mesh<'_>,
    indices: &[u32],
    locks: &[SimplifyVertex],
    target: usize,
) -> (Vec<u32>, f32) {
    let positions: Vec<[f32; 3]> = indices
        .iter()
        .map(|&v| mesh.positions[v as usize])
        .collect();
    let subset_locks: Vec<SimplifyVertex> = indices.iter().map(|&v| locks[v as usize]).collect();
    let local: Vec<u32> = (0..to_u32(indices.len())).collect();
    let vertices = VertexDataAdapter::new(meshopt::typed_to_bytes(&positions), 12, 0)
        .expect("positions are whole float triples");
    let mut error = 0.0_f32;
    let lod = meshopt::simplify_sloppy_with_locks(
        &local,
        &vertices,
        &subset_locks,
        target,
        f32::MAX,
        Some(&mut error),
    );
    // The error is relative to the subset's extent; make it absolute.
    error *= meshopt::simplify_scale(&vertices);
    (lod.iter().map(|&i| indices[i as usize]).collect(), error)
}

/// Appends a group and its clusters to the output; returns its id
/// (`clod::outputGroup` and the caller's callback).
fn output_group(
    output: &mut Output,
    level: &mut LevelReport,
    group: &[Work],
    cluster_indices: &[u32],
    bounds: LodBounds,
    depth: u32,
    mesh: &Mesh<'_>,
) -> u32 {
    let id = to_u32(output.groups.len());
    let groups = &mut output.groups;
    groups.depth.push(depth);
    groups.bounds.push(bounds.sphere);
    groups.error.push(bounds.error);
    groups.cluster_start.push(to_u32(output.clusters.len()));
    groups.cluster_count.push(to_u32(group.len()));
    level.groups += 1;
    for cluster in group {
        let indices =
            &cluster_indices[cluster.index_start..cluster.index_start + cluster.index_count];
        let table = &mut output.clusters;
        table.index_start.push(to_u32(output.indices.len()));
        table.triangle_count.push(to_u32(cluster.index_count / 3));
        table.vertex_count.push(cluster.vertex_count);
        table.group.push(id);
        table.refined.push(cluster.refined);
        table.subset.push(cluster.subset);
        table.bounds.push(cluster_sphere(mesh, indices));
        output.indices.extend_from_slice(indices);
        level.clusters += 1;
        level.triangles += (cluster.index_count / 3) as u64;
    }
    id
}

/// The bounding sphere of one cluster's triangles.
fn cluster_sphere(mesh: &Mesh<'_>, indices: &[u32]) -> [f32; 4] {
    let bounds = meshopt::compute_cluster_bounds(indices, &mesh.vertices);
    [
        bounds.center[0],
        bounds.center[1],
        bounds.center[2],
        bounds.radius,
    ]
}

/// A sphere around `bounds`' spheres, with the largest of their errors
/// (`clod::boundsMerge`).
fn merge_bounds(bounds: impl Iterator<Item = LodBounds>, spheres: &mut Vec<[f32; 4]>) -> LodBounds {
    spheres.clear();
    let mut error = 0.0_f32;
    for b in bounds {
        spheres.push(b.sphere);
        error = error.max(b.error);
    }
    LodBounds {
        sphere: merge_spheres(spheres),
        error,
    }
}

/// A sphere containing every sphere of `spheres` (`meshopt_computeSphereBounds`).
pub(crate) fn merge_spheres(spheres: &[[f32; 4]]) -> [f32; 4] {
    let data = meshopt::typed_to_bytes(spheres);
    let merged = meshopt::compute_sphere_bounds(
        PositionDataAdapter {
            data,
            position_count: spheres.len(),
            position_stride: 16,
            position_offset: 0,
        },
        Some(RadiusDataAdapter {
            data,
            radius_count: spheres.len(),
            radius_stride: 16,
            radius_offset: 12,
        }),
    );
    [
        merged.center[0],
        merged.center[1],
        merged.center[2],
        merged.radius,
    ]
}

/// The index count a group subset of `indices` indices aims at: whole
/// triangles, `ratio` of them rounded down.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a fraction below one of a count that fits"
)]
fn target_indices(indices: usize, ratio: f32) -> usize {
    ((indices / 3) as f32 * ratio) as usize * 3
}

fn distance_sq(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// `n` as a `u32`; the input check bounds every count the build makes.
pub(crate) fn to_u32(n: usize) -> u32 {
    u32::try_from(n).expect("counts are checked to fit 32 bits")
}
