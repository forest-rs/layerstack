// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An asset-valued namespaced render setting keeps the layer that authors
//! it, which OpenUSD 26.11 resolves it next to.
use crate::support::schema_scene as support;
use layerstack::{LayerId, Value};
use layerstack_schemas::{Scene, render::RenderSettingValue};
use serde::Deserialize;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    setting: String,
    authored_path: String,
    authoring_layer: String,
}

#[test]
fn asset_settings_keep_their_authoring_layer() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../../fixtures/render_assets/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    // The root layer, `LayerId(1)`, sublayers `sub/settings.usda` as
    // `LayerId(2)`, as `root.usda` does.
    assert_eq!(oracle.authoring_layer, "sub/settings.usda");
    let (store, live) = support::layered_scene(include_str!(
        "../../fixtures/render_assets/sub/settings.usda"
    ));
    let scene = Scene::new(live.stage(), &store);
    let settings = scene.render_namespaced_settings(store.path("/Render/Settings"), &[]);
    let Some(RenderSettingValue::Asset(asset)) = settings.get(oracle.setting.as_str()) else {
        panic!("{:?}", settings.get(oracle.setting.as_str()));
    };
    assert_eq!(asset.authored_path, oracle.authored_path);
    assert_eq!(
        asset.source.as_ref().map(|source| source.layer),
        Some(LayerId(2))
    );
    assert!(matches!(
        settings.get("ri:luts"),
        Some(RenderSettingValue::Value(
            Value::Array(_) | Value::TypedArray(_)
        ))
    ));
    assert_eq!(
        settings.get("ri:plain"),
        Some(&RenderSettingValue::Value(Value::Int(3)))
    );
}
