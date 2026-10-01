// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property-heavy shared prototypes, with import, composition and destruction
//! timed independently. The source store remains alive during stage destruction.

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, InterpolationType, Layer, LayerId, LiveStage,
    PathInterner, PrimSpec, PropertyPath, PropertySpec, PropertyType, Reference, ResolvedAsset,
    Stage, StageOptions, TokenInterner, Value,
    edit::{EditTarget, Transaction},
};

struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

fn fixture(properties: u32, samples: u32, references: u32) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let prototype = store.path("/Emitter");
    let mut spec = PrimSpec::def();
    for field in 0..properties {
        let name = store.tokens.intern(format!("parameter{field}"));
        let value = f64::from(field);
        let mut property =
            PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)))
                .with_default(Value::Double(value));
        if samples != 0 {
            property = property.with_time_samples(
                (0..samples)
                    .map(|t| (f64::from(t), Value::Double(value + f64::from(t))))
                    .collect(),
            );
        }
        spec = spec.with_property(name, property);
    }
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(prototype, spec);
    for instance in 0..references {
        let path = store.path(&format!("/Instances/Emitter{instance}"));
        layer.insert_prim(
            path,
            PrimSpec::def().with_reference(Reference::new(LayerId(1), prototype)),
        );
    }
    let children = (0..references)
        .map(|instance| store.tokens.intern(format!("Emitter{instance}")))
        .collect();
    let instances = store.path("/Instances");
    layer.insert_prim(instances, PrimSpec::def().with_children(children));
    let root = store.path("/");
    layer.insert_prim(
        root,
        PrimSpec::default().with_children(vec![
            store.tokens.intern("Emitter"),
            store.tokens.intern("Instances"),
        ]),
    );
    store.insert_layer(layer);
    store
}

fn import(bytes: &[u8]) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let result = layerstack_usdc::read_usdc(
        bytes,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .unwrap();
    assert!(
        result.diagnostics.is_empty(),
        "fixture imports without diagnostics"
    );
    store.insert_layer(result.layer);
    store
}

fn bench_storage(c: &mut Criterion) {
    let mut group = c.benchmark_group("composition_storage");
    for (properties, samples) in [(32, 0), (128, 0), (32, 64), (128, 64)] {
        let authored = fixture(properties, samples, 128);
        let bytes = layerstack_usdc::writer::save_layer(
            &authored.layers[&LayerId(1)],
            &authored.tokens,
            &authored.paths,
        )
        .unwrap();
        let mut checked = import(&bytes);
        let stage = Stage::compose(&mut checked, LayerId(1), StageOptions::default());
        assert!(
            stage.composition_errors().is_empty(),
            "fixture composes without errors"
        );
        let root = checked.path("/");
        assert_eq!(
            stage.traverse(root).count(),
            131,
            "all prototype placements compose"
        );
        let field = checked.tokens.intern("parameter0");
        let prim = checked.path("/Instances/Emitter0");
        assert_eq!(
            stage.authored_property_names(prim, &checked).len(),
            usize::try_from(properties).unwrap(),
            "each placement retains all typed properties"
        );
        assert!(
            stage
                .resolve_property_declaration(prim, field)
                .unwrap()
                .type_name
                .is_some(),
            "the declaration survives import and reference composition"
        );
        let property = PropertyPath::new(prim, field);
        assert_eq!(
            stage
                .resolve_property_path_at_time(property, 23.5, InterpolationType::Linear)
                .unwrap()
                .value,
            Value::Double(if samples == 0 { 0.0 } else { 23.5 }),
            "referenced values and samples survive import and composition"
        );
        drop(stage);
        for phase in ["import", "compose", "drop_stage", "drop_store"] {
            let size = format!("128x{properties}_{samples}samples");
            group.bench_function(BenchmarkId::new(phase, size), |b| {
                // iter_custom excludes preparation and untimed destruction,
                // keeping stage and authored-store lifetimes explicit.
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        if phase == "import" {
                            let start = Instant::now();
                            let store = black_box(import(black_box(&bytes)));
                            elapsed += start.elapsed();
                            drop(store);
                            continue;
                        }
                        let mut store = import(&bytes);
                        let start = Instant::now();
                        let stage = black_box(Stage::compose(
                            &mut store,
                            LayerId(1),
                            StageOptions::default(),
                        ));
                        if phase == "compose" {
                            elapsed += start.elapsed();
                        }
                        let start = Instant::now();
                        drop(stage);
                        if phase == "drop_stage" {
                            elapsed += start.elapsed();
                        }
                        let start = Instant::now();
                        drop(store);
                        if phase == "drop_store" {
                            elapsed += start.elapsed();
                        }
                    }
                    elapsed
                });
            });
        }
    }
    group.finish();
}

fn default_edit(store: &mut InMemoryStore) -> Transaction {
    let property = store.property_path("/Emitter.parameter0");
    let mut transaction = Transaction::new();
    transaction.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        Value::Double(-1.0),
    );
    transaction
}

/// Check the edit reaches every placement without changing sampled values.
/// Defaults and samples are independent authored slots (AOUSD Core §12.3).
fn check_default_edit(references: u32, samples: u32) {
    let mut store = fixture(1, samples, references);
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let transaction = default_edit(&mut store);
    let applied = live.apply(&mut store, &transaction).unwrap();
    assert!(applied.changes.resynced.is_empty(), "value-only edit");
    assert_eq!(
        applied.changes.changed_info_only.len(),
        references as usize + 1,
        "source and every placement receive the edit"
    );
    assert!(
        live.stage().composition_errors().is_empty(),
        "no composition errors after editing"
    );
    for instance in 0..references {
        let path = store.property_path(&format!("/Instances/Emitter{instance}.parameter0"));
        assert_eq!(
            live.stage().resolve_field_path(path).unwrap().value,
            Value::Double(-1.0),
            "placement reads the new default"
        );
        let sampled = live
            .stage()
            .resolve_property_path_at_time(path, 23.5, InterpolationType::Linear)
            .unwrap()
            .value;
        assert_eq!(
            sampled,
            Value::Double(if samples == 0 { -1.0 } else { 23.5 }),
            "numeric queries retain sample precedence over the edited default"
        );
        let opinions = live.stage().explain_property_path(path).unwrap();
        let authored_samples = opinions[0].value.time_samples().unwrap_or(&[]);
        assert_eq!(
            authored_samples.len(),
            samples as usize,
            "sample count remains unchanged"
        );
        assert!(
            authored_samples
                .iter()
                .enumerate()
                .all(|(i, (time, value))| {
                    *time == i as f64 && *value == Value::Double(i as f64)
                }),
            "default editing preserves every sample"
        );
    }
    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    assert!(
        undone.changes.resynced.is_empty(),
        "undo remains value-only"
    );
    let first = store.property_path("/Instances/Emitter0.parameter0");
    assert_eq!(
        live.stage().resolve_field_path(first).unwrap().value,
        Value::Double(0.0),
        "undo restores the default"
    );
}

fn bench_sampled_default_edits(c: &mut Criterion) {
    let mut group = c.benchmark_group("sampled_default_edit");
    for references in [1, 128] {
        for samples in [0, 64, 4096] {
            check_default_edit(references, samples);
            let size = format!("{references}references_{samples}samples");
            group.bench_function(BenchmarkId::new("first", &size), |b| {
                // Fresh population matters: a warm loop alone can hide the first
                // detachment of a shared property. Keep setup, inverse disposal
                // and stage/store destruction outside the edit timer.
                b.iter_batched_ref(
                    || {
                        let mut store = fixture(1, samples, references);
                        let live =
                            LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
                        let transaction = default_edit(&mut store);
                        (store, live, transaction)
                    },
                    |(store, live, transaction)| black_box(live.apply(store, transaction).unwrap()),
                    BatchSize::PerIteration,
                );
            });
            group.bench_function(BenchmarkId::new("subsequent", &size), |b| {
                let mut store = fixture(1, samples, references);
                let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
                let mut transaction = default_edit(&mut store);
                transaction = live.apply(&mut store, &transaction).unwrap().inverse;
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let start = Instant::now();
                        let applied = black_box(live.apply(&mut store, &transaction).unwrap());
                        elapsed += start.elapsed();
                        transaction = applied.inverse;
                    }
                    elapsed
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_storage, bench_sampled_default_edits);
criterion_main!(benches);
