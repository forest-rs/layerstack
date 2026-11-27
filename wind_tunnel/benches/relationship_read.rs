// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Read an in-memory layer into a fresh namespace. Fixture construction and
//! encoding are outside the measurement. Each import owns its result.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use layerstack::{InMemoryStore, LayerId, NoAssets};
use std::hint::black_box;

#[path = "support/relationship_layer.rs"]
mod relationship_layer;

fn relationship_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("relationship_read");
    for (depth, leaves) in [(1, 2048), (16, 2048), (1, 16384)] {
        let (store, layer) = relationship_layer::fixture(depth, leaves);
        let text = layerstack_io::usda::save_usda(&layer, store.tokens(), store.paths()).unwrap();
        let bytes = layerstack_io::usdc::save_usdc(&layer, store.tokens(), store.paths()).unwrap();
        let shape = format!("{depth}x{leaves}");
        group.throughput(Throughput::Elements(leaves as u64));
        group.bench_function(BenchmarkId::new("usda", &shape), |b| {
            b.iter(|| {
                let store = InMemoryStore::default();
                black_box(layerstack_io::usda::read_usda(
                    black_box(&text),
                    LayerId(1),
                    store.tokens(),
                    store.paths(),
                    &mut NoAssets,
                ))
            });
        });
        group.bench_function(BenchmarkId::new("usdc", &shape), |b| {
            b.iter(|| {
                let store = InMemoryStore::default();
                layerstack_io::usdc::read_usdc(
                    black_box(&bytes),
                    LayerId(1),
                    store.tokens(),
                    store.paths(),
                    &mut NoAssets,
                )
                .unwrap()
            });
        });
    }
    group.finish();
}

criterion_group!(benches, relationship_read);
criterion_main!(benches);
