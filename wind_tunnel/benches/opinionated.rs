// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ordered-list resolution, reporting, wide run ordering and list-op edits
//! from ten to a million items.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use opinionated::{
    ChainOpinion, ListOp, OpinionOp, apply_list_order_by_key, resolve_list_chain,
    resolve_list_chain_report, resolve_ordered_chain_report,
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
        group.bench_with_input(
            BenchmarkId::new("list_report", format!("{depth}x{width}")),
            &ops,
            |b, ops| {
                b.iter(|| {
                    black_box(resolve_list_chain_report(
                        black_box(&seed),
                        ops.iter().enumerate().map(|(position, op)| (op, position)),
                    ))
                });
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
        group.bench_function(BenchmarkId::new("report", depth), |b| {
            b.iter(|| {
                black_box(resolve_ordered_chain_report(
                    opinions
                        .iter()
                        .zip(&positions)
                        .map(|(op, provenance)| ChainOpinion { op, provenance }),
                ))
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("opinionated/ordering");
    for width in [64, 1_000, 10_000] {
        let items: Vec<usize> = (0..width).collect();
        let op = ListOp::reordered(items.iter().rev().copied().collect());
        group.bench_with_input(BenchmarkId::new("reverse", width), &items, |b, items| {
            b.iter(|| black_box(op.apply_to(black_box(items))));
        });
        group.bench_with_input(
            BenchmarkId::new("keyed_reverse", width),
            &items,
            |b, items| {
                b.iter(|| {
                    let mut result = items.clone();
                    apply_list_order_by_key(&mut result, &op.reorder, |item| *item);
                    black_box(result)
                });
            },
        );
    }
    group.finish();
}

/// A list item as layerstack's list ops hold them: a 32-bit id newtype,
/// like `TokenId` and `PathId`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Id(u32);

/// `ids` as list items.
fn ids(ids: impl IntoIterator<Item = u32>) -> Vec<Id> {
    ids.into_iter().map(Id).collect()
}

/// One list operation applied to a base list, as a benchmark case.
struct ListOpCase {
    name: &'static str,
    base: Vec<Id>,
    op: ListOp<Id>,
}

/// Every edit of a list op at width `n`, each naming about half the base:
/// the shapes of large `inactiveIds`, relationship target and `apiSchemas`
/// list ops.
fn list_op_cases(n: u32) -> Vec<ListOpCase> {
    let base = ids(0..n);
    let half_new = ids(n / 2..n + n / 2);
    let every_other = ids((0..n).step_by(2));
    // Half of these are present, spread through the base; half are new.
    let mixed = ids((0..n).step_by(4).chain(n..n + n / 4));
    let reversed_half: Vec<Id> = every_other.iter().rev().copied().collect();
    let case = |name, op| ListOpCase {
        name,
        base: base.clone(),
        op,
    };
    vec![
        case(
            "explicit",
            ListOp::explicit(base.iter().rev().copied().collect()),
        ),
        case("add", ListOp::added(half_new)),
        case("delete", ListOp::deleted(every_other)),
        case("prepend", ListOp::prepended(mixed.clone())),
        case("append", ListOp::appended(mixed)),
        case("reorder", ListOp::reordered(reversed_half)),
    ]
}

/// A strongest-first chain of every edit, each naming about a quarter of
/// the fallback.
fn list_op_chain(n: u32) -> Vec<ListOp<Id>> {
    let quarter = |offset: u32| ids((offset..n + offset).step_by(4));
    vec![
        ListOp::reordered(quarter(3).into_iter().rev().collect()),
        ListOp::appended(quarter(2)),
        ListOp::prepended(quarter(n / 2 + 1)),
        ListOp::deleted(quarter(1)),
        ListOp::added(quarter(n / 2)),
    ]
}

fn bench_list_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("opinionated/list_ops");
    for n in [10_u32, 100, 10_000, 1_000_000] {
        for case in list_op_cases(n) {
            group.bench_with_input(BenchmarkId::new(case.name, n), &case, |b, case| {
                b.iter(|| black_box(case.op.apply_to(black_box(&case.base))));
            });
        }
        let fallback = ids(0..n);
        let chain = list_op_chain(n);
        group.bench_with_input(BenchmarkId::new("chain", n), &chain, |b, chain| {
            b.iter(|| black_box(resolve_list_chain(black_box(&fallback), chain.iter())));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_lists, bench_list_ops);
criterion_main!(benches);
