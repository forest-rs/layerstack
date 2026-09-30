// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Repeated and unique metadata dictionaries, prepared outside measurement.

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

criterion_group!(benches, bench_write);
criterion_main!(benches);
