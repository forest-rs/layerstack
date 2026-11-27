// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA (text format) parser and writer.
//!
//! This module parses the USDA scene description format as specified in
//! AOUSD Core §16.2 and writes it deterministically. Read a layer with
//! [`read_usda`], and save one with [`save_usda`]. It is organized in these
//! layers; the first four are internal, and the `unstable-internals` feature
//! makes them public modules for this repository's tests, benchmarks and
//! fuzzing, with no semver promise:
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
//! # Reading
//!
//! ```
//! use layerstack::{InMemoryStore, LayerId, NoAssets};
//! use layerstack_io::usda::read_usda;
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
//! [`read_usda`] reads a layer with Layerstack interners and an
//! [`AssetResolver`](layerstack::AssetResolver), which supplies external
//! layers; the reader does not open files, and the caller supplies the
//! complete source string. It recovers from malformed input: inspect
//! [`ReadResult::diagnostics`] (parsing, lowering and emission, in that
//! order) or [`ReadResult::has_errors`] before treating the result as valid,
//! and [`ReadResult::is_rejected`] for a layer the reader refuses as
//! OpenUSD's does.
//!
//! The reader uses the shared grammar while converting numeric scalar,
//! vector and quaternion property arrays, samples and variant branches
//! included, directly from source ranges into native buffers in one numeric
//! pass, without per-element tokens, syntax nodes or generic AST tuples;
//! [`ReadResult::stats`] reports retained syntax and recognized numeric
//! arrays. Structural syntax for declarations and metadata is retained.
//!
//! # Metadata
//!
//! Metadata statements read as their field's registered type, as OpenUSD's
//! text parser reads them: the fields Sdf and usd-core's own plugins register
//! are built in. For plugin-defined metadata, build a
//! [`layerstack::SchemaRegistry`] with the same token interner and use
//! [`read_usda_with_registry`]. A field registered for the kind of spec a
//! statement is on converts its literal (`irIsInvertible = 0` is `false` for
//! a `bool` field); a literal the type cannot hold rejects the layer with a
//! diagnostic naming the field, its type and why
//! ([`diagnostic::Diagnostic::metadata`]). A name registered for no field of
//! that spec keeps its literal's own type; an identifier there keeps its
//! text ([`layerstack::Value::Unregistered`]), as OpenUSD keeps it.
//! Registered defaults remain in the registry; importing does not author
//! them. [`metadata::metadata_value`] reads one USDA literal as a given
//! metadata type. A [`StageDocument`](crate::StageDocument) opened with a
//! registry uses it for every layer it reads.
//!
//! # Writing and saving
//!
//! [`writer::Document`] is an explicit authoring model:
//!
//! ```
//! use layerstack::{InMemoryStore, LayerId, NoAssets};
//! use layerstack_io::usda::read_usda;
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
//! let text = document.to_usda().unwrap();
//!
//! let store = InMemoryStore::default();
//! let read = read_usda(&text, LayerId(1), store.tokens(), store.paths(), &mut NoAssets);
//! assert!(read.diagnostics().next().is_none());
//! ```
//!
//! To save an existing [`Layer`], use [`save_usda`] with that layer's token
//! and path interners, or [`save_usda_to`] to stream the same text into any
//! [`core::fmt::Write`] a piece at a time, without holding all of it. Saving
//! preserves supported authored opinions and asset paths; it does not
//! flatten a composed stage, copy referenced assets, or retain source
//! formatting. The [`save`] module lists exactly what saving supports and
//! rejects; unsupported data returns an error before any output.
//!
//! To export a composed stage, use `Stage::flatten` in `layerstack`, inspect
//! its report and verification results, then serialize its output layer.
//! Flattening bakes sparse array edits into dense values; source-layer
//! saving preserves authored opinions and keeps its stricter representation
//! limits. For binary output use [`usdc`](crate::usdc); for packaging use
//! [`usdz`](crate::usdz).
//!
//! # `no_std` support
//!
//! This module needs only `alloc`. Parsing consumes `&str` buffers. Writing
//! returns an owned string or streams the text into any [`core::fmt::Write`]
//! ([`save::save_usda_to`], [`writer::Document::write_usda_to`]). The `std`
//! feature adds [`writer::IoText`], which streams into a `std::io::Write`;
//! [`StageDocument`](crate::StageDocument) streams saves through its storage.
//! Writer and save errors implement `core::error::Error` without it.
//!
//! [`Layer`]: layerstack::Layer
//! [`PrimSpec`]: layerstack::PrimSpec

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
