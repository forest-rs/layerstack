// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Check an already composed stage with the built-in validation rules.

use std::{hint::black_box, sync::Arc};

use criterion::{Criterion, criterion_group, criterion_main};
use layerstack::{
    InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions, Time,
    Value,
};
use layerstack_schemas::{
    Scene,
    validation::{ValidationContext, ValidationScope},
};

fn fixture(prims: usize, attributes: usize) -> (InMemoryStore, Stage) {
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(LayerId(1));
    let tokens = store.tokens();
    let scope = tokens.intern("Scope");
    let purpose = tokens.intern("purpose");
    let purpose_value = tokens.intern("render");
    let token = PropertyType::parse("token").unwrap();
    let float = PropertyType::parse("float").unwrap();
    let names: Vec<_> = (0..attributes)
        .map(|i| tokens.intern(format!("measure:value{i}")))
        .collect();
    for i in 0..prims {
        let mut spec = PrimSpec::def().with_type_name(scope);
        spec.set_property(
            purpose,
            PropertySpec::typed_attribute(token.clone()).with_default(Value::Token(purpose_value)),
        );
        for &name in &names {
            spec.set_property(name, PropertySpec::typed_attribute(float.clone()));
        }
        layer.define_prim(store.path(&format!("/Scope{i}")), spec, store.paths());
    }
    layer.set_default_prim(Some(tokens.intern("Scope0")));
    let schemas = Arc::new(layerstack_schemas::openusd(tokens));
    store.insert_layer(layer);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    (store, stage)
}

fn validation(c: &mut Criterion) {
    let context = ValidationContext::new(ValidationContext::builtins().iter().copied());
    for (prims, attributes) in [(2048, 3), (16, 512)] {
        let (store, stage) = fixture(prims, attributes);
        let scene = Scene::new(&stage, &store);
        let run = || {
            context
                .validate(&scene, ValidationScope::Stage, &[Time::Default])
                .unwrap()
        };
        assert_eq!(
            run().work.property_specs,
            prims * (attributes + 1),
            "each authored attribute must reach the type check"
        );
        c.bench_function(
            &format!("validation/{prims}_prims_{attributes}_attributes"),
            |b| {
                b.iter(|| black_box(run()));
            },
        );
    }
}

criterion_group!(benches, validation);
criterion_main!(benches);
