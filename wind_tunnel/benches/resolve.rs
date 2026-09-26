// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "bench indices are trivially small"
)]
//! Value resolution benchmarks.
//!
//! Measures the per-query cost of resolving an already composed stage: a
//! sparse array edited in every sublayer, a time-sampled sparse array, a
//! shadowed scalar, a combined dictionary and a chained token list op. These
//! are the paths explanation queries share, so they guard the resolution hot
//! path against diagnostic overhead.

extern crate alloc;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use layerstack::{
    ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, FieldValue, InMemoryStore,
    InterpolationType, Layer, LayerId, ListOp, PrimSpec, PropertyPath, PropertySpec, PropertyType,
    Stage, StageOptions, SublayerEntry, TokenId, Value,
};

const LAYERS: u64 = 20;
const POINTS: usize = 1_000;

struct Scene {
    stage: Stage,
    prim: layerstack::PathId,
    points: PropertyPath,
    animated: PropertyPath,
    size: PropertyPath,
    custom_data: TokenId,
    api_schemas: TokenId,
}

fn float_array(len: usize, scale: f32) -> Value {
    Value::Array((0..len).map(|i| Value::Float(i as f32 * scale)).collect())
}

fn write_edit(index: usize, value: f32) -> Value {
    Value::ArrayEdit(ArrayEdit {
        ops: vec![ArrayEditOp::Write {
            src: ArrayEditOperand::Literal(Value::Float(value)),
            index: ArrayIndex::Position(index as i64),
        }],
    })
}

/// Builds a root layer over `LAYERS - 1` sublayers that all author `/P`.
///
/// The weakest sublayer authors dense arrays; every stronger one authors
/// sparse edits, a scalar, a dictionary entry and a token list op.
fn build() -> Scene {
    let mut store = InMemoryStore::default();
    let points = store.tokens.intern("points");
    let animated = store.tokens.intern("animated");
    let size = store.tokens.intern("size");
    let custom_data = store.tokens.intern("customData");
    let api_schemas = store.tokens.intern("apiSchemas");
    let prim = store.path("/P");
    let float_array_type = PropertyType::new("float", true, Value::Float(0.0));

    let mut root = Layer::new(LayerId(1));
    root.sublayers = (2..=LAYERS)
        .map(|id| SublayerEntry::new(LayerId(id)))
        .collect();
    root.insert_prim(prim, PrimSpec::def());
    store.insert_layer(root);

    for id in 2..=LAYERS {
        let weakest = id == LAYERS;
        let i = id as usize;
        let (points_value, animated_samples) = if weakest {
            (
                float_array(POINTS, 1.0),
                vec![
                    (0.0, float_array(POINTS, 1.0)),
                    (10.0, float_array(POINTS, 2.0)),
                ],
            )
        } else {
            (
                write_edit(i, i as f32),
                vec![(0.0, write_edit(i, 0.0)), (10.0, write_edit(i, 10.0))],
            )
        };
        let entry = Value::Dictionary(vec![(
            alloc::format!("layer{id}").into(),
            Value::Dictionary(vec![("strength".into(), Value::Int(id as i32))]),
        )]);
        let schema = store.tokens.intern(alloc::format!("Api{id}"));
        let spec = PrimSpec::over()
            .with_property(
                points,
                PropertySpec::attribute()
                    .with_type(float_array_type.clone())
                    .with_default(points_value),
            )
            .with_property(
                animated,
                PropertySpec::attribute()
                    .with_type(float_array_type.clone())
                    .with_time_samples(animated_samples),
            )
            .with_property(
                size,
                PropertySpec::attribute()
                    .with_type(PropertyType::new("double", false, Value::Double(0.0)))
                    .with_default(Value::Double(id as f64)),
            )
            .with_field(custom_data, entry)
            .with_field(
                api_schemas,
                FieldValue::TokenListOp(ListOp::prepended(vec![schema])),
            );
        let mut layer = Layer::new(LayerId(id));
        layer.insert_prim(prim, spec);
        store.insert_layer(layer);
    }

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    Scene {
        stage,
        prim,
        points: PropertyPath::new(prim, points),
        animated: PropertyPath::new(prim, animated),
        size: PropertyPath::new(prim, size),
        custom_data,
        api_schemas,
    }
}

fn bench_resolve(c: &mut Criterion) {
    let scene = build();
    let stage = &scene.stage;
    let mut group = c.benchmark_group("resolve");

    group.bench_function("sparse_array_default", |b| {
        b.iter(|| black_box(stage.resolve_property_path(black_box(scene.points))));
    });
    group.bench_function("sparse_array_linear", |b| {
        b.iter(|| {
            black_box(stage.resolve_property_path_at_time(
                black_box(scene.animated),
                black_box(2.5),
                InterpolationType::Linear,
            ))
        });
    });
    group.bench_function("scalar_default", |b| {
        b.iter(|| black_box(stage.resolve_property_path(black_box(scene.size))));
    });
    group.bench_function("scalar_at_time", |b| {
        b.iter(|| {
            black_box(stage.resolve_property_path_at_time(
                black_box(scene.size),
                black_box(2.5),
                InterpolationType::Linear,
            ))
        });
    });
    group.bench_function("dictionary", |b| {
        b.iter(|| black_box(stage.resolve_value(black_box(scene.prim), scene.custom_data)));
    });
    group.bench_function("token_list", |b| {
        b.iter(|| black_box(stage.resolve_token_list(black_box(scene.prim), scene.api_schemas)));
    });

    group.finish();
}

criterion_group!(benches, bench_resolve);
criterion_main!(benches);
