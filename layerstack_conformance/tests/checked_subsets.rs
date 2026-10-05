// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checked topology and subset queries retain deferred source errors.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    ArrayReadError, DeferredArraySource, EditTarget, InMemoryStore, LayerId, LiveStage,
    PropertyPath, TypedArray, Value,
};
use layerstack_schemas::{
    Scene, Time,
    subset::{MeshTopologyError, SubsetError, SubsetProblemKind},
    usd_geom::{GeomSubsetElementType as Element, Imageable, Mesh},
};
use std::sync::Arc;
#[path = "support/schema_scene.rs"]
mod support;

const MESH: &str = r#"#usda 1.0
def Mesh "M" {
    point3f[] points = [(0,0,0),(1,0,0),(0,1,0)]
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0,1,2]
    uniform token subsetFamily:faces:familyType = "partition"
    def GeomSubset "S" {
        uniform token elementType = "face"
        uniform token familyName = "faces"
        int[] indices = [0]
    }
}
"#;
const TET: &str = r#"#usda 1.0
def TetMesh "M" {
    int3[] surfaceFaceVertexIndices = [(0,1,2)]
    int4[] tetVertexIndices = [(0,1,2,3)]
    def GeomSubset "S" {
        uniform token elementType = "face"
        uniform token familyName = "faces"
        int[] indices = [0]
    }
}
"#;
const CURVES: &str = r#"#usda 1.0
def BasisCurves "M" {
    int[] curveVertexCounts = [4]
    def GeomSubset "S" {
        uniform token elementType = "segment"
        uniform token familyName = "faces"
        int[] indices = [0,0]
    }
}
"#;

#[derive(Debug)]
struct Failed {
    kind: Value,
    error: ArrayReadError,
}
impl DeferredArraySource for Failed {
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
        Err(&self.error)
    }
    fn element_kind(&self) -> Value {
        self.kind.clone()
    }
}
fn failed(kind: Value, error: &ArrayReadError) -> Value {
    Value::TypedArray(TypedArray::Deferred(Arc::new(Failed {
        kind,
        error: error.clone(),
    })))
}
fn failure_scene(
    source: &str,
    name: &str,
    kind: Value,
    error: &ArrayReadError,
    time: Time,
) -> (InMemoryStore, LiveStage, PropertyPath) {
    let (mut store, mut live) = support::scene(source);
    let property = store.property_path(name);
    let address = EditTarget::for_layer(LayerId(1)).property(property);
    let mut transaction = layerstack::Transaction::new();
    let value = failed(kind, error);
    match time {
        Time::Default => {
            transaction.set_default(address, value);
        }
        Time::At { .. } => {
            transaction.set_time_sample(address.clone(), 1., value.clone());
            transaction.set_time_sample(address, 2., value);
        }
    }
    live.apply(&mut store, &transaction).unwrap();
    (store, live, property)
}

#[test]
fn subset_members_preserve_errors_in_unassigned_and_both_checked_validations() {
    for time in [Time::Default, Time::held(1.), Time::at(1.5)] {
        let error = ArrayReadError::BudgetExceeded { limit: 19 };
        let (mut store, live, property) =
            failure_scene(MESH, "/M/S.indices", Value::Int(0), &error, time);
        let mesh_path = store.path("/M");
        let scene = Scene::new(live.stage(), &store);
        let geometry = Imageable::new(&scene, mesh_path).unwrap();
        let expected = SubsetError::Decode { property, error };
        assert_eq!(
            geometry.unassigned_subset_indices(&Element::Face, "faces", time),
            Err(expected.clone()),
            "failed members cannot silently assign every face to the fallback"
        );
        assert_eq!(
            geometry.try_validate_subset_family_at(&Element::Face, "faces", time),
            Err(expected.clone()),
            "snapshot validation preserves the selected sample's error"
        );
        assert_eq!(
            geometry.try_validate_subset_family(&Element::Face, "faces"),
            Err(expected),
            "all-sample validation reaches failed index samples"
        );
        let convenience = geometry.validate_subset_family_at(&Element::Face, "faces", time);
        assert!(
            !convenience.is_valid(),
            "lossy validation remains conservative"
        );
        assert_eq!(
            convenience.problems[0].kind,
            SubsetProblemKind::InvalidTopology,
            "documented convenience reporting does not treat failed arrays as empty"
        );
        assert_eq!(
            convenience.problems[0].time, time,
            "snapshot time is retained"
        );
    }
}

#[test]
fn subset_queries_preserve_errors_for_every_supported_topology_array() {
    let cases = [
        (
            MESH.into(),
            Element::Face,
            "/M.faceVertexCounts",
            Value::Int(0),
        ),
        (
            MESH.replace("\"face\"", "\"point\""),
            Element::Point,
            "/M.points",
            Value::Vec3f([0.; 3]),
        ),
        (
            MESH.replace("\"face\"", "\"edge\""),
            Element::Edge,
            "/M.faceVertexIndices",
            Value::Int(0),
        ),
        (
            MESH.replace("\"face\"", "\"edge\""),
            Element::Edge,
            "/M.points",
            Value::Vec3f([0.; 3]),
        ),
        (
            TET.into(),
            Element::Face,
            "/M.surfaceFaceVertexIndices",
            Value::Vec3i([0; 3]),
        ),
        (
            TET.replace("\"face\"", "\"tetrahedron\""),
            Element::Tetrahedron,
            "/M.tetVertexIndices",
            Value::Vec4i([0; 4]),
        ),
        (
            CURVES.into(),
            Element::Segment,
            "/M.curveVertexCounts",
            Value::Int(0),
        ),
    ];
    for (source, element, name, kind) in cases {
        for time in [Time::Default, Time::held(1.), Time::at(1.5)] {
            let error = ArrayReadError::InvalidData(format!("failed {name}").into());
            let (mut store, live, property) =
                failure_scene(&source, name, kind.clone(), &error, time);
            let path = store.path("/M");
            let scene = Scene::new(live.stage(), &store);
            let geometry = Imageable::new(&scene, path).unwrap();
            let expected = SubsetError::Decode { property, error };
            assert_eq!(
                geometry.unassigned_subset_indices(&element, "faces", time),
                Err(expected.clone()),
                "{name} at {time:?} cannot become missing topology"
            );
            assert_eq!(
                geometry.try_validate_subset_family_at(&element, "faces", time),
                Err(expected),
                "{name} at {time:?} preserves decode failure during validation"
            );
        }
    }
}

#[test]
fn mesh_validation_distinguishes_every_failed_array_from_missing_topology() {
    for (name, kind) in [
        ("/M.points", Value::Vec3f([0.; 3])),
        ("/M.faceVertexCounts", Value::Int(0)),
        ("/M.faceVertexIndices", Value::Int(0)),
    ] {
        for time in [Time::Default, Time::held(1.), Time::at(1.5)] {
            let error = ArrayReadError::BudgetExceeded { limit: 23 };
            let (mut store, live, property) = failure_scene(MESH, name, kind.clone(), &error, time);
            let path = store.path("/M");
            let scene = Scene::new(live.stage(), &store);
            let mesh = Mesh::new(&scene, path).unwrap();
            assert_eq!(
                mesh.validate_topology(time),
                Err(MeshTopologyError::Decode { property, error }),
                "{name} at {time:?} retains its original error"
            );
        }
    }
}

#[test]
fn checked_validation_retains_ordinary_missing_topology_as_a_problem() {
    let (mut store, live) = support::scene("#usda 1.0\ndef Mesh \"M\" {}");
    let path = store.path("/M");
    let scene = Scene::new(live.stage(), &store);
    let geometry = Imageable::new(&scene, path).unwrap();
    let validation = geometry
        .try_validate_subset_family(&Element::Face, "faces")
        .unwrap();
    assert!(
        validation
            .problems
            .iter()
            .any(|problem| problem.kind == SubsetProblemKind::InvalidTopology),
        "ordinary missing topology is a successful validation with problems"
    );
    assert_eq!(
        Mesh::new(&scene, path)
            .unwrap()
            .validate_topology(Time::Default),
        Err(MeshTopologyError::MissingAttribute),
        "ordinary missing mesh storage remains distinguishable"
    );
}

#[test]
fn topology_reads_skip_incompatible_defaults_but_keep_numeric_source_selection() {
    for decode_failure in [false, true] {
        let (mut store, _) = support::scene(MESH);
        let path = store.path("/M");
        let property = store.property_path("/M.faceVertexCounts");
        let mut weak = store.layers.remove(&LayerId(1)).unwrap();
        weak.id = LayerId(2);
        let error = ArrayReadError::InvalidData("failed weaker topology".into());
        if decode_failure {
            weak.prims
                .get_mut(&path)
                .unwrap()
                .property_mut(property.property())
                .unwrap()
                .default = Some(failed(Value::Int(0), &error));
        }
        store.insert_layer(weak);
        let mut root = layerstack::Layer::new(LayerId(1));
        root.sublayers
            .push(layerstack::SublayerEntry::new(LayerId(2)));
        root.insert_prim(
            path,
            layerstack::PrimSpec::over().with_property(
                property.property(),
                layerstack::PropertySpec::attribute()
                    .with_default(Value::String("incompatible".into())),
            ),
        );
        store.insert_layer(root);
        let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
        let stage = layerstack::Stage::compose(
            &mut store,
            LayerId(1),
            layerstack::StageOptions {
                schemas: Some(schemas),
                ..Default::default()
            },
        );
        let scene = Scene::new(&stage, &store);
        let geometry = Imageable::new(&scene, path).unwrap();
        let mesh = Mesh::new(&scene, path).unwrap();
        if decode_failure {
            assert_eq!(
                mesh.validate_topology(Time::Default),
                Err(MeshTopologyError::Decode {
                    property,
                    error: error.clone(),
                }),
                "typed default retry must not swallow the weaker decoder error"
            );
            assert_eq!(
                geometry.unassigned_subset_indices(&Element::Face, "faces", Time::Default),
                Err(SubsetError::Decode { property, error }),
                "subset queries keep the same typed default error"
            );
            assert!(
                matches!(
                    geometry.try_validate_subset_family_at(&Element::Face, "faces", Time::Default),
                    Err(SubsetError::Decode { property: failed_property, .. }) if failed_property == property
                ),
                "checked default snapshot validation also reports the weaker source"
            );
        } else {
            assert_eq!(
                mesh.validate_topology(Time::Default),
                Ok(()),
                "incompatible stronger default is skipped"
            );
            assert_eq!(
                geometry.unassigned_subset_indices(&Element::Face, "faces", Time::Default),
                Ok(vec![]),
                "compatible weaker topology supplies the face count"
            );
            assert!(
                geometry
                    .try_validate_subset_family_at(&Element::Face, "faces", Time::Default)
                    .unwrap()
                    .is_valid(),
                "default snapshot validation uses compatible weaker topology"
            );
        }
        assert_eq!(
            mesh.validate_topology(Time::held(1.)),
            Err(MeshTopologyError::MissingAttribute),
            "numeric source selection does not retry a weaker default"
        );
        assert_eq!(
            geometry.unassigned_subset_indices(&Element::Face, "faces", Time::held(1.)),
            Err(SubsetError::MissingTopology),
            "numeric subset reads keep the selected incompatible source"
        );
    }
}
