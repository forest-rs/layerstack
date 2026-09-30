// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render-spec computation against the direct OpenUSD C++ API.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::{Stage, StageOptions, Value};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{
    BindingOptions, MaterialPurpose, PrimView, Scene,
    render::{NamespacedSettings, RenderSettingValue},
    usd_render::RenderSettings,
};
use serde_json::{Value as Json, json};
use std::sync::Arc;

fn extra(settings: &NamespacedSettings, scene: &Scene<'_>) -> Json {
    let mut result = serde_json::Map::new();
    for (name, value) in settings {
        let value = match value {
            RenderSettingValue::Paths(paths) => json!(
                paths
                    .iter()
                    .map(|p| p.display(scene.store().paths(), scene.store().tokens()))
                    .collect::<Vec<_>>()
            ),
            RenderSettingValue::Value(Value::Float(v)) => json!(v),
            RenderSettingValue::Value(Value::Int(v)) => json!(v),
            RenderSettingValue::Value(Value::Bool(v)) => json!(v),
            RenderSettingValue::Value(Value::String(v)) => json!(&**v),
            other => panic!("unexpected fixture setting {other:?}"),
        };
        result.insert(name.to_string(), value);
    }
    Json::Object(result)
}

#[test]
fn render_specs_match_openusd() {
    let oracle: Vec<Json> =
        serde_json::from_str(include_str!("../fixtures/render_spec/oracle.json")).unwrap();
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/render_spec/scene.usda"),
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
    let path = loaded.store.path("/Settings");
    let binding_paths =
        ["/BindingCycle", "/BindingEmptyCycle", "/BindingMissing"].map(|p| loaded.store.path(p));
    let scene = Scene::new(&stage, &loaded.store);
    let settings = RenderSettings::new(&scene, path).unwrap();
    let path_name = |p| loaded.store.paths.resolve(p).display(&loaded.store.tokens);
    for expected in oracle {
        let namespaces: Vec<_> = expected["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let actual = settings.compute_spec(&namespaces);
        assert_eq!(actual.problems.len(), 5, "{:?}", actual.problems);
        let products:Vec<_> = actual.products.iter().map(|p|json!({
            "path":path_name(p.path), "type":p.product_type.as_str(), "name":&*p.name,
            "camera":path_name(p.camera), "resolution":p.resolution, "pixel_aspect":p.pixel_aspect_ratio,
            "policy":p.aspect_ratio_conform_policy.as_str(), "aperture":p.aperture_size,
            "window":p.data_window_ndc, "disable_motion":p.disable_motion_blur, "disable_dof":p.disable_depth_of_field,
            "indices":p.render_var_indices,"settings":extra(&p.namespaced_settings,&scene)
        })).collect();
        let vars:Vec<_> = actual.render_vars.iter().map(|v|json!({
            "path":path_name(v.path),"data_type":&*v.data_type,"source_name":&*v.source_name,
            "source_type":v.source_type.as_str(),"settings":extra(&v.namespaced_settings,&scene)
        })).collect();
        let bindings: Vec<_> = expected["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(index, binding)| {
                let text = binding["path"].as_str().unwrap();
                let path = binding_paths[index];
                assert_eq!(path_name(path), text);
                let bound = PrimView::new(scene, path)
                    .compute_bound_material(&MaterialPurpose::All, BindingOptions::default());
                json!({"path": text, "material": bound.material.map(path_name)})
            })
            .collect();
        let actual = json!({"namespaces":namespaces,"products":products,"vars":vars,
            "included":actual.included_purposes.iter().map(|p|&**p).collect::<Vec<_>>(),
            "material":actual.material_binding_purposes.iter().map(|p|&**p).collect::<Vec<_>>(),
            "settings":extra(&actual.namespaced_settings,&scene), "bindings":bindings});
        assert_eq!(actual, expected);
    }
}
