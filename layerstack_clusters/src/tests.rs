// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! DAG invariants on small grids.

use std::collections::BTreeMap;

use crate::build::to_u32;
use crate::{BuildError, ClusterConfig, ClusterInput, ClusterSet, CutCamera, NO_GROUP};

/// A curved `n` x `n` grid of quads over the unit square, two triangles
/// each, wound counter-clockwise seen from +z.
struct Grid {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
    uvs: Vec<[f32; 2]>,
}

fn height(u: f32, v: f32) -> f32 {
    0.05 * (7.0 * u).sin() * (5.0 * v).cos() + 0.03 * (13.0 * (u + v)).sin()
}

fn grid(n: u32) -> Grid {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    for y in 0..=n {
        for x in 0..=n {
            let (u, v) = (x as f32 / n as f32, y as f32 / n as f32);
            positions.push([u, v, height(u, v)]);
            uvs.push([u, v]);
        }
    }
    let mut indices = Vec::new();
    for y in 0..n {
        for x in 0..n {
            let i = y * (n + 1) + x;
            indices.extend([i, i + 1, i + n + 2, i, i + n + 2, i + n + 1]);
        }
    }
    Grid {
        positions,
        indices,
        uvs,
    }
}

/// Each triangle's subset: 0 left of `x = 0.5`, 1 right of it.
fn halves(grid: &Grid) -> Vec<u32> {
    grid.indices
        .chunks(3)
        .map(|t| {
            let x: f32 = t
                .iter()
                .map(|&v| grid.positions[v as usize][0])
                .sum::<f32>()
                / 3.0;
            u32::from(x > 0.5)
        })
        .collect()
}

fn far_camera(distance: f32) -> CutCamera {
    CutCamera {
        position: [0.5, 0.5, distance],
        projection_y: 1.0 / (0.5_f32).tan(),
        viewport_height: 1080.0,
        z_near: 0.01,
    }
}

/// Cameras near and far, head-on and grazing.
fn cameras() -> Vec<CutCamera> {
    let mut cameras: Vec<CutCamera> = [0.2, 1.0, 5.0, 50.0, 1000.0].map(far_camera).to_vec();
    cameras.push(CutCamera {
        position: [-0.5, 0.1, 0.2],
        ..far_camera(1.0)
    });
    cameras
}

const THRESHOLDS: [f32; 7] = [0.0, 0.25, 1.0, 4.0, 16.0, 128.0, 1e9];

/// Each position's first vertex, so edges compare by position.
fn canonical(positions: &[[f32; 3]]) -> Vec<u32> {
    let mut first = BTreeMap::new();
    positions
        .iter()
        .enumerate()
        .map(|(v, p)| *first.entry(p.map(f32::to_bits)).or_insert(to_u32(v)))
        .collect()
}

/// A triangle with its smallest index first, winding kept.
fn rotated(t: &[u32]) -> [u32; 3] {
    let k = (0..3).min_by_key(|&k| t[k]).expect("three corners");
    [t[k], t[(k + 1) % 3], t[(k + 2) % 3]]
}

fn cut_triangles(set: &ClusterSet, cut: &[u32]) -> Vec<u32> {
    cut.iter()
        .flat_map(|&c| set.cluster_indices(c as usize).iter().copied())
        .collect()
}

fn on_perimeter(p: [f32; 3]) -> bool {
    p[0] == 0.0 || p[0] == 1.0 || p[1] == 0.0 || p[1] == 1.0
}

/// Whether a cut keeps the square's outline: when the clusters it draws
/// come from simplifications of error below a hundredth of the square.
/// Coarser simplification may round the corners off (and says so in its
/// error), so open edges leave the perimeter legitimately.
fn keeps_outline(set: &ClusterSet, cut: &[u32]) -> bool {
    cut.iter().all(|&c| {
        let refined = set.clusters().refined[c as usize];
        refined == NO_GROUP || set.groups().error[refined as usize] < 0.01
    })
}

/// Checks a cut is a crack-free cover of the unit square: no edge has more
/// than two triangles, and two only with opposite windings; with `outline`,
/// every open edge lies on the square's perimeter and the projected area is
/// the square's, so no edge inside is open.
fn assert_watertight(
    positions: &[[f32; 3]],
    canonical: &[u32],
    triangles: &[u32],
    outline: bool,
    context: &str,
) {
    let mut edges: BTreeMap<(u32, u32), (u32, i32)> = BTreeMap::new();
    let mut area = 0.0_f64;
    for t in triangles.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|k| positions[t[k] as usize]);
        area += 0.5 * f64::from((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]));
        for k in 0..3 {
            let (p, q) = (canonical[t[k] as usize], canonical[t[(k + 1) % 3] as usize]);
            let entry = edges.entry((p.min(q), p.max(q))).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += if p < q { 1 } else { -1 };
        }
    }
    for (&(p, q), &(count, direction)) in &edges {
        assert!(count <= 2, "{context}: edge {p}-{q} has {count} triangles");
        if count == 2 {
            assert_eq!(
                direction, 0,
                "{context}: edge {p}-{q} is shared with the same winding"
            );
        } else if outline {
            assert!(
                on_perimeter(positions[p as usize]) && on_perimeter(positions[q as usize]),
                "{context}: open edge {p}-{q} inside the square, a crack"
            );
        }
    }
    if outline {
        assert!(
            (area - 1.0).abs() < 1e-3,
            "{context}: projected area {area}, expected 1"
        );
    }
}

fn build(input: &ClusterInput<'_>) -> ClusterSet {
    ClusterSet::build(input, &ClusterConfig::default()).expect("a valid grid")
}

#[test]
fn leaves_cover_every_input_triangle_once() {
    let grid = grid(48);
    let set = build(&ClusterInput::new(&grid.positions, &grid.indices));
    let clusters = set.clusters();
    let mut leaves: Vec<[u32; 3]> = (0..clusters.len())
        .filter(|&c| clusters.refined[c] == NO_GROUP)
        .flat_map(|c| {
            set.cluster_indices(c)
                .chunks(3)
                .map(rotated)
                .collect::<Vec<_>>()
        })
        .collect();
    let mut input: Vec<[u32; 3]> = grid.indices.chunks(3).map(rotated).collect();
    leaves.sort_unstable();
    input.sort_unstable();
    assert_eq!(
        leaves, input,
        "the leaf clusters hold the input triangles exactly once"
    );
    assert_eq!(set.report().levels[0].triangles, input.len() as u64);
    assert!(
        clusters
            .triangle_count
            .iter()
            .all(|&t| (1..=128).contains(&t))
    );
    assert!(
        clusters
            .vertex_count
            .iter()
            .all(|&v| (3..=128).contains(&v))
    );
}

#[test]
fn errors_and_bounds_are_monotonic_up_the_dag() {
    let grid = grid(64);
    let set = build(&ClusterInput::new(&grid.positions, &grid.indices));
    let (clusters, groups) = (set.clusters(), set.groups());
    assert!(set.levels() >= 4, "a 8192-triangle grid has several levels");
    let mut checked = 0;
    for c in 0..clusters.len() {
        let (group, refined) = (clusters.group[c] as usize, clusters.refined[c]);
        if refined == NO_GROUP {
            continue;
        }
        let refined = refined as usize;
        assert!(
            groups.error[refined] <= groups.error[group],
            "group {group} error {} below its source {refined} error {}",
            groups.error[group],
            groups.error[refined],
        );
        assert!(
            groups.depth[refined] < groups.depth[group],
            "a source group is shallower"
        );
        let (outer, inner) = (groups.bounds[group], groups.bounds[refined]);
        let gap = ((outer[0] - inner[0]).powi(2)
            + (outer[1] - inner[1]).powi(2)
            + (outer[2] - inner[2]).powi(2))
        .sqrt()
            + inner[3]
            - outer[3];
        assert!(
            gap <= 1e-4 * outer[3],
            "group {group} bounds do not contain group {refined}'s"
        );
        checked += 1;
    }
    assert!(checked > 0, "some clusters are simplified");
    // Exactly one root, at the deepest level, with an infinite error.
    let last = set.levels() - 1;
    assert_eq!(groups.depth.iter().filter(|&&d| d == last).count(), 1);
    assert!(groups.error.last().is_some_and(|e| e.is_infinite()));
    for n in 0..set.nodes().len() {
        let nodes = set.nodes();
        if nodes.group[n] == NO_GROUP {
            let start = nodes.child_start[n] as usize;
            for child in start..start + nodes.child_count[n] as usize {
                assert!(
                    nodes.error[child] <= nodes.error[n],
                    "a node's error covers its children's"
                );
            }
        }
    }
}

#[test]
fn every_cut_is_watertight() {
    let grid = grid(64);
    let canonical = canonical(&grid.positions);
    let set = build(&ClusterInput::new(&grid.positions, &grid.indices));
    let (mut cut, mut linear) = (Vec::new(), Vec::new());
    let mut counts = Vec::new();
    let mut strict = 0;
    for camera in cameras() {
        for threshold in THRESHOLDS {
            set.select_cut(&camera, threshold, &mut cut);
            set.select_cut_linear(&camera, threshold, &mut linear);
            let context = format!("camera {:?} at {threshold} px", camera.position);
            cut.sort_unstable();
            assert_eq!(
                cut, linear,
                "{context}: the hierarchy finds what the scan finds"
            );
            let triangles = cut_triangles(&set, &cut);
            let outline = keeps_outline(&set, &cut);
            assert_watertight(&grid.positions, &canonical, &triangles, outline, &context);
            counts.push(triangles.len() / 3);
            strict += usize::from(outline && triangles.len() < grid.indices.len());
        }
    }
    assert!(
        counts.contains(&(grid.indices.len() / 3)),
        "a close cut is the full mesh"
    );
    assert!(
        counts.iter().any(|&c| c <= 128),
        "a far cut is a cluster or so"
    );
    assert!(
        strict >= 5,
        "simplified cuts are checked for cracks: {strict}"
    );
}

#[test]
fn far_cuts_draw_far_fewer_triangles() {
    let grid = grid(64);
    let set = build(&ClusterInput::new(&grid.positions, &grid.indices));
    let triangles = |distance| {
        let mut cut = Vec::new();
        set.select_cut(&far_camera(distance), 1.0, &mut cut);
        cut_triangles(&set, &cut).len() / 3
    };
    let (near, far) = (triangles(0.5), triangles(200.0));
    assert_eq!(
        near,
        grid.indices.len() / 3,
        "up close every triangle shows"
    );
    assert!(
        far * 20 < near,
        "far away a few clusters do: {far} of {near}"
    );
}

#[test]
fn subset_borders_stay_put() {
    let grid = grid(64);
    let subsets = halves(&grid);
    let canonical = canonical(&grid.positions);
    let input = ClusterInput {
        triangle_subsets: Some(&subsets),
        ..ClusterInput::new(&grid.positions, &grid.indices)
    };
    // The locked column leaves the top levels little to collapse, so the
    // sloppy fallback would run there; its output does not keep topology
    // and may open holes inside its group, which the crack check would
    // flag. Regular simplification alone shows the borders hold.
    let config = ClusterConfig {
        sloppy_fallback: false,
        ..ClusterConfig::default()
    };
    let set = ClusterSet::build(&input, &config).expect("a valid grid");
    assert_eq!(set.report().subsets, 2);
    assert!(set.report().levels.iter().all(|l| l.sloppy_groups == 0));
    assert_eq!(
        set.report().subset_border_vertices,
        65,
        "the column at x = 0.5"
    );
    let clusters = set.clusters();
    for c in 0..clusters.len() {
        let subset = clusters.subset[c];
        for &v in set.cluster_indices(c) {
            let x = grid.positions[v as usize][0];
            let inside = if subset == 0 { x <= 0.5 } else { x >= 0.5 };
            assert!(inside, "cluster {c} of subset {subset} reaches x = {x}");
        }
    }
    let border: Vec<u32> = (0..to_u32(grid.positions.len()))
        .filter(|&v| grid.positions[v as usize][0] == 0.5)
        .collect();
    let mut cut = Vec::new();
    for camera in cameras() {
        for threshold in THRESHOLDS {
            set.select_cut(&camera, threshold, &mut cut);
            let triangles = cut_triangles(&set, &cut);
            assert_watertight(
                &grid.positions,
                &canonical,
                &triangles,
                keeps_outline(&set, &cut),
                "subset cut",
            );
            for &v in &border {
                assert!(
                    triangles.contains(&v),
                    "border vertex {v} survives every cut"
                );
            }
        }
    }
}

#[test]
fn seams_are_protected_and_cuts_stay_closed_by_position() {
    // Split the grid's vertices along x = 0.5 into two copies with
    // different texture coordinates, as a face-varying seam does.
    let mut grid = grid(32);
    let n = 32;
    let mut copies = BTreeMap::new();
    for (t, triangle) in grid.indices.clone().chunks(3).enumerate() {
        let right = triangle
            .iter()
            .map(|&v| grid.positions[v as usize][0])
            .sum::<f32>()
            > 1.5;
        if !right {
            continue;
        }
        for (k, &v) in triangle.iter().enumerate() {
            if grid.positions[v as usize][0] == 0.5 {
                let copy = *copies.entry(v).or_insert_with(|| {
                    grid.positions.push(grid.positions[v as usize]);
                    grid.uvs.push([0.9, grid.uvs[v as usize][1]]);
                    to_u32(grid.positions.len() - 1)
                });
                grid.indices[t * 3 + k] = copy;
            }
        }
    }
    assert_eq!(copies.len(), n + 1);
    let input = ClusterInput {
        uvs: Some(&grid.uvs),
        ..ClusterInput::new(&grid.positions, &grid.indices)
    };
    let set = build(&input);
    assert_eq!(set.report().seam_vertices, to_u32(n) + 1);
    let canonical = canonical(&grid.positions);
    let mut cut = Vec::new();
    for camera in cameras() {
        for threshold in THRESHOLDS {
            set.select_cut(&camera, threshold, &mut cut);
            let triangles = cut_triangles(&set, &cut);
            assert_watertight(
                &grid.positions,
                &canonical,
                &triangles,
                keeps_outline(&set, &cut),
                "seam cut",
            );
        }
    }
}

#[test]
fn builds_are_deterministic() {
    let grid = grid(48);
    let subsets = halves(&grid);
    let input = ClusterInput {
        uvs: Some(&grid.uvs),
        triangle_subsets: Some(&subsets),
        ..ClusterInput::new(&grid.positions, &grid.indices)
    };
    assert_eq!(build(&input), build(&input));
}

#[test]
fn a_single_cluster_simplifies_on_only_when_asked() {
    let grid = grid(8);
    let input = ClusterInput::new(&grid.positions, &grid.indices);
    let set = build(&input);
    assert_eq!(set.levels(), 1, "128 triangles are one cluster");
    assert_eq!(set.clusters().len(), 1);
    assert!(set.groups().error[0].is_infinite());
    let mut cut = Vec::new();
    set.select_cut(&far_camera(1e6), 1.0, &mut cut);
    assert_eq!(cut, [0]);

    let config = ClusterConfig {
        simplify_root: true,
        ..ClusterConfig::default()
    };
    let set = ClusterSet::build(&input, &config).expect("a valid grid");
    assert!(set.levels() > 2, "the root cluster keeps simplifying");
    assert!(set.report().levels.iter().all(|l| l.clusters == 1));
    set.select_cut(&far_camera(1e6), 1.0, &mut cut);
    let root = set.clusters().len() - 1;
    assert_eq!(cut, [to_u32(root)]);
    assert!(
        set.clusters().triangle_count[root] < 16,
        "far away a handful of triangles stand in"
    );
    let canonical = canonical(&grid.positions);
    for distance in [0.3, 1.0, 3.0, 10.0, 30.0] {
        set.select_cut(&far_camera(distance), 1.0, &mut cut);
        let triangles = cut_triangles(&set, &cut);
        assert_watertight(
            &grid.positions,
            &canonical,
            &triangles,
            keeps_outline(&set, &cut),
            "root cut",
        );
    }

    let empty = build(&ClusterInput::new(&[], &[]));
    assert_eq!(empty.levels(), 0);
    empty.select_cut(&far_camera(1.0), 1.0, &mut cut);
    assert!(cut.is_empty());
}

#[test]
fn bad_input_is_refused() {
    let grid = grid(2);
    let config = ClusterConfig::default();
    let refuse = |input: ClusterInput<'_>| ClusterSet::build(&input, &config).expect_err("refused");
    assert_eq!(
        refuse(ClusterInput::new(&grid.positions, &grid.indices[..4])),
        BuildError::PartialTriangle { indices: 4 }
    );
    assert_eq!(
        refuse(ClusterInput::new(&grid.positions, &[0, 1, 9])),
        BuildError::IndexOutOfRange {
            at: 2,
            index: 9,
            vertices: 9
        }
    );
    assert!(matches!(
        refuse(ClusterInput {
            uvs: Some(&grid.uvs[..3]),
            ..ClusterInput::new(&grid.positions, &grid.indices)
        }),
        BuildError::LengthMismatch { array: "uvs", .. }
    ));
    let mut positions = grid.positions.clone();
    positions[4][1] = f32::NAN;
    assert_eq!(
        refuse(ClusterInput::new(&positions, &grid.indices)),
        BuildError::NotFinite {
            array: "positions",
            vertex: 4
        }
    );
    let bad = ClusterConfig {
        max_triangles: 512,
        ..ClusterConfig::default()
    };
    assert!(matches!(
        ClusterSet::build(&ClusterInput::new(&grid.positions, &grid.indices), &bad),
        Err(BuildError::Config {
            field: "max_triangles",
            ..
        })
    ));
}

/// The port against upstream `clodBuild` (compiled into the `meshopt`
/// crate natively): the same levels, and triangles per level within 1%, or
/// within 4 triangles at the coarsest levels, where floating-point differences
/// between platforms move a cluster's last few triangles.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn levels_match_upstream_clusterlod() {
    let grid = grid(96);
    let set = build(&ClusterInput::new(&grid.positions, &grid.indices));
    let vertices = meshopt::VertexDataAdapter::new(meshopt::typed_to_bytes(&grid.positions), 12, 0)
        .expect("whole float triples");
    let mesh = meshopt::ClodMesh::new(&grid.indices, &vertices);
    let mut upstream: Vec<u64> = Vec::new();
    let mut groups = 0;
    meshopt::clod_build(
        meshopt::clod_default_config(128),
        &mesh,
        |group, clusters| {
            let depth = usize::try_from(group.depth).expect("a depth");
            if upstream.len() <= depth {
                upstream.resize(depth + 1, 0);
            }
            upstream[depth] += clusters
                .iter()
                .map(|c| (c.indices.len() / 3) as u64)
                .sum::<u64>();
            groups += 1;
            groups - 1
        },
    );
    let ours: Vec<u64> = set.report().levels.iter().map(|l| l.triangles).collect();
    assert_eq!(
        ours.len(),
        upstream.len(),
        "levels: ours {ours:?}, upstream {upstream:?}"
    );
    for (level, (&a, &b)) in ours.iter().zip(&upstream).enumerate() {
        assert!(
            a.abs_diff(b) <= (b / 100).max(4),
            "level {level}: {a} triangles, upstream {b}"
        );
    }
}

#[test]
fn a_file_round_trips_and_refuses_damage() {
    use crate::file::{FILE_VERSION, HEADER_BYTES};
    use crate::{ClusterFile, DecodeError};

    let grid = grid(24);
    let halves = halves(&grid);
    let input = ClusterInput {
        uvs: Some(&grid.uvs),
        triangle_subsets: Some(&halves),
        ..ClusterInput::new(&grid.positions, &grid.indices)
    };
    let file = ClusterFile {
        set: build(&input),
        positions: grid.positions.clone(),
        normals: None,
        uvs: Some(grid.uvs.clone()),
    };
    let bytes = file.encode();
    assert_eq!(bytes.len(), file.encoded_len());
    assert_eq!(bytes.len() % 4, 0, "every column is 4-byte aligned");
    let decoded = ClusterFile::decode(&bytes).expect("its own encoding");
    assert_eq!(
        decoded, file,
        "the set, its report and its vertices come back"
    );

    assert_eq!(
        ClusterFile::decode(&bytes[..bytes.len() - 4]),
        Err(DecodeError::Truncated {
            expected: bytes.len(),
            len: bytes.len() - 4
        })
    );
    let mut other = bytes.clone();
    other[8..12].copy_from_slice(&(FILE_VERSION + 1).to_le_bytes());
    assert_eq!(
        ClusterFile::decode(&other),
        Err(DecodeError::Version {
            found: FILE_VERSION + 1
        })
    );
    // The first index, pointing past the vertices.
    let mut bad = bytes;
    let first_index = HEADER_BYTES + 4 * 5 * grid.positions.len();
    bad[first_index..first_index + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(
        ClusterFile::decode(&bad),
        Err(DecodeError::Inconsistent(_))
    ));
}
