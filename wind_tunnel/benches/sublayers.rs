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
    FieldValue, HashMap, InMemoryStore, Layer, LayerId, Path, PathId, PrimSpec, Specifier, Stage,
    StageOptions, Value,
};

/// Intern an absolute path.
fn p(store: &mut InMemoryStore, s: &str) -> PathId {
    store
        .paths
        .intern(Path::parse_absolute(s, &mut store.tokens).expect("valid path"))
}

/// Build a scene with `n_layers` sublayers, each providing opinions on
/// `n_prims` prims, then compose.
fn build_and_compose(n_layers: usize, n_prims: usize) -> Stage {
    let mut store = InMemoryStore::default();

    let f_value = store.tokens.intern("value");
    let f_priority = store.tokens.intern("priority");

    // Pre-intern prim paths and child name tokens.
    let root = p(&mut store, "/Root");
    let mut child_paths = Vec::with_capacity(n_prims);
    let mut child_tokens = Vec::with_capacity(n_prims);
    for i in 0..n_prims {
        let name = alloc::format!("Prim_{i:04}");
        child_tokens.push(store.tokens.intern(&name));
        child_paths.push(p(&mut store, &alloc::format!("/Root/{name}")));
    }

    // Root layer (LayerId 1) — defines the sublayer chain and the root prim.
    let sublayer_ids: Vec<LayerId> = (2..=(n_layers as u64)).map(LayerId).collect();

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: sublayer_ids,
        prims: HashMap::new(),
    };

    // Root prim with children, plus strongest opinions.
    root_layer.prims.insert(
        root,
        PrimSpec {
            specifier: Some(Specifier::Def),
            authored_children: child_tokens.clone(),
            ..PrimSpec::default()
        },
    );
    for (j, &path) in child_paths.iter().enumerate() {
        let mut spec = PrimSpec {
            specifier: Some(Specifier::Def),
            ..PrimSpec::default()
        };
        spec.fields.insert(
            f_value,
            FieldValue::Value(Value::String(alloc::format!("layer1_prim{j}").into())),
        );
        spec.fields
            .insert(f_priority, FieldValue::Value(Value::Int(1)));
        root_layer.prims.insert(path, spec);
    }
    store.insert_layer(root_layer);

    // Sublayers 2..=n_layers — each provides weaker opinions on all prims.
    for layer_idx in 2..=n_layers {
        let mut layer = Layer {
            id: LayerId(layer_idx as u64),
            sublayers: vec![],
            prims: HashMap::new(),
        };
        for (j, &path) in child_paths.iter().enumerate() {
            let mut spec = PrimSpec {
                specifier: Some(Specifier::Over),
                ..PrimSpec::default()
            };
            spec.fields.insert(
                f_value,
                FieldValue::Value(Value::String(
                    alloc::format!("layer{layer_idx}_prim{j}").into(),
                )),
            );
            let priority = layer_idx as i32;
            spec.fields
                .insert(f_priority, FieldValue::Value(Value::Int(priority)));
            layer.prims.insert(path, spec);
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

criterion_group!(benches, bench_sublayers);
criterion_main!(benches);
