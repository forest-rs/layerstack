// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Direct connections, node graph tracing and material terminals against
//! OpenUSD 26.08. The oracle calls the public C++ APIs through Python.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::{PropertyPath, Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{
    Scene,
    shading::{MaterialTerminal, ShadingIssue},
    usd_shade::Material,
};
use serde::Deserialize;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Deserialize)]
struct Oracle {
    version: String,
    connections: BTreeMap<String, Connections>,
    materials: Vec<Terminal>,
}
#[derive(Deserialize)]
struct Connections {
    sources: Vec<String>,
    invalid: Vec<String>,
    terminals: Vec<String>,
    values: Vec<String>,
}
#[derive(Deserialize)]
struct Terminal {
    material: String,
    context: String,
    terminal: String,
    selected: Option<String>,
}

#[test]
fn shading_matches_openusd() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/shading/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/shading/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let schemas = Arc::new(layerstack_schemas::openusd(&mut loaded.store.tokens));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    for (name, expected) in oracle.connections {
        let path =
            PropertyPath::parse(&name, &mut loaded.store.tokens, &mut loaded.store.paths).unwrap();
        let scene = Scene::new(&stage, &loaded.store);
        let actual = scene.connected_sources(path);
        let display = |p: PropertyPath| p.display(&loaded.store.paths, &loaded.store.tokens);
        assert_eq!(
            actual
                .sources
                .iter()
                .map(|p| display(*p))
                .collect::<Vec<_>>(),
            expected.sources,
            "sources {name}"
        );
        assert_eq!(
            actual
                .invalid
                .iter()
                .map(|p| p.display(&loaded.store.paths, &loaded.store.tokens))
                .collect::<Vec<_>>(),
            expected.invalid,
            "invalid {name}"
        );
        let values = scene.value_sources(path);
        assert_eq!(
            values
                .sources
                .iter()
                .map(|s| display(s.attribute))
                .collect::<Vec<_>>(),
            expected.values,
            "values {name}"
        );
        for source in &values.sources {
            assert_eq!(source.chain.first(), Some(&path));
            assert_eq!(source.chain.last(), Some(&source.attribute));
        }
        let traced = scene.shader_sources(path);
        assert_eq!(
            traced
                .sources
                .iter()
                .map(|s| display(s.output))
                .collect::<Vec<_>>(),
            expected.terminals,
            "terminals {name}"
        );
        for source in traced.sources {
            assert_eq!(source.chain.first(), Some(&path));
            assert_eq!(source.chain.last(), Some(&source.output));
        }
    }
    for expected in oracle.materials {
        let path = loaded.store.path(&expected.material);
        let scene = Scene::new(&stage, &loaded.store);
        let material = Material::new(&scene, path).unwrap();
        let terminal = match expected.terminal.as_str() {
            "surface" => MaterialTerminal::Surface,
            "displacement" => MaterialTerminal::Displacement,
            "volume" => MaterialTerminal::Volume,
            _ => unreachable!(),
        };
        let actual = material.compute_terminal_source(terminal, &[&expected.context]);
        let selected = actual
            .selected()
            .map(|s| s.output.display(&loaded.store.paths, &loaded.store.tokens));
        let selected = selected.filter(|_| actual.shader.is_some());
        assert_eq!(
            selected, expected.selected,
            "{} {} {}",
            expected.material, expected.context, expected.terminal
        );
    }
}

#[test]
fn contexts_and_diagnostics_are_inspectable() {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/shading/scene.usda"),
    );
    let contexts = loaded.store.path("/Contexts");
    let multiple = loaded.store.path("/Multiple");
    let invalid = loaded.store.path("/Invalid");
    let only_ri = loaded.store.path("/OnlyRi");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut loaded.store.tokens));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let scene = Scene::new(&stage, &loaded.store);
    let material = Material::new(&scene, contexts).unwrap();
    let result = material.compute_surface_source(&["neverInterned", "bad", "ri"]);
    assert_eq!(result.context.as_deref(), Some("ri"));
    assert!(
        result
            .trace
            .issues
            .iter()
            .any(|issue| matches!(issue, ShadingIssue::Cycle(_)))
    );
    assert!(
        result
            .trace
            .dependencies
            .iter()
            .any(|d| d.prim == contexts && d.property == "outputs:neverInterned:surface")
    );
    assert_eq!(
        result
            .selected()
            .unwrap()
            .output
            .display(&loaded.store.paths, &loaded.store.tokens),
        "/T.outputs:out"
    );
    assert_eq!(
        material
            .compute_surface_source(&["", "ri"])
            .context
            .as_deref(),
        Some("")
    );
    assert_eq!(
        material.compute_surface_source(&[]).context.as_deref(),
        Some("")
    );
    let ri = Material::new(&scene, only_ri).unwrap();
    assert!(ri.compute_surface_source(&["", "ri"]).selected().is_none());
    assert_eq!(
        ri.compute_surface_source(&["ri"]).context.as_deref(),
        Some("ri")
    );
    let result = Material::new(&scene, multiple)
        .unwrap()
        .compute_surface_source(&[]);
    assert_eq!(
        result.trace.sources.len(),
        2,
        "diamond branches must not look cyclic"
    );
    assert!(result.trace.issues.is_empty());
    let result = Material::new(&scene, invalid)
        .unwrap()
        .compute_surface_source(&[]);
    assert_eq!(result.trace.issues.len(), 4);
    assert!(
        result
            .trace
            .issues
            .iter()
            .any(|issue| matches!(issue, ShadingIssue::NonContainerInput(_)))
    );
}

#[test]
fn retarget_and_undo_preserve_source_and_dependency_evidence() {
    use layerstack::{
        ListOp, LiveStage, TargetPath,
        edit::{EditTarget, Transaction},
    };
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/shading/scene.usda"),
    );
    let material = loaded.store.path("/Through");
    let graph_input = PropertyPath::parse(
        "/Graph.inputs:in",
        &mut loaded.store.tokens,
        &mut loaded.store.paths,
    )
    .unwrap();
    let new_output = PropertyPath::parse(
        "/T.outputs:out",
        &mut loaded.store.tokens,
        &mut loaded.store.paths,
    )
    .unwrap();
    let schemas = Arc::new(layerstack_schemas::openusd(&mut loaded.store.tokens));
    let mut live = LiveStage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let read = |stage: &Stage, store: &layerstack::InMemoryStore| {
        Material::new(&Scene::new(stage, store), material)
            .unwrap()
            .compute_surface_source(&[])
    };
    let before = read(live.stage(), &loaded.store);
    assert!(
        before
            .trace
            .dependencies
            .iter()
            .any(|d| d.prim == graph_input.prim_path() && d.property == "inputs:in")
    );
    let mut transaction = Transaction::new();
    transaction.set_targets(
        EditTarget::for_layer(loaded.root_layer).property(graph_input),
        ListOp::explicit(vec![TargetPath::Property(new_output)]),
    );
    let applied = live.apply(&mut loaded.store, &transaction).unwrap();
    let after = read(live.stage(), &loaded.store);
    assert_eq!(after.selected().unwrap().output, new_output);
    assert_eq!(after.selected().unwrap().chain.len(), 4);
    live.apply(&mut loaded.store, &applied.inverse).unwrap();
    assert_eq!(read(live.stage(), &loaded.store), before);
}

#[test]
fn authored_providers_are_not_evaluated_until_a_time_is_requested() {
    use layerstack_schemas::{
        Time,
        shading::{Port, ValueSourceKind},
    };
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/shading/scene.usda"),
    );
    let schemas = Arc::new(layerstack_schemas::openusd(&mut loaded.store.tokens));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let reader = loaded.store.path("/Values/Reader");
    let scene = Scene::new(&stage, &loaded.store);
    let view = layerstack_schemas::usd_shade::Shader::new(&scene, reader).unwrap();
    let result = view.input("animated").unwrap().value_sources();
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.sources[0].kind, ValueSourceKind::AuthoredValue);
    let provider = Port::get(&scene, result.sources[0].attribute).unwrap();
    assert_eq!(provider.value(Time::Default), None);
    assert_eq!(
        provider.value(Time::at(0.0)),
        Some(layerstack::Value::Float(1.0))
    );
    assert_eq!(
        provider.value(Time::at(1.0)),
        Some(layerstack::Value::Float(2.0))
    );
    assert!(
        view.input("blocked")
            .unwrap()
            .value_sources()
            .sources
            .is_empty()
    );
    let constant = view.input("outputConstant").unwrap().value_sources();
    assert_eq!(constant.sources[0].kind, ValueSourceKind::AuthoredValue);
}
