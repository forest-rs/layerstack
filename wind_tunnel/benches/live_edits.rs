// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Live value, subtree and batch edits in increasingly large, nested stages.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, NamespaceEdit, PrimSpec, PropertyPath, PropertySpec,
    PropertyType, StageOptions, TargetPath, Value,
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
        let mut rotation = PropertySpec::typed_attribute(PropertyType::parse("float3").unwrap());
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
                    PropertySpec::typed_attribute(PropertyType::parse("double3").unwrap())
                        .with_default(Value::Vec3d([0.0; 3])),
                )
                .with_property(rotate, rotation)
                .with_property(
                    scale,
                    PropertySpec::typed_attribute(PropertyType::parse("float3").unwrap())
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
                layer.prim_mut(world).unwrap().authored_children = names;
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

/// One transaction creating `n` sibling prims, each with three attributes,
/// as a procedural generator authors them, then undoing it: the cost of a
/// bulk edit should grow with `n`, not with `n` squared.
fn bench_bulk_create(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_bulk_create");
    group.sample_size(10);
    for n in [1_000, 10_000, 100_000] {
        let mut store = InMemoryStore::default();
        let world = store.path("/World");
        let root = store.path("/");
        let name = store.tokens.intern("World");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(world, PrimSpec::def());
        layer.insert_prim(root, PrimSpec::default().with_children(vec![name]));
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let target = EditTarget::for_layer(LayerId(1));
        let cube = store.tokens.intern("Cube");
        let names = ["x", "y", "z"].map(|name| store.tokens.intern(name));
        let mut create = Transaction::new();
        for i in 0..n {
            let path = store.path(&format!("/World/Item{i}"));
            create.create_prim(target.prim(path), layerstack::Specifier::Def, Some(cube));
            for (at, &property) in names.iter().enumerate() {
                create.create_property(
                    target.property(PropertyPath::new(path, property)),
                    PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
                        .with_default(Value::Double(at as f64)),
                );
            }
        }
        group.bench_with_input(BenchmarkId::new("create", n), &n, |b, _| {
            b.iter_custom(|iterations| {
                let mut total = core::time::Duration::ZERO;
                for _ in 0..iterations {
                    let start = std::time::Instant::now();
                    let applied = live.apply(&mut store, &create).unwrap();
                    total += start.elapsed();
                    black_box(&applied.recomposed);
                    live.apply(&mut store, &applied.inverse).unwrap();
                }
                total
            });
        });
    }
    group.finish();
}

/// A stage whose `/World` holds `existing` child prims, listed by `/World`
/// as a layer read from USDA lists them when `listed`, and the `n`
/// transactions that each create one more child of `/World`.
fn many_creates(
    existing: usize,
    listed: bool,
    n: usize,
) -> (InMemoryStore, LiveStage, Vec<Transaction>) {
    let mut store = InMemoryStore::default();
    let world = store.path("/World");
    let root = store.path("/");
    let name = store.tokens.intern("World");
    let mut layer = Layer::new(LayerId(1));
    let mut children = Vec::new();
    for i in 0..existing {
        let path = store.path(&format!("/World/Rock{i}"));
        layer.insert_prim(path, PrimSpec::def());
        children.push(store.paths.resolve(path).leaf().unwrap());
    }
    let mut parent = PrimSpec::def();
    if listed {
        parent.authored_children = children;
    }
    layer.insert_prim(world, parent);
    layer.insert_prim(root, PrimSpec::default().with_children(vec![name]));
    store.insert_layer(layer);
    let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let target = EditTarget::for_layer(LayerId(1));
    let txns = (0..n)
        .map(|i| {
            let path = store.path(&format!("/World/Plant{i}"));
            let mut txn = Transaction::new();
            txn.create_prim(target.prim(path), layerstack::Specifier::Def, None);
            txn
        })
        .collect();
    (store, live, txns)
}

/// `n` transactions applied one after another, each creating one child of
/// the same parent, as an interactive author adds prims: each edit should
/// cost about the same however many came before, so the total grows with
/// `n`, not with `n` squared. `empty` starts from a childless parent;
/// `listed` and `unlisted` start from a parent with `n` children that it
/// lists (as a layer read from USDA does) or that only their prim specs
/// name (as a layer built by `Layer::insert_prim` does).
fn bench_many_creates(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_many_creates");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for n in [1_000, 2_000, 4_000, 8_000] {
        for (case, existing, listed) in [
            ("empty", 0, false),
            ("unlisted", n, false),
            ("listed", n, true),
        ] {
            group.bench_with_input(BenchmarkId::new(case, n), &n, |b, &n| {
                b.iter_custom(|iterations| {
                    let mut total = core::time::Duration::ZERO;
                    for _ in 0..iterations {
                        let (mut store, mut live, txns) = many_creates(existing, listed, n);
                        let start = std::time::Instant::now();
                        for txn in &txns {
                            black_box(live.apply(&mut store, txn).unwrap());
                        }
                        total += start.elapsed();
                    }
                    total
                });
            });
        }
    }
    group.finish();
}

/// A stage with `existing` root prims, each with two children, and the
/// `n` transactions that each create one more root prim. When `listed`,
/// the pseudo-root and each root prim list their children, as a layer
/// read from USDA does; otherwise only their prim specs name them, as in a
/// layer built by `Layer::insert_prim`, which holds no pseudo-root spec
/// until the first root prim is authored.
fn many_root_creates(
    existing: usize,
    listed: bool,
    n: usize,
) -> (InMemoryStore, LiveStage, Vec<Transaction>) {
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(LayerId(1));
    let leaves = ["Moss", "Lichen"].map(|name| store.tokens.intern(name));
    let mut roots = Vec::new();
    for i in 0..existing {
        let name = format!("/Rock{i}");
        let path = store.path(&name);
        for leaf in ["Moss", "Lichen"] {
            let child = store.path(&format!("{name}/{leaf}"));
            layer.insert_prim(child, PrimSpec::def());
        }
        let mut spec = PrimSpec::def();
        if listed {
            spec.authored_children = leaves.to_vec();
        }
        layer.insert_prim(path, spec);
        roots.push(store.paths.resolve(path).leaf().unwrap());
    }
    if listed {
        let root = store.path("/");
        layer.insert_prim(root, PrimSpec::default().with_children(roots));
    }
    store.insert_layer(layer);
    let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let target = EditTarget::for_layer(LayerId(1));
    let txns = (0..n)
        .map(|i| {
            let path = store.path(&format!("/Plant{i}"));
            let mut txn = Transaction::new();
            txn.create_prim(target.prim(path), layerstack::Specifier::Def, None);
            txn
        })
        .collect();
    (store, live, txns)
}

/// `n` transactions applied one after another, each creating one root
/// prim, on a stage that already has `n` root prims with two children
/// each: as for children of one parent ([`bench_many_creates`]), each edit
/// should compose the pseudo-root's child names once and the new prim,
/// not the stage. `listed` and `unlisted` are as for
/// [`many_root_creates`].
fn bench_many_root_creates(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_many_root_creates");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for n in [1_000, 2_000, 4_000, 8_000] {
        for (case, listed) in [("unlisted", false), ("listed", true)] {
            group.bench_with_input(BenchmarkId::new(case, n), &n, |b, &n| {
                b.iter_custom(|iterations| {
                    let mut total = core::time::Duration::ZERO;
                    for _ in 0..iterations {
                        let (mut store, mut live, txns) = many_root_creates(n, listed, n);
                        let start = std::time::Instant::now();
                        for txn in &txns {
                            black_box(live.apply(&mut store, txn).unwrap());
                        }
                        total += start.elapsed();
                    }
                    total
                });
            });
        }
    }
    group.finish();
}

/// Root prims renamed one after another, each by its own namespace edit,
/// on a stage of `n` root prims with two children each: a rename rewrites
/// the pseudo-root's child names, and should compose them once and the
/// renamed subtree, not the stage. Only the transactions are timed, not
/// preparing them. `listed` and `unlisted` are as for
/// [`many_root_creates`].
fn bench_root_renames(c: &mut Criterion) {
    const RENAMES: usize = 64;
    let mut group = c.benchmark_group("live_root_renames");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for n in [1_000, 2_000, 4_000, 8_000] {
        for (case, listed) in [("unlisted", false), ("listed", true)] {
            group.bench_with_input(BenchmarkId::new(case, n), &n, |b, &n| {
                b.iter_custom(|iterations| {
                    let mut total = core::time::Duration::ZERO;
                    for _ in 0..iterations {
                        let (mut store, mut live, _) = many_root_creates(n, listed, 0);
                        for i in 0..RENAMES {
                            let from = TargetPath::Prim(store.path(&format!("/Rock{i}")));
                            let to = TargetPath::Prim(store.path(&format!("/Stone{i}")));
                            let edit = NamespaceEdit::prepare(
                                live.stage(),
                                &mut store,
                                &EditTarget::for_layer(LayerId(1)),
                                from,
                                to,
                            )
                            .unwrap();
                            let start = std::time::Instant::now();
                            black_box(live.apply(&mut store, edit.transaction()).unwrap());
                            total += start.elapsed();
                        }
                    }
                    total
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
    bench_batch_removals,
    bench_bulk_create,
    bench_many_creates,
    bench_many_root_creates,
    bench_root_renames
);
criterion_main!(benches);
