// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retain geometry and shading queries through an authored edit and its undo.
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PropertyType, StageOptions, Value,
};
use layerstack_schemas::{
    Time,
    bounds::BoundsOptions,
    retained::{Query, RetainedQueries},
    usd_geom::{Cube, Xform},
    usd_shade::{Material, Shader},
};
use std::sync::Arc;

fn main() {
    let mut store = InMemoryStore::default();
    let layer = LayerId(1);
    store.insert_layer(Layer::new(layer));
    let world = store.path("/World");
    let shape = store.path("/World/Shape");
    let material = store.path("/Material");
    let shader = store.path("/Material/Surface");
    let input = store.property_path("/Material/Surface.inputs:roughness");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        layer,
        StageOptions {
            schemas: Some(schemas),
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    let subscription = live.subscribe_changes(|notice| {
        println!("stage revision {}: {:?}", notice.revision, notice.changes);
    });
    let mut queries = RetainedQueries::new(Time::Default, BoundsOptions::default());
    let target = EditTarget::for_layer(layer);
    let mut edit = layerstack_schemas::SchemaEdit::new(live.stage(), &mut store, target.clone());
    Xform::define(&mut edit, world);
    let cube = Cube::define(&mut edit, shape);
    cube.set_size(&mut edit, 2.0);
    let material = Material::define(&mut edit, material);
    let shader = Shader::define(&mut edit, shader);
    let ty = PropertyType::new("float", false, Value::Float(0.0));
    let roughness = material
        .create_input(&mut edit, "roughness", ty.clone())
        .unwrap();
    roughness.set(&mut edit, Value::Float(0.25)).unwrap();
    shader
        .create_input(&mut edit, "roughness", ty)
        .unwrap()
        .set_sources(&mut edit, std::slice::from_ref(&roughness))
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let transform = queries.observe(Query::WorldTransform(shape));
    let bound = queries.observe(Query::WorldBound(world));
    let shading = queries.observe(Query::ShadingValue(input));
    {
        let mut view = queries.view(&mut live, &mut store).unwrap();
        for id in [transform, bound, shading] {
            println!("initial: {:?}", view.poll(id).unwrap());
        }
    }
    let mut edit = layerstack_schemas::SchemaEdit::new(live.stage(), &mut store, target);
    roughness.set(&mut edit, Value::Float(0.5)).unwrap();
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    {
        let mut view = queries.view(&mut live, &mut store).unwrap();
        assert!(
            !view.poll(transform).unwrap().evaluated,
            "material edit preserves transform"
        );
        assert!(
            !view.poll(bound).unwrap().evaluated,
            "material edit preserves bound"
        );
        println!("material edit: {:?}", view.poll(shading).unwrap());
    }
    println!("dependencies: {:?}", queries.dependencies(shading).unwrap());
    live.apply(&mut store, &applied.inverse).unwrap();
    println!(
        "undo: {:?}",
        queries
            .view(&mut live, &mut store)
            .unwrap()
            .poll(shading)
            .unwrap()
    );
    live.unsubscribe_changes(&subscription);
}
