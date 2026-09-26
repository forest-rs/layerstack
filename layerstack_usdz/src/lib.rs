// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDZ (packaged scene) reader and writer for layerstack.
//!
//! Reads USDZ package files per AOUSD Core §16.4 and produces [`Layer`] /
//! [`PrimSpec`] structures compatible with the layerstack composition engine.
//! [`write_usdz`] packages already-serialized layers and media into a
//! conforming archive (see [`writer`]).
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
//! both file reads and memory-mapped I/O.
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
pub mod error;
mod resolver;
pub mod writer;
pub mod zip;

pub use error::UsdzError;
pub use writer::{PackageFile, UsdzWriteError, write_usdz};

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
}

/// Reads a USDZ package from a byte slice and produces a [`Layer`].
///
/// This is the main entry point for the crate. It runs the full pipeline:
/// ZIP parse → USDZ constraint validation → CRC-32 verification → root
/// layer format dispatch → assembly.
///
/// `data` must contain the complete USDZ file contents.
///
/// The `resolver` is used for asset paths that escape the package (i.e.,
/// paths not found among the archive entries). Internal references are
/// resolved within the package automatically.
///
/// Every layer read from or through the package goes into one store, so
/// `resolver` owns their IDs: `layer_id`, which the caller allocates from
/// it, names the root layer, and every other member loaded gets an ID from
/// [`AssetResolver::allocate_layer_id`]. A package whose root layer loads
/// another member needs a resolver that allocates
/// ([`UsdzError::LayerIdUnavailable`]), and a layer ID returned twice
/// fails the read ([`UsdzError::DuplicateLayerId`]).
///
/// Spec: AOUSD Core §16.4.
///
/// ```
/// use layerstack::{
///     AssetResolveError, AssetResolver, LayerId, ResolvedAsset,
///     TokenInterner, PathInterner,
/// };
/// use layerstack_usdz::{read_usdz, UsdzError};
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
/// // Invalid data fails at the ZIP parsing stage.
/// let result = read_usdz(
///     b"not a zip",
///     LayerId(1),
///     &mut TokenInterner::default(),
///     &mut PathInterner::default(),
///     &mut NoAssets,
/// );
/// assert!(result.is_err());
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

    // 4. Create a package-scoped resolver. The outer resolver allocates the
    //    layer IDs of the other members.
    let mut usdz_resolver = resolver::UsdzResolver::new(&archive, resolver);

    // 5. Parse the root layer.
    let root_data = archive.entry_data(root_entry);
    let parsed = resolver::parse_layer_data(
        root_data,
        &root_entry.name,
        layer_id,
        tokens,
        paths,
        &mut usdz_resolver,
    )?;

    // 6. Collect every resolved layer once: those the root layer resolved,
    //    and those resolved beneath them.
    let loaded = usdz_resolver.finish()?;
    let mut resolved_layers = parsed.resolved_layers;
    resolved_layers.extend(loaded.descendants);
    let mut member_paths = loaded.member_paths;
    member_paths.insert(layer_id, root_entry.name.clone());

    // 7. Installing two layers with one ID would silently drop one, so an
    //    outer resolver that handed out an ID twice fails the read.
    let mut ids = BTreeSet::from([layer_id]);
    if let Some(layer) = resolved_layers.iter().find(|layer| !ids.insert(layer.id)) {
        return Err(UsdzError::DuplicateLayerId { id: layer.id });
    }

    Ok(UsdzResult {
        layer: parsed.layer,
        resolved_layers,
        member_paths,
    })
}

/// Returns `true` if the file extension indicates a USD layer.
fn is_usd_extension(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("");
    matches!(ext, "usd" | "usda" | "usdc")
}
