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
    ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, FieldValue, InMemoryStore, Layer,
    LayerId, ListOp, PrimSpec, PropertyPath, PropertySpec, PropertyType, Stage, StageOptions,
    SublayerEntry, Time, TokenId, Value,
};

const LAYERS: u64 = 20;
const POINTS: usize = 1_000;

struct Scene {
    store: InMemoryStore,
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

fn write_edit(index: usize, value: f32, instructions: usize) -> Value {
    Value::ArrayEdit(ArrayEdit {
        ops: (0..instructions)
            .map(|offset| ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Float(value)),
                index: ArrayIndex::Position((index + offset) as i64),
            })
            .collect(),
    })
}

/// Builds a root layer over `LAYERS - 1` sublayers that all author `/P`.
///
/// The weakest sublayer authors dense arrays; every stronger one authors
/// sparse edits, a scalar, a dictionary entry and a token list op.
fn build(layers: u64, points_len: usize, instructions: usize) -> Scene {
    let mut store = InMemoryStore::default();
    let points = store.tokens().intern("points");
    let animated = store.tokens().intern("animated");
    let size = store.tokens().intern("size");
    let custom_data = store.tokens().intern("customData");
    let api_schemas = store.tokens().intern("apiSchemas");
    let prim = store.path("/P");
    let float_array_type = PropertyType::parse("float[]").unwrap();

    let mut root = Layer::new(LayerId(1));
    *root.sublayers_mut() = (2..=layers)
        .map(|id| SublayerEntry::new(LayerId(id)))
        .collect();
    root.insert_prim(prim, PrimSpec::def());
    store.insert_layer(root);

    for id in 2..=layers {
        let weakest = id == layers;
        let i = id as usize;
        let (points_value, animated_samples) = if weakest {
            (
                float_array(points_len, 1.0),
                vec![
                    (0.0, float_array(points_len, 1.0)),
                    (10.0, float_array(points_len, 2.0)),
                ],
            )
        } else {
            (
                write_edit(i, i as f32, instructions),
                vec![
                    (0.0, write_edit(i, 0.0, instructions)),
                    (10.0, write_edit(i, 10.0, instructions)),
                ],
            )
        };
        let entry = Value::Dictionary(vec![(
            alloc::format!("layer{id}").into(),
            Value::Dictionary(vec![("strength".into(), Value::Int(id as i32))]),
        )]);
        let schema = store.tokens().intern(alloc::format!("Api{id}"));
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
                    .with_type(PropertyType::parse("double").unwrap())
                    .with_default(Value::Double(id as f64)),
            )
            .with_field(custom_data, entry)
            .with_field(
                api_schemas,
                FieldValue::TokenListOp(Box::new(ListOp::prepended(vec![schema]))),
            );
        let mut layer = Layer::new(LayerId(id));
        layer.insert_prim(prim, spec);
        store.insert_layer(layer);
    }

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    Scene {
        store,
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
    let scene = build(LAYERS, POINTS, 1);
    // The handles are looked up once, outside the timed loops: each bench
    // measures the resolution its handle query runs.
    let view = scene.stage.view(&scene.store);
    let prim = view.prim(scene.prim).expect("/P");
    let (points, animated, size) = (
        view.attribute(scene.points).expect("points"),
        view.attribute(scene.animated).expect("animated"),
        view.attribute(scene.size).expect("size"),
    );
    let mut group = c.benchmark_group("resolve");

    group.bench_function("sparse_array_default", |b| {
        b.iter(|| black_box(black_box(points).authored_value(Time::Default)));
    });
    group.bench_function("sparse_array_linear", |b| {
        b.iter(|| black_box(black_box(animated).authored_value(Time::at(black_box(2.5)))));
    });
    group.bench_function("scalar_default", |b| {
        b.iter(|| black_box(black_box(size).authored_value(Time::Default)));
    });
    group.bench_function("scalar_at_time", |b| {
        b.iter(|| black_box(black_box(size).authored_value(Time::at(black_box(2.5)))));
    });
    group.bench_function("dictionary", |b| {
        b.iter(|| black_box(black_box(prim).metadata(scene.custom_data)));
    });
    group.bench_function("token_list", |b| {
        b.iter(|| black_box(black_box(prim).metadata(scene.api_schemas)));
    });

    // Tiny arrays expose planning costs; large arrays include materialization.
    // Two layers give a dense-only series; deeper stacks add sparse opinions.
    for (layers, points) in [(2, 1), (20, 1), (100, 1), (100, 1_000)] {
        let scene = build(layers, points, 1);
        let animated = scene
            .stage
            .view(&scene.store)
            .attribute(scene.animated)
            .expect("animated");
        group.bench_function(format!("temporal/{layers}_layers/{points}_elements"), |b| {
            b.iter(|| black_box(black_box(animated).authored_value(Time::at(black_box(2.5)))));
        });
    }

    // Multi-instruction programs separate authored-edit copying from the
    // planner's per-source costs. All writes fit in the dense array.
    let scene = build(20, 1_000, 64);
    let view = scene.stage.view(&scene.store);
    let (points, animated) = (
        view.attribute(scene.points).expect("points"),
        view.attribute(scene.animated).expect("animated"),
    );
    group.bench_function("sparse_program_default", |b| {
        b.iter(|| black_box(black_box(points).authored_value(Time::Default)));
    });
    group.bench_function("sparse_program_linear", |b| {
        b.iter(|| black_box(black_box(animated).authored_value(Time::at(black_box(2.5)))));
    });

    group.finish();
}

criterion_group!(benches, bench_resolve);
criterion_main!(benches);
