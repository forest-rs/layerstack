// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Instancing scaling benchmarks.
//!
//! Measures composition, source edits and unrelated structural edits as the instance count grows.
//! Each instance references a shared coral asset (two child prims: branches
//! and polyps), mirroring the `instancing` example.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec, PropertyType, Reference,
    Stage, StageOptions, SublayerEntry, Value,
    edit::{EditTarget, Transaction},
};

/// Build a store with `n` instanced corals under `/Reef` plus one shared
/// `/Coral` asset.
fn build(n: usize) -> InMemoryStore {
    let mut store = InMemoryStore::default();

    let f_vertices = store.tokens().intern("vertices");
    let f_display_color = store.tokens().intern("primvars:displayColor");
    let branches_tok = store.tokens().intern("branches");
    let polyps_tok = store.tokens().intern("polyps");

    // --- Asset layer (LayerId 2) ---
    let coral = store.path("/Coral");
    let coral_branches = store.path("/Coral/branches");
    let coral_polyps = store.path("/Coral/polyps");

    let mut asset = Layer::new(LayerId(2));

    asset.insert_prim(
        coral,
        PrimSpec::def()
            .with_children(vec![branches_tok, polyps_tok])
            .with_field(f_display_color, Value::string("gray")),
    );
    asset.insert_prim(
        coral_branches,
        PrimSpec::def().with_field(f_vertices, Value::Int(2400)),
    );
    asset.insert_prim(
        coral_polyps,
        PrimSpec::def().with_field(f_vertices, Value::Int(8000)),
    );

    store.insert_layer(asset);

    // --- Reef layer (LayerId 1) ---
    let reef = store.path("/Reef");

    let mut reef_layer = Layer::new(LayerId(1));

    let mut reef_children = Vec::with_capacity(n);

    for i in 0..n {
        let name = alloc::format!("Coral_{i:05}");
        let name_tok = store.tokens().intern(&name);
        reef_children.push(name_tok);

        let path = store.path(&alloc::format!("/Reef/{name}"));

        let mut spec = PrimSpec::def()
            .with_instanceable(true)
            .with_reference(Reference::with_asset(LayerId(2), coral, "coral_asset.usd"));
        // Give every 10th coral a per-instance color override.
        if i % 10 == 0 {
            spec.set_field(f_display_color, Value::string("green"));
        }
        reef_layer.insert_prim(path, spec);
    }

    reef_layer.insert_prim(reef, PrimSpec::def().with_children(reef_children));

    store.insert_layer(reef_layer);

    store
}

fn bench_instancing(c: &mut Criterion) {
    let mut group = c.benchmark_group("instancing_compose");

    for &n in &[100, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| Stage::compose(&mut build(n), LayerId(1), StageOptions::default()));
        });
    }

    group.finish();
}

fn bench_unrelated_override(c: &mut Criterion) {
    let mut group = c.benchmark_group("instancing_unrelated_override");
    let root = LayerId(1);
    let weaker = LayerId(3);
    for n in [100, 1_000] {
        let mut store = build(n);
        let label = store.property_path("/Settings.label");
        let mut source = Layer::new(weaker);
        source.insert_prim(
            label.prim_path(),
            PrimSpec::def().with_property(
                label.property(),
                PropertySpec::typed_attribute(PropertyType::parse("string").unwrap())
                    .with_default(Value::string("original")),
            ),
        );
        store.insert_layer(source);
        store
            .layer_mut(root)
            .unwrap()
            .sublayers_mut()
            .push(SublayerEntry::new(weaker));
        let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
        let mut edit = Transaction::new();
        // Authoring into the stronger layer creates the first property spec there.
        edit.set_default(
            EditTarget::for_layer(root).property(label),
            Value::string("edited"),
        );
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let applied = live.apply(&mut store, &edit).unwrap();
                let undone = live.apply(&mut store, &applied.inverse).unwrap();
                std::hint::black_box((applied.changes, undone.changes));
            });
        });
    }
    group.finish();
}

/// Sixteen independent sources keep the changed fan-out proportional to one
/// group. Both layouts author the same inherit arcs and edit the same source.
fn bench_shared_override(c: &mut Criterion) {
    let mut group = c.benchmark_group("shared_source_override");
    for n in [256, 4_096] {
        for instanceable in [false, true] {
            let mut store = InMemoryStore::default();
            let root = LayerId(1);
            let weaker = LayerId(2);
            let mut layer = Layer::new(weaker);
            let mut sources = Vec::new();
            for i in 0..16 {
                let source = store.path(&format!("/Source{i}"));
                let label = store.property_path(&format!("/Source{i}/Branch.label"));
                sources.push(source);
                layer.define_prim(
                    label.prim_path(),
                    PrimSpec::def().with_property(
                        label.property(),
                        PropertySpec::typed_attribute(PropertyType::parse("string").unwrap())
                            .with_default(Value::string("original")),
                    ),
                    store.paths(),
                );
            }
            for i in 0..n {
                let prim = store.path(&format!("/Reef/Coral{i}"));
                layer.define_prim(
                    prim,
                    PrimSpec::def()
                        .with_inherit(sources[i % sources.len()])
                        .with_instanceable(instanceable),
                    store.paths(),
                );
            }
            store.insert_layer(layer);
            let mut overrides = Layer::new(root);
            overrides.sublayers_mut().push(SublayerEntry::new(weaker));
            store.insert_layer(overrides);
            let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
            let label = store.property_path("/Source0/Branch.label");
            let mut edit = Transaction::new();
            edit.set_default(
                EditTarget::for_layer(root).property(label),
                Value::string("edited"),
            );
            let layout = if instanceable {
                "instanced"
            } else {
                "ordinary"
            };
            group.bench_function(BenchmarkId::new(layout, n), |b| {
                b.iter(|| {
                    let applied = live.apply(&mut store, &edit).unwrap();
                    let undone = live.apply(&mut store, &applied.inverse).unwrap();
                    std::hint::black_box((applied.changes, undone.changes));
                });
            });
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_instancing,
    bench_unrelated_override,
    bench_shared_override
);
criterion_main!(benches);

extern crate alloc;
