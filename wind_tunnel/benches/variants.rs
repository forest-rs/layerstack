#![allow(
    clippy::cast_possible_truncation,
    reason = "bench indices are trivially small"
)]
//! Variant selection benchmarks.
//!
//! Measures `Stage::compose` time as variant set breadth grows.
//! Each prim has a variant set with `n` branches, each branch carrying
//! field opinions and a child prim.

extern crate alloc;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    HashMap, InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, Value, VariantSetSpec,
    VariantSpec,
};

/// Build a scene with `n_prims` prims, each having a variant set with
/// `n_variants` branches (one selected), then compose.
fn build_and_compose(n_prims: usize, n_variants: usize) -> Stage {
    let mut store = InMemoryStore::default();

    let f_material = store.tokens.intern("material");
    let f_detail = store.tokens.intern("detailLevel");
    let vs_name = store.tokens.intern("look");

    let root = store.path("/Root");

    let mut layer = Layer::new(LayerId(1));

    let mut root_children = Vec::with_capacity(n_prims);

    for prim_idx in 0..n_prims {
        let prim_name = alloc::format!("Mesh_{prim_idx:04}");
        let prim_tok = store.tokens.intern(&prim_name);
        root_children.push(prim_tok);
        let prim_path = store.path(&alloc::format!("/Root/{prim_name}"));

        // Build variant set with n_variants branches.
        let mut variants = HashMap::new();
        let mut selected = None;

        for v in 0..n_variants {
            let branch_name = alloc::format!("v{v:03}");
            let branch_tok = store.tokens.intern(&branch_name);

            // Select the middle variant.
            if v == n_variants / 2 {
                selected = Some(branch_tok);
            }

            let mut vspec = VariantSpec::default();
            vspec.fields.insert(
                f_material,
                Value::String(alloc::format!("mat_{branch_name}").into()).into(),
            );
            let detail = v as i32;
            vspec.fields.insert(f_detail, Value::Int(detail).into());

            // Each branch introduces a child prim.
            let child_name = alloc::format!("detail_{branch_name}");
            let child_tok = store.tokens.intern(&child_name);
            vspec.authored_children.push(child_tok);

            // Intern the child path so composition can find it.
            let _child_path = store.path(&alloc::format!("/Root/{prim_name}/{child_name}"));

            variants.insert(branch_tok, vspec);
        }

        let mut spec = PrimSpec::def();
        spec.variant_sets
            .insert(vs_name, VariantSetSpec { variants });
        spec.variant_set_order.push(vs_name);
        if let Some(sel) = selected {
            spec.variant_selections.insert(vs_name, sel);
        }
        layer.insert_prim(prim_path, spec);
    }

    layer.insert_prim(root, {
        let mut spec = PrimSpec::def();
        spec.authored_children = root_children;
        spec
    });

    store.insert_layer(layer);

    Stage::compose(&mut store, LayerId(1), StageOptions::default())
}

fn bench_variants(c: &mut Criterion) {
    let mut group = c.benchmark_group("variant_compose");

    // Scale variant breadth with a fixed number of prims.
    let n_prims = 20;
    for &n_variants in &[5, 20, 100] {
        group.bench_with_input(
            BenchmarkId::new("breadth", alloc::format!("{n_prims}p_{n_variants}v")),
            &(n_prims, n_variants),
            |b, &(np, nv)| {
                b.iter(|| build_and_compose(np, nv));
            },
        );
    }

    // Scale prim count with moderate variant breadth.
    let n_variants = 10;
    for &n_prims in &[10, 100, 500] {
        group.bench_with_input(
            BenchmarkId::new("prims", alloc::format!("{n_prims}p_{n_variants}v")),
            &(n_prims, n_variants),
            |b, &(np, nv)| {
                b.iter(|| build_and_compose(np, nv));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_variants);
criterion_main!(benches);
