// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transfer an already-read Vec into retained encoded storage. Input allocation
//! and initialization, and output destruction, are outside Criterion's timer.
//! Run: `cargo bench -p wind_tunnel --bench encoded_ownership`.
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::encoded_bytes::EncodedBytes;
use std::{hint::black_box, time::Duration};

fn handoff(bytes: Vec<u8>) -> EncodedBytes {
    bytes.into()
}
fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("encoded_handoff");
    group.sample_size(30);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for size in [64_usize, 64 * 1024, 8 * 1024 * 1024] {
        let checked = handoff(vec![0x5a; size]);
        assert_eq!(checked.len(), size, "handoff preserves the complete input");
        assert_eq!(checked[0], 0x5a, "handoff preserves payload bytes");
        group.bench_with_input(BenchmarkId::new("owned_input", size), &size, |b, &size| {
            b.iter_batched(
                || vec![0x5a; size],
                |bytes| black_box(handoff(black_box(bytes))),
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
