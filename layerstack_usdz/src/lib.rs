// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDZ (packaged scene) reader and writer for layerstack.
//!
//! Reads USDZ package files per AOUSD Core §16.4 and produces [`Layer`] /
//! [`PrimSpec`] structures compatible with the layerstack composition engine.
//! [`write_usdz`] packages already-serialized layers and media into a
//! archive with the USDZ ZIP layout (see [`writer`]); it does not validate
//! the serialized contents of its members or collect asset dependencies.
//!
//! USDZ is a constrained ZIP archive containing USD layers and associated
//! media (textures, audio). Constraints (§16.4.1):
//! - All entries are uncompressed (Stored)
//! - No encryption
//! - 32-bit ZIP only (no Zip64)
//! - Entry data offsets are 64-byte aligned
//! - No End of Central Directory comment
//! - First file is the root USD layer
//!
//! The reader operates on a byte slice (`&[u8]`), making it suitable for
//! both file reads and memory-mapped I/O. [`read_usdz_with_options`] accepts
//! a per-member USDC decoder budget. With `std`, retained array loading defers
//! numeric payload decoding; each USDC member owns its encoded byte copy and
//! cache, exposed through [`UsdzResult::retained_values`].
//!
//! # Pipeline
//!
//! ```text
//! &[u8] → ZIP parse → USDZ validation → root layer → format dispatch → Layer
//! ```
//!
//! [`Layer`]: layerstack::doc::Layer
//! [`PrimSpec`]: layerstack::doc::PrimSpec

#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::AssetResolver;
use layerstack::doc::{Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;

pub mod crc32;
pub mod diagnostic;
pub mod error;
pub mod localize;
mod resolver;
pub mod writer;
pub mod zip;

pub use diagnostic::{ImportDiagnostic, MemberDiagnostic};
pub use error::{LayerReadError, UsdzError};
pub use writer::{PackageFile, UsdzWriteError, write_usdz};

/// How USDC numeric values are loaded within a package or stage document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsdcArrayLoading {
    /// Decode numeric arrays during import.
    #[default]
    Eager,
    /// Retain immutable encoded bytes and decode numeric arrays on demand.
    /// Structural validation occurs during import; payload failures occur during
    /// checked reads. Each package member retains a separate byte copy/cache.
    #[cfg(feature = "std")]
    Retained,
}
/// Per-USDC-file decoding policy, including package members.
///
/// The budget covers structural import plus numeric elements and decode work;
/// it is measured in decoder units, not memory bytes. It applies independently
/// to each file. It does not limit transport bytes, USDA parsing or scene-wide
/// memory. The host remains responsible for those budgets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsdcReadOptions {
    /// Whether arrays decode immediately or retain their encoded storage.
    pub arrays: UsdcArrayLoading,
    /// Explicit decoder-unit limit; `None` uses the input-derived default.
    pub decode_budget: Option<u64>,
}
impl UsdcReadOptions {
    /// Creates a fresh budget for one file, shared with future retained reads.
    #[must_use]
    pub fn budget(&self, input_bytes: usize) -> layerstack_usdc::DecodeBudget {
        self.decode_budget.map_or_else(
            || layerstack_usdc::DecodeBudget::for_input(input_bytes),
            layerstack_usdc::DecodeBudget::with_limit,
        )
    }
}

/// The result of successfully reading a USDZ package.
#[derive(Clone, Debug)]
pub struct UsdzResult {
    /// The root layer assembled from the first USD file in the package.
    pub layer: Layer,
    /// Every other layer loaded while reading the package, each once: the
    /// sublayers, references and payloads of the root layer, and theirs in
    /// turn, whether read from the package or from the outer resolver. The
    /// caller must insert all of them into their store before composing.
    pub resolved_layers: Vec<Layer>,
    /// The package path of every layer read from the package, the root
    /// layer's included (`root.usda`, `models/asset.usda`). Layers the
    /// outer resolver loaded are not listed.
    pub member_paths: BTreeMap<LayerId, Arc<str>>,
    /// Recovery diagnostics from loaded package members, each reported once.
    ///
    /// Root diagnostics come first, followed by other members in completion
    /// order; each member preserves parse/lower/emit or assembly order.
    /// Inspect these before accepting a partial result. No diagnostics are
    /// available here for unloaded members or layers decoded by the outer
    /// resolver, which owns its own diagnostic policy.
    pub diagnostics: Vec<MemberDiagnostic>,
    /// Inspection handles for retained USDC package members, including the root.
    /// Handles survive layer clones; dropping them does not invalidate numeric values.
    #[cfg(feature = "std")]
    pub retained_values: BTreeMap<LayerId, layerstack_usdc::RetainedValues>,
}

impl UsdzResult {
    /// Whether any loaded member reported malformed or unrepresented content.
    ///
    /// This checks [`Self::diagnostics`], not composition or viewer support.
    /// `false` does not rule out warnings, unread members, or problems inside
    /// the outer resolver. Callers that require a diagnostic-free import can
    /// instead require `diagnostics.is_empty()`.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.diagnostic.is_error())
    }
}

/// Reads a USDZ package from a byte slice and produces a [`Layer`].
///
/// This is the main entry point for the crate. It runs the full pipeline:
/// ZIP parse → USDZ constraint validation → CRC-32 verification → root
/// layer format dispatch → assembly.
///
/// `data` must contain the complete USDZ file contents.
///
/// Readable layers may recover from malformed or unsupported authored content.
/// `Ok` then contains a partial layer and [`UsdzResult::diagnostics`]; inspect
/// those diagnostics or [`UsdzResult::has_errors`] before using it. A hard
/// decoding failure in the root or any loaded package member returns
/// [`UsdzError::LayerRead`] with the member and original typed cause. Missing
/// assets retain unresolved arcs; missing layer-relative members also retain
/// typed resolver evidence in `diagnostics`.
/// This reads reachable layers, not every unused layer member in the archive.
///
/// A relative asset path authored in a package layer names a member of the
/// package, anchored as OpenUSD anchors it: a path relative to its layer
/// (`./asset.usda`) to the directory of the member that authors it; a
/// search path (`asset.usda`) to that directory, then to the root layer's.
/// The `resolver` receives absolute paths and search paths that name no
/// member, anchored to `layer_id`, which stands for the package.
///
/// Every layer read from or through the package goes into one store, so
/// `resolver` owns their IDs: `layer_id`, which the caller allocates from
/// it, names the root layer, and every other member loaded gets an ID from
/// [`AssetResolver::allocate_layer_id`]. A package whose root layer loads
/// another member needs a resolver that allocates
/// ([`UsdzError::LayerIdUnavailable`]), and a layer ID returned twice
/// fails the read ([`UsdzError::DuplicateLayerId`]).
///
/// Spec: AOUSD Core §16.4, §9.7. OpenUSD:
/// `SdfComputeAssetPathRelativeToLayer` in `pxr/usd/sdf/layerUtils.cpp`.
///
/// ```
/// use layerstack::{
///     AssetResolveError, AssetResolver, LayerId, ResolvedAsset,
///     TokenInterner, PathInterner,
/// };
/// use layerstack_usdz::{read_usdz, write_usdz, ImportDiagnostic, PackageFile};
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
/// // Readable text can still lose an invalid authored value during recovery.
/// let source = b"#usda 1.0\ndef \"Rock\" { float amount = \"wrong\" }\n";
/// let package = write_usdz(&[PackageFile::new("root.usda", source)]).unwrap();
/// let result = read_usdz(
///     &package,
///     LayerId(1),
///     &mut TokenInterner::default(),
///     &mut PathInterner::default(),
///     &mut NoAssets,
/// ).unwrap();
/// assert!(result.has_errors());
/// let diagnostic = &result.diagnostics[0];
/// assert_eq!(&*diagnostic.member, "root.usda");
/// assert!(matches!(diagnostic.diagnostic, ImportDiagnostic::UsdaEmit(_)));
/// // A strict consumer rejects this partial import; an inspector can use
/// // the original diagnostic's source span to highlight the problem.
/// ```
///
/// [`Layer`]: layerstack::doc::Layer
pub fn read_usdz(
    data: &[u8],
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<UsdzResult, UsdzError> {
    read_usdz_with_members(data, layer_id, tokens, paths, resolver, &[])
}

/// Reads a package and explicit additional resident members, even if its root
/// no longer references them. Selected missing members fail atomically. Used
/// by hosts retaining package layer identities across reloads; ordinary import
/// uses `read_usdz`. AOUSD Core §9.7, §16.4.
pub fn read_usdz_with_members(
    data: &[u8],
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
    members: &[&str],
) -> Result<UsdzResult, UsdzError> {
    read_usdz_with_options(
        data,
        layer_id,
        tokens,
        paths,
        resolver,
        members,
        UsdcReadOptions::default(),
    )
}
/// Reads reachable and explicitly selected members with a per-file USDC policy.
/// CRC and structural validation remain eager. Retained payloads must be read
/// through checked queries to distinguish decode failure from missing data.
/// AOUSD Core §9.7 and §16.4 (package members and format dispatch).
pub fn read_usdz_with_options(
    data: &[u8],
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
    members: &[&str],
    options: UsdcReadOptions,
) -> Result<UsdzResult, UsdzError> {
    // 1. Parse ZIP archive with USDZ constraint validation.
    let archive = zip::ZipArchive::parse(data)?;

    // 2. Validate CRC-32 for all entries.
    for entry in archive.entries() {
        let entry_data = archive.entry_data(entry);
        let actual = crc32::crc32(entry_data);
        if actual != entry.crc32 {
            return Err(UsdzError::CrcMismatch {
                entry: entry.name.clone(),
                expected: entry.crc32,
                actual,
            });
        }
    }

    // 3. Find the root layer (first entry must be a USD file).
    let entries = archive.entries();
    if entries.is_empty() {
        return Err(UsdzError::NoRootLayer);
    }
    let root_entry = &entries[0];
    if !is_usd_extension(&root_entry.name) {
        return Err(UsdzError::NoRootLayer);
    }

    // 4. Create a package-scoped resolver, which knows the root layer as
    //    the member it anchors to. The outer resolver allocates the layer
    //    IDs of the other members.
    let mut usdz_resolver = resolver::UsdzResolver::new(
        &archive,
        root_entry.name.clone(),
        layer_id,
        resolver,
        options,
    );

    // 5. Parse the root layer.
    let root_data = archive.entry_data(root_entry);
    let parsed = resolver::parse_layer_data(
        root_data,
        &root_entry.name,
        layer_id,
        tokens,
        paths,
        &mut usdz_resolver,
        options,
    )?;

    for member in members {
        usdz_resolver.load_selected_member(member, tokens, paths)?;
    }

    // 6. Collect every resolved layer once: those the root layer resolved,
    //    and those resolved beneath them.
    let loaded = usdz_resolver.finish()?;
    let mut resolved_layers = parsed.resolved_layers;
    resolved_layers.extend(loaded.descendants);
    let member_paths = loaded.member_paths;
    let mut diagnostics = parsed.diagnostics;
    diagnostics.extend(loaded.diagnostics);

    // 7. Installing two layers with one ID would silently drop one, so an
    //    outer resolver that handed out an ID twice fails the read.
    let mut ids = BTreeSet::from([layer_id]);
    if let Some(layer) = resolved_layers.iter().find(|layer| !ids.insert(layer.id)) {
        return Err(UsdzError::DuplicateLayerId { id: layer.id });
    }

    #[cfg(feature = "std")]
    let retained_values = {
        let mut values = loaded.retained_values;
        if let Some(handle) = parsed.retained_values {
            values.insert(layer_id, handle);
        }
        values
    };
    Ok(UsdzResult {
        layer: parsed.layer,
        resolved_layers,
        member_paths,
        diagnostics,
        #[cfg(feature = "std")]
        retained_values,
    })
}

/// Returns `true` if the file extension indicates a USD layer.
fn is_usd_extension(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("");
    matches!(ext, "usd" | "usda" | "usdc")
}
