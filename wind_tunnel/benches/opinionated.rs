// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ordered-list resolution, reporting and wide run ordering.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use opinionated::{
    ChainOpinion, ListOp, OpinionOp, resolve_list_chain, resolve_ordered_chain_report,
};

fn bench_lists(c: &mut Criterion) {
    let mut group = c.benchmark_group("opinionated/lists");
    for (depth, width) in [(1, 32), (20, 1_000), (100, 1_000)] {
        let seed: Vec<usize> = (0..width).collect();
        let ops: Vec<_> = (0..depth).map(|i| ListOp::appended(vec![i])).collect();
        group.bench_with_input(
            BenchmarkId::new("chain", format!("{depth}x{width}")),
            &ops,
            |b, ops| {
                b.iter(|| black_box(resolve_list_chain(black_box(&seed), ops.iter())));
            },
        );
        let mut cutoff = ops.clone();
        cutoff[0] = ListOp::explicit(vec![width]);
        group.bench_with_input(
            BenchmarkId::new("explicit_cutoff", format!("{depth}x{width}")),
            &cutoff,
            |b, ops| {
                b.iter(|| black_box(resolve_list_chain(black_box(&seed), ops.iter())));
            },
        );
        let opinions: Vec<OpinionOp<(), usize, ()>> =
            ops.into_iter().map(OpinionOp::List).collect();
        let positions: Vec<_> = (0..opinions.len()).collect();
        group.bench_function(
            BenchmarkId::new("report", format!("{depth}x{width}")),
            |b| {
                b.iter(|| {
                    black_box(resolve_ordered_chain_report(
                        opinions
                            .iter()
                            .zip(&positions)
                            .map(|(op, provenance)| ChainOpinion { op, provenance }),
                    ))
                });
            },
        );
    }
    group.finish();

    let mut group = c.benchmark_group("opinionated/ordering");
    for width in [64, 1_000, 10_000] {
        let items: Vec<usize> = (0..width).collect();
        let op = ListOp::reordered(items.iter().rev().copied().collect());
        group.bench_with_input(BenchmarkId::new("reverse", width), &items, |b, items| {
            b.iter(|| black_box(op.apply_to(black_box(items))));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_lists);
criterion_main!(benches);
