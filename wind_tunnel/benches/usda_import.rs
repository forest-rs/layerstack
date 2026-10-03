// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! In-memory USDA import, including intermediate destruction, without I/O or
//! composition. Compare the inspectable AST route with direct typed buffers.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset,
    TokenInterner,
};
use std::hint::black_box;

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

fn import(c: &mut Criterion) {
    let mut group = c.benchmark_group("usda_import");
    for count in [1_000, 100_000] {
        let mut source = String::from("#usda 1.0\ndef Mesh \"Mesh\" { point3f[] points = [");
        for _ in 0..count {
            source.push_str("(1.25, -2.5, 3.75),");
        }
        source.push_str("] }");
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(BenchmarkId::new("ast_emit", count), &source, |b, source| {
            b.iter(|| {
                let mut store = InMemoryStore::default();
                let parsed = layerstack_usda::parser::parse(black_box(source));
                black_box(layerstack_usda::emit::emit(
                    &parsed.layer,
                    LayerId(1),
                    &mut store.tokens,
                    &mut store.paths,
                    &mut NoAssets,
                ));
            });
        });
        group.bench_with_input(BenchmarkId::new("direct", count), &source, |b, source| {
            b.iter(|| {
                let mut store = InMemoryStore::default();
                black_box(layerstack_usda::read_usda(
                    black_box(source),
                    LayerId(1),
                    &mut store.tokens,
                    &mut store.paths,
                    &mut NoAssets,
                ));
            });
        });
    }
    group.finish();
}

criterion_group!(benches, import);
criterion_main!(benches);
