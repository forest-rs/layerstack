// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Nested composition arc benchmarks.
//!
//! Measures `Stage::compose` time for a forest of trees whose opinions come
//! through every kind of arc, nested inside one another: each tree
//! references a shared asset, inherits a stage-level class and specializes
//! a stage-level base; the asset itself inherits and specializes classes of
//! its own, references a bark material from a third layer and selects a
//! seasonal variant. Only composition is timed; building the layers is
//! setup.

extern crate alloc;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    HashMap, InMemoryStore, Layer, LayerId, PrimSpec, Reference, Stage, StageOptions, Value,
    VariantSetSpec, VariantSpec,
};

/// Builds a store whose root layer (`LayerId(1)`) holds `n` trees under
/// `/Forest`, each drawing on the asset layer (`LayerId(2)`) and the bark
/// layer (`LayerId(3)`) through nested arcs.
fn build_forest(n: usize) -> InMemoryStore {
    let mut store = InMemoryStore::default();

    let f_height = store.tokens.intern("height");
    let f_color = store.tokens.intern("color");
    let f_roughness = store.tokens.intern("roughness");
    let f_density = store.tokens.intern("density");
    let trunk_tok = store.tokens.intern("Trunk");
    let canopy_tok = store.tokens.intern("Canopy");
    let season = store.tokens.intern("season");
    let summer = store.tokens.intern("summer");
    let autumn = store.tokens.intern("autumn");

    // --- Bark layer (LayerId 3): a material the asset references. ---
    let bark = store.path("/Bark");
    let mut bark_layer = Layer::new(LayerId(3));
    bark_layer.insert_prim(
        bark,
        PrimSpec::def()
            .with_field(f_roughness, Value::Double(0.8))
            .with_field(f_color, Value::string("brown")),
    );
    store.insert_layer(bark_layer);

    // --- Asset layer (LayerId 2): the shared tree. ---
    let tree = store.path("/Tree");
    let tree_trunk = store.path("/Tree/Trunk");
    let tree_canopy = store.path("/Tree/Canopy");
    let wood_class = store.path("/_class_Wood");
    let leaf_base = store.path("/LeafBase");
    let mut asset = Layer::new(LayerId(2));
    let mut variants = HashMap::new();
    for (name, color) in [(summer, "green"), (autumn, "orange")] {
        let mut branch = VariantSpec::default();
        branch.fields.push(layerstack::FieldEntry {
            name: f_color,
            value: Value::string(color).into(),
        });
        variants.insert(name, branch);
    }
    let mut tree_spec = PrimSpec::def()
        .with_children(vec![trunk_tok, canopy_tok])
        .with_field(f_height, Value::Double(12.0))
        .with_inherit(wood_class)
        .with_reference(Reference::new(LayerId(3), bark));
    tree_spec
        .variant_sets
        .insert(season, VariantSetSpec { variants });
    tree_spec.variant_set_order.push(season);
    tree_spec.variant_selections.insert(season, summer);
    asset.insert_prim(tree, tree_spec);
    asset.insert_prim(
        tree_trunk,
        PrimSpec::def()
            .with_field(f_height, Value::Double(4.0))
            .with_reference(Reference::new(LayerId(3), bark)),
    );
    asset.insert_prim(
        tree_canopy,
        PrimSpec::def()
            .with_field(f_density, Value::Double(0.6))
            .with_specialize(leaf_base),
    );
    asset.insert_prim(
        wood_class,
        PrimSpec::class().with_field(f_roughness, Value::Double(0.5)),
    );
    asset.insert_prim(
        leaf_base,
        PrimSpec::class().with_field(f_color, Value::string("green")),
    );
    store.insert_layer(asset);

    // --- Root layer (LayerId 1): the forest. ---
    let forest = store.path("/Forest");
    let tree_class = store.path("/_class_Tree");
    let base_tree = store.path("/BaseTree");
    let mut root = Layer::new(LayerId(1));
    let mut children = Vec::with_capacity(n);
    for i in 0..n {
        let name = alloc::format!("Tree_{i:05}");
        children.push(store.tokens.intern(&name));
        let path = store.path(&alloc::format!("/Forest/{name}"));
        let mut spec = PrimSpec::def()
            .with_reference(Reference::new(LayerId(2), tree))
            .with_inherit(tree_class)
            .with_specialize(base_tree);
        if i % 7 == 0 {
            spec.variant_selections.insert(season, autumn);
        }
        root.insert_prim(path, spec);
    }
    root.insert_prim(forest, PrimSpec::def().with_children(children));
    root.insert_prim(
        tree_class,
        PrimSpec::class().with_field(f_height, Value::Double(10.0)),
    );
    root.insert_prim(
        base_tree,
        PrimSpec::class().with_field(f_color, Value::string("gray")),
    );
    store.insert_layer(root);

    store
}

fn bench_arcs(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested_arcs_compose");

    for &n in &[10, 100, 250] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || build_forest(n),
                |mut store| Stage::compose(&mut store, LayerId(1), StageOptions::default()),
                BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_arcs);
criterion_main!(benches);
