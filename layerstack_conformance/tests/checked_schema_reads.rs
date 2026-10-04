// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed schema getters distinguish missing geometry from retained decode failure.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    ArrayReadError, DeferredArraySource, LayerId, PropertySpec, Time, TypedArray, Value,
};
use layerstack_schemas::{Scene, usd_geom::Mesh, value};
use std::sync::Arc;
#[path = "support/schema_scene.rs"]
mod support;

#[derive(Debug)]
struct Failed(ArrayReadError);
impl DeferredArraySource for Failed {
    fn materialize(&self) -> Result<&TypedArray, &ArrayReadError> {
        Err(&self.0)
    }
    fn element_kind(&self) -> Value {
        Value::Vec3f([0.; 3])
    }
}

#[test]
fn checked_schema_arrays_preserve_decode_errors_at_default_and_numeric_times() {
    for time in [Time::Default, Time::held(1.), Time::at(1.5)] {
        let (mut store, _) =
            support::scene("#usda 1.0\ndef Mesh \"M\" { point3f[] points = [(1, 2, 3)] }");
        let path = store.path("/M");
        let name = store.tokens.lookup("points").unwrap();
        let error = ArrayReadError::BudgetExceeded { limit: 7 };
        let failed = Value::TypedArray(TypedArray::Deferred(Arc::new(Failed(error.clone()))));
        let spec = store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&path)
            .unwrap()
            .property_mut(name)
            .unwrap();
        if time == Time::Default {
            spec.default = Some(failed.clone());
        } else {
            spec.time_samples =
                Some(vec![(1., failed.clone()), (2., Value::from(vec![[2_f32; 3]]))].into());
        }
        let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
        let live = layerstack::LiveStage::compose(
            &mut store,
            LayerId(1),
            layerstack::StageOptions {
                schemas: Some(schemas),
                ..Default::default()
            },
        );
        let scene = Scene::new(live.stage(), &store);
        let mesh = Mesh::new(&scene, path).unwrap();
        assert_eq!(mesh.try_points(time), Err(error.clone()));
        assert_eq!(
            value::try_read_float3_array_shared(&failed, &store.tokens),
            Err(error.clone())
        );
        assert_eq!(
            value::try_read_array(&failed, &store.tokens, value::read_float3),
            Err(error)
        );
    }
}

#[test]
fn checked_schema_reads_share_native_arrays_and_keep_typed_default_selection() {
    let (mut store, mut live) =
        support::scene("#usda 1.0\ndef Mesh \"M\" { point3f[] points = [(1, 2, 3)] }");
    let path = store.path("/M");
    let points = store.tokens.lookup("points").unwrap();
    let buffer = Arc::new(vec![[1_f32, 2., 3.]]);
    let property = layerstack::PropertyPath::new(path, points);
    let mut tx = layerstack::edit::Transaction::new();
    tx.set_default(
        layerstack::edit::EditTarget::for_layer(LayerId(1)).property(property),
        Value::TypedArray(TypedArray::Vec3f(buffer.clone())),
    );
    live.apply(&mut store, &tx).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let mesh = Mesh::new(&scene, path).unwrap();
    assert!(Arc::ptr_eq(
        &buffer,
        &mesh.try_points(Time::Default).unwrap().unwrap()
    ));
    assert_eq!(mesh.try_normals(Time::Default).unwrap(), None);
    assert_eq!(
        mesh.try_read_value("absent", Time::Default, value::read_int_array_shared)
            .unwrap(),
        None
    );
    assert_eq!(
        mesh.try_read_value("points", Time::Default, value::read_int_array_shared)
            .unwrap(),
        None
    );
    // A wrong dense type can still be skipped at default time, as Get<T> does.
    let mut weak = store.layers.remove(&LayerId(1)).unwrap();
    weak.id = LayerId(2);
    store.insert_layer(weak);
    let mut root = layerstack::Layer::new(LayerId(1));
    root.sublayers
        .push(layerstack::SublayerEntry::new(LayerId(2)));
    root.prims.insert(
        path,
        layerstack::PrimSpec::over().with_property(
            points,
            PropertySpec::attribute().with_default(Value::Int(3)),
        ),
    );
    store.insert_layer(root);
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = layerstack::LiveStage::compose(
        &mut store,
        LayerId(1),
        layerstack::StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    let scene = Scene::new(live.stage(), &store);
    let mesh = Mesh::new(&scene, path).unwrap();
    assert!(Arc::ptr_eq(
        &buffer,
        &mesh.try_points(Time::Default).unwrap().unwrap()
    ));
}

#[test]
fn validated_primvars_preserve_value_and_index_decode_errors() {
    use layerstack_schemas::primvar::{Primvar, PrimvarError};
    for name in ["primvars:x", "primvars:x:indices"] {
        let (mut store, _) = support::scene(
            "#usda 1.0\ndef Mesh \"M\" { point3f[] primvars:x = [(1,2,3)] int[] primvars:x:indices = [0] }",
        );
        let path = store.path("/M");
        let token = store.tokens.lookup(name).unwrap();
        let error = ArrayReadError::InvalidData("bad retained primvar buffer".into());
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&path)
            .unwrap()
            .property_mut(token)
            .unwrap()
            .default = Some(Value::TypedArray(TypedArray::Deferred(Arc::new(Failed(
            error.clone(),
        )))));
        let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
        let live = layerstack::LiveStage::compose(
            &mut store,
            LayerId(1),
            layerstack::StageOptions {
                schemas: Some(schemas),
                ..Default::default()
            },
        );
        let scene = Scene::new(live.stage(), &store);
        let var = Primvar::new(&scene, path, "x").unwrap();
        let expected = PrimvarError::Decode {
            property: layerstack::PropertyPath::new(path, token),
            error,
        };
        assert_eq!(var.validated_values(Time::Default).unwrap_err(), expected);
        assert_eq!(var.compute_flattened(Time::Default).unwrap_err(), expected);
    }
}

#[test]
fn checked_typed_reads_preserve_deferred_schema_fallback_failure() {
    use layerstack::{
        InMemoryStore, Layer, PrimSpec, PropertyDefinition, PropertyType, SchemaDefinition,
        SchemaRegistry, Stage, StageOptions,
    };
    use layerstack_schemas::PrimView;
    let mut store = InMemoryStore::default();
    let path = store.path("/P");
    let ty = store.tokens.intern("Custom");
    let name = store.tokens.intern("points");
    let error = ArrayReadError::BudgetExceeded { limit: 11 };
    let fallback = Value::TypedArray(TypedArray::Deferred(Arc::new(Failed(error.clone()))));
    let mut registry = SchemaRegistry::builder();
    registry.register(
        SchemaDefinition::typed(ty).with_property(
            PropertyDefinition::attribute(name)
                .with_type(PropertyType::new("point3f", true, Value::Vec3f([0.; 3])))
                .with_fallback(fallback),
        ),
    );
    let schemas = Arc::new(registry.build(&mut store.tokens));
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(path, PrimSpec::def().with_type_name(ty));
    store.insert_layer(layer);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    let scene = Scene::new(&stage, &store);
    let prim = PrimView::new(scene, path);
    for time in [Time::Default, Time::held(1.)] {
        assert_eq!(
            prim.try_read_value("points", time, value::read_float3_array_shared),
            Err(error.clone())
        );
    }
}

#[test]
fn checked_default_retry_preserves_weaker_decode_error_before_schema_fallback() {
    use layerstack::{
        InMemoryStore, Layer, PrimSpec, PropertyDefinition, PropertyType, SchemaDefinition,
        SchemaRegistry, Stage, StageOptions, SublayerEntry,
    };
    for incompatible in [Value::Int(3), Value::from(vec![3_i32])] {
        let mut store = InMemoryStore::default();
        let path = store.path("/M");
        let ty = store.tokens.intern("Mesh");
        let points = store.tokens.intern("points");
        let error = ArrayReadError::BudgetExceeded { limit: 7 };
        let mut registry = SchemaRegistry::builder();
        registry.register(
            SchemaDefinition::typed(ty).with_property(
                PropertyDefinition::attribute(points)
                    .with_type(PropertyType::new("point3f", true, Value::Vec3f([0.; 3])))
                    .with_fallback(Value::from(vec![[99_f32; 3]])),
            ),
        );
        let schemas = Arc::new(registry.build(&mut store.tokens));
        let mut weak = Layer::new(LayerId(2));
        weak.insert_prim(
            path,
            PrimSpec::def().with_type_name(ty).with_property(
                points,
                PropertySpec::attribute().with_default(Value::TypedArray(TypedArray::Deferred(
                    Arc::new(Failed(error.clone())),
                ))),
            ),
        );
        store.insert_layer(weak);
        let mut root = Layer::new(LayerId(1));
        root.sublayers.push(SublayerEntry::new(LayerId(2)));
        root.insert_prim(
            path,
            PrimSpec::over()
                .with_property(points, PropertySpec::attribute().with_default(incompatible)),
        );
        store.insert_layer(root);
        let stage = Stage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..Default::default()
            },
        );
        let scene = Scene::new(&stage, &store);
        let mesh = Mesh::new(&scene, path).unwrap();
        assert_eq!(mesh.try_points(Time::Default), Err(error));
        assert_eq!(mesh.points(), None);
    }
}
