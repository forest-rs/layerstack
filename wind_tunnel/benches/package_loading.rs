// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Package checksum throughput over small members and large media buffers.
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
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
fn package_root(c: &mut Criterion) {
    use layerstack::encoded_bytes::EncodedBytes;
    use layerstack_usdz::{
        PackageFile,
        snapshot::{PackageLimits, PackageSnapshot},
        write_usdz,
    };
    use std::{sync::Arc, time::Duration};
    let mut group = c.benchmark_group("package_root");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for size in [0_usize, 8 * 1024 * 1024, 64 * 1024 * 1024] {
        let root = b"#usda 1.0\ndef \"P\" {}\n";
        let media = vec![0x5a; size];
        let bytes: EncodedBytes = Arc::<[u8]>::from(
            write_usdz(&[
                PackageFile::new("root.usda", root),
                PackageFile::new("texture.png", &media),
            ])
            .unwrap(),
        )
        .into();
        let checked = PackageSnapshot::new(bytes.clone(), PackageLimits::default()).unwrap();
        assert_eq!(
            checked.layer("root.usda").unwrap().bytes.as_ref(),
            root,
            "root bytes are correct"
        );
        let mut ranged = layerstack_usdz::range::RangePackage::open(
            bytes.clone(),
            layerstack_usdz::range::RangeLimits::default(),
        )
        .unwrap();
        assert_eq!(
            ranged.layer("root.usda").unwrap().bytes.as_ref(),
            root,
            "range selection returns the same root"
        );
        assert_eq!(
            ranged.stats().verification_bytes,
            root.len() as u64,
            "only the root payload is CRC-verified"
        );
        assert!(
            ranged.stats().bytes_read < 512,
            "opaque media must not be read"
        );
        group.bench_with_input(
            BenchmarkId::new("selected_verification", size),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    let mut package = layerstack_usdz::range::RangePackage::open(
                        black_box(bytes.clone()),
                        layerstack_usdz::range::RangeLimits::default(),
                    )
                    .unwrap();
                    black_box(package.layer("root.usda").unwrap());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("full_verification", size),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    let snapshot =
                        PackageSnapshot::new(black_box(bytes.clone()), PackageLimits::default())
                            .unwrap();
                    black_box(snapshot.layer(snapshot.root_member().unwrap()).unwrap());
                });
            },
        );
    }
    group.finish();
}
criterion_group!(benches, checksum, package_root);
criterion_main!(benches);
