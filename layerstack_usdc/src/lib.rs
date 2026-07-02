// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDC (binary crate format) reader for layerstack.
//!
//! Reads USDC files per AOUSD Core §16.3 and produces [`Layer`] / [`PrimSpec`]
//! structures compatible with the layerstack composition engine.
//!
//! The reader operates on a byte slice (`&[u8]`), making it suitable for both
//! file reads and memory-mapped I/O. The crate is `no_std` by default; enable
//! the `std` feature for convenience wrappers that accept file paths.
//!
//! # Pipeline
//!
//! ```text
//! &[u8] → header → TOC → sections → value reps → assemble → Layer
//! ```
//!
//! [`Layer`]: layerstack::doc::Layer
//! [`PrimSpec`]: layerstack::doc::PrimSpec

#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

pub mod compression;
pub mod error;
pub mod header;
pub mod section;
pub mod toc;
// Value representation decoding pervasively casts u64 file offsets/counts to
// usize. Files larger than 4 GiB on 32-bit targets are unsupported.
#[allow(
    clippy::cast_possible_truncation,
    reason = "pervasive u64→usize casts for file offsets; >4 GiB files unsupported"
)]
pub mod value_rep;
pub mod value_type;

// Scene assembly converts decoded sections into Layer/PrimSpec structures.
// Like value_rep, it pervasively casts u64 indices to usize.
#[allow(
    clippy::cast_possible_truncation,
    reason = "pervasive u64→usize casts for section indices; >4 GiB files unsupported"
)]
pub mod assemble;

use layerstack::AssetResolver;
use layerstack::doc::LayerId;
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;

pub use assemble::AssembleResult;
pub use error::UsdcError;

/// Reads a USDC binary file from a byte slice and produces a [`Layer`].
///
/// This is the main entry point for the crate. It runs the full pipeline:
/// header → TOC → sections → value reps → assemble.
///
/// `data` must contain the complete USDC file contents.
///
/// Spec: AOUSD Core §16.3.
///
/// ```
/// use layerstack::{
///     AssetResolveError, AssetResolver, LayerId, ResolvedAsset,
///     TokenInterner, PathInterner,
/// };
/// use layerstack_usdc::{read_usdc, UsdcError};
///
/// struct NoAssets;
/// impl AssetResolver for NoAssets {
///     fn resolve(&mut self, _: &str, _: Option<LayerId>, _: &mut TokenInterner,
///                _: &mut PathInterner) -> Result<ResolvedAsset, AssetResolveError> {
///         Err(AssetResolveError::NotFound)
///     }
///     fn resolved_path(&self, _: LayerId) -> Option<&str> { None }
/// }
///
/// // Truncated data fails cleanly at the header stage.
/// let result = read_usdc(
///     b"too short",
///     LayerId(1),
///     &mut TokenInterner::default(),
///     &mut PathInterner::default(),
///     &mut NoAssets,
/// );
/// assert!(matches!(result, Err(UsdcError::UnexpectedEof { .. })));
/// ```
///
/// [`Layer`]: layerstack::doc::Layer
pub fn read_usdc(
    data: &[u8],
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<AssembleResult, UsdcError> {
    let hdr = header::parse_header(data)?;
    let toc_sections = toc::parse_toc(data, hdr.toc_offset)?;
    let sections = section::parse_sections(data, &toc_sections)?;
    assemble::assemble(data, &sections, layer_id, tokens, paths, resolver)
}
