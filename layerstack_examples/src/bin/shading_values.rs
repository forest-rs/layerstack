// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Author an interface input and inspect the value supplying a shader input.
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PropertyType, StageOptions, Value, edit::EditTarget,
};
use layerstack_schemas::{
    Scene, SchemaEdit, Time,
    shading::{Port, ValueSourceKind},
    usd_shade::{Material, Shader},
};
use std::sync::Arc;

fn main() {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let material = store.path("/Material");
    let shader = store.path("/Material/Surface");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let material = Material::define(&mut edit, material);
    let shader_handle = Shader::define(&mut edit, shader);
    let ty = PropertyType::new("float", false, Value::Float(0.));
    let interface = material
        .create_input(&mut edit, "roughness", ty.clone())
        .unwrap();
    interface.set(&mut edit, Value::Float(0.25)).unwrap();
    let input = shader_handle
        .create_input(&mut edit, "roughness", ty)
        .unwrap();
    input.set_sources(&mut edit, &[interface]).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let scene = Scene::new(live.stage(), &store);
    let shader = Shader::new(&scene, shader).unwrap();
    let sources = shader.input("roughness").unwrap().value_sources();
    for source in sources.sources {
        let path = source.attribute.display(&store.paths, &store.tokens);
        match source.kind {
            ValueSourceKind::AuthoredValue => {
                let value = Port::get(&scene, source.attribute)
                    .unwrap()
                    .value(Time::Default);
                println!("{path} supplies {value:?}");
                assert_eq!(
                    value,
                    Some(Value::Float(0.25)),
                    "the interface supplies the authored roughness"
                );
            }
            ValueSourceKind::ShaderOutput => println!("evaluate shader output {path}"),
        }
    }
}
