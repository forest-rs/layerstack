// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Author primvars, inspect index animation, and reuse parent inheritance sets.
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PropertyType, StageOptions, Value,
};
use layerstack_schemas::{
    PrimView, Scene, SchemaEdit, Time,
    primvar::Primvar,
    usd_geom::{Mesh, Xform},
};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    let layer = LayerId(1);
    store.insert_layer(Layer::new(layer));
    let root = store.path("/World");
    let mesh_path = store.path("/World/Mesh");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(layer));
    let world = Xform::define(&mut edit, root);
    let tint = world.create_primvar(
        &mut edit,
        "tint",
        PropertyType::new("float", false, Value::Float(0.)),
    )?;
    tint.set(&mut edit, Value::Float(0.7))?; // Unauthored interpolation is constant.
    let mesh = Mesh::define(&mut edit, mesh_path);
    let weights = mesh.create_indexed_primvar(
        &mut edit,
        "weight",
        PropertyType::new("float", true, Value::Float(0.)),
        Value::array(vec![Value::Float(0.25), Value::Float(0.75)]),
        &[0, 1],
        "vertex",
        1,
    )?;
    weights.set_indices(&mut edit, &[0, 1], Some(1.))?;
    weights.set_indices(&mut edit, &[1, 0], Some(2.))?;
    let id = mesh.create_primvar(
        &mut edit,
        "id",
        PropertyType::new("string", false, Value::string("")),
    )?;
    id.set_id_target(&mut edit, None)?; // This prim's composed path.
    let transaction = edit.finish();
    live.apply(&mut store, &transaction)?;

    let scene = Scene::new(live.stage(), &store);
    let inherited = PrimView::new(scene, root).inheritable_primvars();
    let mesh = PrimView::new(scene, mesh_path);
    let tint = mesh
        .find_primvar_with_inheritance_from("tint", &inherited)
        .unwrap();
    println!("Inherited tint: {:?}", tint.value(Time::Default)?);
    let weight = Primvar::new(&scene, mesh_path, "weight").unwrap();
    println!("Value/index sample times: {:?}", weight.sample_times());
    println!(
        "ID target: {:?}",
        Primvar::new(&scene, mesh_path, "id")
            .unwrap()
            .value(Time::Default)?
    );
    Ok(())
}
