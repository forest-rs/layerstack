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
//! Each read also checks what it materialized against what it charged to its
//! `DecodeBudget`: the layer may hold no more values than the units charged,
//! and the read may allocate no more than [`BYTES_PER_UNIT`] bytes per unit
//! (plus [`BASE_BYTES`]). Anything materialized without being charged, such
//! as a copy of a name or path that many specs share, would let a small file
//! expand without bound, and a copy whose size the file controls breaks the
//! byte bound once the file makes it large enough.
//!
//! Spec: AOUSD Core §16.3.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use layerstack::doc::{FieldEntry, FieldValue, Layer, LayerId, Value};
use layerstack::interner::TokenInterner;
use layerstack::listop::ListOp;
use layerstack::path::PathInterner;
use layerstack::property::PropertyEntry;
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};
use layerstack_conformance::workspace_root;
use layerstack_usdc::DecodeBudget;

/// Crate files covering every section and the variable-length encodings:
/// list ops of references with layer offsets, dictionaries, time samples,
/// compressed integer and float arrays, variants, relocates, splines and
/// array edits. They include every OpenUSD-written fixture of
/// `fixtures/usdc_versions` (crate 0.12 to 0.15, splines and array edits)
/// and `fixtures/usdc_budget`, so every decoder runs under both the
/// truncation and mutation sweep and the allocation bound.
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
    "layerstack_conformance/fixtures/usdc_versions/array_edits_strong.usdc",
    "layerstack_conformance/fixtures/usdc_versions/array_edits_weak.usdc",
    "layerstack_conformance/fixtures/usdc_versions/spline_loop_boundary.usdc",
    "layerstack_conformance/fixtures/usdc_versions/spline_time_valued.usdc",
    "layerstack_conformance/fixtures/usdc_versions/sublayers_root.usdc",
    "layerstack_conformance/fixtures/usdc_versions/sublayers_weak.usdc",
    "layerstack_conformance/fixtures/usdc_versions/version_0_12.usdc",
    "layerstack_conformance/fixtures/usdc_versions/version_0_13.usdc",
    "layerstack_conformance/fixtures/usdc_versions/version_0_14.usdc",
    "layerstack_conformance/fixtures/usdc_versions/version_0_15.usdc",
    "layerstack_conformance/fixtures/usdc_budget/shared_array_edit_literal.usdc",
];

/// Mutations per file.
const MUTATIONS: usize = 4000;

/// Bytes a read may allocate per budget unit charged.
///
/// A unit covers one value, element, table entry or spec, or 16 bytes of
/// text, and each is held in several representations while it is read (a
/// `CrateValue` of 112 bytes, then a `Value`, and a spec's own structures):
/// the files and mutations below allocate at most about 200 bytes per unit.
const BYTES_PER_UNIT: u64 = 512;

/// Bytes a read may allocate regardless of what it charges: the interners,
/// maps and error values every read sets up.
const BASE_BYTES: u64 = 64 * 1024;

/// Counts the bytes each thread allocates, so a test can measure what one
/// read materializes, whatever the reader does internally.
struct CountingAllocator;

thread_local! {
    /// Bytes allocated by this thread since it started, growth included.
    static ALLOCATED: Cell<u64> = const { Cell::new(0) };
}

fn count_allocation(bytes: usize) {
    // `try_with` fails only while the thread is being torn down.
    let _ = ALLOCATED.try_with(|allocated| allocated.set(allocated.get() + bytes as u64));
}

// Forwards every call to the system allocator unchanged; it only counts.
#[allow(
    unsafe_code,
    reason = "a global allocator is the only way to observe every allocation a read makes"
)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_allocation(new_size.saturating_sub(layout.size()));
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: CountingAllocator = CountingAllocator;

/// Runs `f`, returning its result and the bytes this thread allocated
/// meanwhile.
fn allocated_by<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let before = ALLOCATED.with(Cell::get);
    let result = f();
    (result, ALLOCATED.with(Cell::get) - before)
}

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

/// Counts the values a layer holds: every metadata field and property slot,
/// and every element of arrays, dictionaries, time samples and list ops.
fn count_values(layer: &Layer) -> u64 {
    fn value(v: &Value) -> u64 {
        1 + match v {
            Value::Array(items) => items.iter().map(value).sum(),
            Value::Dictionary(entries) => entries.iter().map(|(_, v)| value(v)).sum(),
            _ => 0,
        }
    }
    fn list<T>(op: &ListOp<T>) -> u64 {
        let items = op.explicit.iter().flatten().count()
            + op.prepend.len()
            + op.append.len()
            + op.delete.len();
        1 + items as u64
    }
    fn fields(fields: &[FieldEntry]) -> u64 {
        fields
            .iter()
            .map(|f| match &f.value {
                FieldValue::Value(v) => value(v),
                FieldValue::TokenListOp(op) => list(op),
                FieldValue::PathListOp(op) => list(op),
                FieldValue::StringListOp(op) => list(op),
                FieldValue::IntListOp(op) => list(op),
                FieldValue::UIntListOp(op) => list(op),
                FieldValue::Int64ListOp(op) => list(op),
                FieldValue::UInt64ListOp(op) => list(op),
            })
            .sum()
    }
    fn properties(properties: &[PropertyEntry]) -> u64 {
        properties
            .iter()
            .map(|entry| {
                let spec = &entry.spec;
                1 + spec.default.as_ref().map_or(0, value)
                    + spec.time_samples.as_ref().map_or(0, |samples| {
                        1 + samples.iter().map(|(_, v)| value(v)).sum::<u64>()
                    })
                    + spec.targets.as_ref().map_or(0, list)
                    + fields(&spec.metadata)
            })
            .sum()
    }
    let prims = layer.prims.keys().flat_map(|path| layer.prim_specs(*path));
    fields(&layer.metadata)
        + prims
            .map(|prim| {
                let variants = prim.variant_branches().map(|branch| branch.spec);
                fields(&prim.fields)
                    + properties(&prim.properties)
                    + variants
                        .map(|variant| fields(&variant.fields) + properties(&variant.properties))
                        .sum::<u64>()
            })
            .sum::<u64>()
}

/// Reads `data` within `budget`, returning the result and the bytes the
/// read allocated.
fn read_counting(
    data: &[u8],
    budget: &mut DecodeBudget,
) -> (
    Result<layerstack_usdc::AssembleResult, layerstack_usdc::UsdcError>,
    u64,
) {
    allocated_by(|| {
        layerstack_usdc::read_usdc_within(
            data,
            LayerId(1),
            &mut TokenInterner::default(),
            &mut PathInterner::default(),
            &mut NoAssets,
            budget,
        )
    })
}

/// Whether `bytes` allocated by a read are covered by the `used` units it
/// charged.
fn allocation_is_charged(bytes: u64, used: u64) -> bool {
    bytes
        <= BYTES_PER_UNIT
            .saturating_mul(used)
            .saturating_add(BASE_BYTES)
}

/// Reads `data`, returning a failure: a panic, a layer holding more values
/// than the read charged to its budget, or more allocation than it charged.
fn failure(data: &[u8]) -> Option<&'static str> {
    catch_unwind(AssertUnwindSafe(|| {
        let mut budget = DecodeBudget::for_input(data.len());
        let (result, bytes) = read_counting(data, &mut budget);
        if !allocation_is_charged(bytes, budget.used()) {
            return Some("uncharged allocation");
        }
        match result {
            Ok(read) if count_values(&read.layer) > budget.used() => Some("uncharged values"),
            _ => None,
        }
    }))
    .unwrap_or(Some("panicked"))
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
/// the inputs that failed.
fn failing(inputs: impl Iterator<Item = (String, Vec<u8>)>) -> Vec<String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let failures = inputs
        .filter_map(|(label, data)| failure(&data).map(|why| format!("{label}: {why}")))
        .collect();
    std::panic::set_hook(hook);
    failures
}

fn assert_none_fail(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} inputs failed, first: {:?}",
        failures.len(),
        &failures[..failures.len().min(10)]
    );
}

#[test]
fn every_truncation_reads_cleanly() {
    let failures = failing(FILES.iter().flat_map(|name| {
        let data = read_file(name);
        (0..data.len()).map(move |len| (format!("{name} truncated to {len}"), data[..len].to_vec()))
    }));
    assert_none_fail(&failures);
}

#[test]
fn byte_mutations_read_cleanly() {
    let failures = failing(FILES.iter().enumerate().flat_map(|(seed, name)| {
        let data = read_file(name);
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15 ^ (seed as u64 + 1));
        (0..MUTATIONS).map(move |i| (format!("{name} mutation {i}"), mutate(&data, &mut rng)))
    }));
    assert_none_fail(&failures);
}

/// The unmodified files hold only charged values and allocate only what
/// they charge too, and hold values at all, so the checks above are not
/// vacuous. A file may fail only on a feature the reader does not support
/// (`spline_loop_boundary.usdc` authors `loopBoundaryTime`).
#[test]
fn unmodified_files_hold_only_charged_values() {
    let mut total = 0;
    for name in FILES {
        let data = read_file(name);
        let mut budget = DecodeBudget::for_input(data.len());
        let (read, bytes) = read_counting(&data, &mut budget);
        let used = budget.used();
        assert!(
            allocation_is_charged(bytes, used),
            "{name}: {bytes} bytes allocated, {used} units charged"
        );
        let read = match read {
            Ok(read) => read,
            Err(layerstack_usdc::UsdcError::UnsupportedFeature { .. }) => continue,
            Err(e) => panic!("{name}: {e}"),
        };
        let values = count_values(&read.layer);
        assert!(values <= used, "{name}: {values} values, {used} charged");
        total += values;
    }
    assert!(total > 100, "{total}");
}

/// Writes a crate file of `specs` prims that all author one field named
/// with `name_len` bytes. The writer shares one fieldset, and so one field
/// name, between them, as OpenUSD does.
fn shared_field_name_file(specs: usize, name_len: usize) -> Vec<u8> {
    use layerstack_usdc::value_type::SpecForm;
    use layerstack_usdc::writer::{Spec, Value, write_crate};

    let name = "x".repeat(name_len);
    let children = (0..specs).map(|i| format!("P{i}")).collect();
    let mut all = vec![
        Spec::new("/", SpecForm::PseudoRoot)
            .with_field("primChildren", Value::TokenVector(children)),
    ];
    all.extend((0..specs).map(|i| {
        Spec::new(format!("/P{i}"), SpecForm::Prim).with_field(name.as_str(), Value::Int(1))
    }));
    write_crate(&all).expect("write crate")
}

/// Many specs sharing a long field name (through one fieldset) are charged
/// for each use of the name, so a budget below the names' total fails, and
/// what a read allocates stays within what it charges however long the
/// shared name is.
#[test]
fn shared_field_names_are_charged_by_length() {
    // Enough specs that copying the name for each would outgrow any bound
    // the file's own size (which holds the name once) could explain.
    const SPECS: usize = 256;
    const NAME_LEN: usize = 64 * 1024;
    let data = shared_field_name_file(SPECS, NAME_LEN);
    let names = (SPECS * NAME_LEN / 16) as u64;

    let mut budget = DecodeBudget::for_input(data.len());
    let (read, bytes) = read_counting(&data, &mut budget);
    let read = read.expect("read");
    assert_eq!(read.layer.prims.len(), SPECS + 1);
    let used = budget.used();
    assert!(
        allocation_is_charged(bytes, used),
        "{bytes} bytes allocated, {used} units charged"
    );
    assert!(
        used >= names,
        "{used} units charged for {names} units of names"
    );

    let limit = names / 2;
    let (read, _) = read_counting(&data, &mut DecodeBudget::with_limit(limit));
    assert!(
        matches!(
            read,
            Err(layerstack_usdc::UsdcError::DecodeBudgetExceeded { limit: l }) if l == limit
        ),
        "{:?}",
        read.map(|_| ())
    );
}

/// Array edit instructions that all reuse one 64 KiB literal (OpenUSD
/// stores it once in the edit's literal table) share the converted literal
/// instead of copying it per instruction, so the read stays within what it
/// charges. See `fixtures/usdc_budget/generate.py`.
#[test]
fn shared_array_edit_literals_stay_within_the_budget() {
    let data =
        read_file("layerstack_conformance/fixtures/usdc_budget/shared_array_edit_literal.usdc");
    let mut budget = DecodeBudget::for_input(data.len());
    let (read, bytes) = read_counting(&data, &mut budget);
    let read = read.expect("read");
    let used = budget.used();
    assert!(
        allocation_is_charged(bytes, used),
        "{bytes} bytes allocated, {used} units charged"
    );

    // Every literal instruction form, for both attributes, holds the one
    // literal.
    let mut literal_ops = 0;
    for prim in read.layer.prims.values() {
        for entry in &prim.properties {
            let Some(Value::ArrayEdit(edit)) = &entry.spec.default else {
                continue;
            };
            for op in &edit.ops {
                use layerstack::{ArrayEditOp, ArrayEditOperand};
                let literal = match op {
                    ArrayEditOp::Write {
                        src: ArrayEditOperand::Literal(v),
                        ..
                    }
                    | ArrayEditOp::Insert {
                        src: ArrayEditOperand::Literal(v),
                        ..
                    }
                    | ArrayEditOp::MinSizeFill { fill: v, .. }
                    | ArrayEditOp::ResizeFill { fill: v, .. } => v,
                    _ => continue,
                };
                assert!(
                    matches!(literal, Value::String(_) | Value::Token(_)),
                    "{literal:?}"
                );
                literal_ops += 1;
            }
        }
    }
    assert_eq!(literal_ops, 2 * (4 * 64 + 2));
}

/// A spline value blob of `knots` Hermite knots (`TsSpline` format 1,
/// double values, held extrapolation), optionally followed by one byte the
/// format does not allow.
fn spline_value(
    knots: u32,
    trailing_byte: bool,
) -> (layerstack_usdc::value_rep::RawValueRep, Vec<u8>) {
    let mut blob = vec![0x91, 0x09];
    blob.extend_from_slice(&knots.to_le_bytes());
    for i in 0..knots {
        blob.push(0x0a);
        for v in [f64::from(i), 1.0, 0.0, 0.0] {
            blob.extend_from_slice(&v.to_le_bytes());
        }
    }
    if trailing_byte {
        blob.push(0);
    }
    let mut data = vec![0_u8; 8];
    data.extend_from_slice(&(blob.len() as u64).to_le_bytes());
    data.extend_from_slice(&blob);
    data.extend_from_slice(&0_u64.to_le_bytes());
    let mut rep = [0_u8; 8];
    rep[0] = 8;
    rep[6] = layerstack_usdc::value_type::ValueType::Spline as u8;
    (layerstack_usdc::value_rep::RawValueRep::new(rep), data)
}

/// Spline knots are charged before they are parsed, so a budget too small
/// for them fails before allocating them, whether or not the blob would
/// otherwise parse.
#[test]
fn spline_knots_are_charged_before_parsing() {
    use layerstack_usdc::section::CrateSections;
    use layerstack_usdc::value_rep::{CrateValue, decode_value_within};

    let sections = CrateSections {
        tokens: Vec::new(),
        strings: Vec::new(),
        fields: Vec::new(),
        fieldsets: Vec::new(),
        paths: Vec::new(),
        specs: Vec::new(),
        version: layerstack_usdc::CrateVersion::NEWEST_READABLE,
    };
    let knots = 100_000;

    let (rep, data) = spline_value(knots, false);
    let mut budget = DecodeBudget::with_limit(u64::MAX);
    let (value, bytes) = allocated_by(|| decode_value_within(&rep, &data, &sections, &mut budget));
    let Ok(CrateValue::Spline(spline)) = value else {
        panic!("expected a spline, got {value:?}");
    };
    assert_eq!(spline.knots.len(), knots as usize);
    assert!(
        allocation_is_charged(bytes, budget.used()),
        "{bytes} bytes, {} units",
        budget.used()
    );

    for trailing_byte in [false, true] {
        let (rep, data) = spline_value(knots, trailing_byte);
        let mut budget = DecodeBudget::with_limit(1);
        let (value, bytes) =
            allocated_by(|| decode_value_within(&rep, &data, &sections, &mut budget));
        assert_eq!(
            value.err(),
            Some(layerstack_usdc::UsdcError::DecodeBudgetExceeded { limit: 1 }),
            "trailing byte: {trailing_byte}"
        );
        assert!(
            allocation_is_charged(bytes, 1),
            "trailing byte: {trailing_byte}: {bytes} bytes allocated within one unit"
        );
    }
}
