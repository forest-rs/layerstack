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
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

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
