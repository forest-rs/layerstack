// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Mesh topology reads and validation, against OpenUSD call by call.
//!
//! `scripts/mesh_topology_oracle.py` calls C++ `UsdGeomMesh::ValidateTopology`
//! on hand-written cases, and reads each mesh of
//! `fixtures/mesh_topology/scene.usda` at the default time and at times 1
//! and 2: its face counts, face indices, hole indices, orientation,
//! subdivision scheme and point count, and `ValidateTopology` on them. The
//! test replays the cases through `validate_mesh_topology` and the reads
//! through `Mesh::topology`.
//!
//! One difference is deliberate: OpenUSD sums `faceVertexCounts` without
//! checking their signs, so negative counts that sum to the number of
//! indices pass (`[4, -1]` over three indices). Layerstack refuses any
//! negative count (`MeshTopologyError::InvalidFaceCounts`); the cases that
//! show it are named in `NEGATIVE_COUNTS`.

use crate::support::schema_scene as support;

use layerstack::Time;
use layerstack_schemas::Scene;
use layerstack_schemas::subset::{MeshTopologyError, validate_mesh_topology};
use layerstack_schemas::usd_geom::Mesh;
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/mesh_topology/oracle.json");
const SCENE: &str = include_str!("../../fixtures/mesh_topology/scene.usda");

/// The cases where OpenUSD accepts negative face counts and Layerstack does not.
const NEGATIVE_COUNTS: [&str; 2] = [
    "negative counts summing to the index count",
    "negative counts summing to zero",
];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    validate: Vec<Case>,
    meshes: Vec<Read>,
}

#[derive(Deserialize)]
struct Case {
    case: String,
    face_vertex_indices: Vec<i32>,
    face_vertex_counts: Vec<i32>,
    points: usize,
    valid: bool,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct Read {
    mesh: String,
    time: String,
    face_vertex_counts: Option<Vec<i32>>,
    face_vertex_indices: Option<Vec<i32>>,
    hole_indices: Option<Vec<i32>>,
    orientation: String,
    subdivision_scheme: String,
    points: usize,
    valid: bool,
    reason: Option<String>,
}

/// Checks that `result` is the outcome OpenUSD's `reason` describes: the
/// same failure, or none.
fn assert_same_outcome(
    label: &str,
    result: &Result<(), MeshTopologyError>,
    valid: bool,
    reason: Option<&str>,
) {
    assert_eq!(
        result.is_ok(),
        valid,
        "{label}: {result:?} against {reason:?}"
    );
    let Err(error) = result else { return };
    let reason = reason.expect("OpenUSD gives a reason");
    match error {
        MeshTopologyError::SizeMismatch => {
            assert!(
                reason.starts_with("Sum of faceVertexCounts"),
                "{label}: {reason}"
            );
        }
        MeshTopologyError::InvalidVertexIndex(index) => {
            assert!(
                reason.starts_with(&format!("Out of range face vertex index {index}:")),
                "{label}: {reason}"
            );
        }
        // OpenUSD reports a negative count as a sum mismatch, when the
        // counts do not happen to sum to the index count.
        MeshTopologyError::InvalidFaceCounts => {
            assert!(
                reason.starts_with("Sum of faceVertexCounts"),
                "{label}: {reason}"
            );
        }
        other => panic!("{label}: unexpected {other:?}"),
    }
}

#[test]
fn validate_topology_matches_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    for case in &oracle.validate {
        let result = validate_mesh_topology(
            &case.face_vertex_indices,
            &case.face_vertex_counts,
            case.points,
        );
        if NEGATIVE_COUNTS.contains(&case.case.as_str()) {
            assert!(case.valid, "{}: OpenUSD accepts it", case.case);
            assert_eq!(
                result,
                Err(MeshTopologyError::InvalidFaceCounts),
                "{}",
                case.case
            );
            continue;
        }
        assert_same_outcome(&case.case, &result, case.valid, case.reason.as_deref());
    }
}

#[test]
fn mesh_topology_reads_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for read in &oracle.meshes {
        let label = format!("{} at {}", read.mesh, read.time);
        let time = match read.time.as_str() {
            "default" => Time::Default,
            time => Time::at(time.parse().unwrap()),
        };
        let mesh = Mesh::get(&scene, read.mesh.as_str()).unwrap();
        let topology = mesh.topology(time);
        let or_empty = |values: &Option<Vec<i32>>| values.clone().unwrap_or_default();
        assert_eq!(
            *topology.face_vertex_counts,
            or_empty(&read.face_vertex_counts),
            "{label}"
        );
        assert_eq!(
            *topology.face_vertex_indices,
            or_empty(&read.face_vertex_indices),
            "{label}"
        );
        assert_eq!(
            *topology.hole_indices,
            or_empty(&read.hole_indices),
            "{label}"
        );
        assert_eq!(topology.orientation.as_str(), read.orientation, "{label}");
        assert_eq!(
            topology.subdivision_scheme.as_str(),
            read.subdivision_scheme,
            "{label}"
        );
        assert_eq!(topology.point_count, read.points, "{label}");
        assert_same_outcome(
            &label,
            &topology.validation,
            read.valid,
            read.reason.as_deref(),
        );
    }
}
