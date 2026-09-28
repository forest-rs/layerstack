// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Cost of cache maintenance for a precise, unrelated property edit.
use criterion::{Criterion, criterion_group, criterion_main};
use layerstack::{
    Changes, InMemoryStore, Layer, LayerId, PrimPropertyChanges, PrimSpec, PropertyChange,
    PropertyField, Stage, StageOptions,
};
use layerstack_schemas::{
    Scene, Time, XformCache,
    bounds::{BoundsCache, BoundsOptions},
};
use std::{hint::black_box, sync::Arc};

fn bench(c: &mut Criterion) {
    let mut store = InMemoryStore::default();
    let root = store.path("/World");
    let cube = store.tokens.intern("Cube");
    let roughness = store.tokens.intern("inputs:roughness");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(root, PrimSpec::def());
    let mut paths = Vec::new();
    for i in 0..1000 {
        let path = store.path(&format!("/World/P{i}"));
        layer.insert_prim(path, PrimSpec::def().with_type_name(cube));
        paths.push(path);
    }
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut store, LayerId(1), options);
    let scene = Scene::new(&stage, &store);
    let mut xforms = XformCache::new(Time::Default);
    let mut bounds = BoundsCache::new(Time::Default, BoundsOptions::default());
    for &path in &paths {
        black_box(xforms.local_to_world(&scene, path));
    }
    black_box(bounds.world_bound(&scene, root).unwrap());
    let changes = Changes {
        changed_info_only: vec![root],
        property_changes: vec![PrimPropertyChanges {
            prim: root,
            fields: vec![PropertyChange {
                name: roughness,
                field: PropertyField::Default,
            }],
        }],
        ..Changes::default()
    };
    c.bench_function("unrelated_property/xform_leaf", |b| {
        b.iter(|| {
            xforms.apply_changes(&scene, &changes);
            black_box(xforms.local_to_world(&scene, paths[999]));
        });
    });
    c.bench_function("unrelated_property/world_bound", |b| {
        b.iter(|| {
            bounds.apply_changes(&scene, &changes);
            black_box(bounds.world_bound(&scene, root).unwrap());
        });
    });
}
criterion_group!(benches, bench);
criterion_main!(benches);
