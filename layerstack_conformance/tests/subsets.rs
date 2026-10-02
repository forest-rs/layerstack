// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Subset family and mesh checks against reference behavior and malformed storage.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{
    Scene, Time,
    subset::{MeshTopologyError, validate_mesh_topology},
    usd_geom::{GeomSubsetElementType as Element, Imageable},
};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    path: String,
    element: String,
    valid: bool,
    reason: String,
    subsets: Vec<String>,
    family_type: String,
    unassigned: Vec<i32>,
}
fn element(s: &str) -> Element {
    match s {
        "face" => Element::Face,
        "point" => Element::Point,
        "edge" => Element::Edge,
        "segment" => Element::Segment,
        "tetrahedron" => Element::Tetrahedron,
        _ => panic!("unknown element"),
    }
}
#[test]
fn family_validation_and_discovery_match_openusd() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/subsets/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, live) = support::scene(include_str!("../fixtures/subsets/scene.usda"));
    for row in oracle.rows {
        let p = store.path(&row.path);
        let scene = Scene::new(live.stage(), &store);
        let g = Imageable::new(&scene, p).unwrap();
        let e = element(&row.element);
        assert_eq!(
            g.validate_subset_family(&e, "materialBind").is_valid(),
            row.valid,
            "{}: {}",
            row.path,
            row.reason
        );
        assert_eq!(g.subset_family_type("materialBind"), row.family_type);
        assert_eq!(
            g.geom_subsets(None, Some("materialBind"))
                .into_iter()
                .map(|s| store.paths.display(s.path(), &store.tokens))
                .collect::<Vec<_>>(),
            row.subsets
        );
        assert_eq!(
            g.unassigned_subset_indices(&e, "materialBind", Time::at(0.))
                .unwrap(),
            row.unassigned,
            "{}",
            row.path
        );
    }
}
#[test]
fn malformed_counts_and_unsorted_edges_are_safe_and_deterministic() {
    assert_eq!(validate_mesh_topology(&[0, 1, 2], &[3], 3), Ok(()));
    assert_eq!(
        validate_mesh_topology(&[], &[-1], 3),
        Err(MeshTopologyError::InvalidFaceCounts)
    );
    assert_eq!(
        validate_mesh_topology(&[0], &[2], 3),
        Err(MeshTopologyError::SizeMismatch)
    );
    assert_eq!(
        validate_mesh_topology(&[3], &[1], 3),
        Err(MeshTopologyError::InvalidVertexIndex(3))
    );
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef Mesh \"M\" {\n point3f[] points = [(0,0,0),(1,0,0),(0,1,0)]\n int[] faceVertexCounts = [3]\n int[] faceVertexIndices = [0,1,2]\n def GeomSubset \"S\" {\n uniform token elementType = \"edge\"\n uniform token familyName = \"edges\"\n int[] indices = [1,2,0,1]\n }\n}\n",
    );
    let path = store.path("/M");
    let scene = Scene::new(live.stage(), &store);
    let g = Imageable::new(&scene, path).unwrap();
    assert_eq!(
        g.unassigned_subset_indices(&Element::Edge, "edges", Time::Default)
            .unwrap(),
        [0, 2]
    );
    assert!(
        g.validate_subset_family(&Element::Other("bogus".into()), "edges")
            .problems
            .iter()
            .any(|p| p.kind == layerstack_schemas::subset::SubsetProblemKind::InvalidGeometry)
    );
}

#[test]
fn incomplete_pairs_do_not_merge_between_subsets() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def Mesh "M" {
 point3f[] points = [(0,0,0),(1,0,0),(0,1,0)]
 int[] faceVertexCounts = [3]
 int[] faceVertexIndices = [0,1,2]
 def GeomSubset "A" {
  uniform token elementType = "edge"
  int[] indices = [0]
 }
 def GeomSubset "B" {
  uniform token elementType = "edge"
  int[] indices = [1,2]
 }
}
"#,
    );
    let path = store.path("/M");
    let scene = Scene::new(live.stage(), &store);
    let g = Imageable::new(&scene, path).unwrap();
    assert_eq!(
        g.unassigned_subset_indices(&Element::Edge, "", Time::Default)
            .unwrap(),
        [0, 1, 0, 2]
    );
    assert!(!g.validate_subset_family(&Element::Edge, "").is_valid());
}
