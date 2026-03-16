//! Instancing scaling benchmarks.
//!
//! Measures `Stage::compose` time as the number of instanced prims grows.
//! Each instance references a shared coral asset (two child prims: branches
//! and polyps), mirroring the `instancing` example.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    FieldValue, HashMap, InMemoryStore, Layer, LayerId, Path, PathId, PrimSpec, Reference,
    Specifier, Stage, StageOptions, Value,
};

/// Intern an absolute path.
fn p(store: &mut InMemoryStore, s: &str) -> PathId {
    store
        .paths
        .intern(Path::parse_absolute(s, &mut store.tokens).expect("valid path"))
}

/// Build a store with `n` instanced corals under `/Reef` plus one shared
/// `/Coral` asset, then compose and return the stage.
fn build_and_compose(n: usize) -> Stage {
    let mut store = InMemoryStore::default();

    let f_vertices = store.tokens.intern("vertices");
    let f_display_color = store.tokens.intern("primvars:displayColor");
    let branches_tok = store.tokens.intern("branches");
    let polyps_tok = store.tokens.intern("polyps");

    // --- Asset layer (LayerId 2) ---
    let coral = p(&mut store, "/Coral");
    let coral_branches = p(&mut store, "/Coral/branches");
    let coral_polyps = p(&mut store, "/Coral/polyps");

    let mut asset = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };

    let mut coral_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        authored_children: vec![branches_tok, polyps_tok],
        ..PrimSpec::default()
    };
    coral_spec.fields.insert(
        f_display_color,
        FieldValue::Value(Value::String("gray".into())),
    );
    asset.prims.insert(coral, coral_spec);

    let mut branches_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        ..PrimSpec::default()
    };
    branches_spec
        .fields
        .insert(f_vertices, FieldValue::Value(Value::Int(2400)));
    asset.prims.insert(coral_branches, branches_spec);

    let mut polyps_spec = PrimSpec {
        specifier: Some(Specifier::Def),
        ..PrimSpec::default()
    };
    polyps_spec
        .fields
        .insert(f_vertices, FieldValue::Value(Value::Int(8000)));
    asset.prims.insert(coral_polyps, polyps_spec);

    store.insert_layer(asset);

    // --- Reef layer (LayerId 1) ---
    let reef = p(&mut store, "/Reef");

    let mut reef_layer = Layer {
        id: LayerId(1),
        sublayers: vec![],
        prims: HashMap::new(),
    };

    let mut reef_children = Vec::with_capacity(n);

    for i in 0..n {
        let name = alloc::format!("Coral_{i:05}");
        let name_tok = store.tokens.intern(&name);
        reef_children.push(name_tok);

        let path = p(&mut store, &alloc::format!("/Reef/{name}"));

        let mut spec = PrimSpec {
            specifier: Some(Specifier::Def),
            instanceable: Some(true),
            ..PrimSpec::default()
        };
        spec.references.append.push(Reference {
            layer: LayerId(2),
            prim_path: coral,
            asset: Some("coral_asset.usd".into()),
        });
        // Give every 10th coral a per-instance color override.
        if i % 10 == 0 {
            spec.fields.insert(
                f_display_color,
                FieldValue::Value(Value::String("green".into())),
            );
        }
        reef_layer.prims.insert(path, spec);
    }

    reef_layer.prims.insert(
        reef,
        PrimSpec {
            specifier: Some(Specifier::Def),
            authored_children: reef_children,
            ..PrimSpec::default()
        },
    );

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
