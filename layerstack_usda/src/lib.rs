// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA (text format) parser and writer for layerstack.
//!
//! This crate provides a parser for the USDA scene
//! description format as specified in AOUSD Core §16.2, plus a deterministic
//! writer. It is organized in these layers:
//!
//! 1. **Lexer** ([`lexer`]) — Tokenizes USDA source into a stream of
//!    [`Token`](lexer::Token)s with span information. Whitespace, comments,
//!    and all syntactic punctuation are preserved as tokens to support
//!    lossless round-tripping.
//!
//! 2. **CST** (concrete syntax tree) — A lossless, whitespace-preserving
//!    tree representation of the source. Every byte of the original input
//!    can be recovered from the CST. This enables formatters, refactoring
//!    tools, and syntax highlighting.
//!
//! 3. **AST** (abstract syntax tree) — A typed tree stripped of
//!    syntactic noise. Represents what was *authored* in the file, not what
//!    composition produces.
//!
//! 4. **Emit** ([`emit`]) — Converts the AST into layerstack's [`Layer`] /
//!    [`PrimSpec`] document model for composition.
//!
//! 5. **Writer** ([`writer`]) — Serializes an explicit *authored* document
//!    (prims, typed attributes with `custom`/`uniform` qualifiers, and
//!    layer/prim/attribute metadata) to deterministic USDA text.
//!
//! 6. **Save** ([`save`]) — Lowers an authored [`Layer`] to that document,
//!    the single route by which both the USDA and the USDC writers save a
//!    layer, rejecting what it cannot write before any output.
//!
//! The parser supports error recovery: malformed input produces partial
//! trees with diagnostics rather than hard failures.
//!
//! For scene loading, [`read_usda`] uses the shared grammar while converting
//! numeric property arrays directly from source ranges into native buffers.
//! It avoids per-element syntax nodes and generic AST tuples. Use the parser
//! APIs when the syntax or inspectable AST is needed.
//!
//! # Quick start
//!
//! ```
//! use layerstack::{
//!     AssetResolveError, AssetResolver, InMemoryStore, LayerId, ResolvedAsset,
//!     TokenInterner, PathInterner,
//! };
//! use layerstack_usda::read_usda;
//!
//! // Minimal resolver that rejects all asset paths (no external files).
//! struct NoAssets;
//! impl AssetResolver for NoAssets {
//!     fn resolve(&mut self, _: &str, _: Option<LayerId>, _: &mut TokenInterner,
//!                _: &mut PathInterner) -> Result<ResolvedAsset, AssetResolveError> {
//!         Err(AssetResolveError::NotFound)
//!     }
//!     fn resolved_path(&self, _: LayerId) -> Option<&str> { None }
//! }
//!
//! let source = "#usda 1.0\ndef Xform \"Root\" {\n    custom string greeting = \"hello\"\n}\n";
//!
//! let mut store = InMemoryStore::default();
//! let result = read_usda(
//!     source,
//!     LayerId(1),
//!     &mut store.tokens,
//!     &mut store.paths,
//!     &mut NoAssets,
//! );
//! assert!(result.parse_diagnostics.is_empty());
//! assert!(result.lower_diagnostics.is_empty());
//! assert!(result.emitted.diagnostics.is_empty());
//! assert!(!result.emitted.layer.prims.is_empty());
//! ```
//!
//! # `no_std` support
//!
//! This crate uses `no_std` with `alloc` and requires Rust 1.89 or later.
//! Parsing consumes `&str` buffers and writing returns owned strings. The
//! declared `std` feature currently adds no APIs; file I/O belongs to the
//! caller. Writer and save errors implement `core::error::Error` without it.
//!
//! [`Layer`]: layerstack::Layer
//! [`PrimSpec`]: layerstack::PrimSpec

#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

#[allow(
    clippy::cast_possible_truncation,
    reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
)]
pub mod lexer;

#[allow(
    clippy::cast_possible_truncation,
    reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
)]
mod span;
pub use span::{Span, TextPosition};

pub mod ast;
pub mod diagnostic;
use layerstack::ident;

#[allow(
    clippy::cast_possible_truncation,
    reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
)]
pub mod cst;

#[allow(
    clippy::cast_possible_truncation,
    reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
)]
pub mod parser;

#[allow(
    clippy::cast_possible_truncation,
    reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
)]
pub mod lower;

#[allow(
    clippy::cast_possible_truncation,
    reason = "USDA value conversions intentionally narrow numeric types"
)]
pub mod emit;

mod read;
pub use read::{ReadResult, ReadStats, read_usda};

pub mod save;
mod spline_text;
pub mod writer;
