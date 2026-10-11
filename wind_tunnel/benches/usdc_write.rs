// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Metadata and scalar numeric arrays, prepared outside measurement.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack_usdc::writer::{Spec, SpecForm, Specifier, Value, write_crate};

fn fixture(count: u32, width: u32, unique: bool, nested: bool) -> Vec<Spec> {
    let mut specs = vec![Spec::new("/", SpecForm::PseudoRoot)];
    for prim in 0..count {
        let mut entries: Vec<_> = (0..width)
            .map(|field| {
                let number = f64::from(field) + if unique { f64::from(prim) } else { 0.0 };
                (format!("field{field:03}"), Value::Double(number))
            })
            .collect();
        if prim % 2 == 1 {
            entries.reverse();
        }
        let value = if nested {
            Value::Dictionary(vec![
                (
                    "description".into(),
                    Value::String("shared metadata".into()),
                ),
                ("settings".into(), Value::Dictionary(entries)),
            ])
        } else {
            Value::Dictionary(entries)
        };
        specs.push(
            Spec::new(format!("/Rock{prim}"), SpecForm::Prim)
                .with_field("specifier", Value::Specifier(Specifier::Def))
                .with_field("customData", value),
        );
    }
    specs
}

fn bench_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("usdc_dictionary_write");
    for (count, width, unique, nested) in [
        (1000, 8, false, false),
        (1000, 64, false, false),
        (1000, 8, true, false),
        (1000, 8, false, true),
    ] {
        let specs = fixture(count, width, unique, nested);
        let label = format!(
            "{count}x{width}_{}_{}",
            if unique { "unique" } else { "repeated" },
            if nested { "nested" } else { "flat" },
        );
        eprintln!("{label}: {} bytes", write_crate(&specs).unwrap().len());
        group.bench_with_input(BenchmarkId::from_parameter(label), &specs, |b, specs| {
            b.iter(|| black_box(write_crate(black_box(specs)).unwrap()));
        });
    }
    group.finish();
}

fn bench_numeric_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("usdc_numeric_write");
    for len in [64, 1_000_000] {
        for encoding in ["integral", "lookup", "plain"] {
            let values = (0..len)
                .map(|i| match encoding {
                    "integral" => (i % 256) as f32,
                    "lookup" => [0.25, 1.5, -0.75][i % 3],
                    _ => i as f32 * 0.125 + 0.0625,
                })
                .collect();
            let specs = numeric_fixture(Value::FloatArray(values));
            group.bench_with_input(BenchmarkId::new(encoding, len), &specs, |b, specs| {
                b.iter(|| black_box(write_crate(black_box(specs)).unwrap()));
            });
        }
    }
    for (name, value) in [
        ("half_lookup", Value::HalfArray(vec![0x3800; 1_000_000])),
        ("double_lookup", Value::DoubleArray(vec![0.5; 1_000_000])),
    ] {
        let specs = numeric_fixture(value);
        group.bench_with_input(BenchmarkId::new(name, 1_000_000), &specs, |b, specs| {
            b.iter(|| black_box(write_crate(black_box(specs)).unwrap()));
        });
    }
    group.finish();
}

fn numeric_fixture(value: Value) -> Vec<Spec> {
    vec![
        Spec::new("/", SpecForm::PseudoRoot),
        Spec::new("/Root", SpecForm::Prim)
            .with_field("specifier", Value::Specifier(Specifier::Def)),
        Spec::new("/Root.values", SpecForm::Attribute).with_field("default", value),
    ]
}

criterion_group!(benches, bench_write, bench_numeric_write);
criterion_main!(benches);
