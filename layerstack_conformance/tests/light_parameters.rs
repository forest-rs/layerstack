// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Renderer-neutral typed groups preserve USD units and unresolved asset state.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{
    Scene, Time,
    light::{DomePoleAxis, LightInputStatus, LightInputs, LightTextureFormat},
};

#[test]
fn applied_groups_preserve_defaults_authored_units_and_asset_readiness() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
(metersPerUnit = 0.01)
def SphereLight "L" (prepend apiSchemas = ["ShapingAPI", "ShadowAPI"]) {
 float inputs:shaping:cone:angle = 135
 float inputs:shaping:focus = -2
 float inputs:shadow:distance = 40
}
def SphereLight "Bare" {}
def SphereLight "Custom" { custom float inputs:shaping:focus = 2 }
"#,
    );
    let light = store.path("/L");
    let bare = store.path("/Bare");
    let custom = store.path("/Custom");
    let scene = Scene::new(live.stage(), &store);
    let inputs = LightInputs::read(&scene, light, Time::Default, &[]).unwrap();
    let shaping = inputs.shaping().unwrap().unwrap();
    assert_eq!((shaping.cone_angle, shaping.focus), (135., -2.));
    assert_eq!(shaping.focus_tint, [0.; 3]);
    assert_eq!(shaping.ies_file.status, LightInputStatus::Unavailable);
    assert_eq!(shaping.ies_angle_scale, 0.);
    assert!(!shaping.ies_normalize);
    let shadow = inputs.shadow().unwrap().unwrap();
    assert!(shadow.enable);
    assert_eq!(shadow.color, [0.; 3]);
    assert_eq!(
        (shadow.distance, shadow.falloff, shadow.falloff_gamma),
        (40., -1., 1.)
    );
    for path in [bare, custom] {
        let inputs = LightInputs::read(&scene, path, Time::Default, &[]).unwrap();
        assert!(inputs.shaping().unwrap().is_none());
        assert!(inputs.shadow().unwrap().is_none());
    }
}
#[test]
fn unresolved_controls_remain_errors_and_assets_remain_inspectable() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def SphereLight "L" (prepend apiSchemas = ["ShapingAPI", "ShadowAPI"]) {
 float inputs:shaping:focus.connect = </L/Shader.outputs:focus>
 asset inputs:shaping:ies:file.connect = </L/Shader.outputs:file>
 string inputs:shadow:falloff = "wrong"
 def Shader "Shader" { float outputs:focus; asset outputs:file }
}
"#,
    );
    let path = store.path("/L");
    let inputs =
        LightInputs::read(&Scene::new(live.stage(), &store), path, Time::Default, &[]).unwrap();
    let error = inputs.shaping().unwrap_err();
    assert_eq!(error.input, "shaping:focus");
    assert_eq!(error.status, LightInputStatus::ShaderRequired);
    assert_eq!(
        inputs.input("shaping:ies:file").unwrap().status,
        LightInputStatus::ShaderRequired
    );
    let error = inputs.shadow().unwrap_err();
    assert_eq!(error.input, "shadow:falloff");
    assert!(error.invalid_value);
}
#[test]
fn dome_versions_keep_their_distinct_pole_semantics_and_texture_evidence() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
(upAxis = "Z")
def DomeLight "Legacy" { asset inputs:texture:file = @./sky.exr@ }
def DomeLight_1 "Modern" { token inputs:texture:format = "latlong" }
def DomeLight_1 "Bad" { token poleAxis = "invalid" }
def DomeLight "BadFormat" { token inputs:texture:format = "unknown" }
"#,
    );
    let legacy = store.path("/Legacy");
    let modern = store.path("/Modern");
    let bad = store.path("/Bad");
    let format = store.path("/BadFormat");
    let scene = Scene::new(live.stage(), &store);
    let input = LightInputs::read(&scene, legacy, Time::Default, &[]).unwrap();
    let env = input.environment().unwrap().unwrap();
    assert_eq!(env.pole_axis, None);
    assert_eq!(env.texture_format, LightTextureFormat::Automatic);
    assert_eq!(
        env.texture_file.constant().unwrap().text.as_deref(),
        Some("./sky.exr")
    );
    let input = LightInputs::read(&scene, modern, Time::Default, &[]).unwrap();
    let env = input.environment().unwrap().unwrap();
    assert_eq!(env.pole_axis, Some(DomePoleAxis::Scene));
    assert_eq!(env.texture_format, LightTextureFormat::Latlong);
    assert_eq!(env.texture_file.status, LightInputStatus::Unavailable);
    assert_eq!(
        LightInputs::read(&scene, bad, Time::Default, &[])
            .unwrap()
            .environment()
            .unwrap_err()
            .input,
        "poleAxis"
    );
    assert_eq!(
        LightInputs::read(&scene, format, Time::Default, &[])
            .unwrap()
            .environment()
            .unwrap_err()
            .input,
        "texture:format"
    );
}
