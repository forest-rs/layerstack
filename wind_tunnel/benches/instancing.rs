//! Instancing scaling benchmarks.
//!
//! Measures `Stage::compose` time as the number of instanced prims grows.
//! Each instance references a shared coral asset (two child prims: branches
//! and polyps), mirroring the `instancing` example.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Reference, Stage, StageOptions, Value};

/// Build a store with `n` instanced corals under `/Reef` plus one shared
/// `/Coral` asset, then compose and return the stage.
fn build_and_compose(n: usize) -> Stage {
    let mut store = InMemoryStore::default();

    let f_vertices = store.tokens.intern("vertices");
    let f_display_color = store.tokens.intern("primvars:displayColor");
    let branches_tok = store.tokens.intern("branches");
    let polyps_tok = store.tokens.intern("polyps");

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
        let name_tok = store.tokens.intern(&name);
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

    // --- Compose ---
    Stage::compose(&mut store, LayerId(1), StageOptions::default())
}

fn bench_instancing(c: &mut Criterion) {
    let mut group = c.benchmark_group("instancing_compose");

    for &n in &[100, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| build_and_compose(n));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_instancing);
criterion_main!(benches);

extern crate alloc;
