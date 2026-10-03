// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Engine lighting capture and recovery through public APIs.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{InterpolationType, LayerId, TargetPath, Value};
use layerstack_schemas::{
    Scene, Time,
    light::{
        LightCaptureError, LightInputStatus as Status, LightInputs, LightKind, LightLinkMembership,
        LightShape,
    },
};
#[test]
fn owned_capture_survives_scene_and_guides_typed_engine_parameters() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
( metersPerUnit = 1
  upAxis = "Z" )
def Xform "World" {
 double3 xformOp:translate = (1,2,3)
 uniform token[] xformOpOrder = ["xformOp:translate"]
 def SphereLight "Light" {
  float inputs:radius = 2
  float inputs:exposure = 3
  bool treatAsPoint = true
 }
}
"#,
    );
    let path = store.path("/World/Light");
    let capture =
        LightInputs::read(&Scene::new(live.stage(), &store), path, Time::Default, &[]).unwrap();
    drop(live);
    drop(store);
    assert_eq!(capture.kind, LightKind::Sphere);
    assert_eq!(capture.world_transform[3], [1., 2., 3., 1.]);
    assert_eq!(capture.meters_per_unit, 1.);
    assert_eq!(capture.up_axis, "Z");
    assert_eq!(
        capture.shape().unwrap(),
        LightShape::Sphere {
            radius: 2.,
            treat_as_point: true
        }
    );
    assert_eq!(capture.photometry().unwrap().exposure, 3.);
    assert!(capture.transform_problems.is_empty());
}
#[test]
fn constants_network_execution_failures_and_ambiguous_providers_are_distinct() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def SphereLight "Light" {
 custom float inputs:gain = 2
 float inputs:intensity = 500
 float inputs:intensity.connect = </Light.inputs:gain>
 float inputs:exposure.connect = </Light/Shader.outputs:out>
 color3f inputs:color.connect = [</Light.inputs:c1>, </Light.inputs:c2>]
 custom color3f inputs:c1 = (1,0,0)
 custom color3f inputs:c2 = (0,1,0)
 float inputs:diffuse.connect = </Missing.outputs:out>
 custom float inputs:cycle.connect = </Light.inputs:cycle>
 float inputs:specular = None
 def Shader "Shader" { float outputs:out }
}
"#,
    );
    let light = store.path("/Light");
    let capture =
        LightInputs::read(&Scene::new(live.stage(), &store), light, Time::Default, &[]).unwrap();
    assert_eq!(capture.float("intensity").unwrap(), 2.);
    assert_eq!(
        capture.input("intensity").unwrap().own.value,
        Some(Value::Float(500.))
    );
    for (name, status) in [
        ("exposure", Status::ShaderRequired),
        ("color", Status::MultipleProviders),
        ("diffuse", Status::InvalidConnections),
        ("cycle", Status::InvalidConnections),
        ("specular", Status::Unavailable),
    ] {
        let input = capture.input(name).unwrap();
        assert_eq!(input.status, status, "{name}");
        assert!(input.constant().is_none());
    }
    assert_eq!(capture.photometry().unwrap_err().input, "color");
    assert!(
        capture
            .dependencies
            .iter()
            .any(|d| d.property == "outputs:out")
    );
}
#[test]
fn custom_emitters_and_non_emitters_have_explicit_classification() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def Xform "Custom" (prepend apiSchemas = ["LightAPI"]) {}
def Mesh "Mesh" (prepend apiSchemas = ["MeshLightAPI"]) {}
def CylinderLight "Cylinder" {}
def LightFilter "Filter" {}
"#,
    );
    for (name, kind) in [
        ("/Custom", LightKind::Custom),
        ("/Mesh", LightKind::Mesh),
        ("/Cylinder", LightKind::Cylinder),
    ] {
        let path = store.path(name);
        let capture =
            LightInputs::read(&Scene::new(live.stage(), &store), path, Time::Default, &[]).unwrap();
        assert_eq!(capture.kind, kind);
        if kind == LightKind::Cylinder {
            assert_eq!(
                capture.shape().unwrap(),
                LightShape::Cylinder {
                    radius: 0.5,
                    length: 1.,
                    treat_as_line: false
                }
            );
        }
    }
    let filter = store.path("/Filter");
    let missing = store.path("/Missing");
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        LightInputs::read(&scene, filter, Time::Default, &[]).unwrap_err(),
        LightCaptureError::NotLight(filter)
    );
    assert_eq!(
        LightInputs::read(&scene, missing, Time::Default, &[]).unwrap_err(),
        LightCaptureError::MissingPrim(missing)
    );
}
#[test]
fn relationship_and_transform_failures_are_localized() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def SphereLight "Light" {
 rel light:filters = [</Bridge.forward>, </Missing>]
 uniform token[] xformOpOrder = ["xformOp:translate:missing"]
}
def Scope "Bridge" { rel forward = </Filter> }
def LightFilter "Filter" {}
"#,
    );
    let path = store.path("/Light");
    let filter = store.path("/Filter");
    let bridge = store.path("/Bridge");
    let capture =
        LightInputs::read(&Scene::new(live.stage(), &store), path, Time::Default, &[]).unwrap();
    assert!(
        capture
            .relationships
            .iter()
            .find(|(n, _)| n == "light:filters")
            .unwrap()
            .1
            .contains(&TargetPath::Prim(filter))
    );
    assert!(
        capture
            .dependencies
            .iter()
            .any(|d| d.prim == bridge && d.property == "forward")
    );
    assert_eq!(capture.relationship_issues.len(), 1);
    assert_eq!(capture.transform_problems.len(), 1);
}
#[test]
fn link_decisions_are_owned_ordered_and_distinguish_shadows_from_illumination() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def SphereLight "Light" {
 rel collection:lightLink:excludes = </Hidden>
 rel collection:shadowLink:excludes = </Visible>
}
def Scope "Visible" {}
def Scope "Hidden" {}
"#,
    );
    let light = store.path("/Light");
    let visible = TargetPath::Prim(store.path("/Visible"));
    let hidden = TargetPath::Prim(store.path("/Hidden"));
    let links = LightLinkMembership::read(
        &Scene::new(live.stage(), &store),
        light,
        &[visible, hidden, visible],
    )
    .unwrap();
    drop(live);
    drop(store);
    assert_eq!(links.illumination.included, [true, false, true]);
    assert_eq!(links.shadows.included, [false, true, false]);
    assert!(links.shadows.problems.is_empty());
}
#[test]
fn malformed_numeric_values_and_forwarding_cycles_remain_inspectable() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def SphereLight "Light" {
 string inputs:intensity = "bad"
 rel light:filters = </Bridge.forward>
}
def Scope "Bridge" { rel forward = </Bridge.forward> }
"#,
    );
    let light = store.path("/Light");
    let capture =
        LightInputs::read(&Scene::new(live.stage(), &store), light, Time::Default, &[]).unwrap();
    assert!(capture.float("intensity").unwrap_err().invalid_value);
    assert!(capture.relationship_issues.iter().any(|i| i.cycle));
}
#[test]
fn filter_port_capture_and_asset_source_evidence_use_the_same_owned_model() {
    let (mut store, _) = support::scene(
        r#"#usda 1.0
def LightFilter "Filter" { custom float inputs:gain = 0.25 }
def DomeLight "Dome" { asset inputs:texture:file = @textures/sky.exr@ }
"#,
    );
    let filter = store.path("/Filter");
    let dome = store.path("/Dome");
    let schemas = std::sync::Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = layerstack::LiveStage::compose(
        &mut store,
        LayerId(1),
        layerstack::StageOptions {
            schemas: Some(schemas),
            with_provenance: true,
            ..layerstack::StageOptions::default()
        },
    );
    let scene = Scene::new(live.stage(), &store);
    let port = layerstack_schemas::PrimView::new(scene, filter)
        .input("gain")
        .unwrap();
    let owned =
        layerstack_schemas::light::LightInput::read(&scene, port.path(), Time::Default).unwrap();
    assert_eq!(owned.constant().unwrap().value, Some(Value::Float(0.25)));
    let inputs = LightInputs::read(&scene, dome, Time::Default, &[]).unwrap();
    let texture = inputs.input("texture:file").unwrap().constant().unwrap();
    assert_eq!(texture.text.as_deref(), Some("textures/sky.exr"));
    assert!(texture.provenance.is_some());
}
#[test]
fn authored_blocks_follow_cpp_default_versus_numeric_time_behavior() {
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n float inputs:specular = None\n}\n",
    );
    let light = store.path("/Light");
    let scene = Scene::new(live.stage(), &store);
    let default = LightInputs::read(&scene, light, Time::Default, &[]).unwrap();
    assert_eq!(
        default.input("specular").unwrap().status,
        Status::Unavailable
    );
    let sampled = LightInputs::read(
        &scene,
        light,
        Time::At {
            code: 1.,
            interpolation: InterpolationType::Linear,
        },
        &[],
    )
    .unwrap();
    assert_eq!(sampled.float("specular").unwrap(), 1.);
}
#[test]
fn connected_interface_without_a_provider_does_not_become_a_schema_constant() {
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n float inputs:intensity.connect = </Interface.inputs:gain>\n}\ndef NodeGraph \"Interface\" {\n float inputs:gain\n}\n",
    );
    let light = store.path("/Light");
    let scene = Scene::new(live.stage(), &store);
    let capture = LightInputs::read(&scene, light, Time::Default, &[]).unwrap();
    let input = capture.input("intensity").unwrap();
    assert!(input.providers.sources.is_empty());
    assert!(input.providers.issues.is_empty());
    assert_eq!(input.own.value, Some(Value::Float(1.)));
    assert_eq!(input.status, Status::Unavailable);
    assert!(input.constant().is_none());
}

#[test]
fn shape_flags_use_non_port_attributes_even_with_conflicting_custom_inputs() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def SphereLight "Sphere" {
 bool treatAsPoint = false
 custom bool inputs:treatAsPoint = true
}
def CylinderLight "Cylinder" {
 bool treatAsLine = true
 custom float inputs:treatAsLine = 7
}
"#,
    );
    let sphere = store.path("/Sphere");
    let cylinder = store.path("/Cylinder");
    let scene = Scene::new(live.stage(), &store);
    let sphere = LightInputs::read(&scene, sphere, Time::Default, &[]).unwrap();
    let cylinder = LightInputs::read(&scene, cylinder, Time::Default, &[]).unwrap();
    assert_eq!(
        sphere.shape().unwrap(),
        LightShape::Sphere {
            radius: 0.5,
            treat_as_point: false
        }
    );
    assert!(sphere.boolean("treatAsPoint").unwrap());
    assert_eq!(
        cylinder.shape().unwrap(),
        LightShape::Cylinder {
            radius: 0.5,
            length: 1.,
            treat_as_line: true
        }
    );
    assert_eq!(cylinder.float("treatAsLine").unwrap(), 7.);
}

#[test]
fn shared_forwarding_subgraphs_have_bounded_capture_and_cycle_diagnostics() {
    use std::fmt::Write;
    for (depth, cycle) in [(40, false), (12, true)] {
        let mut text = String::from(
            "#usda 1.0\ndef SphereLight \"Light\" {\n rel light:filters = </Bridge.a0>\n}\ndef Scope \"Bridge\" {\n",
        );
        for level in 0..depth {
            for prefix in ["a", "b"] {
                if level + 1 < depth {
                    writeln!(
                        text,
                        " rel {prefix}{level} = [</Bridge.a{}>, </Bridge.b{}>]",
                        level + 1,
                        level + 1
                    )
                    .unwrap();
                } else {
                    if cycle && prefix == "a" {
                        writeln!(
                            text,
                            " rel {prefix}{level} = [</Bridge.{prefix}{level}>, </Filter>]"
                        )
                        .unwrap();
                    } else {
                        writeln!(text, " rel {prefix}{level} = </Filter>").unwrap();
                    }
                }
            }
        }
        text.push_str("}\ndef LightFilter \"Filter\" {}\n");
        let (mut store, live) = support::scene(&text);
        let light = store.path("/Light");
        let bridge = store.path("/Bridge");
        let filter = store.path("/Filter");
        let capture =
            LightInputs::read(&Scene::new(live.stage(), &store), light, Time::Default, &[])
                .unwrap();
        assert_eq!(capture.relationship_issues.len(), usize::from(cycle));
        assert_eq!(
            capture
                .dependencies
                .iter()
                .filter(|d| d.prim == bridge)
                .count(),
            2 * depth - 1
        );
        assert_eq!(
            capture
                .relationships
                .iter()
                .find(|(name, _)| name == "light:filters")
                .unwrap()
                .1,
            [TargetPath::Prim(filter)]
        );
    }
}
