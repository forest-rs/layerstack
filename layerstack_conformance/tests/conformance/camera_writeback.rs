// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Atomic camera writeback, native parent conversion and rollback.
use crate::support::schema_scene as support;
use layerstack::{
    EditTarget, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, SublayerEntry, Value,
};
use layerstack_schemas::{
    Scene, SchemaEdit, Time, XformCache,
    camera::{CameraWriteError, ComputedCamera},
    usd_geom::{Camera, CameraEdit},
};

fn source(store: &mut layerstack::InMemoryStore, live: &layerstack::LiveStage) -> ComputedCamera {
    let path = store.path("/SourceParent/Source");
    Camera::new(&Scene::new(live.stage(), store), path)
        .unwrap()
        .compute_camera(&mut XformCache::new(Time::Default))
        .unwrap()
}
fn close(actual: &[[f64; 4]; 4], expected: &[[f64; 4]; 4]) {
    for (a, b) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert!(
            (a - b).abs() < 1e-10 * b.abs().max(1.),
            "{actual:?} != {expected:?}"
        );
    }
}
#[test]
fn numeric_writeback_matches_native_parent_conversion_and_undo() {
    let (mut store, mut live) =
        support::scene(include_str!("../../fixtures/camera_writeback.usda"));
    let camera = source(&mut store, &live);
    let path = store.path("/DestinationParent/Destination");
    let before = store.layers().cloned().collect::<Vec<_>>();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    CameraEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap()
        .set_from_camera_at(&mut edit, 2., &camera)
        .unwrap();
    let transaction = edit.finish();
    let report = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let actual = Camera::new(&scene, path)
        .unwrap()
        .compute_camera(&mut XformCache::new(Time::at(2.)))
        .unwrap();
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../../fixtures/camera_writeback.json")).unwrap();
    let expected: [[f64; 4]; 4] = serde_json::from_value(oracle["world"].clone()).unwrap();
    close(actual.camera_to_world(), &expected);
    let local = XformCache::new(Time::at(2.))
        .local_transform(&scene, path)
        .unwrap()
        .matrix;
    close(
        &local,
        &serde_json::from_value(oracle["local"].clone()).unwrap(),
    );
    assert_eq!(
        actual.aperture(),
        serde_json::from_value::<[f32; 2]>(oracle["aperture"].clone()).unwrap(),
        "lens aperture matches native"
    );
    assert_eq!(
        actual.aperture_offset(),
        camera.aperture_offset(),
        "offsets are copied"
    );
    assert_eq!(
        actual.clipping_range(),
        camera.clipping_range(),
        "clipping range is copied"
    );
    assert_eq!(
        actual.clipping_planes(),
        camera.clipping_planes(),
        "planes are copied"
    );
    assert_eq!(
        actual.focal_length(),
        camera.focal_length(),
        "focal length is copied"
    );
    assert_eq!(actual.f_stop(), camera.f_stop(), "f-stop is copied");
    assert_eq!(
        actual.focus_distance(),
        camera.focus_distance(),
        "focus distance is copied"
    );
    assert_eq!(
        actual.shutter_offsets(),
        camera.shutter_offsets(),
        "whole snapshot also includes shutter offsets"
    );
    let old = Camera::new(&scene, path)
        .unwrap()
        .horizontal_aperture_at(Time::at(1.));
    assert_eq!(old, Some(35.), "other numeric samples remain");
    live.apply(&mut store, &report.inverse).unwrap();
    assert_eq!(
        store.layer(LayerId(1)).unwrap().prims(),
        before
            .iter()
            .find(|layer| layer.id() == LayerId(1))
            .unwrap()
            .prims(),
        "undo restores all declarations and reset order"
    );
}

#[test]
fn default_writeback_keeps_numeric_slots() {
    let (mut store, mut live) =
        support::scene(include_str!("../../fixtures/camera_writeback.usda"));
    let camera = source(&mut store, &live);
    let path = store.path("/DestinationParent/Destination");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    CameraEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap()
        .set_from_camera(&mut edit, &camera)
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let dest = Camera::new(&scene, path).unwrap();
    assert_eq!(dest.horizontal_aperture(), Some(40.), "default is replaced");
    assert_eq!(
        dest.horizontal_aperture_at(Time::at(1.)),
        Some(35.),
        "default writes retain numeric samples"
    );
}

#[test]
fn stronger_last_field_and_untyped_declarations_reject_without_partial_edits() {
    for bad_type in [false, true] {
        let (mut store, mut live) =
            support::scene(include_str!("../../fixtures/camera_writeback.usda"));
        let camera = source(&mut store, &live);
        let path = store.path("/Empty");
        let field = store.tokens.intern("shutter:close");
        let ty = store.tokens.intern("Camera");
        let prop = if bad_type {
            PropertySpec::attribute().with_default(Value::Double(3.))
        } else {
            PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
                .with_default(Value::Double(3.))
        };
        let root = store.layer_mut(LayerId(1)).unwrap();
        root.insert_prim(
            path,
            PrimSpec::def()
                .with_type_name(ty)
                .with_property(field, prop),
        );
        root.sublayers_mut().push(SublayerEntry::new(LayerId(2)));
        store.insert_layer(Layer::new(LayerId(2)));
        live.synchronize(&mut store);
        let before = store.layers().cloned().collect::<Vec<_>>();
        let target = LayerId(if bad_type { 1 } else { 2 });
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(target));
        let dest = CameraEdit::new(&edit, path)
            .expect("outside any instance")
            .unwrap();
        dest.set_focus_distance(&mut edit, 99.);
        let pending = edit.transaction().clone();
        let error = dest.set_from_camera(&mut edit, &camera).unwrap_err();
        assert!(
            matches!(
                error,
                CameraWriteError::StrongerSource {
                    attribute: "shutter:close",
                    ..
                } | CameraWriteError::IncompatibleType {
                    attribute: "shutter:close"
                }
            ),
            "last-field failure is reported: {error:?}"
        );
        assert_eq!(
            edit.transaction(),
            &pending,
            "late preflight failure preserves the parent's edits"
        );
        assert_eq!(
            store.layers().cloned().collect::<Vec<_>>(),
            before,
            "preflight does not mutate any layer"
        );
    }
}

#[test]
fn singular_parent_and_invalid_time_leave_batch_unchanged() {
    let (mut store, mut live) =
        support::scene(include_str!("../../fixtures/camera_writeback.usda"));
    let camera = source(&mut store, &live);
    let path = store.path("/DestinationParent/Destination");
    let parent = store.path("/DestinationParent");
    let scale = store.tokens.intern("xformOp:scale");
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prim_mut(parent)
        .unwrap()
        .property_mut(scale)
        .unwrap()
        .default = Some(Value::Vec3d([0., 1., 1.]));
    live.synchronize(&mut store);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let dest = CameraEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    let before = edit.transaction().clone();
    assert!(
        matches!(
            dest.set_from_camera(&mut edit, &camera),
            Err(CameraWriteError::InvalidParentTransform)
        ),
        "singular parent rejects before matrix or lens writes"
    );
    assert!(
        matches!(
            dest.set_from_camera_at(&mut edit, f64::NAN, &camera),
            Err(CameraWriteError::Inspection { .. })
        ),
        "nonfinite time rejects"
    );
    assert_eq!(
        edit.transaction(),
        &before,
        "failed calls collect no partial camera"
    );
}

#[test]
fn stronger_transform_order_rejects_the_entire_camera_group() {
    let (mut store, mut live) =
        support::scene(include_str!("../../fixtures/camera_writeback.usda"));
    let camera = source(&mut store, &live);
    let path = store.path("/DestinationParent/Destination");
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .sublayers_mut()
        .push(SublayerEntry::new(LayerId(2)));
    store.insert_layer(Layer::new(LayerId(2)));
    live.synchronize(&mut store);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(2)));
    let dest = CameraEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap();
    dest.set_focus_distance(&mut edit, 99.);
    let before = edit.transaction().clone();
    assert!(
        matches!(
            dest.set_from_camera(&mut edit, &camera),
            Err(CameraWriteError::Transform(
                layerstack_schemas::MatrixXformError::StrongerSource {
                    attribute: "xformOpOrder",
                    ..
                }
            ))
        ),
        "matrix order failure propagates through whole-camera guard"
    );
    assert_eq!(
        edit.transaction(),
        &before,
        "matrix guard failure preserves earlier lens edits only"
    );
}

#[test]
fn reference_target_maps_all_camera_samples_and_matrix_together() {
    let (mut store, mut live) =
        support::scene(include_str!("../../fixtures/camera_writeback.usda"));
    let camera = source(&mut store, &live);
    let path = store.path("/Use");
    let asset = store.path("/Asset");
    let camera_type = store.tokens.intern("Camera");
    let mut referenced = Layer::new(LayerId(2));
    referenced.insert_prim(asset, PrimSpec::def().with_type_name(camera_type));
    store.insert_layer(referenced);
    store.layer_mut(LayerId(1)).unwrap().insert_prim(
        path,
        PrimSpec::def().with_reference(
            layerstack::Reference::new(LayerId(2), asset).with_layer_offset(
                layerstack::LayerOffset {
                    offset: 10.,
                    scale: 2.,
                },
            ),
        ),
    );
    live.synchronize(&mut store);
    let node = live
        .stage()
        .prim(path, &store)
        .unwrap()
        .composition_graph()
        .nodes()
        .find(|(_, node)| node.arc_kind() == layerstack::ArcKind::References)
        .unwrap()
        .0;
    let target = EditTarget::for_node(live.stage(), path, node).unwrap();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    CameraEdit::new(&edit, path)
        .expect("outside any instance")
        .unwrap()
        .set_from_camera_at(&mut edit, 14., &camera)
        .unwrap();
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    for name in [
        "focalLength",
        "xformOp:transform",
        "shutter:open",
        "clippingPlanes",
    ] {
        let name = store.tokens.lookup(name).unwrap();
        let property = store.layer(LayerId(2)).unwrap().prims()[&asset]
            .property(name)
            .unwrap();
        assert_eq!(
            property.time_samples.as_ref().unwrap()[0].0,
            2.,
            "all camera samples map through the edit target's time offset"
        );
    }
    let scene = Scene::new(live.stage(), &store);
    let actual = Camera::new(&scene, path)
        .unwrap()
        .compute_camera(&mut XformCache::new(Time::at(14.)))
        .unwrap();
    close(actual.camera_to_world(), camera.camera_to_world());
    live.apply(&mut store, &applied.inverse).unwrap();
    assert!(
        store.layer(LayerId(2)).unwrap().prims()[&asset]
            .properties
            .is_empty(),
        "undo removes the whole mapped camera replacement"
    );
}

#[test]
fn nonfinite_extra_planes_and_unreadable_parent_ops_reject_without_edits() {
    let text =
        include_str!("../../fixtures/camera_writeback.usda").replace("(1,0,0,2)", "(nan,0,0,2)");
    let (mut store, mut live) = support::scene(&text);
    let camera = source(&mut store, &live);
    let path = store.path("/DestinationParent/Destination");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert!(
        matches!(
            CameraEdit::new(&edit, path)
                .expect("outside any instance")
                .unwrap()
                .set_from_camera(&mut edit, &camera),
            Err(CameraWriteError::InvalidClippingPlanes)
        ),
        "consumer-owned extra planes are checked before authoring"
    );
    assert_eq!(
        edit.finish(),
        layerstack::Transaction::new(),
        "invalid planes collect no edits"
    );
    let parent = store.path("/DestinationParent");
    let op = store.tokens.intern("xformOp:missing");
    let order = store.tokens.lookup("xformOpOrder").unwrap();
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prim_mut(parent)
        .unwrap()
        .property_mut(order)
        .unwrap()
        .default = Some(Value::Array(vec![Value::Token(op)]));
    live.synchronize(&mut store);
    let (mut valid_store, valid_live) =
        support::scene(include_str!("../../fixtures/camera_writeback.usda"));
    let valid = source(&mut valid_store, &valid_live);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert!(
        matches!(
            CameraEdit::new(&edit, path)
                .expect("outside any instance")
                .unwrap()
                .set_from_camera(&mut edit, &valid),
            Err(CameraWriteError::InvalidParentTransform)
        ),
        "parent evaluation diagnostics reject instead of writing against identity fallback"
    );
    assert_eq!(
        edit.finish(),
        layerstack::Transaction::new(),
        "invalid parent collects no edits"
    );
}
