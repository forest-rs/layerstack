// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Indexed USDC opening versus complete layer import, and selective reads.
//! Files are prepared outside measurement; all cases use the same bytes.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{
    AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, TokenInterner,
};
use layerstack_usdc::{
    CrateFile, DecodeBudget,
    writer::{Spec, SpecForm, Value, write_crate},
};

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

fn fixture(count: u32, len: u32) -> Vec<u8> {
    let mut specs = vec![Spec::new("/", SpecForm::PseudoRoot)];
    for i in 0..count {
        specs.push(Spec::new(format!("/Rock{i}"), SpecForm::Prim).with_field(
            "specifier",
            Value::Specifier(layerstack_usdc::writer::Specifier::Def),
        ));
        specs.push(
            Spec::new(format!("/Rock{i}.weights"), SpecForm::Attribute)
                .with_field("typeName", Value::Token("float[]".into()))
                .with_field(
                    "default",
                    Value::FloatArray(
                        (0..len)
                            .map(|j| f32::from(u16::try_from((j + i) % 65536).unwrap()))
                            .collect(),
                    ),
                ),
        );
    }
    write_crate(&specs).unwrap()
}

fn bench_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("usdc_open");
    for (count, len) in [(1, 262_144), (32, 4096)] {
        let bytes = fixture(count, len);
        let size = format!("{count}x{len}");
        group.bench_with_input(BenchmarkId::new("indexed", &size), &bytes, |b, bytes| {
            b.iter(|| {
                let mut budget = DecodeBudget::for_input(bytes.len());
                black_box(CrateFile::open(black_box(bytes), &mut budget).unwrap());
            });
        });
        group.bench_with_input(
            BenchmarkId::new("indexed_read_one", &size),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    let mut budget = DecodeBudget::for_input(bytes.len());
                    let file = CrateFile::open(black_box(bytes), &mut budget).unwrap();
                    black_box(
                        file.spec("/Rock0.weights")
                            .unwrap()
                            .field("default")
                            .unwrap()
                            .decode(&mut budget)
                            .unwrap(),
                    );
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("materialized", &size),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    black_box(
                        layerstack_usdc::read_usdc(
                            black_box(bytes),
                            LayerId(1),
                            &mut TokenInterner::default(),
                            &mut PathInterner::default(),
                            &mut NoAssets,
                        )
                        .unwrap(),
                    );
                });
            },
        );
    }
    group.finish();
}
criterion_group!(benches, bench_open);
criterion_main!(benches);
