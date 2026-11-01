// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Model draw modes resolve as C++ resolves them.
//!
//! `fixtures/model_draw_mode/oracle.json` is what
//! `scripts/model_draw_mode_oracle.py` records with OpenUSD's
//! `UsdGeomModelAPI::ComputeModelDrawMode`, called on every prim once per
//! parent mode (none, which walks the ancestors, and explicit ones).
use crate::support::schema_scene as support;
use layerstack_schemas::Scene;
use serde::Deserialize;
use std::collections::BTreeMap;

const SCENE: &str = include_str!("../../fixtures/model_draw_mode/scene.usda");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    prims: BTreeMap<String, Prim>,
}
#[derive(Deserialize)]
struct Prim {
    model: bool,
    modes: BTreeMap<String, String>,
}

#[test]
fn draw_modes_resolve_as_cpp_resolves_them() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../../fixtures/model_draw_mode/oracle.json")).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    assert!(oracle.prims.len() > 10);
    for (path, expected) in &oracle.prims {
        let id = store.path(path);
        assert_eq!(scene.is_model(id), expected.model, "{path} is a model");
        for (parent, mode) in &expected.modes {
            let parent_mode = (!parent.is_empty()).then_some(parent.as_str());
            assert_eq!(
                &scene.compute_model_draw_mode(id, parent_mode),
                mode,
                "{path} with parent {parent:?}"
            );
        }
    }
}
