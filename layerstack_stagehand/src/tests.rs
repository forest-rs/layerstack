// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plans against hand-computed expectations: Hydra's fan order, holes and
//! left-handed winding, concave coverage, vertex splits, normals, subsets,
//! refills and implicit shapes.

use alloc::vec;
use alloc::vec::Vec;

use crate::{
    ChannelState, DrawRange, DropReason, FaceNoteKind, FaceSubsets, Interpolation, MeshBuffers,
    MeshInput, MeshOptions, MeshPlan, MeshTopology, NormalsSource, Orientation, PlanMismatch,
    Primvar, Shape, SmoothNormals, Tessellation, TopologyProblem, Triangulation, tessellate,
};

fn mesh<'a>(counts: &'a [i32], indices: &'a [i32], points: &'a [[f32; 3]]) -> MeshInput<'a> {
    MeshInput {
        topology: MeshTopology {
            face_vertex_counts: counts,
            face_vertex_indices: indices,
            ..MeshTopology::default()
        },
        points,
        ..MeshInput::default()
    }
}

fn plan(input: &MeshInput<'_>, triangulation: Triangulation) -> (MeshPlan, MeshBuffers) {
    let options = MeshOptions {
        triangulation,
        ..MeshOptions::default()
    };
    let plan = MeshPlan::new(input, &options);
    let buffers = plan.buffers(input).unwrap();
    (plan, buffers)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(a: [f64; 3]) -> f64 {
    libm::sqrt(dot(a, a))
}

fn wide(p: [f32; 3]) -> [f64; 3] {
    p.map(f64::from)
}

/// Twice the vector area of a polygon (its Newell normal).
fn vector_area(polygon: &[[f64; 3]]) -> [f64; 3] {
    let mut n = [0.0; 3];
    for i in 0..polygon.len() {
        let c = cross(polygon[i], polygon[(i + 1) % polygon.len()]);
        n = [n[0] + c[0], n[1] + c[1], n[2] + c[2]];
    }
    n
}

/// Points shared by the fan tests: a square 0-3, a triangle 1-4-2, a convex
/// pentagon 4-8, and 9, 10 for a hole.
const FAN_POINTS: [[f32; 3]; 11] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
    [2.0, 0.0, 0.0],
    [3.0, 0.0, 0.0],
    [3.5, 1.0, 0.0],
    [3.0, 2.0, 0.0],
    [2.0, 2.0, 0.0],
    [0.0, 2.0, 0.0],
    [1.0, 2.0, 0.0],
];
/// A quad, a triangle, a pentagon, a two-vertex face and a quad listed as
/// a hole.
const FAN_COUNTS: [i32; 5] = [4, 3, 5, 2, 4];
const FAN_INDICES: [i32; 18] = [0, 1, 2, 3, 1, 4, 2, 4, 5, 6, 7, 8, 0, 1, 3, 2, 10, 9];

#[test]
fn right_handed_faces_fan_in_hydras_order_skipping_holes_and_degenerate_faces() {
    let mut input = mesh(&FAN_COUNTS, &FAN_INDICES, &FAN_POINTS);
    input.topology.hole_indices = &[4];
    for triangulation in [Triangulation::HydraFan, Triangulation::Concave] {
        let (plan, buffers) = plan(&input, triangulation);
        assert_eq!(
            plan.triangle_points(),
            vec![
                [0, 1, 2],
                [0, 2, 3],
                [1, 4, 2],
                [4, 5, 6],
                [4, 6, 7],
                [4, 7, 8]
            ]
        );
        assert_eq!(buffers.triangle_faces, [0, 0, 1, 2, 2, 2]);
        let report = plan.report();
        assert_eq!(
            (report.faces, report.drawn_faces, report.triangles),
            (5, 3, 6)
        );
        assert_eq!((report.hole_faces, report.degenerate_faces), (1, 1));
        assert_eq!(report.concave_faces, 0);
        assert_eq!(
            report
                .face_notes
                .iter()
                .map(|note| (note.face, note.kind))
                .collect::<Vec<_>>(),
            [(3, FaceNoteKind::Degenerate), (4, FaceNoteKind::Hole)]
        );
        // One vertex per point drawn: no primvar splits them.
        assert_eq!(report.vertices, 9);
    }
}

#[test]
fn left_handed_faces_flip_and_rotate_as_hydra_does() {
    let mut input = mesh(&FAN_COUNTS, &FAN_INDICES, &FAN_POINTS);
    input.topology.hole_indices = &[4];
    input.topology.orientation = Orientation::LeftHanded;
    let (plan, buffers) = plan(&input, Triangulation::HydraFan);
    // `HdMeshUtil::ComputeTriangleIndices`: (0, j + 2, j + 1), the first
    // triangle of a face of more than three rotated to (2, 1, 0), the last
    // to (nv - 2, 0, nv - 1).
    assert_eq!(
        plan.triangle_points(),
        vec![
            [2, 1, 0],
            [2, 0, 3],
            [1, 2, 4],
            [6, 5, 4],
            [4, 7, 6],
            [7, 4, 8]
        ]
    );
    // Every triangle now faces -Z, and computed normals with them.
    for t in buffers.indices.as_chunks::<3>().0.iter() {
        let [a, b, c] = [0, 1, 2].map(|k| wide(buffers.positions[t[k] as usize]));
        assert!(cross(sub(b, a), sub(c, a))[2] < 0.0);
    }
    assert!(buffers.normals.iter().all(|n| n[2] < 0.0));
}

#[test]
fn unsorted_hole_indices_skip_only_what_hydras_walk_meets() {
    let counts = [3, 3, 3, 3];
    let indices = [0, 1, 2, 0, 2, 3, 0, 1, 3, 1, 2, 3];
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let mut input = mesh(&counts, &indices, &points);
    input.topology.hole_indices = &[3, 1];
    let (plan, _) = plan(&input, Triangulation::Concave);
    let report = plan.report();
    assert_eq!(report.hole_faces, 1);
    assert_eq!(report.unmatched_hole_indices, 1);
    assert_eq!(report.triangles, 3);
}

/// Checks a concave face's triangles against the face: in range, none
/// inverted against the face's normal (or all inverted, `flipped`), and
/// their areas summing to the face's.
fn assert_covers(polygon: &[[f32; 3]], buffers: &MeshBuffers, flipped: bool) {
    let polygon: Vec<[f64; 3]> = polygon.iter().map(|&p| wide(p)).collect();
    let normal = vector_area(&polygon);
    let area = length(normal) / 2.0;
    let mut sum = 0.0;
    for t in buffers.indices.as_chunks::<3>().0.iter() {
        assert!(
            t.iter().all(|&i| (i as usize) < buffers.positions.len()),
            "index in range"
        );
        let [a, b, c] = [0, 1, 2].map(|k| wide(buffers.positions[t[k] as usize]));
        let n = cross(sub(b, a), sub(c, a));
        // A sliver whose corners are collinear but for rounding (the L's
        // corners 1, 3 and 5 are) has no facing to invert.
        let facing = dot(n, normal) * if flipped { -1.0 } else { 1.0 };
        assert!(
            facing >= -1e-6 * length(n) * length(normal),
            "triangle {t:?} of {polygon:?} is inverted ({facing}, flipped {flipped})"
        );
        sum += length(n) / 2.0;
    }
    assert!(
        (sum - area).abs() < 1e-5 * area,
        "triangles cover {sum}, the face {area}"
    );
}

/// An L of six corners in the plane z = 0, with its reflex corner at 3.
const L: [[f32; 3]; 6] = [
    [0.0, 0.0, 0.0],
    [2.0, 0.0, 0.0],
    [2.0, 1.0, 0.0],
    [1.0, 1.0, 0.0],
    [1.0, 2.0, 0.0],
    [0.0, 2.0, 0.0],
];

/// An arrow of seven corners, its reflex corners next to its first one,
/// where a fan from corner 0 covers area outside it.
const ARROW: [[f32; 3]; 7] = [
    [1.0, 1.0, 0.0],
    [0.0, 0.0, 0.0],
    [3.0, 0.0, 0.0],
    [4.0, 1.5, 0.0],
    [3.0, 3.0, 0.0],
    [0.0, 3.0, 0.0],
    [1.0, 2.0, 0.0],
];

#[test]
fn concave_faces_are_clipped_to_cover_exactly_the_face() {
    for polygon in [&L[..], &ARROW[..]] {
        // Also tilted out of every axis plane, to test the projection.
        let tilted: Vec<[f32; 3]> = polygon
            .iter()
            .map(|&[x, y, _]| [x, 0.6 * y, 0.8 * y + 0.3 * x])
            .collect();
        for points in [polygon.to_vec(), tilted] {
            let corners = i32::try_from(points.len()).unwrap();
            let counts = [corners];
            let indices: Vec<i32> = (0..corners).collect();
            for orientation in [Orientation::RightHanded, Orientation::LeftHanded] {
                let mut input = mesh(&counts, &indices, &points);
                input.topology.orientation = orientation;
                let (plan, buffers) = plan(&input, Triangulation::Concave);
                let report = plan.report();
                assert_eq!(
                    (
                        report.concave_faces,
                        report.clipped_faces,
                        report.fallback_faces
                    ),
                    (1, 1, 0)
                );
                assert_eq!(report.face_notes[0].kind, FaceNoteKind::ConcaveClipped);
                assert_eq!(report.triangles as usize, points.len() - 2);
                assert_covers(&points, &buffers, orientation == Orientation::LeftHanded);
            }
        }
    }
}

#[test]
fn the_hydra_fan_draws_a_concave_face_as_hydra_does_and_says_so() {
    let counts = [7];
    let indices = [0, 1, 2, 3, 4, 5, 6];
    let input = mesh(&counts, &indices, &ARROW);
    let (plan, buffers) = plan(&input, Triangulation::HydraFan);
    assert_eq!(
        plan.triangle_points(),
        vec![[0, 1, 2], [0, 2, 3], [0, 3, 4], [0, 4, 5], [0, 5, 6]]
    );
    assert_eq!(
        plan.report().face_notes[0].kind,
        FaceNoteKind::ConcaveFanned
    );
    assert_eq!(plan.report().concave_faces, 1);
    // The fan's triangles do not sum to the face's area: some fold back.
    let face = length(vector_area(&ARROW.map(wide))) / 2.0;
    let fan: f64 = buffers
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let [a, b, c] = [0, 1, 2].map(|k| wide(buffers.positions[t[k] as usize]));
            length(cross(sub(b, a), sub(c, a))) / 2.0
        })
        .sum();
    assert!((fan - face).abs() > 0.1, "{fan} against {face}");
}

#[test]
fn a_self_intersecting_concave_face_falls_back_to_the_fan() {
    // The last two edges cross the first.
    let points = [
        [0.0, 0.0, 0.0],
        [4.0, 0.0, 0.0],
        [4.0, 4.0, 0.0],
        [2.0, -1.0, 0.0],
        [0.0, 4.0, 0.0],
    ];
    let counts = [5];
    let indices = [0, 1, 2, 3, 4];
    let input = mesh(&counts, &indices, &points);
    let (plan, _) = plan(&input, Triangulation::Concave);
    let report = plan.report();
    assert_eq!((report.concave_faces, report.fallback_faces), (1, 1));
    assert!(matches!(
        report.face_notes[0].kind,
        FaceNoteKind::ConcaveFellBack(_)
    ));
    assert_eq!(
        plan.triangle_points(),
        vec![[0, 1, 2], [0, 2, 3], [0, 3, 4]]
    );
}

/// A unit cube: 8 points, 6 outward quads.
const CUBE_POINTS: [[f32; 3]; 8] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [1.0, 0.0, 1.0],
    [1.0, 1.0, 1.0],
    [0.0, 1.0, 1.0],
];
const CUBE_COUNTS: [i32; 6] = [4; 6];
const CUBE_INDICES: [i32; 24] = [
    0, 3, 2, 1, // -z
    4, 5, 6, 7, // +z
    0, 1, 5, 4, // -y
    2, 3, 7, 6, // +y
    1, 2, 6, 5, // +x
    0, 4, 7, 3, // -x
];
const CUBE_NORMALS: [[f32; 3]; 6] = [
    [0.0, 0.0, -1.0],
    [0.0, 0.0, 1.0],
    [0.0, -1.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
];

#[test]
fn uniform_normals_split_every_corner_by_face() {
    let mut input = mesh(&CUBE_COUNTS, &CUBE_INDICES, &CUBE_POINTS);
    input.normals = Some(Primvar::new(&CUBE_NORMALS, Interpolation::Uniform));
    let (plan, buffers) = plan(&input, Triangulation::Concave);
    let report = plan.report();
    assert_eq!((report.triangles, report.vertices), (12, 24));
    assert_eq!(report.normals, NormalsSource::Authored);
    for (t, &face) in buffers
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .zip(&buffers.triangle_faces)
    {
        for &v in t {
            assert_eq!(buffers.normals[v as usize], CUBE_NORMALS[face as usize]);
        }
    }
}

#[test]
fn computed_normals_are_smooth_per_point() {
    let input = mesh(&CUBE_COUNTS, &CUBE_INDICES, &CUBE_POINTS);
    let (plan, buffers) = plan(&input, Triangulation::Concave);
    assert_eq!(plan.report().vertices, 8);
    assert_eq!(plan.report().normals, NormalsSource::Computed);
    // Each corner's normal points out along its diagonal.
    for (position, normal) in buffers.positions.iter().zip(&buffers.normals) {
        let out = position.map(|c| c - 0.5);
        let n = wide(*normal);
        assert!((length(n) - 1.0).abs() < 1e-6);
        assert!(
            dot(n, wide(out)) / length(wide(out)) > 0.999,
            "{normal:?} at {position:?}"
        );
    }
}

#[test]
fn face_varying_and_indexed_primvars_split_only_where_values_differ() {
    // Two quads sharing the edge 1-4; `st` is face-varying and agrees on
    // the shared edge, so 6 vertices; color is indexed uniform, differing
    // per face, which splits the shared edge: 8 vertices.
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 1.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ];
    let counts = [4, 4];
    let indices = [0, 1, 4, 5, 1, 2, 3, 4];
    let st = [
        [0.0, 0.0],
        [0.5, 0.0],
        [0.5, 1.0],
        [0.0, 1.0],
        [0.5, 0.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.5, 1.0],
    ];
    let mut input = mesh(&counts, &indices, &points);
    input.st = Some(Primvar::new(&st, Interpolation::FaceVarying));
    let (plan_st, buffers) = plan(&input, Triangulation::Concave);
    assert_eq!(plan_st.report().vertices, 6);
    assert_eq!(
        plan_st.report().st,
        ChannelState::Used {
            interpolation: Interpolation::FaceVarying,
            indexed: false
        }
    );
    let uvs = buffers.st.unwrap();
    for (position, uv) in buffers.positions.iter().zip(&uvs) {
        assert_eq!(uv[0], position[0] / 2.0);
    }
    let colors = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    input.display_color = Some(Primvar::new(&colors, Interpolation::Uniform).with_indices(&[1, 0]));
    let (plan_color, buffers) = plan(&input, Triangulation::Concave);
    assert_eq!(plan_color.report().vertices, 8);
    let drawn = buffers.colors.unwrap();
    for (t, &face) in buffers
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .zip(&buffers.triangle_faces)
    {
        let expected = if face == 0 { colors[1] } else { colors[0] };
        assert!(t.iter().all(|&v| drawn[v as usize] == expected));
    }
}

#[test]
fn a_primvar_that_does_not_cover_the_mesh_is_dropped_and_reported() {
    let mut input = mesh(&CUBE_COUNTS, &CUBE_INDICES, &CUBE_POINTS);
    let st = [[0.0, 0.0]; 4];
    input.st = Some(Primvar::new(&st, Interpolation::Vertex));
    input.display_color =
        Some(Primvar::new(&[[1.0, 1.0, 1.0]], Interpolation::Constant).with_indices(&[3]));
    let (plan, buffers) = plan(&input, Triangulation::Concave);
    assert_eq!(
        plan.report().st,
        ChannelState::Dropped(DropReason::TooFew {
            expected: 8,
            found: 4
        })
    );
    assert_eq!(
        plan.report().display_color,
        ChannelState::Dropped(DropReason::Index(3))
    );
    assert!(buffers.st.is_none() && buffers.colors.is_none());
    assert_eq!(plan.report().triangles, 12);
}

#[test]
fn subsets_become_draw_ranges_in_order_then_the_rest() {
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [2.0, 0.0, 0.0],
    ];
    let counts = [3, 3, 3];
    let indices = [0, 1, 2, 0, 2, 3, 1, 4, 2];
    let mut input = mesh(&counts, &indices, &points);
    // Subset 0 holds face 2; subset 1 faces 0, 2 (held already) and 7 (no
    // such face).
    input.subsets = FaceSubsets {
        faces: &[2, 0, 2, 7],
        ends: &[1, 4],
    };
    let (plan, buffers) = plan(&input, Triangulation::Concave);
    assert_eq!(buffers.triangle_faces, [2, 0, 1]);
    assert_eq!(
        buffers.ranges,
        [
            DrawRange {
                subset: Some(0),
                first: 0,
                count: 3
            },
            DrawRange {
                subset: Some(1),
                first: 3,
                count: 3
            },
            DrawRange {
                subset: None,
                first: 6,
                count: 3
            },
        ]
    );
    let report = plan.report();
    assert_eq!(
        (
            report.subsets[0].faces,
            report.subsets[1].faces,
            report.unassigned_faces
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (
            report.subsets[1].already_claimed,
            report.subsets[1].out_of_range
        ),
        (1, 1)
    );
}

#[test]
fn an_invalid_topology_draws_nothing_and_says_why() {
    let points = [[0.0, 0.0, 0.0]; 3];
    let cases: [(&[i32], &[i32], TopologyProblem); 3] = [
        (&[4, -1], &[0, 1, 2], TopologyProblem::FaceCounts),
        (
            &[3, 3],
            &[0, 1, 2],
            TopologyProblem::SizeMismatch {
                counts: 6,
                indices: 3,
            },
        ),
        (
            &[3],
            &[0, 1, 5],
            TopologyProblem::VertexIndex {
                index: 5,
                points: 3,
            },
        ),
    ];
    for (counts, indices, problem) in cases {
        let mut input = mesh(counts, indices, &points);
        input.subsets = FaceSubsets {
            faces: &[0],
            ends: &[1],
        };
        let (plan, buffers) = plan(&input, Triangulation::Concave);
        assert_eq!(plan.report().topology_problem, Some(problem));
        assert!(buffers.indices.is_empty() && buffers.positions.is_empty());
        assert_eq!(buffers.ranges.len(), 2);
    }
}

#[test]
fn a_refill_moves_points_without_triangulating_again() {
    let counts = [6];
    let indices = [0, 1, 2, 3, 4, 5];
    let input = mesh(&counts, &indices, &L);
    let (plan, mut buffers) = plan(&input, Triangulation::Concave);
    let before = buffers.indices.clone();
    assert!(plan.same_topology(&input.topology));
    let moved: Vec<[f32; 3]> = L.iter().map(|&[x, y, z]| [x + 10.0, y, z + 1.0]).collect();
    plan.refill(&mut buffers, &moved, None).unwrap();
    assert_eq!(buffers.indices, before);
    for (position, &point) in buffers.positions.iter().zip(plan.vertex_points()) {
        assert_eq!(*position, moved[point as usize]);
    }
    assert_eq!(
        plan.refill(&mut buffers, &moved[..5], None),
        Err(PlanMismatch::PointCount {
            expected: 6,
            found: 5
        })
    );
    let other = [4, 2];
    assert!(!plan.same_topology(&MeshTopology {
        face_vertex_counts: &other,
        ..input.topology
    }));
}

#[test]
fn a_refill_reads_new_authored_normals_of_the_same_layout() {
    let mut input = mesh(&CUBE_COUNTS, &CUBE_INDICES, &CUBE_POINTS);
    input.normals = Some(Primvar::new(&CUBE_NORMALS, Interpolation::Uniform));
    let (plan, mut buffers) = plan(&input, Triangulation::Concave);
    let flipped = CUBE_NORMALS.map(|n| n.map(|c| -c));
    let normals = Primvar::new(&flipped, Interpolation::Uniform);
    plan.refill(&mut buffers, &CUBE_POINTS, Some(&normals))
        .unwrap();
    for (t, &face) in buffers
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .zip(&buffers.triangle_faces)
    {
        assert!(
            t.iter()
                .all(|&v| buffers.normals[v as usize] == flipped[face as usize])
        );
    }
    let per_point = [[0.0, 0.0, 1.0]; 8];
    assert_eq!(
        plan.refill(
            &mut buffers,
            &CUBE_POINTS,
            Some(&Primvar::new(&per_point, Interpolation::Vertex))
        ),
        Err(PlanMismatch::Channel("normals"))
    );
}

/// Checks a closed shape about the origin: unit normals, and every
/// triangle facing away from the center.
fn assert_closed_outward(buffers: &MeshBuffers) {
    assert!(
        buffers
            .normals
            .iter()
            .all(|n| (length(wide(*n)) - 1.0).abs() < 1e-5)
    );
    for t in buffers.indices.as_chunks::<3>().0.iter() {
        let [a, b, c] = [0, 1, 2].map(|k| wide(buffers.positions[t[k] as usize]));
        let n = cross(sub(b, a), sub(c, a));
        let center = [
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ];
        assert!(dot(n, center) > 0.0, "triangle {t:?} faces inward");
    }
}

#[test]
fn implicit_shapes_have_the_documented_counts_and_face_out() {
    use crate::Axis;
    let t = Tessellation {
        segments: 12,
        rings: 8,
    };
    let (s, r) = (12, 8);
    let cases = [
        (Shape::Cube { size: 2.0 }, 24, 12),
        (
            Shape::Sphere { radius: 1.5 },
            (r + 1) * (s + 1),
            2 * s * (r - 1),
        ),
        (
            Shape::Cylinder {
                radius: 1.0,
                height: 2.0,
                axis: Axis::Y,
            },
            4 * (s + 1) + 2,
            4 * s,
        ),
        (
            Shape::Cone {
                radius: 1.0,
                height: 2.0,
                axis: Axis::X,
            },
            3 * (s + 1) + 1,
            2 * s,
        ),
        (
            Shape::Capsule {
                radius: 0.5,
                height: 1.0,
                axis: Axis::Z,
            },
            (2 * (r / 2) + 2) * (s + 1),
            2 * s * (2 * (r / 2)),
        ),
        (
            Shape::Cylinder1 {
                radius_top: 0.5,
                radius_bottom: 1.0,
                height: 2.0,
                axis: Axis::Z,
            },
            4 * (s + 1) + 2,
            4 * s,
        ),
        // A zero radius: no cap there and no degenerate side triangles.
        (
            Shape::Cylinder1 {
                radius_top: 0.0,
                radius_bottom: 1.0,
                height: 2.0,
                axis: Axis::Y,
            },
            4 * (s + 1) + 2 - (s + 2),
            2 * s,
        ),
        (
            Shape::Capsule1 {
                radius_top: 0.25,
                radius_bottom: 0.5,
                height: 1.0,
                axis: Axis::X,
            },
            (2 * (r / 2) + 2) * (s + 1),
            2 * s * (2 * (r / 2)),
        ),
    ];
    for (shape, vertices, triangles) in cases {
        let buffers = tessellate(&shape, &t);
        assert_eq!(buffers.positions.len(), vertices as usize, "{shape:?}");
        assert_eq!(buffers.indices.len(), 3 * triangles as usize, "{shape:?}");
        assert_eq!(
            buffers.ranges,
            [DrawRange {
                subset: None,
                first: 0,
                count: 3 * triangles
            }]
        );
        assert_closed_outward(&buffers);
    }
}

#[test]
fn implicit_shapes_honour_their_sizes_and_axes() {
    use crate::Axis;
    let t = Tessellation::default();
    let extent = |buffers: &MeshBuffers| {
        let mut max = [f32::MIN; 3];
        for p in &buffers.positions {
            for k in 0..3 {
                max[k] = max[k].max(p[k]);
            }
        }
        max
    };
    assert_eq!(
        extent(&tessellate(&Shape::Cube { size: 3.0 }, &t)),
        [1.5; 3]
    );
    let capsule = extent(&tessellate(
        &Shape::Capsule {
            radius: 0.5,
            height: 2.0,
            axis: Axis::X,
        },
        &t,
    ));
    assert!((capsule[0] - 1.5).abs() < 1e-6, "{capsule:?}");
    let cone = tessellate(
        &Shape::Cone {
            radius: 1.0,
            height: 4.0,
            axis: Axis::Y,
        },
        &t,
    );
    assert!((extent(&cone)[1] - 2.0).abs() < 1e-6);
    // A plane faces +axis, its width and length along the schema's
    // directions: for axis X, width along Z and length along Y.
    for (axis, normal, size) in [
        (Axis::X, [1.0, 0.0, 0.0], [0.0, 2.0, 1.0]),
        (Axis::Y, [0.0, 1.0, 0.0], [1.0, 0.0, 2.0]),
        (Axis::Z, [0.0, 0.0, 1.0], [1.0, 2.0, 0.0]),
    ] {
        let plane = tessellate(
            &Shape::Plane {
                width: 2.0,
                length: 4.0,
                axis,
            },
            &t,
        );
        assert!(plane.normals.iter().all(|n| *n == normal));
        assert_eq!(extent(&plane), size, "{axis:?}");
        let [a, b, c] = [0, 1, 2].map(|k| wide(plane.positions[plane.indices[k] as usize]));
        assert!(
            dot(cross(sub(b, a), sub(c, a)), wide(normal)) > 0.0,
            "{axis:?} winds toward +axis"
        );
    }
}

#[test]
fn a_lone_concave_faces_reflex_corner_gets_hydras_flipped_normal() {
    // At the L's reflex corner (point 3) the corner's cross product turns
    // the other way, and with no other face to outweigh it, its smooth
    // normal faces -Z, as Hydra computes it.
    let counts = [6];
    let indices = [0, 1, 2, 3, 4, 5];
    let input = mesh(&counts, &indices, &L);
    let (plan, buffers) = plan(&input, Triangulation::Concave);
    for (&point, normal) in plan.vertex_points().iter().zip(&buffers.normals) {
        let expected = if point == 3 { -1.0 } else { 1.0 };
        assert_eq!(*normal, [0.0, 0.0, expected], "point {point}");
    }
}

#[test]
fn face_oriented_normals_turn_the_reflex_corner_to_the_face_and_change_nothing_else() {
    let counts = [6];
    let indices = [0, 1, 2, 3, 4, 5];
    for (orientation, front) in [
        (Orientation::RightHanded, 1.0),
        (Orientation::LeftHanded, -1.0),
    ] {
        let mut input = mesh(&counts, &indices, &L);
        input.topology.orientation = orientation;
        let options = MeshOptions {
            normals: SmoothNormals::FaceOriented,
            ..MeshOptions::default()
        };
        let plan = MeshPlan::new(&input, &options);
        assert_eq!(plan.report().smooth_normals, SmoothNormals::FaceOriented);
        let buffers = plan.buffers(&input).unwrap();
        assert!(
            buffers.normals.iter().all(|n| *n == [0.0, 0.0, front]),
            "{orientation:?}: {:?}",
            buffers.normals
        );
        // Under Hydra's, the reflex corner alone faces the other way.
        let hydra = MeshPlan::new(&input, &MeshOptions::default());
        let hydra_normals = hydra.buffers(&input).unwrap().normals;
        for (&point, normal) in hydra.vertex_points().iter().zip(&hydra_normals) {
            let expected = if point == 3 { -front } else { front };
            assert_eq!(
                *normal,
                [0.0, 0.0, expected],
                "{orientation:?} point {point}"
            );
        }
        // A refill keeps the plan's mode.
        let mut refilled = buffers.clone();
        plan.refill(&mut refilled, &L, None).unwrap();
        assert_eq!(refilled.normals, buffers.normals);
    }
    // Where every face is convex, both modes agree bit for bit: an open box
    // of five quads, bent at every edge.
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
        [0.0, 1.0, 1.0],
    ];
    let counts = [4, 4, 4, 4, 4];
    let indices = [0, 3, 2, 1, 0, 1, 5, 4, 1, 2, 6, 5, 2, 3, 7, 6, 3, 0, 4, 7];
    let input = mesh(&counts, &indices, &points);
    let hydra = MeshPlan::new(&input, &MeshOptions::default());
    let oriented = MeshPlan::new(
        &input,
        &MeshOptions {
            normals: SmoothNormals::FaceOriented,
            ..MeshOptions::default()
        },
    );
    assert_eq!(
        hydra.buffers(&input).unwrap(),
        oriented.buffers(&input).unwrap()
    );
}

#[test]
fn the_one_revisions_meet_their_ends_and_reduce_to_the_originals() {
    use crate::Axis;
    let t = Tessellation::default();
    let along = |buffers: &MeshBuffers| {
        let z = buffers.positions.iter().map(|p| p[2]);
        (
            z.clone().fold(f32::MAX, f32::min),
            z.fold(f32::MIN, f32::max),
        )
    };
    // Equal radii: the original shapes, vertex for vertex (to rounding).
    let close = |a: &MeshBuffers, b: &MeshBuffers| {
        assert_eq!(a.indices, b.indices);
        for (p, q) in a.positions.iter().zip(&b.positions) {
            assert!((0..3).all(|k| (p[k] - q[k]).abs() < 1e-6), "{p:?} {q:?}");
        }
    };
    close(
        &tessellate(
            &Shape::Capsule1 {
                radius_top: 0.5,
                radius_bottom: 0.5,
                height: 1.0,
                axis: Axis::Z,
            },
            &t,
        ),
        &tessellate(
            &Shape::Capsule {
                radius: 0.5,
                height: 1.0,
                axis: Axis::Z,
            },
            &t,
        ),
    );
    close(
        &tessellate(
            &Shape::Cylinder1 {
                radius_top: 1.0,
                radius_bottom: 1.0,
                height: 2.0,
                axis: Axis::Z,
            },
            &t,
        ),
        &tessellate(
            &Shape::Cylinder {
                radius: 1.0,
                height: 2.0,
                axis: Axis::Z,
            },
            &t,
        ),
    );
    // A frustum: radius 1 at the bottom, 0.5 at the top.
    let frustum = tessellate(
        &Shape::Cylinder1 {
            radius_top: 0.5,
            radius_bottom: 1.0,
            height: 2.0,
            axis: Axis::Z,
        },
        &t,
    );
    assert_eq!(along(&frustum), (-1.0, 1.0));
    for p in &frustum.positions {
        let r = libm::sqrtf(p[0] * p[0] + p[1] * p[1]);
        let limit = 0.75 - 0.25 * p[2];
        assert!(r <= limit + 1e-5, "{p:?} outside the frustum");
    }
    // A capsule of radii 0.25 (top) and 0.5 (bottom), height 1: the poles
    // reach OpenUSD's extent along the axis (`capsule_1.cpp`,
    // `_ComputeExtentMax`): `h/2 - s*top + top*sec` and
    // `-h/2 - s*bottom - bottom*sec`, `s = 0.25`, `sec = sqrt(1 + s²)`.
    let capsule = tessellate(
        &Shape::Capsule1 {
            radius_top: 0.25,
            radius_bottom: 0.5,
            height: 1.0,
            axis: Axis::Z,
        },
        &t,
    );
    let sec = libm::sqrt(1.0625_f64);
    let (min, max) = along(&capsule);
    assert!(
        (f64::from(max) - (0.5 - 0.0625 + 0.25 * sec)).abs() < 1e-6,
        "{max}"
    );
    assert!(
        (f64::from(min) - (-0.5 - 0.125 - 0.5 * sec)).abs() < 1e-6,
        "{min}"
    );
    // The rims sit at the side's ends, where the caps' normals lean as the
    // side's: the side is the band between them, smooth across.
    let rim = |z: f32| {
        capsule
            .positions
            .iter()
            .zip(&capsule.normals)
            .filter(|(p, _)| (p[2] - z).abs() < 1e-6)
            .map(|(p, n)| (libm::sqrtf(p[0] * p[0] + p[1] * p[1]), n[2]))
            .collect::<Vec<_>>()
    };
    let lean = 0.25 / libm::sqrtf(1.0625);
    for (z, radius) in [(0.5, 0.25), (-0.5, 0.5)] {
        let ring = rim(z);
        assert_eq!(ring.len(), 33, "the rim at {z}");
        for (r, nz) in ring {
            assert!((r - radius).abs() < 1e-6 && (nz - lean).abs() < 1e-6);
        }
    }
}
