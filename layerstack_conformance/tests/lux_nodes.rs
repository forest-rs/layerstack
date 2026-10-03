// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Intrinsic light-node descriptions compared with the complete C++ Sdr catalogue.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{TokenInterner, Value};
use layerstack_schemas::{light::builtin_light_node_definitions, shading::PortKind};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    identifier: String,
    source_type: String,
    context: String,
    subdomain: String,
    ports: Vec<Port>,
}
#[derive(Deserialize)]
struct Port {
    name: String,
    kind: String,
    usd_type: String,
    default: serde_json::Value,
    asset_identifier: bool,
}
fn json(value: Option<&Value>, tokens: &TokenInterner) -> serde_json::Value {
    match value {
        None => serde_json::Value::Null,
        Some(Value::Bool(v)) => serde_json::json!(v),
        Some(Value::Float(v)) => serde_json::json!(f64::from(*v)),
        Some(Value::Vec3f(v)) => serde_json::json!(v.map(f64::from)),
        Some(Value::Token(v)) => serde_json::json!(tokens.resolve(*v)),
        Some(Value::Asset(v) | Value::String(v)) => serde_json::json!(v.as_ref()),
        other => panic!("unexpected catalogue default {other:?}"),
    }
}
#[test]
fn entire_discovered_catalogue_matches_cpp_names_usd_types_defaults_and_metadata() {
    let oracle: Oracle = serde_json::from_str(include_str!("../fixtures/lux_nodes.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let mut tokens = TokenInterner::default();
    let nodes = builtin_light_node_definitions(&mut tokens);
    assert_eq!(
        nodes.iter().map(|n| n.identifier).collect::<Vec<_>>(),
        oracle
            .nodes
            .iter()
            .map(|n| n.identifier.as_str())
            .collect::<Vec<_>>()
    );
    for (actual, expected) in nodes.iter().zip(oracle.nodes) {
        assert_eq!(actual.source_type, expected.source_type);
        assert_eq!(actual.context, expected.context);
        assert_eq!(actual.subdomain, expected.subdomain);
        assert_eq!(actual.discovery_type, "usd-schema-gen");
        assert_eq!(
            actual.ports.len(),
            expected.ports.len(),
            "{}",
            actual.identifier
        );
        for (port, expected) in actual.ports.iter().zip(expected.ports) {
            assert_eq!(port.name, expected.name, "{}", actual.identifier);
            assert_eq!(
                port.kind,
                if expected.kind == "input" {
                    PortKind::Input
                } else {
                    PortKind::Output
                }
            );
            assert_eq!(port.property_type.type_name.as_ref(), expected.usd_type);
            assert_eq!(
                json(port.default.as_ref(), &tokens),
                expected.default,
                "{} {}",
                actual.identifier,
                port.name
            );
            assert_eq!(port.asset_identifier, expected.asset_identifier);
        }
    }
}
#[test]
fn catalogue_preserves_schema_origin_and_separates_prim_and_node_identity() {
    let mut tokens = TokenInterner::default();
    let nodes = builtin_light_node_definitions(&mut tokens);
    assert!(
        !nodes
            .iter()
            .any(|node| node.identifier == "PluginLight" || node.identifier == "LightFilter")
    );
    let mesh = nodes
        .iter()
        .find(|node| node.identifier == "MeshLight")
        .unwrap();
    assert_eq!(mesh.schema, "MeshLightAPI");
    let sphere = nodes
        .iter()
        .find(|node| node.identifier == "SphereLight")
        .unwrap();
    for (name, source) in [
        ("color", "LightAPI"),
        ("radius", "SphereLight"),
        ("shadow:enable", "ShadowAPI"),
        ("shaping:ies:file", "ShapingAPI"),
    ] {
        let port = sphere.ports.iter().find(|port| port.name == name).unwrap();
        assert_eq!(port.source_schema, source);
        assert_eq!(port.property, format!("inputs:{name}"));
    }
    let ies = sphere
        .ports
        .iter()
        .find(|port| port.name == "shaping:ies:file")
        .unwrap();
    assert!(ies.asset_identifier);
    assert_eq!(ies.default, None);
}
