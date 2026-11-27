// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! In-memory and streamed USDA saves of an already-authored namespace.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::{fmt, hint::black_box};

#[path = "support/relationship_layer.rs"]
mod relationship_layer;

struct ByteCount(usize);

impl fmt::Write for ByteCount {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0 += black_box(text).len();
        Ok(())
    }
}

fn relationship_save(c: &mut Criterion) {
    let mut group = c.benchmark_group("usda_relationship_save");
    for depth in [1, 16] {
        let (store, layer) = relationship_layer::fixture(depth, 2048);
        group.bench_function(BenchmarkId::new("string", depth), |b| {
            b.iter(|| {
                layerstack_io::usda::save_usda(black_box(&layer), store.tokens(), store.paths())
                    .unwrap()
            });
        });
        group.bench_function(BenchmarkId::new("stream", depth), |b| {
            b.iter(|| {
                let mut sink = ByteCount(0);
                layerstack_io::usda::save_usda_to(
                    black_box(&layer),
                    store.tokens(),
                    store.paths(),
                    &mut sink,
                )
                .unwrap();
                sink.0
            });
        });
    }
    group.finish();
}

criterion_group!(benches, relationship_save);
criterion_main!(benches);
