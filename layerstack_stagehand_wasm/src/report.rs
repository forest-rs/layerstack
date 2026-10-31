// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A [`MeshReport`] as the JSON the TypeScript `MeshReport` reads.

use alloc::string::String;
use core::fmt::Write;

use layerstack_stagehand::{
    ChannelState, CurveEvaluation, CurveGeometry, CurveNoteKind, CurveProblem, CurvesReport,
    FaceNoteKind, MeshReport, NormalsSource, NurbsError, SmoothNormals, Triangulation,
};

/// `text` as a JSON string.
fn quoted(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn channel(out: &mut String, state: &ChannelState) {
    match state {
        ChannelState::Absent => out.push_str(r#"{"state":"absent"}"#),
        ChannelState::Used {
            interpolation,
            indexed,
        } => {
            let _ = write!(
                out,
                r#"{{"state":"used","interpolation":"{}","indexed":{indexed}}}"#,
                interpolation.as_str()
            );
        }
        ChannelState::Dropped(reason) => {
            out.push_str(r#"{"state":"dropped","reason":"#);
            quoted(out, &alloc::format!("{reason}"));
            out.push('}');
        }
    }
}

/// The report as JSON.
pub(crate) fn json(report: &MeshReport) -> String {
    let mut out = String::from("{");
    out.push_str(r#""topologyProblem":"#);
    match &report.topology_problem {
        None => out.push_str("null"),
        Some(problem) => {
            let _ = write!(out, r#"{{"code":"{}","reason":"#, problem.code());
            quoted(&mut out, &alloc::format!("{problem}"));
            out.push('}');
        }
    }
    let triangulation = match report.triangulation {
        Triangulation::HydraFan => "hydraFan",
        _ => "concave",
    };
    let smooth = match report.smooth_normals {
        SmoothNormals::FaceOriented => "faceOriented",
        _ => "hydra",
    };
    let normals = match report.normals {
        NormalsSource::Authored => "authored",
        _ => "computed",
    };
    let _ = write!(
        out,
        r#","triangulation":"{triangulation}","smoothNormals":"{smooth}","orientation":"{}","subdivisionScheme":"{}","refined":false"#,
        report.orientation.as_str(),
        report.subdivision_scheme.as_str(),
    );
    for (name, value) in [
        ("faces", report.faces),
        ("drawnFaces", report.drawn_faces),
        ("points", report.points),
        ("triangles", report.triangles),
        ("vertices", report.vertices),
        ("triangleCorners", report.triangle_corners),
        ("holeFaces", report.hole_faces),
        ("degenerateFaces", report.degenerate_faces),
        ("concaveFaces", report.concave_faces),
        ("clippedFaces", report.clipped_faces),
        ("fallbackFaces", report.fallback_faces),
        ("unmatchedHoleIndices", report.unmatched_hole_indices),
        ("unassignedFaces", report.unassigned_faces),
    ] {
        let _ = write!(out, r#","{name}":{value}"#);
    }
    let _ = write!(out, r#","normals":"{normals}","channels":{{"normals":"#);
    channel(&mut out, &report.normals_channel);
    out.push_str(r#","st":"#);
    channel(&mut out, &report.st);
    out.push_str(r#","displayColor":"#);
    channel(&mut out, &report.display_color);
    out.push_str(r#"},"uvSets":["#);
    for (i, set) in report.uv_sets.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"name":"#);
        quoted(&mut out, &set.name);
        out.push_str(r#","channel":"#);
        channel(&mut out, &set.state);
        out.push('}');
    }
    out.push_str(r#"],"subsets":["#);
    for (i, subset) in report.subsets.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            r#"{{"faces":{},"triangles":{},"outOfRange":{},"alreadyClaimed":{}}}"#,
            subset.faces, subset.triangles, subset.out_of_range, subset.already_claimed
        );
    }
    out.push_str(r#"],"faceNotes":["#);
    for (i, note) in report.face_notes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            r#"{{"face":{},"kind":"{}""#,
            note.face,
            note.kind.code()
        );
        if let FaceNoteKind::ConcaveFellBack(error) = &note.kind {
            out.push_str(r#","error":"#);
            quoted(&mut out, &alloc::format!("{error}"));
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

/// A curves report as the JSON the TypeScript `CurvesReport` reads.
pub(crate) fn curves_json(report: &CurvesReport) -> String {
    let mut out = String::from(r#"{"evaluation":"#);
    match report.evaluation {
        CurveEvaluation::Basis {
            curve_type,
            basis,
            wrap,
        } => {
            let _ = write!(
                out,
                r#"{{"kind":"basis","type":"{}","basis":{},"wrap":"{}"}}"#,
                curve_type.as_str(),
                basis.map_or(String::from("null"), |b| alloc::format!(
                    "\"{}\"",
                    b.as_str()
                )),
                wrap.as_str()
            );
        }
        CurveEvaluation::Nurbs => out.push_str(r#"{"kind":"nurbs"}"#),
        CurveEvaluation::Hermite => out.push_str(r#"{"kind":"hermite"}"#),
    }
    out.push_str(r#","fallbacks":["#);
    for (i, fallback) in report.fallbacks.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, r#"{{"attribute":"{}","token":"#, fallback.attribute);
        quoted(&mut out, &fallback.token);
        let _ = write!(out, r#","used":"{}"}}"#, fallback.used);
    }
    out.push_str(r#"],"segments":["#);
    for (i, segments) in report.segments.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{segments}");
    }
    out.push(']');
    for (name, value) in [
        ("curves", report.curves),
        ("drawnCurves", report.drawn_curves),
        ("samples", report.samples),
        ("vertices", report.vertices),
        ("lines", report.lines),
        ("triangles", report.triangles),
        ("degenerateStations", report.degenerate_stations),
        ("clampedWidths", report.clamped_widths),
    ] {
        let _ = write!(out, r#","{name}":{value}"#);
    }
    match report.geometry {
        CurveGeometry::Lines => out.push_str(r#","geometry":{"kind":"lines"}"#),
        CurveGeometry::Tubes { sides } => {
            let _ = write!(out, r#","geometry":{{"kind":"tubes","sides":{sides}}}"#);
        }
        CurveGeometry::Ribbons => out.push_str(r#","geometry":{"kind":"ribbons"}"#),
    }
    out.push_str(r#","channels":{"widths":"#);
    channel(&mut out, &report.widths);
    out.push_str(r#","normals":"#);
    channel(&mut out, &report.normals);
    out.push_str(r#","displayColor":"#);
    channel(&mut out, &report.colors);
    out.push_str(r#","displayOpacity":"#);
    channel(&mut out, &report.opacities);
    out.push_str(r#"},"notes":["#);
    for (i, note) in report.notes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, r#"{{"curve":{},"#, note.curve);
        match note.kind {
            CurveNoteKind::Skipped { count } => {
                let _ = write!(out, r#""kind":"skipped","count":{count}}}"#);
            }
            CurveNoteKind::Repeated { count } => {
                let _ = write!(out, r#""kind":"repeated","count":{count}}}"#);
            }
            CurveNoteKind::Unused { unused } => {
                let _ = write!(out, r#""kind":"unused","unused":{unused}}}"#);
            }
            CurveNoteKind::Nurbs(error) => {
                // A code, not the error's text: its `Display` costs the
                // module more than the TypeScript's own words for it.
                let code = match error {
                    NurbsError::Order { .. } => "order",
                    NurbsError::TooFewPoints { .. } => "tooFewPoints",
                    NurbsError::KnotCount { .. } => "knotCount",
                    NurbsError::Knots { .. } => "knots",
                    NurbsError::WeightCount { .. } => "weightCount",
                    NurbsError::Weight { .. } => "weight",
                    NurbsError::EmptyDomain => "emptyDomain",
                    NurbsError::EmptyRange => "emptyRange",
                    NurbsError::Point { .. } => "point",
                    // `NurbsError` may grow; a newer reason reads as `other`.
                    _ => "other",
                };
                let _ = write!(out, r#""kind":"nurbs","code":"{code}"}}"#);
            }
        }
    }
    out.push_str(r#"],"problem":"#);
    match report.problem {
        None => out.push_str("null"),
        Some(problem) => {
            let code = match problem {
                CurveProblem::NegativeCount { .. } => "negativeCount",
                CurveProblem::PointCount { .. } => "pointCount",
                CurveProblem::Arrays { .. } => "arrays",
            };
            let reason = match problem {
                CurveProblem::NegativeCount { curve, count } => {
                    alloc::format!("curve {curve} has the negative vertex count {count}")
                }
                CurveProblem::PointCount { expected, found } => {
                    alloc::format!(
                        "the vertex counts sum to {expected}, but there are {found} points"
                    )
                }
                CurveProblem::Arrays { attribute } => {
                    alloc::format!("`{attribute}` does not match the curves")
                }
            };
            let _ = write!(out, r#"{{"code":"{code}","reason":"#);
            quoted(&mut out, &reason);
            out.push('}');
        }
    }
    // The extent leaves through `StagehandCurves::extent`, as numbers:
    // formatting floats as text would cost the module more than the rest.
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use layerstack_stagehand::{MeshInput, MeshOptions, MeshPlan, MeshTopology};

    use super::json;

    #[test]
    fn a_report_is_json_with_every_count() {
        let points = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]];
        let input = MeshInput {
            topology: MeshTopology {
                face_vertex_counts: &[3, 2],
                face_vertex_indices: &[0, 1, 2, 0, 1],
                ..MeshTopology::default()
            },
            points: &points,
            ..MeshInput::default()
        };
        let plan = MeshPlan::new(&input, &MeshOptions::default());
        let text = json(plan.report());
        assert!(
            text.starts_with(
                r#"{"topologyProblem":null,"triangulation":"concave","smoothNormals":"hydra","orientation":"rightHanded""#
            ),
            "{text}"
        );
        assert!(text.contains(r#""faces":2,"drawnFaces":1"#), "{text}");
        assert!(
            text.contains(r#""channels":{"normals":{"state":"absent"}"#),
            "{text}"
        );
        assert!(
            text.ends_with(r#""subsets":[],"faceNotes":[{"face":1,"kind":"degenerate"}]}"#),
            "{text}"
        );
    }
}
