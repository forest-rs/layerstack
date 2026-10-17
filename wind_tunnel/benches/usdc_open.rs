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
        _: &TokenInterner,
        _: &PathInterner,
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
                        let tokens = TokenInterner::default();
                        let paths = PathInterner::default();
                        let read = layerstack_usdc::read_usdc_lazy(
                            Arc::clone(bytes),
                            LayerId(1),
                            &tokens,
                            &paths,
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
                            &TokenInterner::default(),
                            &PathInterner::default(),
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
fn bench_integer_import(c: &mut Criterion) {
    let mut group = c.benchmark_group("usdc_integer_import");
    group.sample_size(15);
    group.warm_up_time(std::time::Duration::from_millis(300));
    group.measurement_time(std::time::Duration::from_secs(1));
    for count in [64_u32, 262_144] {
        let cases = [
            (
                "int",
                Value::IntArray(
                    (0..count)
                        .map(|i| i32::try_from((i * 17) % 65536).unwrap())
                        .collect(),
                ),
            ),
            (
                "uint",
                Value::UIntArray((0..count).map(|i| (i * 17) % 65536).collect()),
            ),
            (
                "int64",
                Value::Int64Array((0..count).map(|i| i64::from((i * 17) % 65536)).collect()),
            ),
            (
                "uint64",
                Value::UInt64Array((0..count).map(|i| u64::from((i * 17) % 65536)).collect()),
            ),
        ];
        for (kind, values) in cases {
            let bytes = write_crate(&[
                Spec::new("/", SpecForm::PseudoRoot),
                Spec::new("/Mesh", SpecForm::Prim).with_field(
                    "specifier",
                    Value::Specifier(layerstack_usdc::writer::Specifier::Def),
                ),
                Spec::new("/Mesh.indices", SpecForm::Attribute)
                    .with_field("typeName", Value::Token(format!("{kind}[]")))
                    .with_field("default", values),
            ])
            .unwrap();
            let tokens = TokenInterner::default();
            let paths = PathInterner::default();
            let read =
                layerstack_usdc::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets)
                    .unwrap();
            assert!(read.diagnostics.is_empty(), "fixture imports completely");
            let field = tokens.intern("indices");
            let value = read
                .layer
                .prims
                .values()
                .next()
                .unwrap()
                .property(field)
                .unwrap()
                .default
                .as_ref()
                .unwrap();
            assert_eq!(
                value.array_ref().unwrap().len(),
                usize::try_from(count).unwrap(),
                "all integer elements survive import"
            );
            group.bench_function(BenchmarkId::new(kind, count), |b| {
                b.iter_custom(|iterations| {
                    let mut elapsed = std::time::Duration::ZERO;
                    for _ in 0..iterations {
                        let tokens = TokenInterner::default();
                        let paths = PathInterner::default();
                        let start = std::time::Instant::now();
                        let read = layerstack_usdc::read_usdc(
                            black_box(&bytes),
                            LayerId(1),
                            &tokens,
                            &paths,
                            &mut NoAssets,
                        )
                        .unwrap();
                        elapsed += start.elapsed();
                        black_box(&read);
                        drop(read);
                    }
                    elapsed
                });
            });
        }
    }
    group.finish();
}
criterion_group!(
    benches,
    bench_open,
    bench_integer_decode,
    bench_integer_import
);
criterion_main!(benches);
