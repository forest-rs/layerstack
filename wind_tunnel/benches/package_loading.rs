// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Package checksum throughput over small members and large media buffers.
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

fn checksum(c: &mut Criterion) {
    let mut group = c.benchmark_group("usdz_crc");
    for size in [16_usize, 64 * 1024, 8 * 1024 * 1024] {
        let mut state = 0x1234_5678_u32;
        let bytes: Vec<_> = (0..size)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect();
        group.throughput(Throughput::Bytes(
            u64::try_from(size).expect("buffer size fits u64"),
        ));
        group.bench_function(size.to_string(), |b| {
            b.iter(|| black_box(layerstack_usdz::crc32::crc32(black_box(&bytes))));
        });
    }
    group.finish();
}
criterion_group!(benches, checksum);
criterion_main!(benches);
