// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use layerstack::{
    InMemoryStore, Layer, LayerId, PathId, PrimSpec, PropertySpec, PropertyType, Stage,
    StageOptions, Value,
};
use std::sync::Arc;

pub(crate) fn scene(
    n: usize,
    animated_every: usize,
    pivot: bool,
) -> (InMemoryStore, Stage, Vec<PathId>) {
    let mut store = InMemoryStore::default();
    let xform = store.tokens.intern("Xform");
    let order = store.tokens.intern("xformOpOrder");
    let names = [
        "xformOp:translate",
        "xformOp:rotateXYZ",
        "xformOp:scale",
        "xformOp:translate:pivot",
    ];
    let tokens = names.map(|name| store.tokens.intern(name));
    let inverse = store.tokens.intern("!invert!xformOp:translate:pivot");
    let reset = store.tokens.intern("!resetXformStack!");
    let mut layer = Layer::new(LayerId(1));
    let mut paths = Vec::new();
    for i in 0..n {
        let group = i / 16;
        if i % 16 == 0 {
            let parent = store.path(&format!("/World/G{group}"));
            layer.insert_prim(parent, PrimSpec::def().with_type_name(xform));
        }
        let path = store.path(&format!("/World/G{group}/P{i}"));
        let mut ops = vec![tokens[0], tokens[1], tokens[2]];
        if pivot {
            ops = vec![tokens[0], tokens[3], tokens[1], tokens[2], inverse];
        }
        if i % 7 == 0 {
            ops.insert(0, reset);
        }
        let mut prim = PrimSpec::def().with_type_name(xform).with_property(
            order,
            PropertySpec::typed_attribute(PropertyType::new(
                "token",
                true,
                Value::Token(tokens[0]),
            ))
            .with_default(Value::Array(ops.into_iter().map(Value::Token).collect())),
        );
        for (op, token) in tokens
            .into_iter()
            .enumerate()
            .take(if pivot { 4 } else { 3 })
        {
            let v = match op {
                0 => [i as f64 % 11.0, 2.0, 3.0],
                1 => [10.0, 20.0, 30.0],
                2 => [1.0, 1.2, 0.8],
                _ => [2.0, 3.0, 4.0],
            };
            let mut spec = PropertySpec::typed_attribute(PropertyType::new(
                "double3",
                false,
                Value::Vec3d([0.0; 3]),
            ))
            .with_default(Value::Vec3d(v));
            if op == 0 && animated_every != 0 && i % animated_every == 0 {
                spec = spec.with_time_samples(vec![
                    (0.0, Value::Vec3d(v)),
                    (10.0, Value::Vec3d([v[0] + 10.0, v[1] + 5.0, v[2] - 2.0])),
                ]);
            }
            prim = prim.with_property(token, spec);
        }
        layer.insert_prim(path, prim);
        paths.push(path);
    }
    let root = store.path("/World");
    layer.insert_prim(root, PrimSpec::def().with_type_name(xform));
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut store, LayerId(1), options);
    (store, stage, paths)
}
