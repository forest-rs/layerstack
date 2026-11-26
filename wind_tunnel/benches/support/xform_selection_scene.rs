// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use layerstack::{
    InMemoryStore, Layer, LayerId, LayerOffset, LiveStage, PathId, PrimSpec, PropertySpec,
    PropertyType, StageOptions, SublayerEntry, TokenId, Value,
};
use std::sync::Arc;

pub(crate) const SOURCE: LayerId = LayerId(1);
pub(crate) const LEAVES: usize = 5_000;

pub(crate) fn scene(
    every: usize,
    offset: bool,
) -> (InMemoryStore, LiveStage, Vec<PathId>, TokenId) {
    let mut store = InMemoryStore::default();
    let [
        xform,
        order,
        translate,
        scale,
        pivot,
        inverse,
        euler,
        rotate,
        orient,
        matrix,
        reset,
    ] = [
        "Xform",
        "xformOpOrder",
        "xformOp:translate",
        "xformOp:scale",
        "xformOp:translate:pivot",
        "!invert!xformOp:translate:pivot",
        "xformOp:rotateXYZ",
        "xformOp:rotateZ",
        "xformOp:orient",
        "xformOp:transform",
        "!resetXformStack!",
    ]
    .map(|name| store.tokens().intern(name));
    let mut layer = Layer::new(SOURCE);
    let stack = |ops: Vec<TokenId>| {
        PrimSpec::def().with_type_name(xform).with_property(
            order,
            PropertySpec::typed_attribute(PropertyType::parse("token[]").unwrap())
                .with_default(Value::Array(ops.into_iter().map(Value::Token).collect())),
        )
    };
    let value = |ty, first: Value, last| {
        let attr = PropertySpec::typed_attribute(PropertyType::parse(ty).unwrap())
            .with_default(first.clone());
        match last {
            Some(last) => attr.with_time_samples(vec![(0., first), (16., last)]),
            None => attr,
        }
    };
    layer.insert_prim(
        store.path("/World"),
        stack(vec![translate, euler])
            .with_property(
                translate,
                value("double3", Value::Vec3d([2., 3., 5.]), None),
            )
            .with_property(euler, value("double3", Value::Vec3d([13., -7., 5.]), None)),
    );
    let mut paths = Vec::with_capacity(LEAVES);
    for i in 0..LEAVES {
        if i % 20 == 0 {
            let group = store.path(&format!("/World/G{}", i / 20));
            let last = (every != 0 && i % 200 == 0).then_some(Value::Vec3d([1.1, 1.2, 1.5]));
            let scale_attr = value("double3", Value::Vec3d([0.8, 1.4, 1.05]), last);
            layer.insert_prim(group, stack(vec![scale]).with_property(scale, scale_attr));
        }
        let path = store.path(&format!("/World/G{}/P{i}", i / 20));
        let matrix_value = |y| {
            Value::Matrix4d(Box::new([
                1., 0., 0., 0., 0., y, 0., 0., 0., 0., 1., 0., 2., 0., 0., 1.,
            ]))
        };
        let (mut ordering, token, ty, first, last) = match i % 4 {
            0 => (
                vec![translate, pivot, euler, scale, inverse],
                euler,
                "double3",
                Value::Vec3d([9., -24., 13.]),
                Value::Vec3d([72., 5., 2.]),
            ),
            1 => (
                vec![translate, orient, scale],
                orient,
                "quatd",
                Value::Quatd([0.8, 0.6, 0., 0.]),
                Value::Quatd([0.8, 0., 0.6, 0.]),
            ),
            2 => (
                vec![translate, rotate, scale],
                rotate,
                "double",
                Value::Double(-17.),
                Value::Double(36.),
            ),
            _ => (
                vec![translate, matrix],
                matrix,
                "matrix4d",
                matrix_value(1.),
                matrix_value(1.2),
            ),
        };
        if i % 7 == 0 {
            ordering.insert(0, reset);
        }
        let moving = every != 0 && i % every == 0;
        let x = (i % 31) as f64;
        let mut prim = stack(ordering)
            .with_property(
                translate,
                value(
                    "double3",
                    Value::Vec3d([x, 2., 3.]),
                    moving.then_some(Value::Vec3d([x + 16., 14., -8.])),
                ),
            )
            .with_property(token, value(ty, first, moving.then_some(last)));
        if i % 4 != 3 {
            prim = prim.with_property(
                scale,
                value("double3", Value::Vec3d([1.2, 0.75, 1.05]), None),
            );
        }
        if i % 4 == 0 {
            prim = prim.with_property(pivot, value("double3", Value::Vec3d([1., 2., 3.]), None));
        }
        layer.insert_prim(path, prim);
        paths.push(path);
    }
    store.insert_layer(layer);
    let root = if offset {
        let mut layer = Layer::new(LayerId(2));
        layer.sublayers_mut().push(SublayerEntry::with_offset(
            SOURCE,
            LayerOffset {
                offset: 2.,
                scale: 1.25,
            },
        ));
        let root = layer.id();
        store.insert_layer(layer);
        root
    } else {
        SOURCE
    };
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(store.tokens()))),
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, root, options);
    (store, live, paths, translate)
}
