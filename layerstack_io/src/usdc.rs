// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDC (binary crate format) reader and writer.
//!
//! Reads USDC files per AOUSD Core §16.3 and produces [`Layer`] / [`PrimSpec`]
//! structures compatible with the layerstack composition engine. The
//! [`writer`] module writes crate files from authored documents, layers, or
//! explicitly authored specs.
//!
//! # Reading
//!
//! [`read_usdc`] takes the complete file as `&[u8]`, a layer ID, token and
//! path interners, and a Layerstack [`AssetResolver`]. Its result holds the
//! authored layer, the externally resolved layers to insert into the
//! caller's store, and diagnostics for authored content that could not be
//! represented: inspect those as well as the outer `Result` before assuming
//! a complete import. It reads byte slices, so it serves file reads and
//! memory-mapped input alike.
//!
//! Crate versions 0.4 through 0.16 are readable, following OpenUSD v26.11 for
//! the versions after AOUSD Core 1.0.1. The [`version`] module lists what each
//! version adds, and which errors report versions and features this reader
//! does not support.
//!
//! Decoding is bounded by an input-derived [`DecodeBudget`];
//! [`read_usdc_within`] takes an explicit budget for materialized data.
//! Files larger than 4 GiB are unsupported on 32-bit targets. Asset
//! resolution and its resource policy remain the caller's.
//!
//! For selective access, [`CrateFile`] opens the structural tables and lets
//! callers inspect specs and decode individual fields on demand. It does not
//! assemble a layer or load dependencies. [`read_usdc`] remains the bulk import
//! path, using compact array decoding directly into layer values.
//!
//! # Retained values
//!
//! With the `std` feature, `read_usdc_lazy` takes immutable `Arc<[u8]>`
//! input and assembles the same layer, retaining numeric defaults and time
//! samples as `TypedArray::Deferred`, and `token[]` and `string[]` attribute
//! defaults of at least `MIN_RETAINED_TEXT_ARRAY` elements as
//! `Value::Deferred`, which decode into the native arrays an eager read
//! holds. Everything composition reads stays eager. `try_materialize` on
//! either borrows a cached decoded value or cached error; value resolution
//! reads through deferred values, and `Attribute::try_get` and
//! `Attribute::try_read` report decode failures explicitly, where ordinary
//! accessors return `None`.
//!
//! Each file gets a thread-safe, immutable cache, with one decode per
//! distinct value representation and one budget shared across assembly and
//! later reads; `RetainedValues::stats` reports retained bytes, live
//! sources, cached element bytes, failures, decode attempts and remaining
//! budget without decoding. Caches stay alive through layer and query
//! clones. Saving materializes retained values and fails before writing
//! output when one cannot decode. A workload that reads every value retains
//! both encoded bytes and decoded buffers, so eager import remains useful
//! there. There is no memory mapping, cache eviction or background work.
//!
//! # Writing
//!
//! ```
//! use layerstack_io::usda::writer::{Attribute, Document, Prim, Value};
//!
//! let mut root = Prim::def("Xform", "Root");
//! root.push_property(Attribute::new(
//!     "greeting", "string", Value::String("hello".into()),
//! ).custom());
//!
//! let mut document = Document::new();
//! document.default_prim = Some("Root".into());
//! document.prims.push(root);
//! let bytes = layerstack_io::usdc::writer::write_document(&document).unwrap();
//! assert_eq!(&bytes[..8], b"PXR-USDC");
//! ```
//!
//! [`save_usdc`] saves an existing [`Layer`] using its interners, through
//! the same lowering as USDA ([`usda::save`](crate::usda::save)) and with
//! its limitations. The binary document writer additionally rejects
//! registered metadata of the wrong type. It keeps unregistered metadata as
//! OpenUSD's text parser does: dictionaries and list ops as themselves, and
//! any other value as its text. For applications that own crate-level specs
//! and fields, [`writer::write_crate`] is the lower-level route: the caller
//! supplies hierarchy and field semantics, without document validation.
//!
//! The writer starts at crate version 0.8.0 and upgrades only as far as the
//! content needs ([`writer::required_version`]): 0.9.0 for timecode
//! values, 0.10.0 for path expressions, 0.11.0 for relocates, 0.12.0 to
//! 0.16.0 for splines by the features they use, 0.14.0 for native array
//! edits and 0.16.0 for durations. Reading a version does not imply writing
//! every feature of it. Repeated arrays and metadata
//! dictionaries, nested ones included, share encoded payloads, preserving
//! value types and floating-point bits.
//!
//! # Pipeline
//!
//! ```text
//! &[u8] → header → TOC → sections → value reps → assemble → Layer
//! ```
//!
//! The stages are internal: the `unstable-internals` feature makes their
//! modules public for this repository's tests, benchmarks and fuzzing, with
//! no semver promise.
//!
//! [`Layer`]: layerstack::doc::Layer
//! [`PrimSpec`]: layerstack::doc::PrimSpec
//! [`AssetResolver`]: layerstack::AssetResolver

#[cfg(feature = "std")]
mod lazy;
#[cfg(feature = "std")]
pub use lazy::{
    LazyReadResult, MIN_RETAINED_TEXT_ARRAY, RetainedValueStats, RetainedValues, read_usdc_lazy,
    read_usdc_lazy_range_within, read_usdc_lazy_within,
};

internal_mod!(compression);
pub mod error;
internal_mod!(header);
pub mod reader;
internal_mod!(section);
mod threads;
internal_mod!(toc);
// Value representation decoding pervasively casts u64 file offsets/counts to
// usize. Files larger than 4 GiB on 32-bit targets are unsupported.
mod numeric_array;
internal_mod!(
    #[allow(
        clippy::cast_possible_truncation,
        reason = "pervasive u64→usize casts for file offsets; >4 GiB files unsupported"
    )]
    value_rep
);
internal_mod!(value_type);
pub mod version;
pub mod writer;

// Scene assembly converts decoded sections into Layer/PrimSpec structures.
// Like value_rep, it pervasively casts u64 indices to usize.
internal_mod!(
    #[allow(
        clippy::cast_possible_truncation,
        reason = "pervasive u64→usize casts for section indices; >4 GiB files unsupported"
    )]
    assemble
);

use layerstack::AssetResolver;
use layerstack::doc::LayerId;
use layerstack::interner::TokenInterner;
use layerstack::parallel::Threads;
use layerstack::path::PathInterner;

pub use assemble::{AssembleDiagnostic, AssembleResult};
pub use error::UsdcError;
pub use reader::{CrateField, CrateFile, CrateSpec};
pub use value_rep::{
    CrateArrayEdit, CrateArrayEditOp, CrateListOp, CrateValue, DecodeBudget, RawValueRep,
};
pub use value_type::ValueType as CrateValueType;
pub use version::CrateVersion;
pub use writer::save_usdc;

/// Reads a USDC binary file from a byte slice and produces a [`Layer`].
///
/// This is the main USDC entry point. It runs the full pipeline:
/// header → TOC → sections → value reps → assemble.
///
/// `data` must contain the complete USDC file contents. Decoding is bounded
/// by [`DecodeBudget::for_input`]; see [`read_usdc_within`].
///
/// Spec: AOUSD Core §16.3.
///
/// ```
/// use layerstack::{LayerId, NoAssets, PathInterner, TokenInterner};
/// use layerstack_io::usdc::{read_usdc, UsdcError};
///
///
/// // Truncated data fails cleanly at the header stage.
/// let result = read_usdc(
///     b"too short",
///     LayerId(1),
///     &TokenInterner::default(),
///     &PathInterner::default(),
///     &mut NoAssets,
/// );
/// assert!(matches!(result, Err(UsdcError::UnexpectedEof { .. })));
/// ```
///
/// [`Layer`]: layerstack::doc::Layer
pub fn read_usdc(
    data: &[u8],
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<AssembleResult, UsdcError> {
    let mut budget = DecodeBudget::for_input(data.len());
    read_usdc_within(
        data,
        layer_id,
        tokens,
        paths,
        resolver,
        &mut budget,
        &Threads::new(1),
    )
}

/// Reads a USDC binary file like [`read_usdc`], charging everything it
/// materializes to `budget` instead of the default
/// [`DecodeBudget::for_input`] budget.
///
/// A file can reference one value, name or path any number of times, and
/// each reference would otherwise cost its own copy; the budget bounds the
/// total. Exceeding it fails with [`UsdcError::DecodeBudgetExceeded`].
///
/// The read runs on the calling thread and the threads `threads` spares
/// while it runs (with the `std` feature): a read started from a pool
/// shares that pool's budget, and uses only the threads its other tasks
/// leave idle. It reads the same layer on any number of threads
/// ([`assemble::assemble`]).
pub fn read_usdc_within(
    data: &[u8],
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    budget: &mut DecodeBudget,
    threads: &Threads,
) -> Result<AssembleResult, UsdcError> {
    let hdr = header::parse_header(data)?;
    let toc_sections = toc::parse_toc(data, hdr.toc_offset)?;
    let version = hdr.crate_version();
    let sections = section::parse_sections_on(data, &toc_sections, version, budget, threads)?;
    assemble::assemble(
        data, &sections, layer_id, tokens, paths, resolver, budget, threads,
    )
}
