// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Downstream generation keeps typed authoring, sampling and exact undo.
#![allow(missing_docs, reason = "integration tests")]
include!(concat!(env!("OUT_DIR"), "/custom_modules.rs"));
use layerstack::{
    InMemoryStore, InterpolationType, Layer, LayerId, LiveStage, StageOptions, edit::EditTarget,
};
use layerstack_schemas::{Scene, SchemaEdit};
use std::sync::Arc;
#[test]
fn inherited_ports_compile_and_author_through_the_public_api() {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let path = store.path("/Paint");
    let before = store.layers[&LayerId(1)].clone();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let node = custom::StudioWetPaint::define(&mut edit, path);
    node.set_tint(&mut edit, custom::StudioWetPaint::tint_default())
        .unwrap();
    node.set_weights(&mut edit, &custom::StudioWetPaint::weights_default())
        .unwrap();
    node.set_precision(&mut edit, custom::StudioWetPaint::precision_default())
        .unwrap();
    node.set_enabled(&mut edit, false).unwrap();
    node.set_roughness_at(&mut edit, 0.0, 0.2).unwrap();
    node.set_roughness_at(&mut edit, 2.0, 0.6).unwrap();
    node.create_surface_output(&mut edit).unwrap();
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let node = custom::StudioWetPaint::new(&scene, path).unwrap();
    assert_eq!(node.weights(), Some(vec![0.25, 0.75].into()));
    assert_eq!(node.precision(), Some(0.5));
    assert_eq!(node.enabled(), Some(false));
    assert_eq!(node.tint(), Some([1.0, 0.5, 0.25]));
    assert_eq!(node.roughness_at(1.0, InterpolationType::Linear), Some(0.4));
    assert_eq!(node.coat(), None);
    assert_eq!(custom::StudioWetPaint::coat_default(), 0.8);
    assert_eq!(custom::StudioWetPaint::roughness_default(), 0.4);
    assert!(node.surface_output().is_some());
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(store.layers[&LayerId(1)], before);
}

#[test]
fn selected_materialx_interfaces_author_usd_without_loading_implementations() {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let path = store.path("/MaterialX");
    let rgba = store.path("/Rgba");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    materialx::NdRgba::define(&mut edit, rgba)
        .create_out_output(&mut edit)
        .unwrap();
    let node = materialx::NdPaint::define(&mut edit, path);
    node.set_gain(&mut edit, materialx::NdPaint::gain_default())
        .unwrap();
    node.set_file(&mut edit, &materialx::NdPaint::file_default())
        .unwrap();
    node.set_matrix(&mut edit, materialx::NdPaint::matrix_default())
        .unwrap();
    node.create_out_output(&mut edit).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let node = materialx::NdPaint::new(&scene, path).unwrap();
    assert_eq!(node.gain(), Some(0.75));
    assert_eq!(node.file().as_deref(), Some("paint.exr"));
    assert_eq!(
        node.matrix(),
        Some([[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]])
    );
    assert_eq!(materialx::NdPaint::label_default().as_ref(), "paint & coat");
    assert_eq!(materialx::NdPaint::tint_default(), [1., 0.5, 0.25]);
    assert_eq!(node.tint(), None);
    assert!(node.out_output().is_some());
    let output = materialx::NdRgba::new(&scene, rgba)
        .unwrap()
        .out_output()
        .unwrap();
    assert_eq!(
        output.property_type().unwrap().type_name.as_ref(),
        "color4f"
    );
}

#[test]
fn user_literals_compile_and_round_trip_without_rust_path_rewriting() {
    const LABEL: &str =
        "crate::example alloc::sync::Arc:: alloc::vec::Vec< alloc::boxed::Box:: ::alloc::";
    assert_eq!(custom::CrateLiteral::ID, "crate::literal");
    assert_eq!(custom::CrateLiteral::label_default().as_ref(), LABEL);
    assert_eq!(custom::CrateLiteral::mode_default(), "crate::mode");
    assert_eq!(
        custom::CrateLiteral::file_default().as_ref(),
        "crate::asset"
    );
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let path = store.path("/Literal");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let node = custom::CrateLiteral::define(&mut edit, path);
    node.set_label(&mut edit, &custom::CrateLiteral::label_default())
        .unwrap();
    node.set_mode(&mut edit, custom::CrateLiteral::mode_default())
        .unwrap();
    node.set_file(&mut edit, &custom::CrateLiteral::file_default())
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let node = custom::CrateLiteral::new(&scene, path).unwrap();
    assert_eq!(node.label().as_deref(), Some(LABEL));
    assert_eq!(node.mode(), Some("crate::mode"));
    assert_eq!(node.file().as_deref(), Some("crate::asset"));
}
