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

pub mod error;
pub mod header;
pub mod toc;
pub mod value_type;
