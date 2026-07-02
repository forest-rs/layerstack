// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA (text format) parser for layerstack.
//!
//! This crate provides a production-quality parser for the USDA scene
//! description format as specified in AOUSD Core §16.2. It is designed
//! around three layers:
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
//! 3. **AST** (abstract syntax tree) — A typed, validated tree stripped of
//!    syntactic noise. Represents what was *authored* in the file, not what
//!    composition produces.
//!
//! 4. **Emit** ([`emit`]) — Converts the AST into layerstack's [`Layer`] /
//!    [`PrimSpec`] document model for composition.
//!
//! The parser supports error recovery: malformed input produces partial
//! trees with diagnostics rather than hard failures.
//!
//! # Quick start
//!
//! ```
//! use layerstack::{
//!     AssetResolveError, AssetResolver, InMemoryStore, LayerId, ResolvedAsset,
//!     TokenInterner, PathInterner,
//! };
//! use layerstack_usda::{parser, emit};
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
//! let ast = parser::parse(source);
//! let result = emit::emit(
//!     &ast.layer,
//!     LayerId(1),
//!     &mut store.tokens,
//!     &mut store.paths,
//!     &mut NoAssets,
//! );
//! assert!(!result.layer.prims.is_empty());
//! ```
//!
//! # `no_std` support
//!
//! This crate is `no_std` by default, operating on byte slices (`&[u8]`)
//! and `&str` buffers. Enable the `std` feature for file I/O convenience
//! methods and `std::error::Error` integration.
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
