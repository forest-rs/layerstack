//! USDZ (packaged scene) reader for layerstack.
//!
//! Reads USDZ package files per AOUSD Core §16.4 and produces [`Layer`] /
//! [`PrimSpec`] structures compatible with the layerstack composition engine.
//!
//! USDZ is a constrained ZIP archive containing USD layers and associated
//! media (textures, audio). Constraints (§16.4.1):
//! - All entries are uncompressed (Stored)
//! - No encryption
//! - 32-bit ZIP only (no Zip64)
//! - Local file header offsets are 64-byte aligned
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
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

extern crate alloc;

use alloc::vec::Vec;

use layerstack::AssetResolver;
use layerstack::doc::{Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;

pub mod crc32;
pub mod error;
mod resolver;
pub mod zip;

pub use error::UsdzError;

/// The result of successfully reading a USDZ package.
#[derive(Clone, Debug)]
pub struct UsdzResult {
    /// The root layer assembled from the first USD file in the package.
    pub layer: Layer,
    /// Additional layers produced by resolving asset paths within the
    /// package (sublayers, references, payloads). The caller should insert
    /// these into their store.
    pub resolved_layers: Vec<Layer>,
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
/// Spec: AOUSD Core §16.4.
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

    // 4. Create a package-scoped resolver.
    //    Layer IDs within the package start after the root layer's ID.
    let mut usdz_resolver = resolver::UsdzResolver::new(&archive, layer_id.0 + 1, resolver);

    // 5. Parse the root layer.
    let root_data = archive.entry_data(root_entry);
    let layer = resolver::parse_layer_data(
        root_data,
        &root_entry.name,
        layer_id,
        tokens,
        paths,
        &mut usdz_resolver,
    )?;

    // 6. Collect all resolved layers.
    let resolved_layers = usdz_resolver.pending_layers;

    Ok(UsdzResult {
        layer,
        resolved_layers,
    })
}

/// Returns `true` if the file extension indicates a USD layer.
fn is_usd_extension(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("");
    matches!(ext, "usd" | "usda" | "usdc")
}
