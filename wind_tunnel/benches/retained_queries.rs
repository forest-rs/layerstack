// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained-query routing, clean polling and time-only evaluation.
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertyPath, PropertySpec,
    PropertyType, StageOptions, Transaction, Value,
};
use layerstack_schemas::{
    Time,
    bounds::BoundsOptions,
    retained::{Query, QuerySession, RetainedQueries},
};
use std::{hint::black_box, sync::Arc};

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("retained_queries");
    group.sample_size(10);
    for count in [1000, 10000] {
        let mut store = InMemoryStore::default();
        let material = store.tokens.intern("Material");
        let value = store.tokens.intern("inputs:value");
        let unused = store.tokens.intern("inputs:unused");
        let mut layer = Layer::new(LayerId(1));
        let mut paths = Vec::new();
        let ty = PropertyType::new("float", false, Value::Float(0.0));
        for i in 0..count {
            let path = store.path(&format!("/M{i}"));
            paths.push(path);
            layer.insert_prim(
                path,
                PrimSpec::def()
                    .with_type_name(material)
                    .with_property(
                        value,
                        PropertySpec::typed_attribute(ty.clone()).with_time_samples(vec![
                            (0.0, Value::Float(0.0)),
                            (1.0, Value::Float(1.0)),
                        ]),
                    )
                    .with_property(
                        unused,
                        PropertySpec::typed_attribute(ty.clone()).with_default(Value::Float(0.0)),
                    ),
            );
        }
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        // The host-owned path synchronizes once before a batch, including the
        // source-generation scan. Keep this separate from owning-session polls.
        {
            let mut live = LiveStage::compose(&mut store, LayerId(1), options.clone());
            let mut queries = RetainedQueries::new(Time::at(0.0), BoundsOptions::default());
            let ids: Vec<_> = paths
                .iter()
                .map(|&p| queries.observe(Query::ShadingValue(PropertyPath::new(p, value))))
                .collect();
            {
                let mut view = queries.view(&mut live, &mut store).unwrap();
                for &id in &ids {
                    black_box(view.poll(id));
                }
            }
            group.bench_with_input(
                BenchmarkId::new("synchronize_and_clean_poll_all", count),
                &count,
                |b, _| {
                    b.iter(|| {
                        let mut view = queries.view(&mut live, &mut store).unwrap();
                        for &id in &ids {
                            black_box(view.poll(id));
                        }
                    });
                },
            );
        }
        let mut session = QuerySession::new(
            store,
            LayerId(1),
            options,
            Time::at(0.0),
            BoundsOptions::default(),
        );
        let ids: Vec<_> = paths
            .iter()
            .map(|&p| session.observe(Query::ShadingValue(PropertyPath::new(p, value))))
            .collect();
        for &id in &ids {
            black_box(session.poll(id));
        }
        group.bench_with_input(BenchmarkId::new("clean_poll_all", count), &count, |b, _| {
            b.iter(|| {
                for &id in &ids {
                    black_box(session.poll(id));
                }
            });
        });
        group.bench_with_input(
            BenchmarkId::new("prepare_transform_queries", count),
            &count,
            |b, _| {
                b.iter(|| {
                    for &path in &paths {
                        let id = session.observe(Query::WorldTransform(path));
                        black_box(session.poll(id));
                        session.remove(id);
                    }
                });
            },
        );
        let mut frame = false;
        group.bench_with_input(
            BenchmarkId::new("animated_providers", count),
            &count,
            |b, _| {
                b.iter(|| {
                    frame = !frame;
                    session.set_time(Time::at(if frame { 0.25 } else { 0.75 }));
                    for &id in &ids {
                        black_box(session.poll(id));
                    }
                });
            },
        );
        let mut txn = Transaction::new();
        txn.set_default(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(paths[0], unused)),
            Value::Float(0.5),
        );
        group.bench_with_input(
            BenchmarkId::new("unrelated_edit_routing", count),
            &count,
            |b, _| {
                b.iter(|| {
                    black_box(session.apply(&txn).unwrap());
                });
            },
        );
    }
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
