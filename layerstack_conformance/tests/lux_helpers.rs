// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounded Lux helper conformance, evidence and transactional recovery.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{EditTarget, LayerId, TargetPath, Variability};
use layerstack_schemas::{
    Scene, SchemaEdit,
    light::{BlackbodyError, DomeOrientation, LightHelperError, blackbody_temperature_rgb},
    usd_lux::{DomeLight, DomeLightEdit, LightApi, LightApiEdit, LightFilter},
};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    blackbody: Vec<Blackbody>,
    selections: Vec<Selection>,
    blocked_selections: Vec<BlockedSelection>,
    links: Vec<Link>,
    dome_order: Vec<String>,
    dome_angle: f32,
}
#[derive(Deserialize)]
struct Blackbody {
    kelvin: f32,
    rgb: [f32; 3],
}
#[derive(Deserialize)]
struct Selection {
    contexts: Vec<String>,
    light: String,
    filter: String,
}
#[derive(Deserialize)]
struct Link {
    kind: String,
    included: Vec<String>,
}
const SCENE: &str = include_str!("../fixtures/lux_helpers/scene.usda");
fn oracle() -> Oracle {
    serde_json::from_str(include_str!("../fixtures/lux_helpers/oracle.json")).unwrap()
}
#[test]
fn blackbody_matches_cpp_float_arithmetic_and_rejects_nonfinite_kelvin() {
    let oracle = oracle();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    for row in oracle.blackbody {
        assert_eq!(
            blackbody_temperature_rgb(row.kelvin).unwrap(),
            row.rgb,
            "{} K",
            row.kelvin
        );
    }
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            blackbody_temperature_rgb(invalid),
            Err(BlackbodyError::NonFiniteTemperature)
        );
    }
    assert_eq!(
        blackbody_temperature_rgb(f32::MAX),
        blackbody_temperature_rgb(10000.)
    );
    assert_eq!(
        blackbody_temperature_rgb(f32::MIN),
        blackbody_temperature_rgb(1000.)
    );
}
#[test]
fn contextual_identifiers_and_link_membership_match_cpp_with_selection_evidence() {
    let (mut store, live) = support::scene(SCENE);
    let light_path = store.path("/Light");
    let filter_path = store.path("/Filter");
    let targets: Vec<_> = ["/Included", "/Excluded", "/Unshadowed"]
        .map(|p| (p, TargetPath::Prim(store.path(p))))
        .into();
    let scene = Scene::new(live.stage(), &store);
    let light = LightApi::get(&scene, light_path).unwrap();
    let filter = LightFilter::new(&scene, filter_path).unwrap();
    for row in oracle().selections {
        let contexts: Vec<_> = row.contexts.iter().map(String::as_str).collect();
        assert_eq!(light.select_shader_id(&contexts).id, row.light);
        assert_eq!(filter.select_shader_id(&contexts).id, row.filter);
    }
    let selected = light.select_shader_id(&["absent", "empty", "gpu", "ray"]);
    assert_eq!(selected.context.as_deref(), Some("gpu"));
    assert_eq!(selected.property, "gpu:light:shaderId");
    assert_eq!(
        selected.consulted_properties,
        [
            "absent:light:shaderId",
            "empty:light:shaderId",
            "gpu:light:shaderId"
        ]
    );
    assert_eq!(light.select_shader_id(&["absent"]).context, None);
    for row in oracle().links {
        let collection = match row.kind.as_str() {
            "light" => light.light_link_collection(),
            "shadow" => light.shadow_link_collection(),
            "filter" => filter.filter_link_collection(),
            _ => unreachable!(),
        };
        assert_eq!(collection.include_root(), Some(true));
        let query = collection.membership_query();
        let included: Vec<_> = targets
            .iter()
            .filter(|(_, path)| query.is_included(&scene, *path).is_included())
            .map(|(name, _)| name.to_string())
            .collect();
        assert_eq!(included, row.included);
    }
}
#[test]
fn contextual_authoring_is_uniform_noncustom_and_reversible() {
    let (mut store, mut live) = support::scene(SCENE);
    let path = store.path("/Light");
    let name = store.tokens.intern("engine:gpu:light:shaderId");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let light = LightApiEdit::new(&edit, path).unwrap();
    light
        .set_shader_id_for_render_context(&mut edit, "engine:gpu", "EngineGpuLight")
        .unwrap();
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let declaration = live
        .stage()
        .resolve_property_declaration(path, name)
        .unwrap();
    assert_eq!(declaration.variability, Variability::Uniform);
    assert!(!declaration.custom);
    assert_eq!(declaration.type_name.unwrap().type_name.as_ref(), "token");
    assert_eq!(
        LightApi::get(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .select_shader_id(&["engine:gpu"])
            .id,
        "EngineGpuLight"
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert!(
        live.stage()
            .resolve_property_declaration(path, name)
            .is_none()
    );
}
#[test]
fn invalid_shader_id_edits_are_atomic() {
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n float bad:light:shaderId = 1\n rel rel:light:shaderId\n}\n",
    );
    let path = store.path("/Light");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let light = LightApiEdit::new(&edit, path).unwrap();
    for context in ["bad", "rel"] {
        assert!(matches!(
            light.set_shader_id_for_render_context(&mut edit, context, "id"),
            Err(LightHelperError::WrongPropertyType { .. })
        ));
        assert!(edit.transaction().is_empty());
    }
    assert!(matches!(
        light.set_shader_id_for_render_context(&mut edit, "bad::context", "id"),
        Err(LightHelperError::InvalidContext(_))
    ));
    assert!(edit.transaction().is_empty());
}
#[test]
fn dome_orientation_matches_cpp_preserves_reset_and_reports_noop() {
    let reference = oracle();
    let (mut store, mut live) = support::scene(SCENE);
    let path = store.path("/Dome");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let dome = DomeLightEdit::new(&edit, path).unwrap();
    assert_eq!(
        dome.orient_to_stage_up_axis(&mut edit).unwrap(),
        DomeOrientation::Authored
    );
    let len = edit.transaction().len();
    assert_eq!(
        dome.orient_to_stage_up_axis(&mut edit).unwrap(),
        DomeOrientation::AlreadyOriented
    );
    assert_eq!(edit.transaction().len(), len);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let dome = DomeLight::new(&scene, path).unwrap();
    assert_eq!(dome.xform_op_order().unwrap(), reference.dome_order);
    assert_eq!(
        dome.read_value(
            "xformOp:rotateX:orientToStageUpAxis",
            layerstack_schemas::value::read_float
        ),
        Some(reference.dome_angle)
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    let (mut store, live) = support::scene("#usda 1.0\ndef DomeLight \"Dome\" {}\n");
    let path = store.path("/Dome");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert_eq!(
        DomeLightEdit::new(&edit, path)
            .unwrap()
            .orient_to_stage_up_axis(&mut edit)
            .unwrap(),
        DomeOrientation::NotZUp
    );
    assert!(edit.transaction().is_empty());
}

#[test]
fn filter_context_authoring_and_existing_declarations_preserve_opinions() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef LightFilter \"Filter\" {\n custom token existing:lightFilter:shaderId = \"Old\"\n}\n",
    );
    let path = store.path("/Filter");
    let existing = store.tokens.intern("existing:lightFilter:shaderId");
    let created = store.tokens.intern("new:lightFilter:shaderId");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let filter = layerstack_schemas::usd_lux::LightFilterEdit::new(&edit, path).unwrap();
    filter
        .set_shader_id_for_render_context(&mut edit, "existing", "New")
        .unwrap();
    filter
        .set_shader_id_for_render_context(&mut edit, "new", "GpuFilter")
        .unwrap();
    filter
        .set_shader_id_for_render_context(&mut edit, "", "Default")
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let declaration = live
        .stage()
        .resolve_property_declaration(path, existing)
        .unwrap();
    assert!(declaration.custom);
    assert_eq!(declaration.variability, Variability::Varying);
    let declaration = live
        .stage()
        .resolve_property_declaration(path, created)
        .unwrap();
    assert!(!declaration.custom);
    assert_eq!(declaration.variability, Variability::Uniform);
    let scene = Scene::new(live.stage(), &store);
    let filter = LightFilter::new(&scene, path).unwrap();
    assert_eq!(filter.select_shader_id(&["existing"]).id, "New");
    assert_eq!(filter.select_shader_id(&["new"]).id, "GpuFilter");
    assert_eq!(filter.select_shader_id(&[]).id, "Default");
}
#[test]
fn dome_orientation_preserves_existing_values_and_rejects_unordered_wrong_types() {
    let (mut store, live) = support::scene(
        "#usda 1.0\n(upAxis = \"Z\")\ndef DomeLight \"Dome\" {\n float xformOp:rotateX:orientToStageUpAxis = 45\n uniform token[] xformOpOrder = [\"!invert!xformOp:rotateX:orientToStageUpAxis\"]\n}\n",
    );
    let path = store.path("/Dome");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert_eq!(
        DomeLightEdit::new(&edit, path)
            .unwrap()
            .orient_to_stage_up_axis(&mut edit)
            .unwrap(),
        DomeOrientation::AlreadyOriented
    );
    assert!(edit.transaction().is_empty());
    let (mut store, live) = support::scene(
        "#usda 1.0\n(upAxis = \"Z\")\ndef DomeLight \"Dome\" {\n token xformOp:rotateX:orientToStageUpAxis = \"bad\"\n}\n",
    );
    let path = store.path("/Dome");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert_eq!(
        DomeLightEdit::new(&edit, path)
            .unwrap()
            .orient_to_stage_up_axis(&mut edit),
        Err(LightHelperError::InvalidOrientationOp(path))
    );
    assert!(edit.transaction().is_empty());
}

#[test]
fn malformed_ordered_dome_operation_is_a_diagnostic_instead_of_a_false_noop() {
    let (mut store, live) = support::scene(
        "#usda 1.0\n(upAxis = \"Z\")\ndef DomeLight \"Dome\" {\n uniform token[] xformOpOrder = [\"xformOp:rotateX:orientToStageUpAxis\"]\n}\n",
    );
    let path = store.path("/Dome");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert_eq!(
        DomeLightEdit::new(&edit, path)
            .unwrap()
            .orient_to_stage_up_axis(&mut edit),
        Err(LightHelperError::InvalidOrientationOp(path))
    );
    assert!(edit.transaction().is_empty());
}

#[derive(Deserialize)]
struct BlockedSelection {
    path: String,
    filter: bool,
    contexts: Vec<String>,
    id: String,
}
#[test]
fn blocked_shader_identifiers_match_cpp_and_preserve_typed_default_skipping() {
    let (mut store, live) = support::scene(SCENE);
    for row in oracle().blocked_selections {
        let path = store.path(&row.path);
        let scene = Scene::new(live.stage(), &store);
        let contexts: Vec<_> = row.contexts.iter().map(String::as_str).collect();
        let selected = if row.filter {
            LightFilter::new(&scene, path)
                .unwrap()
                .select_shader_id(&contexts)
        } else {
            LightApi::get(&scene, path)
                .unwrap()
                .select_shader_id(&contexts)
        };
        assert_eq!(selected.id, row.id, "{} {:?}", row.path, contexts);
    }
    let path = store.path("/Blocked");
    let scene = Scene::new(live.stage(), &store);
    let light = LightApi::get(&scene, path).unwrap();
    let selected = light.select_shader_id(&["gpu"]);
    assert!(selected.id.is_empty());
    assert_eq!(selected.context, None);
    assert_eq!(selected.property, "light:shaderId");
    assert_eq!(
        selected.consulted_properties,
        ["gpu:light:shaderId", "light:shaderId"]
    );
    let selected = light.select_shader_id(&["gpu", "ray"]);
    assert_eq!(selected.id, "RayAfterBlock");
    assert_eq!(selected.context.as_deref(), Some("ray"));
}
