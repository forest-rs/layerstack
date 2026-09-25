// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Robustness of the USDC reader against malformed input.
//!
//! A crate file is untrusted input: whatever its bytes, reading it must
//! return a layer or a `UsdcError`, never panic, recurse without bound or
//! allocate beyond what the file's contents justify. These tests take real
//! crate files and read every truncation of them and a fixed set of
//! deterministic byte mutations. The mutations come from a fixed-seed
//! generator, so every run reads the same inputs.
//!
//! Spec: AOUSD Core §16.3.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use layerstack::doc::LayerId;
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};
use layerstack_conformance::workspace_root;

/// Crate files covering every section and the variable-length encodings:
/// list ops of references with layer offsets, dictionaries, time samples,
/// compressed integer and float arrays, variants, relocates, splines and
/// array edits.
const FILES: &[&str] = &[
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_listops.usdc",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_dict.usdc",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_timesamples.usdc",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_double.usdc",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_int.usdc",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_variants.usdc",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_relocates.usdc",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_matrix4d.usdc",
    "core-spec-supplemental-release_dec2025/composition/tests/assets/ReferenceListOpsWithOffsets_root/sub.usd",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_splines.usdc",
    "core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary/gen_pathexpression.usdc",
    "layerstack_conformance/fixtures/usdc_versions/sublayers_root.usdc",
    "layerstack_conformance/fixtures/usdc_versions/version_0_13.usdc",
    "layerstack_conformance/fixtures/usdc_versions/array_edits_weak.usdc",
];

/// Mutations per file.
const MUTATIONS: usize = 4000;

/// A resolver that resolves nothing, so reading stays within one file.
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

/// Reads `data`, returning whether the reader panicked.
fn panics(data: &[u8]) -> bool {
    catch_unwind(AssertUnwindSafe(|| {
        let _ = layerstack_usdc::read_usdc(
            data,
            LayerId(1),
            &mut TokenInterner::default(),
            &mut PathInterner::default(),
            &mut NoAssets,
        );
    }))
    .is_err()
}

/// A xorshift64 generator: deterministic, and enough to spread mutations.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).expect("fits")
    }
}

/// Applies mutation `i` to a copy of `original`.
///
/// Mutations overwrite one byte with a random value, or one little-endian
/// 4- or 8-byte field with a boundary value: counts, sizes, offsets and
/// indexes are fields of those widths, and their boundary values reach the
/// overflow, allocation and recursion paths.
fn mutate(original: &[u8], rng: &mut XorShift) -> Vec<u8> {
    let mut data = original.to_vec();
    let len = data.len() as u64;
    let boundary = [
        0,
        1,
        len - 1,
        len,
        len + 1,
        u64::from(u32::MAX),
        u64::MAX,
        u64::MAX / 2,
        1 << 31,
        1 << 40,
    ];
    let value = boundary[rng.below(boundary.len())];
    match rng.below(4) {
        0 => {
            let pos = rng.below(data.len());
            data[pos] = rng.next().to_le_bytes()[0];
        }
        1 if data.len() >= 4 => {
            let pos = rng.below(data.len() - 3);
            data[pos..pos + 4].copy_from_slice(&value.to_le_bytes()[..4]);
        }
        _ if data.len() >= 8 => {
            let pos = rng.below(data.len() - 7);
            data[pos..pos + 8].copy_from_slice(&value.to_le_bytes());
        }
        _ => {}
    }
    data
}

fn read_file(name: &str) -> Vec<u8> {
    let path: PathBuf = workspace_root().join(name);
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert!(data.starts_with(b"PXR-USDC"), "{name} is not a crate file");
    data
}

/// Runs `inputs` through the reader with the panic hook silenced, and lists
/// the inputs that panicked.
fn panicking(inputs: impl Iterator<Item = (String, Vec<u8>)>) -> Vec<String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let failures = inputs
        .filter(|(_, data)| panics(data))
        .map(|(label, _)| label)
        .collect();
    std::panic::set_hook(hook);
    failures
}

fn assert_none_panic(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} inputs panicked the reader, first: {:?}",
        failures.len(),
        &failures[..failures.len().min(10)]
    );
}

#[test]
fn every_truncation_reads_without_panicking() {
    let failures = panicking(FILES.iter().flat_map(|name| {
        let data = read_file(name);
        (0..data.len()).map(move |len| (format!("{name} truncated to {len}"), data[..len].to_vec()))
    }));
    assert_none_panic(&failures);
}

#[test]
fn byte_mutations_read_without_panicking() {
    let failures = panicking(FILES.iter().enumerate().flat_map(|(seed, name)| {
        let data = read_file(name);
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15 ^ (seed as u64 + 1));
        (0..MUTATIONS).map(move |i| (format!("{name} mutation {i}"), mutate(&data, &mut rng)))
    }));
    assert_none_panic(&failures);
}
