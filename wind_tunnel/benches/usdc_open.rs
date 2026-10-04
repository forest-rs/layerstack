// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Indexed USDC opening versus complete layer import, and selective reads.
//! Files are prepared outside measurement; all cases use the same bytes.

use std::{hint::black_box, sync::Arc};

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
        let retained_bytes: Arc<[u8]> = bytes.clone().into();
        for demand in if count == 1 {
            vec![0_u32, 1]
        } else {
            vec![0_u32, 1, count]
        } {
            let label = if demand == 0 {
                "retained"
            } else if demand == 1 {
                "retained_read_one"
            } else {
                "retained_read_all"
            };
            // count=1 intentionally has only one demand case.
            if count == 1 && label == "retained_read_all" {
                continue;
            }
            group.bench_with_input(
                BenchmarkId::new(label, &size),
                &retained_bytes,
                |b, bytes| {
                    b.iter(|| {
                        let mut tokens = TokenInterner::default();
                        let mut paths = PathInterner::default();
                        let read = layerstack_usdc::read_usdc_lazy(
                            Arc::clone(bytes),
                            LayerId(1),
                            &mut tokens,
                            &mut paths,
                            &mut NoAssets,
                        )
                        .unwrap();
                        let weights = tokens.lookup("weights").unwrap();
                        for i in 0..demand {
                            let name = tokens.lookup(&format!("Rock{i}")).unwrap();
                            let path = paths
                                .lookup(&layerstack::Path::root().join(&[name]))
                                .unwrap();
                            let layerstack::Value::TypedArray(array) = read.assembled.layer.prims
                                [&path]
                                .property(weights)
                                .unwrap()
                                .default
                                .as_ref()
                                .unwrap()
                            else {
                                panic!("numeric array");
                            };
                            black_box(array.try_materialize().unwrap());
                        }
                        black_box(read);
                    });
                },
            );
        }
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
fn bench_integer_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("usdc_integer_decode");
    for width in [4, 8] {
        for count in [64_usize, 262_144] {
            for mixed in [false, true] {
                let mut bytes = 1_i64.to_le_bytes()[..width].to_vec();
                bytes.resize(width + count.div_ceil(4), if mixed { 0xe4 } else { 0 });
                if mixed {
                    for i in 0..count {
                        let size = match i % 4 {
                            0 => 0,
                            1 => width / 4,
                            2 => width / 2,
                            _ => width,
                        };
                        bytes.extend_from_slice(&(-7_i64).to_le_bytes()[..size]);
                    }
                }
                let pattern = if mixed { "mixed" } else { "common" };
                group.bench_with_input(
                    BenchmarkId::new(format!("{width}/{pattern}"), count),
                    &bytes,
                    |b, bytes| {
                        b.iter(|| {
                            black_box(
                                layerstack_usdc::compression::decode_integer_array(
                                    black_box(bytes),
                                    count,
                                    width,
                                )
                                .unwrap(),
                            );
                        });
                    },
                );
            }
        }
    }
    group.finish();
}
criterion_group!(benches, bench_open, bench_integer_decode);
criterion_main!(benches);
