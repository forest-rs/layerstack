// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Curves and points against OpenUSD's own answers and hand-computed
//! values.

#![expect(
    clippy::cast_possible_truncation,
    reason = "the oracle's JSON numbers are the fixture's float and int values"
)]

use alloc::vec::Vec;

use exedra_curves::NurbsError;

use crate::{
    ChannelState, CurveBasis, CurveEvaluation, CurveForm, CurveGeometry, CurveNoteKind,
    CurveOptions, CurveProblem, CurveStyle, CurveType, CurveWrap, CurvesInput, CurvesPlan,
    DropReason, Interpolation, PointShape, PointsInput, Primvar, basis_data_sizes,
    basis_segment_counts, curves_extent, fill_points, interpolation_for_size, points_extent,
};

/// usd-core 26.11's answers (`stagehand_curves_oracle.py`).
const ORACLE: &str =
    include_str!("../../layerstack_conformance/fixtures/stagehand_curves/oracle.json");

fn interpolation_token(interpolation: Option<Interpolation>) -> &'static str {
    match interpolation {
        None => "",
        Some(Interpolation::Constant) => "constant",
        Some(Interpolation::Uniform) => "uniform",
        Some(Interpolation::Varying) => "varying",
        Some(Interpolation::Vertex) => "vertex",
        Some(Interpolation::FaceVarying) => "faceVarying",
    }
}

fn vec3s(value: &serde_json::Value) -> Vec<[f32; 3]> {
    value
        .as_array()
        .expect("an array")
        .iter()
        .map(|v| {
            let v = v.as_array().expect("a vector");
            core::array::from_fn(|k| v[k].as_f64().expect("a number") as f32)
        })
        .collect()
}

fn extent(value: &serde_json::Value) -> Option<[[f32; 3]; 2]> {
    (!value.is_null()).then(|| {
        let v = vec3s(value);
        [v[0], v[1]]
    })
}

fn floats(value: &serde_json::Value) -> Option<Vec<f32>> {
    value.as_array().map(|values| {
        values
            .iter()
            .map(|v| v.as_f64().expect("a number") as f32)
            .collect()
    })
}

#[test]
fn segment_counts_data_sizes_and_extents_match_openusd() {
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).expect("the oracle");
    let rows = oracle["basisCurves"].as_array().expect("rows");
    assert!(rows.len() > 40, "every type, basis and wrap");
    for row in rows {
        let path = row["path"].as_str().unwrap();
        let curve_type = CurveType::from_token(row["type"].as_str().unwrap()).unwrap();
        let basis = CurveBasis::from_token(row["basis"].as_str().unwrap()).unwrap();
        let wrap = CurveWrap::from_token(row["wrap"].as_str().unwrap()).unwrap();
        let counts: Vec<i32> = row["curveVertexCounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_i64().unwrap() as i32)
            .collect();
        let segments: Vec<i32> = row["segmentCounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_i64().unwrap() as i32)
            .collect();
        assert_eq!(
            basis_segment_counts(curve_type, basis, wrap, &counts),
            segments,
            "{path}"
        );
        let sizes = basis_data_sizes(curve_type, basis, wrap, &counts).unwrap();
        assert_eq!(sizes.uniform, row["uniform"].as_u64().unwrap(), "{path}");
        assert_eq!(sizes.vertex, row["vertex"].as_u64().unwrap(), "{path}");
        // `null`: C++'s varying size wraps around below 4 vertices (B035).
        if let Some(varying) = row["varying"].as_u64() {
            assert_eq!(sizes.varying, varying, "{path}");
        }
        for (size, token) in row["interpolationForSize"].as_object().unwrap() {
            let size: u64 = size.parse().unwrap();
            assert_eq!(
                interpolation_token(interpolation_for_size(sizes, size)),
                token.as_str().unwrap(),
                "{path} at {size}"
            );
        }
        // The fixture's points: point i is (i, i * i % 5, i / 2).
        let total: i32 = counts.iter().sum();
        let points: Vec<[f32; 3]> = (0..total)
            .map(|i| [i as f32, ((i * i) % 5) as f32, 0.5 * i as f32])
            .collect();
        let widths = floats(&row["widths"]).unwrap_or_default();
        assert_eq!(
            curves_extent(&points, &widths),
            extent(&row["extent"]),
            "{path}"
        );
    }
    for row in oracle["points"].as_array().unwrap() {
        let path = row["path"].as_str().unwrap();
        let points = vec3s(&row["points"]);
        let widths = floats(&row["widths"]);
        if widths.is_some() {
            assert_eq!(
                points_extent(&points, widths.as_deref()),
                extent(&row["extent"]),
                "{path}"
            );
        }
        assert_eq!(
            points_extent(&points, widths.as_deref()),
            extent(&row["extentFromPlugins"]),
            "{path}"
        );
    }
    for kind in ["nurbsCurves", "hermiteCurves"] {
        for row in oracle[kind].as_array().unwrap() {
            let points = vec3s(&row["points"]);
            let widths = floats(&row["widths"]).unwrap_or_default();
            assert_eq!(
                curves_extent(&points, &widths),
                extent(&row["extentFromPlugins"]),
                "{}",
                row["path"]
            );
        }
    }
}

fn basis(curve_type: &'static str, basis: &'static str, wrap: &'static str) -> CurveForm<'static> {
    CurveForm::Basis {
        curve_type,
        basis,
        wrap,
    }
}

fn line(n: usize) -> Vec<[f32; 3]> {
    (0..n).map(|i| [i as f32, 0.0, 0.0]).collect()
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < 1e-6)
}

#[test]
fn catmull_rom_runs_from_its_second_vertex_to_its_second_to_last() {
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [2.0, 0.0, 0.0],
        [3.0, 1.0, 0.0],
        [4.0, 0.0, 0.0],
    ];
    let input = CurvesInput {
        form: basis("cubic", "catmullRom", "nonperiodic"),
        vertex_counts: &[5],
        points: &points,
        ..CurvesInput::default()
    };
    let plan = CurvesPlan::new(&input, &CurveOptions::default());
    let buffers = plan.buffers(&input).unwrap();
    assert_eq!(buffers.report.segments, [2]);
    assert_eq!(buffers.report.vertices, 2 * 8 + 1);
    assert_eq!(buffers.report.lines, 16);
    assert_eq!(buffers.positions[0], [1.0, 1.0, 0.0]);
    assert_eq!(buffers.positions[8], [2.0, 0.0, 0.0]);
    assert_eq!(buffers.positions[16], [3.0, 1.0, 0.0]);
    // At u = 1/2 of the first segment, by the Catmull-Rom matrix:
    // (-P0 + 9 P1 + 9 P2 - P3) / 16.
    assert!(near(buffers.positions[4], [1.5, 0.5, 0.0]));
    assert_eq!(
        buffers.report.evaluation,
        CurveEvaluation::Basis {
            curve_type: CurveType::Cubic,
            basis: Some(CurveBasis::CatmullRom),
            wrap: CurveWrap::Nonperiodic,
        }
    );
}

#[test]
fn pinned_curves_begin_and_end_at_their_end_vertices() {
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 2.0, 0.0],
        [3.0, 2.0, 0.0],
        [4.0, 0.0, 0.0],
    ];
    for name in ["bspline", "catmullRom"] {
        let input = CurvesInput {
            form: basis("cubic", name, "pinned"),
            vertex_counts: &[4],
            points: &points,
            ..CurvesInput::default()
        };
        let buffers = CurvesPlan::new(&input, &CurveOptions::default())
            .buffers(&input)
            .unwrap();
        // `ComputeSegmentCounts`: n - 1.
        assert_eq!(buffers.report.segments, [3], "{name}");
        assert!(near(buffers.positions[0], points[0]), "{name}");
        assert!(
            near(*buffers.positions.last().unwrap(), points[3]),
            "{name}"
        );
    }
    // A pinned B-spline's first segment is (2 P0 - P1, P0, P1, P2): at 0,
    // (P[-1] + 4 P0 + P1) / 6 = P0, and at 1, (P0 + 4 P1 + P2) / 6.
    let input = CurvesInput {
        form: basis("cubic", "bspline", "pinned"),
        vertex_counts: &[4],
        points: &points,
        ..CurvesInput::default()
    };
    let buffers = CurvesPlan::new(&input, &CurveOptions::default())
        .buffers(&input)
        .unwrap();
    assert!(near(
        buffers.positions[8],
        [(0.0 + 4.0 + 3.0) / 6.0, 10.0 / 6.0, 0.0]
    ));
}

#[test]
fn a_periodic_bspline_ring_closes_and_sweeps_as_a_tube() {
    let points: Vec<[f32; 3]> = (0..8)
        .map(|i| {
            let angle = core::f32::consts::TAU * i as f32 / 8.0;
            [libm::cosf(angle), 0.0, libm::sinf(angle)]
        })
        .collect();
    let input = CurvesInput {
        form: basis("cubic", "bspline", "periodic"),
        vertex_counts: &[8],
        points: &points,
        widths: Some(Primvar::new(&[0.2], Interpolation::Constant)),
        ..CurvesInput::default()
    };
    let options = CurveOptions {
        style: CurveStyle::Swept,
        samples: 4,
        sides: 6,
    };
    let buffers = CurvesPlan::new(&input, &options).buffers(&input).unwrap();
    assert_eq!(buffers.report.segments, [8]);
    // 8 segments of 4 samples, the first repeated at the end.
    assert_eq!(buffers.report.vertices, 33);
    assert_eq!(buffers.positions[0], buffers.positions[32]);
    assert_eq!(buffers.report.geometry, CurveGeometry::Tubes { sides: 6 });
    let swept = buffers.swept.as_ref().unwrap();
    assert_eq!(swept.positions.len(), 33 * 6);
    assert_eq!(buffers.report.triangles, 32 * 6 * 2);
    assert_eq!(swept.ranges[0].count, buffers.report.triangles * 3);
    assert_eq!(buffers.widths, alloc::vec![0.2; 33]);
    assert_eq!(buffers.report.degenerate_stations, 0);
}

#[test]
fn widths_follow_their_interpolation() {
    let points = line(5);
    // B-spline, nonperiodic, 5 vertices: 2 segments, 3 varying values.
    let input = CurvesInput {
        form: basis("cubic", "bspline", "nonperiodic"),
        vertex_counts: &[5],
        points: &points,
        widths: Some(Primvar::new(&[1.0, 2.0, 3.0], Interpolation::Varying)),
        ..CurvesInput::default()
    };
    let buffers = CurvesPlan::new(&input, &CurveOptions::default())
        .buffers(&input)
        .unwrap();
    assert_eq!(buffers.widths[0], 1.0);
    assert_eq!(buffers.widths[4], 1.5);
    assert_eq!(buffers.widths[8], 2.0);
    assert_eq!(buffers.widths[16], 3.0);
    // Vertex widths are weighted like the points: a B-spline of
    // (0, 6, 0, 6, 0) at the first joint is (0 + 24 + 0) / 6.
    let input = CurvesInput {
        widths: Some(Primvar::new(
            &[0.0, 6.0, 0.0, 6.0, 0.0],
            Interpolation::Vertex,
        )),
        ..input
    };
    let buffers = CurvesPlan::new(&input, &CurveOptions::default())
        .buffers(&input)
        .unwrap();
    assert_eq!(buffers.widths[0], 4.0);
    // Too few, and face-varying, are dropped with their reasons.
    let input = CurvesInput {
        widths: Some(Primvar::new(&[1.0, 2.0], Interpolation::Varying)),
        colors: Some(Primvar::new(&[[1.0, 0.0, 0.0]], Interpolation::FaceVarying)),
        ..input
    };
    let buffers = CurvesPlan::new(&input, &CurveOptions::default())
        .buffers(&input)
        .unwrap();
    assert_eq!(
        buffers.report.widths,
        ChannelState::Dropped(DropReason::TooFew {
            expected: 3,
            found: 2
        })
    );
    assert_eq!(
        buffers.report.colors,
        ChannelState::Dropped(DropReason::Interpolation(Interpolation::FaceVarying))
    );
    assert!(buffers.widths.iter().all(|&w| w == 1.0));
}

#[test]
fn notes_problems_and_fallbacks_are_reported() {
    let points = line(5);
    let input = CurvesInput {
        form: basis("cubic", "bezier", "nonperiodic"),
        vertex_counts: &[5],
        points: &points,
        ..CurvesInput::default()
    };
    let report = CurvesPlan::new(&input, &CurveOptions::default())
        .report()
        .clone();
    assert_eq!(report.segments, [1]);
    assert_eq!(report.notes[0].kind, CurveNoteKind::Unused { unused: 1 });
    let input = CurvesInput {
        form: basis("cubic", "power", "nonperiodic"),
        ..input
    };
    let buffers = CurvesPlan::new(&input, &CurveOptions::default())
        .buffers(&input)
        .unwrap();
    assert_eq!(buffers.report.fallbacks[0].attribute, "basis");
    assert_eq!(buffers.report.fallbacks[0].token, "power");
    assert_eq!(buffers.positions, points);
    let input = CurvesInput {
        vertex_counts: &[4],
        ..input
    };
    assert_eq!(
        CurvesPlan::new(&input, &CurveOptions::default())
            .report()
            .problem,
        Some(CurveProblem::PointCount {
            expected: 4,
            found: 5
        })
    );
    let input = CurvesInput {
        vertex_counts: &[6, -1],
        ..input
    };
    assert_eq!(
        CurvesPlan::new(&input, &CurveOptions::default())
            .report()
            .problem,
        Some(CurveProblem::NegativeCount {
            curve: 1,
            count: -1
        })
    );
}

#[test]
fn a_plan_refills_for_new_points_and_refuses_another_topology() {
    let points = line(4);
    let input = CurvesInput {
        form: basis("linear", "bezier", "periodic"),
        vertex_counts: &[4],
        points: &points,
        ..CurvesInput::default()
    };
    let plan = CurvesPlan::new(&input, &CurveOptions::default());
    let buffers = plan.buffers(&input).unwrap();
    // Linear periodic: through the vertices and back to the first.
    assert_eq!(buffers.report.vertices, 5);
    assert_eq!(buffers.report.lines, 4);
    let moved: Vec<[f32; 3]> = points.iter().map(|p| [p[0], 1.0, p[2]]).collect();
    let refilled = plan
        .buffers(&CurvesInput {
            points: &moved,
            ..input
        })
        .unwrap();
    assert_eq!(refilled.positions[3], [3.0, 1.0, 0.0]);
    assert_eq!(refilled.lines, buffers.lines);
    assert!(!plan.same_topology(&CurvesInput {
        form: basis("linear", "bezier", "nonperiodic"),
        ..input
    }));
}

#[test]
fn a_ribbon_faces_its_normals() {
    let points = line(4);
    let input = CurvesInput {
        form: basis("cubic", "bezier", "nonperiodic"),
        vertex_counts: &[4],
        points: &points,
        widths: Some(Primvar::new(&[0.5], Interpolation::Constant)),
        normals: Some(Primvar::new(&[[0.0, 1.0, 0.0]], Interpolation::Constant)),
        ..CurvesInput::default()
    };
    let options = CurveOptions {
        style: CurveStyle::Swept,
        ..CurveOptions::default()
    };
    let buffers = CurvesPlan::new(&input, &options).buffers(&input).unwrap();
    assert_eq!(buffers.report.geometry, CurveGeometry::Ribbons);
    let swept = buffers.swept.unwrap();
    assert_eq!(swept.positions.len(), 9 * 2);
    assert!(swept.normals.iter().all(|&n| n == [0.0, 1.0, 0.0]));
    assert_eq!(buffers.report.triangles, 8 * 2);
}

#[test]
fn a_weighted_nurbs_quarter_is_a_circle() {
    let points = [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
    let input = CurvesInput {
        form: CurveForm::Nurbs {
            order: &[3],
            knots: &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            ranges: &[[0.0, 1.0]],
            weights: &[1.0, core::f64::consts::FRAC_1_SQRT_2, 1.0],
        },
        vertex_counts: &[3],
        points: &points,
        widths: Some(Primvar::new(&[0.1, 0.1, 0.1], Interpolation::Vertex)),
        ..CurvesInput::default()
    };
    let buffers = CurvesPlan::new(&input, &CurveOptions::default())
        .buffers(&input)
        .unwrap();
    assert_eq!(buffers.report.evaluation, CurveEvaluation::Nurbs);
    assert_eq!(buffers.report.segments, [1]);
    assert_eq!(buffers.positions.len(), 9);
    for p in &buffers.positions {
        let r = libm::sqrtf(p[0] * p[0] + p[1] * p[1]);
        assert!((r - 1.0).abs() < 1e-6, "{p:?}");
    }
    assert_eq!(
        buffers.report.widths,
        ChannelState::Dropped(DropReason::Interpolation(Interpolation::Vertex))
    );
}

#[test]
fn a_nurbs_jump_ends_its_lines_and_sweeps() {
    // Order 2 with the interior knot 1 twice: two pieces, (0,0,0)-(1,0,0)
    // and (2,1,0)-(3,1,0), and a jump between them.
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [2.0, 1.0, 0.0],
        [3.0, 1.0, 0.0],
    ];
    let input = CurvesInput {
        form: CurveForm::Nurbs {
            order: &[2],
            knots: &[0.0, 0.0, 1.0, 1.0, 2.0, 2.0],
            ranges: &[],
            weights: &[],
        },
        vertex_counts: &[4],
        points: &points,
        widths: Some(Primvar::new(&[0.5], Interpolation::Constant)),
        ..CurvesInput::default()
    };
    let options = CurveOptions {
        style: CurveStyle::Swept,
        samples: 2,
        sides: 3,
    };
    let buffers = CurvesPlan::new(&input, &options).buffers(&input).unwrap();
    assert_eq!(buffers.report.segments, [2]);
    // Both sides of the jump: the first piece's end, then the second's
    // start at the same parameter.
    assert_eq!(
        buffers.positions,
        [
            [0.0, 0.0, 0.0],
            [0.5, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [2.5, 1.0, 0.0],
            [3.0, 1.0, 0.0],
        ]
    );
    // No line segment and no band crosses the jump.
    assert_eq!(buffers.lines, [0, 1, 1, 2, 3, 4, 4, 5]);
    let swept = buffers.swept.as_ref().unwrap();
    assert_eq!(swept.positions.len(), 6 * 3);
    assert_eq!(buffers.report.triangles, 2 * 2 * 3 * 2);
    // The second piece starts from the frame the first ended on.
    assert_eq!(swept.normals[2 * 3..3 * 3], swept.normals[3 * 3..4 * 3]);
    assert_eq!(buffers.report.degenerate_stations, 0);
}

#[test]
fn nurbs_weights_are_topology_and_an_empty_range_is_noted() {
    let points = [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
    let input = CurvesInput {
        form: CurveForm::Nurbs {
            order: &[3],
            knots: &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            ranges: &[],
            weights: &[1.0, 0.5, 1.0],
        },
        vertex_counts: &[3],
        points: &points,
        ..CurvesInput::default()
    };
    let plan = CurvesPlan::new(&input, &CurveOptions::default());
    let reweighted = CurvesInput {
        form: CurveForm::Nurbs {
            order: &[3],
            knots: &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            ranges: &[],
            weights: &[1.0, 0.0, 1.0],
        },
        ..input
    };
    assert!(!plan.same_topology(&reweighted));
    assert_eq!(
        CurvesPlan::new(&reweighted, &CurveOptions::default())
            .report()
            .notes[0]
            .kind,
        CurveNoteKind::Nurbs(NurbsError::Weight { index: 1 })
    );
    let outside = CurvesInput {
        form: CurveForm::Nurbs {
            order: &[3],
            knots: &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            ranges: &[[2.0, 3.0]],
            weights: &[],
        },
        ..input
    };
    let buffers = CurvesPlan::new(&outside, &CurveOptions::default())
        .buffers(&outside)
        .unwrap();
    assert_eq!(buffers.report.drawn_curves, 0);
    assert_eq!(
        buffers.report.notes[0].kind,
        CurveNoteKind::Nurbs(NurbsError::EmptyRange)
    );
    // A point refilled to a value that is not finite leaves the curve
    // undrawn with the control it names, not a curve of NaNs.
    let broken = [[1.0, 0.0, 0.0], [f32::NAN, 1.0, 0.0], [0.0, 1.0, 0.0]];
    let refilled = CurvesInput {
        points: &broken,
        ..input
    };
    let buffers = plan.buffers(&refilled).unwrap();
    assert_eq!(buffers.report.drawn_curves, 0);
    assert_eq!(
        buffers.report.notes[0].kind,
        CurveNoteKind::Nurbs(NurbsError::Point { index: 1 })
    );
}

#[test]
fn widths_that_undershoot_are_drawn_as_zero() {
    // Catmull-Rom widths (1, 0, 0, 0) between the middle two are
    // -u (1 - u)^2 / 2: below zero inside the segment.
    let points = line(4);
    let input = CurvesInput {
        form: basis("cubic", "catmullRom", "nonperiodic"),
        vertex_counts: &[4],
        points: &points,
        widths: Some(Primvar::new(&[1.0, 0.0, 0.0, 0.0], Interpolation::Vertex)),
        ..CurvesInput::default()
    };
    let options = CurveOptions {
        style: CurveStyle::Swept,
        ..CurveOptions::default()
    };
    let buffers = CurvesPlan::new(&input, &options).buffers(&input).unwrap();
    assert_eq!(buffers.report.clamped_widths, 7);
    assert!(buffers.widths.iter().all(|&w| w == 0.0));
    // Swept all the same, every ring collapsed.
    assert_eq!(buffers.report.triangles, 8 * 8 * 2);
    assert_eq!(buffers.report.degenerate_stations, 9);
}

#[test]
fn hermite_curves_follow_their_tangents() {
    let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
    let tangents = [[0.0, 1.0, 0.0], [0.0, -1.0, 0.0]];
    let input = CurvesInput {
        form: CurveForm::Hermite {
            tangents: &tangents,
        },
        vertex_counts: &[2],
        points: &points,
        ..CurvesInput::default()
    };
    let buffers = CurvesPlan::new(&input, &CurveOptions::default())
        .buffers(&input)
        .unwrap();
    assert_eq!(buffers.positions[0], [0.0, 0.0, 0.0]);
    assert_eq!(buffers.positions[8], [1.0, 0.0, 0.0]);
    // At 1/2: (P0 + P1) / 2 + (T0 - T1) / 8.
    assert!(near(buffers.positions[4], [0.5, 0.25, 0.0]));
}

#[test]
fn points_keep_their_ids_and_are_discs_with_normals() {
    let points = line(3);
    let filled = fill_points(&PointsInput {
        points: &points,
        widths: Some(Primvar::new(&[0.5, 1.0, 2.0], Interpolation::Vertex)),
        colors: Some(Primvar::new(&[[1.0, 0.5, 0.0]], Interpolation::Constant)),
        ids: &[7, 3, 9],
        ..PointsInput::default()
    });
    assert_eq!(filled.ids, [7, 3, 9]);
    assert_eq!(filled.report.shape, PointShape::Spheres);
    assert_eq!(filled.colors.unwrap().len(), 3);
    assert_eq!(
        filled.report.extent,
        Some([[-0.25, -1.0, -1.0], [3.0, 1.0, 1.0]])
    );
    let filled = fill_points(&PointsInput {
        points: &points,
        normals: Some(Primvar::new(&[[0.0, 0.0, 2.0]], Interpolation::Constant)),
        ids: &[1],
        ..PointsInput::default()
    });
    assert_eq!(filled.report.shape, PointShape::Discs);
    assert_eq!(filled.normals.unwrap()[2], [0.0, 0.0, 1.0]);
    assert!(filled.ids.is_empty());
    assert!(matches!(filled.report.ids, ChannelState::Dropped(_)));
    assert_eq!(filled.widths, [1.0; 3]);
}
