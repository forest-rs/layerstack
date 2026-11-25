// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::Scene;
use alloc::sync::Arc;
use layerstack::{EditTarget, InMemoryStore, Layer, LayerId, LiveStage, StageOptions, Time};

const ROOT: LayerId = LayerId(1);
const INPUTS: PbrMaterialInputs = PbrMaterialInputs {
    base_color: [0.25, 0.5, 0.75],
    roughness: 0.375,
    metallic: 0.625,
    opacity: 0.875,
};

fn stage() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(ROOT));
    let options = StageOptions {
        schemas: Some(Arc::new(crate::openusd(store.tokens()))),
        ..StageOptions::default()
    };
    let stage = LiveStage::compose(&mut store, ROOT, options);
    (store, stage)
}

#[test]
fn both_terminals_and_their_shared_inputs_compose() {
    let (mut store, mut live) = stage();
    let path = store.path("/Looks/Paint");
    let target = EditTarget::for_layer(ROOT);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    let handles = define_pbr_material(&mut edit, path, INPUTS).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let scene = Scene::new(live.stage(), &store);
    let material = Material::get(&scene, path).unwrap();
    let config = MaterialXConfigApi::get(&scene, path).unwrap();
    assert_eq!(config.config_mtlx_version().as_deref(), Some("1.39"));
    let open_pbr = OpenPbrSurface::get(&scene, handles.open_pbr.path()).unwrap();
    let preview = PreviewSurface::get(&scene, handles.preview.path()).unwrap();
    assert_eq!(preview.use_specular_workflow(), Some(0));
    assert_eq!(preview.opacity_mode(), Some("presence"));

    let materialx = material.compute_surface_source(&["mtlx"]);
    assert_eq!(materialx.context.as_deref(), Some("mtlx"));
    assert_eq!(materialx.shader, Some(open_pbr.path()));
    let universal = material.compute_surface_source(&["unknown"]);
    assert_eq!(universal.context.as_deref(), Some(""));
    assert_eq!(universal.shader, Some(preview.path()));

    let expected = [
        (
            "baseColor",
            "color3f",
            value::write_float3(INPUTS.base_color, store.tokens()),
            open_pbr.base_color_input(),
            preview.diffuse_color_input(),
        ),
        (
            "roughness",
            "float",
            Value::from(INPUTS.roughness),
            open_pbr.specular_roughness_input(),
            preview.roughness_input(),
        ),
        (
            "metallic",
            "float",
            Value::from(INPUTS.metallic),
            open_pbr.base_metalness_input(),
            preview.metallic_input(),
        ),
        (
            "opacity",
            "float",
            Value::from(INPUTS.opacity),
            open_pbr.geometry_opacity_input(),
            preview.opacity_input(),
        ),
    ];
    for (name, ty, value, mx_input, preview_input) in expected {
        let source = material.input(name).unwrap();
        assert_eq!(source.property_type().unwrap().type_name(), ty);
        let authored = scene
            .attribute(source.path())
            .unwrap()
            .authored_value(Time::Default)
            .unwrap();
        assert_eq!(authored.value, value, "{name}");
        for input in [mx_input.unwrap(), preview_input.unwrap()] {
            assert_eq!(input.property_type().unwrap().type_name(), ty);
            assert!(
                scene
                    .attribute(input.path())
                    .unwrap()
                    .authored_value(Time::Default)
                    .is_none()
            );
            assert!(input.can_connect(source.path()).is_ok());
            assert_eq!(input.connected_sources().sources, [source.path()]);
            let providers = input.value_sources();
            assert!(providers.issues.is_empty());
            assert_eq!(providers.sources.len(), 1);
            assert_eq!(providers.sources[0].attribute, source.path());
        }
    }
}

#[test]
fn failed_graph_leaves_the_parent_transaction_intact() {
    let (mut store, live) = stage();
    let path = store.path("/Looks/Paint");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    let material = Material::define(&mut edit, path).unwrap();
    material
        .create_input(&mut edit, "opacity", PropertyType::parse("int").unwrap())
        .unwrap();
    let baseline = edit.transaction().len();

    assert!(matches!(
        define_pbr_material(&mut edit, path, INPUTS),
        Err(PbrMaterialError::Port(PortError::TypeMismatch { name })) if name == "inputs:opacity"
    ));
    assert_eq!(edit.transaction().len(), baseline);
    assert_eq!(edit.property_kind(path, "inputs:baseColor"), None);
    assert_eq!(edit.property_kind(path, "config:mtlx:version"), None);
    assert_eq!(edit.property_kind(path, "outputs:mtlx:surface"), None);
}

#[test]
fn invalid_values_leave_queued_edits_intact() {
    let (mut store, live) = stage();
    let existing_path = store.path("/Existing");
    let path = store.path("/Looks/Paint");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    Material::define(&mut edit, existing_path).unwrap();
    let baseline = edit.transaction().len();

    assert!(matches!(
        define_pbr_material(
            &mut edit,
            path,
            PbrMaterialInputs {
                base_color: [0.0, f32::INFINITY, 0.0],
                ..INPUTS
            }
        ),
        Err(PbrMaterialError::NonFiniteBaseColor)
    ));
    for (input, values) in [
        (
            "roughness",
            PbrMaterialInputs {
                roughness: f32::NAN,
                ..INPUTS
            },
        ),
        (
            "metallic",
            PbrMaterialInputs {
                metallic: -0.1,
                ..INPUTS
            },
        ),
        (
            "opacity",
            PbrMaterialInputs {
                opacity: 1.1,
                ..INPUTS
            },
        ),
    ] {
        assert!(matches!(
            define_pbr_material(&mut edit, path, values),
            Err(PbrMaterialError::InvalidFactor { input: rejected, .. }) if rejected == input
        ));
    }
    assert_eq!(edit.transaction().len(), baseline);
    assert!(!edit.exists(path));
}
