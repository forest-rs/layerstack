// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA (text format) parser and writer for layerstack.
//!
//! This crate provides a parser for the USDA scene
//! description format as specified in AOUSD Core §16.2, plus a deterministic
//! writer. Read a layer with [`read_usda`], and save one with [`save_usda`].
//! It is organized in these layers; the first four are internal, and the
//! `unstable-internals` feature makes them public modules for this
//! repository's tests, benchmarks and fuzzing, with no semver promise:
//!
//! 1. **Lexer** — Tokenizes USDA source into a stream of
//!    tokens with span information. Whitespace, comments,
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
//! 4. **Emit** — Converts the AST into layerstack's [`Layer`] /
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
//! It avoids per-element syntax nodes and generic AST tuples.
//! For plugin metadata, [`read_usda_with_registry`] uses caller-registered
//! types and spec targets,
//! preserving the ordinary literal inference for unregistered field names;
//! an identifier there keeps its text ([`layerstack::Value::Unregistered`]),
//! as OpenUSD keeps it.
//!
//! # Quick start
//!
//! ```
//! use layerstack::{InMemoryStore, LayerId, NoAssets};
//! use layerstack_usda::read_usda;
//!
//! // `NoAssets` resolves no asset path: the layer reads on its own.
//! let source = "#usda 1.0\ndef Xform \"Root\" {\n    custom string greeting = \"hello\"\n}\n";
//!
//! let mut store = InMemoryStore::default();
//! let result = read_usda(
//!     source,
//!     LayerId(1),
//!     store.tokens(),
//!     store.paths(),
//!     &mut NoAssets,
//! );
//! assert!(result.parse_diagnostics.is_empty());
//! assert!(result.lower_diagnostics.is_empty());
//! assert!(result.emitted.diagnostics.is_empty());
//! assert!(!result.emitted.layer.prims().is_empty());
//! ```
//!
//! # `no_std` support
//!
//! This crate uses `no_std` with `alloc` and requires Rust 1.89 or later.
//! Parsing consumes `&str` buffers. Writing returns an owned string or
//! streams the text into any [`core::fmt::Write`] a piece at a time
//! ([`save::save_usda_to`], [`writer::Document::write_usda_to`]), so a caller
//! can write a file without holding its text. The `std` feature adds
//! [`writer::IoText`], which streams into a `std::io::Write`; opening files
//! belongs to the caller (`layerstack_io` streams saves through its
//! storage). Writer and save errors implement `core::error::Error` without
//! it.
//!
//! [`Layer`]: layerstack::Layer
//! [`PrimSpec`]: layerstack::PrimSpec

#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

/// Declares a module that is public with the `unstable-internals` feature,
/// for this repository's tests, benchmarks and fuzzing, and private without
/// it: its items are format internals that follow no semver promise.
macro_rules! internal_mod {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[cfg(feature = "unstable-internals")]
        pub mod $name;
        $(#[$attr])*
        #[cfg(not(feature = "unstable-internals"))]
        #[allow(
            unreachable_pub,
            dead_code,
            reason = "public only with the `unstable-internals` feature"
        )]
        mod $name;
    };
}

internal_mod!(
    #[allow(
        clippy::cast_possible_truncation,
        reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
    )]
    lexer
);

#[allow(
    clippy::cast_possible_truncation,
    reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
)]
mod span;
pub use span::{Span, TextPosition};

internal_mod!(ast);
pub mod diagnostic;
use layerstack::ident;

internal_mod!(
    #[allow(
        clippy::cast_possible_truncation,
        reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
    )]
    cst
);

internal_mod!(
    #[allow(
        clippy::cast_possible_truncation,
        reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
    )]
    parser
);

internal_mod!(
    #[allow(
        clippy::cast_possible_truncation,
        reason = "USDA files >4GB are unrealistic; u32 spans are intentional"
    )]
    lower
);

internal_mod!(
    #[allow(
        clippy::cast_possible_truncation,
        reason = "USDA value conversions intentionally narrow numeric types"
    )]
    emit
);

mod byte_order_mark;
pub use byte_order_mark::ByteOrderMark;
pub mod metadata;
mod numeric;
mod read;
mod token_source;
pub use emit::{EmitResult, metadata_from_text};
pub use read::{ReadResult, ReadStats, read_usda, read_usda_with_registry};

pub mod save;
pub use save::{save_usda, save_usda_to};
mod spline_text;
pub mod writer;
