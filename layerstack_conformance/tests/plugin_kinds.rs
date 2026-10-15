// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Compile and use plugin-generated kinds with schema model queries.
#![allow(missing_docs, reason = "integration tests")]
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/custom_kinds.rs"));
}

#[test]
fn generated_kinds_drive_model_queries_and_reject_collisions_atomically() {
    use layerstack::{
        FieldValue, InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, Value,
    };
    use layerstack_schemas::{KindRegistry, Scene};
    let mut kinds = KindRegistry::new();
    generated::register(&mut kinds).unwrap();
    assert!(
        kinds.is_a("stack_leaf", "component"),
        "forward plugin base resolves"
    );
    assert!(
        !kinds.is_a("stack_root", "model"),
        "root kind stays outside models"
    );
    let before = kinds.clone();
    assert!(
        generated::register(&mut kinds).is_err(),
        "duplicate plugin publication rejects"
    );
    assert_eq!(kinds, before, "collision leaves caller registry unchanged");
    let mut store = InMemoryStore::default();
    let path = store.path("/Model");
    let kind = store.tokens.intern("kind");
    let leaf = store.tokens.intern("stack_leaf");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        path,
        PrimSpec::def().with_field(kind, FieldValue::Value(Value::Token(leaf))),
    );
    store.insert_layer(layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let scene = Scene::new(&stage, &store).with_kinds(&kinds);
    assert!(
        scene.is_model(path),
        "generated custom kinds participate in model queries"
    );
}
