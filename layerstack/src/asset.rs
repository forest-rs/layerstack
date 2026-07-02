// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset resolution: mapping asset path URIs to layers.
//!
//! Asset paths appear in references, payloads, and sublayer includes. Before
//! composition can use them, they must be resolved to concrete [`LayerId`]s
//! with their layer data loaded into a [`LayerStore`](crate::doc::LayerStore).
//!
//! The [`AssetResolver`] trait defines this mapping. Implementations handle
//! the full resolution pipeline described in AOUSD Core §9:
//!
//! - Protocol handling (§9.3): interpreting URI schemes
//! - Relative path resolution (§9.4): resolving paths relative to an anchor
//! - Search path resolution (§9.5): searching configured directories
//! - Extension resolution (§9.6): probing `.usda` / `.usdc` / `.usd`
//! - Package resolution (§9.7): locating assets within USDZ archives
//!
//! # Usage pattern
//!
//! ```ignore
//! // 1. Build a resolver (implementation-specific).
//! let mut resolver = MyResolver::new(&["assets/", "shared/"]);
//!
//! // 2. Resolve asset paths — the resolver returns the layer.
//! let resolved = resolver.resolve(
//!     "props/robot.usda",
//!     Some(root_layer),
//!     &mut store.tokens,
//!     &mut store.paths,
//! )?;
//!
//! // 3. Insert the layer into your store (if it's new).
//! if let Some(layer) = resolved.layer {
//!     store.insert_layer(layer);
//! }
//!
//! // 4. Build the reference with the resolved LayerId.
//! let reference = Reference {
//!     layer: resolved.layer_id,
//!     prim_path: target_path,
//!     asset: Some("props/robot.usda".into()),
//! };
//! ```

use alloc::sync::Arc;

use crate::{
    doc::{Layer, LayerId},
    interner::TokenInterner,
    path::PathInterner,
};

/// The result of successfully resolving an asset path.
///
/// On the first resolution of a given path, `layer` is `Some` and the caller
/// should insert it into their [`LayerStore`](crate::doc::LayerStore). On
/// subsequent resolutions of the same path (deduplication), `layer` is `None`.
#[derive(Clone, Debug)]
pub struct ResolvedAsset {
    /// The layer ID assigned to the resolved asset.
    pub layer_id: LayerId,
    /// The canonical resolved path, after search path and extension probing.
    ///
    /// This may differ from the input path (e.g., `"robot.usd"` might resolve
    /// to `"/assets/props/robot.usdc"`).
    pub resolved_path: Arc<str>,
    /// The resolved layer data, or `None` if this was a cache hit.
    ///
    /// When `Some`, the caller must insert this layer into their store before
    /// composition. When `None`, the layer was already returned by a previous
    /// call to [`AssetResolver::resolve`] for the same asset path.
    pub layer: Option<Layer>,
}

/// Reasons an asset path could not be resolved.
///
/// Spec: AOUSD Core §9.2 (asset identifiers), §9.5 (search path failure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetResolveError {
    /// The asset path could not be found after exhausting all search paths
    /// and extension probing.
    NotFound,
    /// The asset was located but could not be loaded (parse error, I/O
    /// failure, unsupported format, etc.).
    LoadError(Arc<str>),
}

/// Resolves asset path strings to layers.
///
/// This trait is the standard interface for integrating external asset sources
/// (filesystems, databases, network services, USDZ packages) with
/// layerstack's composition pipeline. The resolver is responsible for:
///
/// 1. Locating the asset (applying search paths, extension probing, etc.)
/// 2. Loading and parsing the layer data
/// 3. Assigning a [`LayerId`] and returning the [`Layer`]
/// 4. Deduplicating: repeated resolution of the same path returns the same
///    [`LayerId`] with `layer: None`
///
/// The caller is responsible for inserting returned layers into their
/// [`LayerStore`](crate::doc::LayerStore).
///
/// Spec: AOUSD Core §9 (asset resolution).
pub trait AssetResolver {
    /// Resolves an asset path, returning the layer data.
    ///
    /// The resolver uses the provided interners to build paths and tokens
    /// within the resolved layer, ensuring they share the same interning
    /// namespace as the rest of the scene.
    ///
    /// # Parameters
    ///
    /// - `asset_path`: the raw URI string from a reference, payload, or
    ///   sublayer include (e.g., `"props/robot.usda"`, `"./local.usda"`).
    /// - `anchor`: the [`LayerId`] of the layer containing the arc that
    ///   references this asset. Used for relative path resolution (§9.4).
    ///   `None` for top-level / root resolution.
    /// - `tokens`: shared token interner for the scene.
    /// - `paths`: shared path interner for the scene.
    ///
    /// # Returns
    ///
    /// A [`ResolvedAsset`] on success. If `resolved.layer` is `Some`, the
    /// caller must insert it into their store. If `None`, the layer was
    /// already returned by a previous call (deduplication hit).
    fn resolve(
        &mut self,
        asset_path: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError>;

    /// Returns the canonical resolved path for a previously resolved layer.
    ///
    /// Returns `None` if `id` was not produced by this resolver.
    fn resolved_path(&self, id: LayerId) -> Option<&str>;
}
