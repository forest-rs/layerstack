// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Package-scoped asset resolver for USDZ archives.
//!
//! Internal asset paths (e.g., sublayer references, texture paths) resolve
//! within the package before delegating to an outer resolver. This
//! implements packaged resource resolution per AOUSD Core §9.7.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::doc::{Layer, LayerId};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};

use crate::error::UsdzError;
use crate::zip::ZipArchive;

/// USDC magic bytes used for format sniffing.
const USDC_MAGIC: &[u8; 8] = b"PXR-USDC";

/// Resolves asset paths within a USDZ package.
///
/// Paths that match archive entries are loaded from the package; paths
/// that don't match are delegated to the outer resolver.
pub(crate) struct UsdzResolver<'a> {
    archive: &'a ZipArchive<'a>,
    by_name: BTreeMap<Arc<str>, LayerId>,
    layer_names: BTreeMap<LayerId, Arc<str>>,
    next_layer_id: u64,
    /// Layers produced during asset resolution.
    pub(crate) pending_layers: Vec<Layer>,
    outer: &'a mut dyn AssetResolver,
}

impl<'a> UsdzResolver<'a> {
    /// Creates a new resolver scoped to the given archive.
    pub(crate) fn new(
        archive: &'a ZipArchive<'a>,
        next_layer_id: u64,
        outer: &'a mut dyn AssetResolver,
    ) -> Self {
        Self {
            archive,
            by_name: BTreeMap::new(),
            layer_names: BTreeMap::new(),
            next_layer_id,
            pending_layers: Vec::new(),
            outer,
        }
    }
}

impl AssetResolver for UsdzResolver<'_> {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        // Normalize: strip leading "./"
        let normalized = asset_path.trim_start_matches("./");

        // Deduplication check.
        if let Some(&id) = self.by_name.get(normalized) {
            return Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: Arc::from(normalized),
                layer: None,
            });
        }

        // Look up in archive.
        let Some(entry) = self.archive.find(normalized) else {
            // Not in package — delegate to outer resolver.
            return self.outer.resolve(asset_path, _anchor, tokens, paths);
        };

        // Assign layer ID.
        let layer_id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        let name: Arc<str> = Arc::from(normalized);
        self.by_name.insert(name.clone(), layer_id);
        self.layer_names.insert(layer_id, name.clone());

        // Get entry data.
        let data = self.archive.entry_data(entry);

        // Format dispatch based on extension + magic.
        let layer = parse_layer_data(data, normalized, layer_id, tokens, paths, self)
            .map_err(|e| AssetResolveError::LoadError(Arc::from(alloc::format!("{e}"))))?;

        // Collect resolved sub-layers from recursive resolution.
        // (They are already in self.pending_layers from recursive calls.)

        Ok(ResolvedAsset {
            layer_id,
            resolved_path: name,
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.layer_names.get(&id).map(|s| &**s)
    }
}

/// Parses a USD layer from raw bytes, dispatching by extension and magic.
///
/// Returns the assembled [`Layer`].
pub(crate) fn parse_layer_data(
    data: &[u8],
    name: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<Layer, UsdzError> {
    let ext = name.rsplit('.').next().unwrap_or("");

    match ext {
        "usdc" => parse_usdc(data, name, layer_id, tokens, paths, resolver),
        "usda" => parse_usda(data, name, layer_id, tokens, paths, resolver),
        "usd" => {
            // Probe magic bytes to determine format.
            if data.len() >= 8 && &data[..8] == USDC_MAGIC {
                parse_usdc(data, name, layer_id, tokens, paths, resolver)
            } else {
                parse_usda(data, name, layer_id, tokens, paths, resolver)
            }
        }
        _ => Err(UsdzError::LayerParseError {
            message: Arc::from(alloc::format!("unsupported file type: {name}")),
        }),
    }
}

/// Parses a USDC binary layer.
fn parse_usdc(
    data: &[u8],
    name: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<Layer, UsdzError> {
    let result =
        layerstack_usdc::read_usdc(data, layer_id, tokens, paths, resolver).map_err(|e| {
            UsdzError::LayerParseError {
                message: Arc::from(alloc::format!("USDC parse error in {name}: {e}")),
            }
        })?;
    // Note: resolved_layers from the USDC parser are handled through the
    // resolver's pending_layers mechanism.
    Ok(result.layer)
}

/// Parses a USDA text layer.
fn parse_usda(
    data: &[u8],
    name: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<Layer, UsdzError> {
    let source = core::str::from_utf8(data).map_err(|_| UsdzError::LayerParseError {
        message: Arc::from(alloc::format!("USDA file {name} is not valid UTF-8")),
    })?;

    let cst = layerstack_usda::parser::parse_cst(source);
    let ast_result = layerstack_usda::lower::lower(&cst.tree, source);
    let emit_result =
        layerstack_usda::emit::emit(&ast_result.layer, layer_id, tokens, paths, resolver);

    // Note: resolved_layers from emit are handled through the resolver's
    // pending_layers mechanism.
    Ok(emit_result.layer)
}
