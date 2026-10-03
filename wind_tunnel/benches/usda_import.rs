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

// Payload-only experiment for a future event sink. Both candidates validate
// the same flat point-array grammar and produce identical buffers. This is
// deliberately not a second USD importer: it excludes declarations, metadata,
// recovery, role types and composition. Its purpose is to measure the cost of
// rescanning a source range after structural recognition.
fn point_events<'a>(source: &'a str, mut point: impl FnMut([&'a str; 3])) {
    use layerstack_usda::lexer::{Lexer, TokenKind};
    let mut tokens = Lexer::new(source)
        .filter(|token| !matches!(token.kind, TokenKind::Whitespace | TokenKind::Newline))
        .peekable();
    assert_eq!(
        tokens.next().unwrap().kind,
        TokenKind::LeftBracket,
        "point payload starts with an array"
    );
    while tokens.peek().unwrap().kind != TokenKind::RightBracket {
        assert_eq!(
            tokens.next().unwrap().kind,
            TokenKind::LeftParen,
            "point elements are tuples"
        );
        let components = std::array::from_fn(|_| {
            let first = tokens.next().unwrap();
            let number = if first.kind == TokenKind::Minus {
                tokens.next().unwrap()
            } else {
                first
            };
            assert_eq!(
                number.kind,
                TokenKind::Number,
                "fixture components are numeric literals"
            );
            let text = &source[first.span.start as usize..number.span.end as usize];
            if tokens
                .peek()
                .is_some_and(|token| token.kind == TokenKind::Comma)
            {
                tokens.next();
            }
            text
        });
        assert_eq!(
            tokens.next().unwrap().kind,
            TokenKind::RightParen,
            "point tuples contain exactly three components"
        );
        point(components);
        if tokens
            .peek()
            .is_some_and(|token| token.kind == TokenKind::Comma)
        {
            tokens.next();
        }
    }
    tokens.next();
    assert!(
        tokens.next().is_none(),
        "payload ends after its closing bracket"
    );
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD point values narrow parsed doubles to floats"
)]
fn event_payload(c: &mut Criterion) {
    let count = 100_000;
    let mut source = String::from("[");
    for _ in 0..count {
        source.push_str("(1.25, -2.5, 3.75),");
    }
    source.push(']');
    let collect = |source: &str| {
        let mut points = Vec::with_capacity(count);
        point_events(source, |components| {
            points.push(components.map(|text| text.parse::<f64>().unwrap() as f32));
        });
        points
    };
    assert_eq!(
        collect(&source),
        vec![[1.25, -2.5, 3.75]; count],
        "event sink preserves every point"
    );
    let mut group = c.benchmark_group("numeric_events_payload");
    group.throughput(Throughput::Bytes(source.len() as u64));
    group.bench_function("range_then_decode", |b| {
        b.iter(|| {
            point_events(black_box(&source), |point| {
                black_box(point);
            });
            black_box(collect(black_box(&source)))
        });
    });
    group.bench_function("single_pass_sink", |b| {
        b.iter(|| black_box(collect(black_box(&source))));
    });
    group.finish();
}

criterion_group!(benches, import, event_payload);
criterion_main!(benches);
