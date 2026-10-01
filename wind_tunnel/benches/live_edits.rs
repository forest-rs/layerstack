// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Live value, subtree and batch edits in increasingly large, nested stages.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertyPath, PropertySpec, PropertyType,
    StageOptions, Value,
    edit::{EditTarget, Transaction},
};

fn build(
    n: usize,
    fanout: usize,
) -> (
    InMemoryStore,
    LiveStage,
    layerstack::PathId,
    layerstack::TokenId,
) {
    let mut store = InMemoryStore::default();
    let translate = store.tokens.intern("xformOp:translate");
    let rotate = store.tokens.intern("xformOp:rotateXYZ");
    let scale = store.tokens.intern("xformOp:scale");
    let xform = store.tokens.intern("Xform");
    let mut layer = Layer::new(LayerId(1));
    let mut names = vec![String::from("/World")];
    for i in 1..n {
        names.push(format!("{}/P{i}", names[(i - 1) / fanout]));
    }
    let mut leaf = store.path("/World");
    for (i, name) in names.iter().enumerate() {
        let path = store.path(name);
        leaf = path;
        let mut rotation = PropertySpec::typed_attribute(PropertyType::new(
            "float3",
            false,
            Value::Vec3f([0.0; 3]),
        ));
        if i % 97 == 0 {
            rotation.time_samples = Some(
                vec![
                    (0.0, Value::Vec3f([0.0; 3])),
                    (10.0, Value::Vec3f([90.0; 3])),
                ]
                .into(),
            );
        } else {
            rotation.default = Some(Value::Vec3f([0.0; 3]));
        }
        layer.insert_prim(
            path,
            PrimSpec::def()
                .with_type_name(xform)
                .with_property(
                    translate,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "double3",
                        false,
                        Value::Vec3d([0.0; 3]),
                    ))
                    .with_default(Value::Vec3d([0.0; 3])),
                )
                .with_property(rotate, rotation)
                .with_property(
                    scale,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "float3",
                        false,
                        Value::Vec3f([1.0; 3]),
                    ))
                    .with_default(Value::Vec3f([1.0; 3])),
                ),
        );
    }
    store.insert_layer(layer);
    let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    (store, live, leaf, translate)
}

fn bench_live_edits(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_value_edit");
    group.sample_size(10);
    for n in [1_000, 10_000, 100_000] {
        let (mut store, mut live, leaf, translate) = build(n, 10);
        let mut txn = Transaction::new();
        txn.set_default(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(leaf, translate)),
            Value::Vec3d([1.0; 3]),
        );
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                // Alternating exact inverses avoids benchmarking no-op sets.
                let applied = live.apply(&mut store, &txn).unwrap();
                black_box(&applied.recomposed);
                txn = applied.inverse;
            });
        });
    }
    group.finish();
}

fn bench_local_structure(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_local_structure");
    group.sample_size(10);
    for n in [1_000, 10_000, 100_000] {
        for (case, count, fanout) in [
            ("toggle_1", 1, 10),
            ("toggle_11", 11, 10),
            ("wide_toggle_1", 1, n),
            ("wide_parent_toggle_1", 1, n),
            ("authored_wide_parent_toggle_1", 1, n),
        ] {
            let (mut store, mut live, leaf, _) = build(n, fanout);
            if case == "authored_wide_parent_toggle_1" {
                // Parsed layers retain explicit child names; programmatic
                // insertion can instead derive hierarchy from prim paths.
                let world = store.path("/World");
                let names = live
                    .stage()
                    .children_of(world)
                    .unwrap()
                    .iter()
                    .map(|path| store.paths.resolve(*path).leaf().unwrap())
                    .collect();
                let layer = store.layers.get_mut(&LayerId(1)).unwrap();
                layer.prims.get_mut(&world).unwrap().authored_children = names;
                layer.touch();
                live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
            }
            let parent = if case.ends_with("parent_toggle_1") {
                store.path("/World")
            } else {
                leaf
            };
            let name = format!(
                "{}/Chunk",
                store.paths.resolve(parent).display(&store.tokens)
            );
            let target = store.path(&name);
            let mut setup = Transaction::new();
            setup.create_prim(
                EditTarget::for_layer(LayerId(1)).prim(target),
                layerstack::Specifier::Def,
                None,
            );
            for i in 1..count {
                let child = store.path(&format!("{name}/Child{i}"));
                setup.create_prim(
                    EditTarget::for_layer(LayerId(1)).prim(child),
                    layerstack::Specifier::Def,
                    None,
                );
            }
            live.apply(&mut store, &setup).unwrap();
            let mut txn = Transaction::new();
            txn.remove_spec(EditTarget::for_layer(LayerId(1)).prim(target));
            group.bench_with_input(BenchmarkId::new(case, n), &n, |b, _| {
                b.iter(|| {
                    let applied = live.apply(&mut store, &txn).unwrap();
                    black_box(&applied.recomposed);
                    txn = applied.inverse;
                });
            });
        }
    }
    group.finish();
}

fn bench_batch_removals(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_batch_remove");
    group.sample_size(10);
    for n in [1_000, 10_000, 100_000] {
        for (case, count, fanout) in [
            ("2", 2, 10),
            ("16", 16, 10),
            ("128", 128, 10),
            ("wide_128", 128, n),
            ("wide_1024", 1024, n),
        ] {
            let (mut store, mut live, leaf, _) = build(n, fanout);
            let parent = if case.starts_with("wide_") {
                store.path("/World")
            } else {
                leaf
            };
            let prefix = store
                .paths
                .resolve(parent)
                .display(&store.tokens)
                .to_string();
            let mut setup = Transaction::new();
            let mut txn = Transaction::new();
            for i in 0..count {
                let path = store.path(&format!("{prefix}/Batch{i}"));
                let address = EditTarget::for_layer(LayerId(1)).prim(path);
                setup.create_prim(address.clone(), layerstack::Specifier::Def, None);
                txn.remove_spec(address);
            }
            live.apply(&mut store, &setup).unwrap();
            group.bench_with_input(BenchmarkId::new(case, n), &n, |b, _| {
                b.iter(|| {
                    // Each sample is one original-command deletion plus its
                    // exact undo. Replaying raw redo would bypass discovery.
                    let removed = live.apply(&mut store, &txn).unwrap();
                    let restored = live.apply(&mut store, &removed.inverse).unwrap();
                    black_box((&removed.changes, &restored.changes));
                });
            });
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_live_edits,
    bench_local_structure,
    bench_batch_removals
);
criterion_main!(benches);
