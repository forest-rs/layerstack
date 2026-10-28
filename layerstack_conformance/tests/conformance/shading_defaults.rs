// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An unknown `info:implementationSource` resolves by `info:id`, and an
//! empty `connectability` is `full`, as OpenUSD 26.11 reads them.
use crate::support::schema_scene as support;
use layerstack::Time;
use layerstack_schemas::{
    PrimView, Scene,
    shading::{
        MaterialNetwork, MaterialTerminal, nodes::PreviewSurface,
        preview_surface::PreviewSurfaceNetwork,
    },
    usd_shade::{Material, NodeDefApi},
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    nodes: Vec<Node>,
    surfaces: Vec<Surface>,
    connections: Vec<Connection>,
}
#[derive(Deserialize)]
struct Node {
    path: String,
    authored: Option<String>,
    effective: String,
}
#[derive(Deserialize)]
struct Surface {
    material: String,
    surface: Option<String>,
}
#[derive(Deserialize)]
struct Connection {
    destination: String,
    source: String,
    can_connect: bool,
}

fn oracle() -> Oracle {
    serde_json::from_str(include_str!("../../fixtures/shading_defaults/oracle.json")).unwrap()
}

#[test]
fn unknown_implementation_source_resolves_by_id_as_cpp_does() {
    let oracle = oracle();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(include_str!("../../fixtures/shading_defaults/scene.usda"));
    let scene = Scene::new(live.stage(), &store);
    for node in &oracle.nodes {
        let view = NodeDefApi::from_prim_unchecked(PrimView::new(scene, store.path(&node.path)));
        assert_eq!(
            view.implementation_source()
                .as_ref()
                .map(|source| source.as_str()),
            node.authored.as_deref(),
            "{}: the authored token stays visible",
            node.path
        );
        assert_eq!(
            view.effective_implementation_source().as_str(),
            node.effective,
            "{}",
            node.path
        );
    }
    for row in &oracle.surfaces {
        let material = store.path(&row.material);
        let source = Material::new(&scene, material)
            .unwrap()
            .compute_surface_source(&[]);
        let surface = source
            .shader
            .map(|p| store.paths().display(p, store.tokens()));
        assert_eq!(
            surface.as_deref(),
            row.surface.as_deref(),
            "{}",
            row.material
        );
        // C++ resolves the selected shader by `info:id` whenever its source
        // falls back to `id`, so an engine still draws the Preview Surface.
        let by_id = oracle
            .nodes
            .iter()
            .find(|node| Some(&node.path) == row.surface.as_ref())
            .is_some_and(|node| node.effective == "id");
        let surface_path = source.shader.unwrap();
        assert_eq!(
            PreviewSurface::new(&scene, surface_path).is_some(),
            by_id,
            "{}",
            row.material
        );
        let network = MaterialNetwork::capture(
            &scene,
            material,
            MaterialTerminal::Surface,
            &[],
            Time::Default,
        )
        .unwrap();
        let node = network.node(surface_path).unwrap();
        assert_eq!(node.implemented_by_id(), by_id, "{}", row.material);
        let preview = PreviewSurfaceNetwork::capture(Arc::new(network), 16);
        assert_eq!(
            preview.surface,
            by_id.then_some(surface_path),
            "{}",
            row.material
        );
    }
    // The texture whose source is also unknown is followed by its `info:id`.
    let unknown = store.path("/Unknown");
    let network = MaterialNetwork::capture(
        &scene,
        unknown,
        MaterialTerminal::Surface,
        &[],
        Time::Default,
    )
    .unwrap();
    let preview = PreviewSurfaceNetwork::capture(Arc::new(network), 16);
    assert!(preview.is_complete(), "{:?}", preview.issues);
    assert!(
        preview
            .input("emissiveColor")
            .is_some_and(|input| input.expression.is_some())
    );
    assert!(
        preview
            .input("diffuseColor")
            .is_some_and(|input| input.expression.is_some())
    );
}

#[test]
fn empty_connectability_is_full_as_cpp_reads_it() {
    let oracle = oracle();
    let (store, live) = support::scene(include_str!("../../fixtures/shading_defaults/scene.usda"));
    let scene = Scene::new(live.stage(), &store);
    for row in &oracle.connections {
        let destination = store.property_path(&row.destination);
        let source = store.property_path(&row.source);
        assert_eq!(
            scene
                .validate_shading_connection(destination, source)
                .is_ok(),
            row.can_connect,
            "{} <- {}",
            row.destination,
            row.source
        );
    }
}
