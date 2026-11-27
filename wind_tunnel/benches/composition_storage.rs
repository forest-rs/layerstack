// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property-heavy shared prototypes, with import, composition and destruction
//! timed independently. The source store remains alive during stage destruction.

use std::{
    hint::black_box,
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, Layer, LayerId, ListOp, LiveStage, PathId,
    PathInterner, PrimSpec, PropertyDefinition, PropertyEntry, PropertyPath, PropertySpec,
    PropertyType, Reference, ResolvedAsset, SchemaDefinition, SchemaRegistry, Stage, StageOptions,
    Time, TokenId, TokenInterner, Value,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::Domain;

struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
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
        let name = store.tokens().intern(format!("parameter{field}"));
        let value = f64::from(field);
        let mut property = PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
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
        .map(|instance| store.tokens().intern(format!("Emitter{instance}")))
        .collect();
    let instances = store.path("/Instances");
    layer.insert_prim(instances, PrimSpec::def().with_children(children));
    let root = store.path("/");
    layer.insert_prim(
        root,
        PrimSpec::default().with_children(vec![
            store.tokens().intern("Emitter"),
            store.tokens().intern("Instances"),
        ]),
    );
    store.insert_layer(layer);
    store
}

fn import(bytes: &[u8]) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let result = layerstack_io::usdc::read_usdc(
        bytes,
        LayerId(1),
        store.tokens(),
        store.paths(),
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
        let bytes = layerstack_io::usdc::save_usdc(
            authored.layer(LayerId(1)).unwrap(),
            authored.tokens(),
            authored.paths(),
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
        let field = checked.tokens().intern("parameter0");
        let prim = checked.path("/Instances/Emitter0");
        assert_eq!(
            stage
                .prim(prim, &checked)
                .unwrap()
                .authored_property_names()
                .len(),
            usize::try_from(properties).unwrap(),
            "each placement retains all typed properties"
        );
        assert!(
            stage
                .view(&checked)
                .property(PropertyPath::new(prim, field))
                .and_then(|p| p.declaration())
                .unwrap()
                .type_name
                .is_some(),
            "the declaration survives import and reference composition"
        );
        let property = PropertyPath::new(prim, field);
        assert_eq!(
            stage
                .view(&checked)
                .attribute(property)
                .and_then(|a| a.authored_value(Time::at(23.5)))
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
        applied.changes.changed_properties.len(),
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
            live.stage()
                .view(&store)
                .attribute(path)
                .and_then(|a| a.authored_value(Time::Default))
                .unwrap()
                .value,
            Value::Double(-1.0),
            "placement reads the new default"
        );
        let sampled = live
            .stage()
            .view(&store)
            .attribute(path)
            .and_then(|a| a.authored_value(Time::at(23.5)))
            .unwrap()
            .value;
        assert_eq!(
            sampled,
            Value::Double(if samples == 0 { -1.0 } else { 23.5 }),
            "numeric queries retain sample precedence over the edited default"
        );
        let opinions = live
            .stage()
            .view(&store)
            .property(path)
            .map(|p| p.opinions())
            .unwrap();
        let authored_samples = opinions
            .first()
            .and_then(|opinion| opinion.value().time_samples())
            .unwrap_or(&[]);
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
        live.stage()
            .view(&store)
            .attribute(first)
            .and_then(|a| a.authored_value(Time::Default))
            .unwrap()
            .value,
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

struct AppliedSchemaFixture {
    store: InMemoryStore,
    options: StageOptions,
    expansion_rules: Vec<(PathId, Range<usize>)>,
    allowed_tokens: TokenId,
}

fn applied_schema_fixture(
    prims: usize,
    instances: usize,
    shared_instances: bool,
    base_properties: usize,
) -> AppliedSchemaFixture {
    let mut store = InMemoryStore::default();
    let tokens = store.tokens();
    let mut builder = SchemaRegistry::builder();
    layerstack_schemas::register(&mut builder, &[Domain::UsdGeom], tokens).unwrap();
    let scope = tokens.intern("Scope");
    let type_name = if base_properties == 0 {
        scope
    } else {
        let base_name = tokens.intern("WideBase");
        let mut base = SchemaDefinition::typed(base_name).with_parent(scope);
        let property_type = PropertyType::parse("int").unwrap();
        for index in 0..base_properties {
            let name = tokens.intern(format!("baseProperty{index}"));
            base = base.with_property(
                PropertyDefinition::attribute(name)
                    .with_type(property_type.clone())
                    .with_fallback(Value::Int(0)),
            );
        }
        builder.register(base);
        let derived = tokens.intern("WideScope");
        builder.register(SchemaDefinition::typed(derived).with_parent(base_name));
        derived
    };
    let options = StageOptions {
        schemas: Some(Arc::new(builder.build(tokens))),
        ..StageOptions::default()
    };
    let api_schemas = tokens.intern("apiSchemas");
    let allowed_tokens = tokens.intern("allowedTokens");
    let includes = Arc::new(PropertySpec::relationship());
    let mut layer = Layer::new(LayerId(1));
    let mut expansion_rules = Vec::with_capacity(prims);
    for prim_index in 0..prims {
        let path = store.path(&format!("/Scope{prim_index}"));
        let tokens = store.tokens();
        let mut prim = PrimSpec::def().with_type_name(type_name);
        let mut apis = Vec::with_capacity(instances);
        let offset = if shared_instances {
            0
        } else {
            prim_index * instances
        };
        let rows = offset..offset + instances;
        for index in rows.clone() {
            apis.push(tokens.intern(format!("CollectionAPI:row_{index}")));
            prim.properties.push(PropertyEntry {
                name: tokens.intern(format!("collection:row_{index}:includes")),
                spec: Arc::clone(&includes),
            });
        }
        layer.insert_prim(path, prim.with_field(api_schemas, ListOp::explicit(apis)));
        expansion_rules.push((path, rows));
    }
    store.insert_layer(layer);
    AppliedSchemaFixture {
        store,
        options,
        expansion_rules,
        allowed_tokens,
    }
}

fn bench_applied_schemas(c: &mut Criterion) {
    let mut group = c.benchmark_group("applied_schema_composition");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    for (label, prims, instances, shared, base_properties) in [
        ("wide_1024", 1, 1024, true, 0),
        ("wide_8192", 1, 8192, true, 0),
        ("wide_32768", 1, 32768, true, 0),
        ("shared_128x128", 128, 128, true, 256),
        ("distinct_128x128", 128, 128, false, 256),
    ] {
        let AppliedSchemaFixture {
            mut store,
            options,
            expansion_rules,
            allowed_tokens,
        } = applied_schema_fixture(prims, instances, shared, base_properties);
        let schemas = options.schemas.as_ref().unwrap();
        let definition_size = |name| {
            schemas
                .schema_definition(store.tokens().lookup(name).unwrap())
                .unwrap()
                .properties()
                .len()
        };
        let expected_properties = base_properties
            + definition_size("Scope")
            + instances * definition_size("CollectionAPI");
        let stage = Stage::compose(&mut store, LayerId(1), options.clone());
        assert!(
            stage.composition_errors().is_empty(),
            "fixture composes without errors"
        );
        let mut properties = Vec::with_capacity(prims * instances);
        for (path, rows) in &expansion_rules {
            let prim = stage.prim(*path, &store).unwrap();
            let definition = prim.definition().unwrap();
            assert_eq!(
                definition.applied_schemas().len(),
                instances,
                "every application reaches the composed definition"
            );
            assert_eq!(
                definition.properties().len(),
                expected_properties,
                "the definition contains every inherited and applied property"
            );
            for index in rows.clone() {
                let name = format!("collection:row_{index}:expansionRule");
                let property = prim.property(name.as_str()).unwrap();
                assert!(
                    property.metadata(allowed_tokens).is_some(),
                    "instance has schema metadata"
                );
                properties.push(property);
            }
        }
        group.bench_function(BenchmarkId::new("read_metadata", label), |b| {
            b.iter(|| {
                for property in black_box(&properties) {
                    black_box(property.metadata(allowed_tokens));
                }
            });
        });
        drop(properties);
        drop(stage);
        group.bench_function(BenchmarkId::new("compose", label), |b| {
            // A fresh store measures first composition, including names
            // instantiated by the applied schemas. Setup and drop are untimed.
            b.iter_batched_ref(
                || applied_schema_fixture(prims, instances, shared, base_properties),
                |fixture| {
                    black_box(Stage::compose(
                        &mut fixture.store,
                        LayerId(1),
                        fixture.options.clone(),
                    ))
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_storage,
    bench_sampled_default_edits,
    bench_applied_schemas
);
criterion_main!(benches);
