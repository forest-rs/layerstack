// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Author the same layer as records, a schema-aware batch, and a generated
//! transaction. Each scope applies two named collections, sets a uniform
//! boolean on each, and targets another scope. Only authoring is timed.
//! Schema loading, path creation, composition, serialization, and dropping
//! the previous layer are outside the measurements.

use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use layerstack::{
    EditTarget, FieldValue, InMemoryStore, Layer, LayerId, ListOp, PathId, PrimSpec, PropertySpec,
    PropertyType, SchemaRegistry, Stage, StageOptions, TargetPath, TokenId, Value,
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

    fn transaction(&mut self) {
        let mut edit = SchemaEdit::new(&self.stage, &mut self.store, EditTarget::for_layer(ROOT));
        for (i, &path) in self.paths.iter().enumerate() {
            Scope::define(&mut edit, path).unwrap();
            for (name, target) in COLLECTIONS.into_iter().zip(neighbors(&self.paths, i)) {
                CollectionApi::apply(&mut edit, path, name)
                    .unwrap()
                    .set_include_root(&mut edit, false)
                    .set_includes(&mut edit, &[target]);
            }
        }
        edit.finish().apply(&mut self.store).unwrap();
    }

    fn text(&self) -> String {
        layerstack_usda::save_usda(
            self.store.layer(ROOT).unwrap(),
            self.store.tokens(),
            self.store.paths(),
        )
        .unwrap()
    }
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
    let routes: [(&str, Author); 3] = [
        ("records", Fixture::records),
        ("batch", Fixture::batch),
        ("transaction", Fixture::transaction),
    ];
    for n in [64, 1024] {
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

criterion_group!(benches, bench);
criterion_main!(benches);
