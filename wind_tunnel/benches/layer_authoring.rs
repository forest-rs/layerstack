// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Author the same layer as records, a schema-aware batch, and a generated
//! transaction, with and without one group per prim. Each scope applies
//! two named collections, sets a uniform boolean on each, and targets
//! another scope. Only authoring is timed.
//! Schema loading, path creation, composition, serialization, and dropping
//! the previous layer are outside the measurements.

use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use layerstack::{
    EditTarget, FieldValue, InMemoryStore, Layer, LayerId, ListOp, PathId, PrimSpec,
    PropertyDefinition, PropertyEntry, PropertySpec, PropertyType, SchemaDefinition,
    SchemaRegistry, Stage, StageOptions, TargetPath, TokenId, Value,
};
use layerstack_schemas::{LayerBatch, SchemaEdit, usd::CollectionApi, usd_geom::Scope};

const ROOT: LayerId = LayerId(1);
const COLLECTIONS: [&str; 2] = ["forward", "backward"];

/// The field names a producer that builds records can intern once per store.
struct CollectionTokens {
    applied: TokenId,
    include_root: TokenId,
    includes: TokenId,
}

struct Fixture {
    store: InMemoryStore,
    stage: Stage,
    schemas: Arc<SchemaRegistry>,
    paths: Vec<PathId>,
    collections: [CollectionTokens; 2],
    scope: TokenId,
    api_schemas: TokenId,
}

impl Fixture {
    fn new(prims: usize) -> Self {
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(ROOT));
        let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
        let paths = (0..prims)
            .map(|i| store.path(&format!("/Group{i}")))
            .collect();
        let tokens = store.tokens();
        let scope = tokens.intern("Scope");
        let api_schemas = tokens.intern("apiSchemas");
        let collections = COLLECTIONS.map(|name| CollectionTokens {
            applied: tokens.intern(format!("CollectionAPI:{name}")),
            include_root: tokens.intern(format!("collection:{name}:includeRoot")),
            includes: tokens.intern(format!("collection:{name}:includes")),
        });
        let stage = Stage::compose(
            &mut store,
            ROOT,
            StageOptions {
                schemas: Some(schemas.clone()),
                ..StageOptions::default()
            },
        );
        Self {
            store,
            stage,
            schemas,
            paths,
            collections,
            scope,
            api_schemas,
        }
    }

    fn records(&mut self) {
        let mut layer = Layer::new(ROOT);
        let children = self
            .paths
            .iter()
            .map(|&path| self.store.paths().resolve(path).leaf().unwrap())
            .collect();
        layer.insert_prim(
            self.store.path("/"),
            PrimSpec::default().with_children(children),
        );
        let boolean = PropertyType::parse("bool").unwrap();
        for (i, &path) in self.paths.iter().enumerate() {
            let mut prim = PrimSpec::def().with_type_name(self.scope).with_field(
                self.api_schemas,
                FieldValue::TokenListOp(Box::new(ListOp::prepended(
                    self.collections.iter().map(|c| c.applied).collect(),
                ))),
            );
            for (collection, target) in self.collections.iter().zip(neighbors(&self.paths, i)) {
                prim.set_property(
                    collection.include_root,
                    PropertySpec::typed_attribute(boolean.clone())
                        .uniform()
                        .with_default(Value::Bool(false)),
                );
                prim.set_property(
                    collection.includes,
                    PropertySpec::relationship().with_targets(ListOp::explicit(vec![target])),
                );
            }
            layer.insert_prim(path, prim);
        }
        self.store.insert_layer(layer);
    }

    fn batch(&mut self) {
        let mut batch = LayerBatch::new(ROOT, &self.store, &self.schemas).unwrap();
        for (i, &path) in self.paths.iter().enumerate() {
            batch
                .define(path, Scope::SCHEMA, |prim| {
                    for (name, target) in COLLECTIONS.into_iter().zip(neighbors(&self.paths, i)) {
                        let mut collection = prim.apply(CollectionApi::SCHEMA, Some(name))?;
                        collection.set(CollectionApi::INCLUDE_ROOT, Value::Bool(false))?;
                        collection.set_targets(CollectionApi::INCLUDES, vec![target])?;
                    }
                    Ok(())
                })
                .unwrap();
        }
        let layer = batch.finish();
        self.store.insert_layer(layer);
    }

    fn transaction(&mut self, grouped: bool) {
        let mut edit = SchemaEdit::new(&self.stage, &mut self.store, EditTarget::for_layer(ROOT));
        for (i, &path) in self.paths.iter().enumerate() {
            let author = |edit: &mut SchemaEdit<'_>| {
                Scope::define(edit, path).unwrap();
                for (name, target) in COLLECTIONS.into_iter().zip(neighbors(&self.paths, i)) {
                    CollectionApi::apply(edit, path, name)
                        .unwrap()
                        .set_include_root(edit, false)
                        .set_includes(edit, &[target]);
                }
            };
            if grouped {
                edit.group(|child| {
                    author(child);
                    Ok::<_, core::convert::Infallible>(())
                })
                .unwrap();
            } else {
                author(&mut edit);
            }
        }
        edit.finish().apply(&mut self.store).unwrap();
    }

    fn text(&self) -> String {
        layer_text(self.store.layer(ROOT).unwrap(), &self.store)
    }
}

fn layer_text(layer: &Layer, store: &InMemoryStore) -> String {
    layerstack_usda::save_usda(layer, store.tokens(), store.paths()).unwrap()
}

fn neighbors(paths: &[PathId], i: usize) -> [TargetPath; 2] {
    let n = paths.len();
    [
        TargetPath::Prim(paths[(i + 1) % n]),
        TargetPath::Prim(paths[(i + n - 1) % n]),
    ]
}

type Author = fn(&mut Fixture);

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("layer_authoring");
    group.sample_size(10);
    let routes: [(&str, Author); 4] = [
        ("records", Fixture::records),
        ("batch", Fixture::batch),
        ("transaction", |fixture| fixture.transaction(false)),
        ("grouped_transaction", |fixture| fixture.transaction(true)),
    ];
    for n in [64, 1024, 8000] {
        let mut fixture = Fixture::new(n);
        fixture.records();
        let expected = fixture.text();
        group.throughput(Throughput::Elements(n as u64));
        for (name, author) in routes {
            fixture.store.insert_layer(Layer::new(ROOT));
            author(&mut fixture);
            assert!(fixture.text() == expected, "{name} produced different USDA");
            group.bench_function(BenchmarkId::new(name, n), |b| {
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        fixture.store.insert_layer(Layer::new(ROOT));
                        let start = Instant::now();
                        author(&mut fixture);
                        elapsed += start.elapsed();
                        black_box(fixture.store.layer(ROOT).unwrap());
                    }
                    elapsed
                });
            });
        }
    }
    group.finish();
}

struct WideCollection {
    instance: String,
    attribute: String,
    relationship: String,
    tokens: CollectionTokens,
    include_root: bool,
}

struct WideFixture {
    store: InMemoryStore,
    schemas: SchemaRegistry,
    root: PathId,
    path: PathId,
    target: TargetPath,
    scope: TokenId,
    api_schemas: TokenId,
    collections: Vec<WideCollection>,
}

#[derive(Clone, Copy)]
enum Writes {
    ApplyOnly,
    Contiguous,
    Grouped,
}

impl WideFixture {
    fn new(instances: usize) -> Self {
        let store = InMemoryStore::default();
        let root = store.path("/");
        let path = store.path("/Collections");
        let target = TargetPath::Prim(store.path("/Target"));
        let tokens = store.tokens();
        let schemas = layerstack_schemas::registry(&[layerstack_schemas::Domain::UsdGeom], tokens);
        let scope = tokens.intern("Scope");
        let api_schemas = tokens.intern("apiSchemas");
        let collections = (0..instances)
            .map(|index| {
                let instance = format!("collection_{index}");
                let attribute = format!("collection:{instance}:includeRoot");
                let relationship = format!("collection:{instance}:includes");
                WideCollection {
                    tokens: CollectionTokens {
                        applied: tokens.intern(format!("CollectionAPI:{instance}")),
                        include_root: tokens.intern(&attribute),
                        includes: tokens.intern(&relationship),
                    },
                    instance,
                    attribute,
                    relationship,
                    include_root: index % 2 != 0,
                }
            })
            .collect();
        Self {
            store,
            schemas,
            root,
            path,
            target,
            scope,
            api_schemas,
            collections,
        }
    }

    fn records(&self, with_properties: bool) -> Layer {
        let mut layer = Layer::new(ROOT);
        let name = self.store.paths().resolve(self.path).leaf().unwrap();
        layer.insert_prim(self.root, PrimSpec::default().with_children(vec![name]));
        let mut prim = PrimSpec::def().with_type_name(self.scope).with_field(
            self.api_schemas,
            ListOp::prepended(self.collections.iter().map(|c| c.tokens.applied).collect()),
        );
        if with_properties {
            let boolean = PropertyType::parse("bool").unwrap();
            // Every name is unique, so append the records in their authored order.
            prim.properties.reserve(self.collections.len() * 2);
            for collection in &self.collections {
                prim.properties.extend([
                    PropertyEntry {
                        name: collection.tokens.include_root,
                        spec: Arc::new(
                            PropertySpec::typed_attribute(boolean.clone())
                                .uniform()
                                .with_default(Value::Bool(collection.include_root)),
                        ),
                    },
                    PropertyEntry {
                        name: collection.tokens.includes,
                        spec: Arc::new(
                            PropertySpec::relationship()
                                .with_targets(ListOp::explicit(vec![self.target])),
                        ),
                    },
                ]);
            }
        }
        layer.insert_prim(self.path, prim);
        layer
    }

    fn batch(&self, writes: Writes) -> Layer {
        let mut batch = LayerBatch::new(ROOT, &self.store, &self.schemas).unwrap();
        batch
            .define(self.path, Scope::SCHEMA, |prim| {
                for collection in &self.collections {
                    let mut api = prim.apply(CollectionApi::SCHEMA, Some(&collection.instance))?;
                    if matches!(writes, Writes::Contiguous) {
                        api.set(
                            CollectionApi::INCLUDE_ROOT,
                            Value::Bool(collection.include_root),
                        )?;
                        api.set_targets(CollectionApi::INCLUDES, vec![self.target])?;
                    }
                }
                if matches!(writes, Writes::Grouped) {
                    for collection in &self.collections {
                        prim.set(&collection.attribute, Value::Bool(collection.include_root))?;
                        prim.set_targets(&collection.relationship, vec![self.target])?;
                    }
                }
                Ok(())
            })
            .unwrap();
        batch.finish()
    }
}

fn bench_wide(c: &mut Criterion) {
    let mut group = c.benchmark_group("wide_layer_authoring");
    group.sample_size(10);
    let routes = [
        ("records", None),
        ("batch_apply_only", Some(Writes::ApplyOnly)),
        ("batch_contiguous", Some(Writes::Contiguous)),
        ("batch_grouped", Some(Writes::Grouped)),
    ];
    for n in [8, 32, 128, 512, 2048, 32768] {
        group.throughput(Throughput::Elements(n as u64));
        for (name, writes) in routes {
            let mut fixture = None;
            let author = |fixture: &WideFixture| match writes {
                Some(writes) => fixture.batch(writes),
                None => fixture.records(true),
            };
            group.bench_function(BenchmarkId::new(name, n), |b| {
                let fixture = fixture.get_or_insert_with(|| {
                    let fixture = WideFixture::new(n);
                    if let Some(writes) = writes {
                        let expected = fixture.records(!matches!(writes, Writes::ApplyOnly));
                        assert!(
                            layer_text(&author(&fixture), &fixture.store)
                                == layer_text(&expected, &fixture.store),
                            "{name} must author the same schema applications and property specs"
                        );
                    }
                    fixture
                });
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let start = Instant::now();
                        let layer = author(fixture);
                        elapsed += start.elapsed();
                        black_box(layer);
                    }
                    elapsed
                });
            });
        }
    }
    group.finish();
}

/// Isolate ordinary typed writes from API application and template resolution.
fn bench_typed_properties(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_typed_properties");
    let store = InMemoryStore::default();
    let tokens = store.tokens();
    let mut definition = SchemaDefinition::typed(tokens.intern("Settings"));
    let names: Vec<_> = (0..64).map(|n| format!("setting{n}")).collect();
    let integer = PropertyType::parse("int").unwrap();
    for name in &names {
        definition = definition.with_property(
            PropertyDefinition::attribute(tokens.intern(name)).with_type(integer.clone()),
        );
    }
    let mut registry = SchemaRegistry::builder();
    registry.register(definition);
    let schemas = registry.build(tokens);
    let paths: Vec<_> = (0..256)
        .map(|n| store.path(&format!("/Settings{n}")))
        .collect();
    group.throughput(Throughput::Elements(paths.len() as u64));
    for width in [0, 1, 8, 16, 32, 64] {
        group.bench_function(BenchmarkId::from_parameter(width), |b| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let start = Instant::now();
                    let mut batch = LayerBatch::new(ROOT, &store, &schemas).unwrap();
                    for &path in &paths {
                        batch
                            .define(path, "Settings", |prim| {
                                for name in &names[..width] {
                                    prim.set(name, Value::Int(7))?;
                                }
                                Ok(())
                            })
                            .unwrap();
                    }
                    let layer = batch.finish();
                    elapsed += start.elapsed();
                    black_box(layer);
                }
                elapsed
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench, bench_wide, bench_typed_properties);
criterion_main!(benches);
