// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ConnectionSourceInfo::new` against OpenUSD's
//! `UsdShadeConnectionSourceInfo(stage, sourcePath)`, path by path.
//!
//! `scripts/connection_source_info_oracle.py` records, for each path text
//! on `fixtures/connection_source_info/scene.usda`, whether the info is
//! valid, its name, its type (`Input`, `Output` or `Invalid`), its type
//! name and its source prim. layerstack must give the same for each, but
//! for a relational attribute path, which it reads as no property path
//! (see `ConnectionSourceInfo`).
//!
//! Spec: AOUSD Core §7.6.4.2.3 (connections), §8.5 (path grammar).

use crate::support::schema_scene as support;

use layerstack_schemas::Scene;
use layerstack_schemas::shading::{ConnectionSourceInfo, PortKind};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/connection_source_info/oracle.json");
const SCENE: &str = include_str!("../../fixtures/connection_source_info/scene.usda");

/// The path OpenUSD reads as `inputs:y` of `/Mat/Source`, where layerstack
/// reads no property path.
const RELATIONAL_ATTRIBUTE: &str = "/Mat/Source.outputs:out[/X].inputs:y";

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    sources: Vec<Source>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Source {
    path: String,
    valid: bool,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    type_name: String,
    source: String,
}

#[test]
fn source_infos_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    assert!(oracle.sources.len() >= 30, "every path is recorded");
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for expected in &oracle.sources {
        let info = ConnectionSourceInfo::new(&scene, &expected.path);
        let actual = Source {
            path: expected.path.clone(),
            valid: info.is_valid(),
            name: info.name().into(),
            kind: match info.kind() {
                Some(PortKind::Input) => "Input",
                Some(PortKind::Output) => "Output",
                None => "Invalid",
            }
            .into(),
            type_name: info
                .type_name()
                .map(|ty| ty.type_name().to_owned())
                .unwrap_or_default(),
            source: info
                .prim()
                .map(|prim| store.paths().display(prim, store.tokens()))
                .unwrap_or_default(),
        };
        if expected.path == RELATIONAL_ATTRIBUTE {
            assert!(expected.valid, "OpenUSD reads it as a source");
            assert_eq!(info, ConnectionSourceInfo::default(), "{}", expected.path);
            continue;
        }
        assert_eq!(&actual, expected, "{}", expected.path);
        // A valid info is the source a connection takes.
        let source = info.source();
        assert_eq!(source.is_some(), expected.valid, "{}", expected.path);
        if let Some(source) = source {
            let prefix = if expected.kind == "Input" {
                "inputs:"
            } else {
                "outputs:"
            };
            assert_eq!(
                source.name(),
                format!("{prefix}{}", expected.name),
                "{}",
                expected.path
            );
            assert_eq!(Some(source.prim()), info.prim());
        }
    }
}
