// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in light and filter connectability against C++.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{PathInterner, PropertyPath, TokenInterner};
use layerstack_schemas::{PrimView, Scene};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
    providers: Vec<String>,
}
#[derive(Deserialize)]
struct Row {
    destination: String,
    source: String,
    allowed: bool,
}
fn property(paths: &mut PathInterner, tokens: &mut TokenInterner, text: &str) -> PropertyPath {
    let (prim, name) = text.split_once('.').unwrap();
    PropertyPath::new(
        paths.intern(layerstack::Path::parse_absolute(prim, tokens).unwrap()),
        tokens.intern(name),
    )
}
#[test]
fn connection_policy_matches_cpp_across_scopes_interfaces_and_container_kinds() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/lux_connections.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, live) = support::scene(include_str!("../fixtures/lux_connections.usda"));
    for row in oracle.rows {
        let dest = property(&mut store.paths, &mut store.tokens, &row.destination);
        let source = property(&mut store.paths, &mut store.tokens, &row.source);
        let scene = Scene::new(live.stage(), &store);
        assert_eq!(
            scene.validate_shading_connection(dest, source).is_ok(),
            row.allowed,
            "{} <- {}",
            row.destination,
            row.source
        );
    }
    let light = store.path("/Light");
    let scene = Scene::new(live.stage(), &store);
    let providers = PrimView::new(scene, light)
        .input("a")
        .unwrap()
        .value_sources();
    assert!(providers.issues.is_empty());
    assert_eq!(
        providers
            .sources
            .iter()
            .map(|s| s.attribute.display(&store.paths, &store.tokens))
            .collect::<Vec<_>>(),
        oracle.providers
    );
}
