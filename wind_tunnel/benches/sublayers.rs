// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![allow(
    clippy::cast_possible_truncation,
    reason = "bench indices are trivially small"
)]
//! Sublayer stacking benchmarks.
//!
//! Measures `Stage::compose` time as the number of sublayers grows.
//! Each sublayer contributes an opinion on a shared set of prims,
//! exercising opinion traversal and strength ordering.

extern crate alloc;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, PropertyType, Reference, Stage,
    StageOptions, SublayerEntry, Value,
};

/// Build a scene with `n_layers` sublayers, each providing opinions on
/// `n_prims` prims, then compose.
fn build_and_compose(n_layers: usize, n_prims: usize) -> Stage {
    let mut store = InMemoryStore::default();

    let f_value = store.tokens().intern("value");
    let f_priority = store.tokens().intern("priority");

    // Pre-intern prim paths and child name tokens.
    let root = store.path("/Root");
    let mut child_paths = Vec::with_capacity(n_prims);
    let mut child_tokens = Vec::with_capacity(n_prims);
    for i in 0..n_prims {
        let name = alloc::format!("Prim_{i:04}");
        child_tokens.push(store.tokens().intern(&name));
        child_paths.push(store.path(&alloc::format!("/Root/{name}")));
    }

    // Root layer (LayerId 1) — defines the sublayer chain and the root prim.
    let sublayer_ids: Vec<SublayerEntry> = (2..=(n_layers as u64))
        .map(|n| SublayerEntry::new(LayerId(n)))
        .collect();

    let mut root_layer = Layer::new(LayerId(1));
    *root_layer.sublayers_mut() = sublayer_ids;

    // Root prim with children, plus strongest opinions.
    root_layer.insert_prim(root, PrimSpec::def().with_children(child_tokens.clone()));
    for (j, &path) in child_paths.iter().enumerate() {
        root_layer.insert_prim(
            path,
            PrimSpec::def()
                .with_field(f_value, Value::string(alloc::format!("layer1_prim{j}")))
                .with_field(f_priority, 1),
        );
    }
    store.insert_layer(root_layer);

    // Sublayers 2..=n_layers — each provides weaker opinions on all prims.
    for layer_idx in 2..=n_layers {
        let mut layer = Layer::new(LayerId(layer_idx as u64));
        for (j, &path) in child_paths.iter().enumerate() {
            let priority = layer_idx as i32;
            layer.insert_prim(
                path,
                PrimSpec::over()
                    .with_field(
                        f_value,
                        Value::string(alloc::format!("layer{layer_idx}_prim{j}")),
                    )
                    .with_field(f_priority, priority),
            );
        }
        store.insert_layer(layer);
    }

    Stage::compose(&mut store, LayerId(1), StageOptions::default())
}

fn bench_sublayers(c: &mut Criterion) {
    let mut group = c.benchmark_group("sublayer_compose");

    // Scale sublayer count with a fixed number of prims.
    let n_prims = 50;
    for &n_layers in &[10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("layers", alloc::format!("{n_layers}x{n_prims}")),
            &(n_layers, n_prims),
            |b, &(nl, np)| {
                b.iter(|| build_and_compose(nl, np));
            },
        );
    }

    // Scale prim count with a moderate layer stack.
    let n_layers = 20;
    for &n_prims in &[10, 100, 500] {
        group.bench_with_input(
            BenchmarkId::new("prims", alloc::format!("{n_layers}x{n_prims}")),
            &(n_layers, n_prims),
            |b, &(nl, np)| {
                b.iter(|| build_and_compose(nl, np));
            },
        );
    }

    group.finish();
}

#[derive(Clone, Copy)]
enum LocalOpinions {
    SingleLayer,
    Sublayers,
    WithReferences,
}

fn prepare_local_opinions(count: usize, scene: LocalOpinions) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let mut strong = Layer::new(LayerId(1));
    let mut weak = Layer::new(LayerId(2));
    let mut asset = Layer::new(LayerId(3));
    let target = store.path("/Asset");
    let label = store.tokens().intern("label");
    let display = store.tokens().intern("display");
    let ty = PropertyType::parse("string").expect("string type");
    let property =
        |text: &str| PropertySpec::typed_attribute(ty.clone()).with_default(Value::string(text));
    if !matches!(scene, LocalOpinions::SingleLayer) {
        strong.sublayers_mut().push(SublayerEntry::new(weak.id()));
    }
    if matches!(scene, LocalOpinions::WithReferences) {
        asset.define_prim(
            target,
            PrimSpec::def().with_property(label, property("asset")),
            store.paths(),
        );
    }
    for n in 0..count {
        let path = store.path(&alloc::format!("/Group{}/Item{}", n / 64, n % 64));
        let mut spec = PrimSpec::def().with_property(label, property("original"));
        if matches!(scene, LocalOpinions::WithReferences) && n % 8 == 0 {
            spec = spec.with_reference(Reference::new(asset.id(), target));
        }
        if matches!(scene, LocalOpinions::SingleLayer) {
            strong.define_prim(path, spec, store.paths());
        } else {
            weak.define_prim(path, spec, store.paths());
            if n % 4 == 0 {
                strong.define_prim(
                    path,
                    PrimSpec::over().with_property(display, property("extra")),
                    store.paths(),
                );
            }
        }
    }
    store.insert_layer(asset);
    store.insert_layer(weak);
    store.insert_layer(strong);
    store
}

fn bench_local_opinions(c: &mut Criterion) {
    let mut group = c.benchmark_group("compose_local_opinions");
    for count in [1_024, 65_536] {
        for (name, scene) in [
            ("single_layer", LocalOpinions::SingleLayer),
            ("sublayers", LocalOpinions::Sublayers),
            ("with_references", LocalOpinions::WithReferences),
        ] {
            let mut store = prepare_local_opinions(count, scene);
            let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
            assert!(
                stage.composition_errors().is_empty(),
                "the prepared scene composes without errors"
            );
            let work = stage.composition_work();
            let prims = count + count.div_ceil(64) + 1;
            let references = if matches!(scene, LocalOpinions::WithReferences) {
                count.div_ceil(8)
            } else {
                0
            };
            assert_eq!(
                work.composed_prim_indexes, prims,
                "all authored prims compose"
            );
            assert_eq!(
                work.prim_index_nodes,
                prims + references,
                "each reference adds a node to its prim index"
            );
            drop(stage);

            group.bench_function(BenchmarkId::new(name, count), |b| {
                b.iter(|| Stage::compose(&mut store, LayerId(1), StageOptions::default()));
            });
        }
    }
    group.finish();
}

/// Every prim has one property. Vary how many layers actually author each
/// prim independently of the number of layers in the stack.
fn prepare_source_density(layers: usize, authors: usize, prims: usize) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let root = store.path("/Root");
    let field = store.tokens().intern("value");
    let mut stack: Vec<_> = (0..layers)
        .map(|at| Layer::new(LayerId(1 + at as u64)))
        .collect();
    let mut children = vec![Vec::new(); layers];
    *stack[0].sublayers_mut() = stack[1..]
        .iter()
        .map(|l| SublayerEntry::new(l.id()))
        .collect();
    for n in 0..prims {
        let path = store.path(&alloc::format!("/Root/Item{n}"));
        let name = store.paths().resolve(path).leaf().expect("a child name");
        for offset in 0..authors {
            let at = (n + offset) % layers;
            stack[at].insert_prim(path, PrimSpec::def().with_field(field, at as i32));
            children[at].push(name);
        }
    }
    for (mut layer, names) in stack.into_iter().zip(children) {
        layer.insert_prim(root, PrimSpec::def().with_children(names));
        store.insert_layer(layer);
    }
    store
}

fn bench_source_density(c: &mut Criterion) {
    let mut group = c.benchmark_group("compose_layer_sources");
    for (layers, authors) in [(1, 1), (8, 1), (8, 2), (8, 8), (64, 1), (64, 64)] {
        let mut store = prepare_source_density(layers, authors, 1_024);
        group.bench_function(format!("{authors}_of_{layers}"), |b| {
            b.iter(|| Stage::compose(&mut store, LayerId(1), StageOptions::default()));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_sublayers,
    bench_local_opinions,
    bench_source_density
);
criterion_main!(benches);
