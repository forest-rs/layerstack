// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where a document's layers are, for anchoring the asset paths they
//! author, and the document's flatten, which anchors them as OpenUSD's
//! does.
//!
//! OpenUSD anchors an asset path through its resolver, which knows each
//! layer's resolved path (`SdfComputeAssetPathRelativeToLayer`, through
//! `ArResolver::CreateIdentifier`). A stage composes layers by id and knows
//! no locations, so the document, which read each layer from storage under
//! an identifier, is the resolver that does: [`LayerLocations`].

use alloc::string::String;

use layerstack::{
    AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, TokenInterner,
    asset::{FormatArgs, create_identifier},
    stage::flatten::{FlattenError, FlattenRequirements, Flattened},
};

use crate::{Catalog, IoError, StageDocument, Storage};

/// The locations of a document's layers, as an [`AssetResolver`] that
/// anchors asset paths and loads nothing
/// ([`StageDocument::layer_locations`]).
///
/// A layer's location is the identifier the document read it from
/// ([`StageDocument::identifier`]), without file format arguments; a layer
/// read from no identifier, such as an anonymous or host-owned layer, has
/// none, and a path relative to it (`./bark.png`) cannot be anchored.
///
/// [`AssetResolver::anchor_asset_path`] creates the identifier OpenUSD's
/// default resolver does ([`create_identifier`],
/// `ArDefaultResolver::_CreateIdentifier`): a path relative to its layer
/// is anchored to the layer's directory, and a search path
/// (`textures/bark.png`) is anchored there too when [`Storage::exists`]
/// finds the asset beside the layer, and kept otherwise, including when
/// the storage cannot say ([`IoErrorKind::Unsupported`], or an asset it
/// cannot supply yet, [`IoErrorKind::Unavailable`]). Inside a package, as
/// `SdfComputeAssetPathRelativeToLayer` anchors: a path relative to its
/// layer is anchored to the packaged layer (the package read as a layer
/// stands for its root layer); a search path is the member beside the
/// packaged layer, or else beside the package's root layer, which the
/// document's copy of the package answers, and otherwise is anchored to
/// the package file as a search path any other layer authors is.
///
/// [`AssetResolver::resolve`] reads nothing and fails with
/// [`AssetResolveError::Unavailable`]: the document loads layers itself
/// ([`StageDocument::load_requested_assets`]).
///
/// Spec: AOUSD Core §9.4 (relative asset paths), §9.5 (search paths).
///
/// [`IoErrorKind::Unsupported`]: crate::IoErrorKind::Unsupported
/// [`IoErrorKind::Unavailable`]: crate::IoErrorKind::Unavailable
pub struct LayerLocations<'a, B> {
    catalog: &'a Catalog,
    storage: &'a B,
}

impl<B> core::fmt::Debug for LayerLocations<'_, B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LayerLocations").finish_non_exhaustive()
    }
}

impl<B: Storage> AssetResolver for LayerLocations<'_, B> {
    fn resolve(
        &mut self,
        _asset_path: &str,
        _anchor: Option<LayerId>,
        _tokens: &TokenInterner,
        _paths: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::Unavailable)
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.catalog
            .sources
            .get(&id)
            .map(|source| FormatArgs::strip(&source.identifier))
    }

    fn anchor_asset_path(&self, asset_path: &str, anchor: LayerId) -> Option<String> {
        let location = self.resolved_path(anchor).unwrap_or("");
        let exists = |identifier: &str| self.storage.exists(identifier).is_ok_and(|exists| exists);
        let Some(package) = self.catalog.sources.get(&anchor).and_then(|s| s.package) else {
            return create_identifier(asset_path, location, exists);
        };
        let Some(archive) = self.catalog.package_bytes.get(&package) else {
            return create_identifier(asset_path, location, exists);
        };
        let outer = self.catalog.package_identifier(package);
        // `SdfComputeAssetPathRelativeToLayer` anchors a path inside the
        // package to the packaged layer, the package read as a layer
        // standing for its root layer.
        let packaged = match self.catalog.package_roots.get(&anchor) {
            Some(root) => alloc::format!("{outer}[{root}]"),
            None => String::from(location),
        };
        if !is_search_path(asset_path) {
            return create_identifier(asset_path, &packaged, exists);
        }
        // A search path is the member beside the packaged layer, or else
        // beside the package's root layer, as layer reads select it; then
        // anchored to the package file as any other layer's
        // (`ArSplitPackageRelativePathOuter`), or kept.
        match self
            .catalog
            .package_member(archive, asset_path, anchor, package)
        {
            Ok(Some(member)) => Some(alloc::format!("{outer}[{member}]")),
            _ => create_identifier(asset_path, outer, exists),
        }
    }
}

/// Whether `asset_path` is a search path: relative, but not to its layer
/// (`./`, `../`), nor an expression or a URI. OpenUSD looks for one
/// inside the package that authors it
/// (`SdfComputeAssetPathRelativeToLayer`).
fn is_search_path(asset_path: &str) -> bool {
    !asset_path.is_empty()
        && !asset_path.starts_with(['.', '/', '\\', '`'])
        && !asset_path.contains(':')
}

impl<B: Storage> StageDocument<B> {
    /// Where the document's layers are, as the resolver that anchors the
    /// asset paths they author as OpenUSD's default resolver does
    /// ([`LayerLocations`]): for [`layerstack::Stage::flatten`],
    /// [`layerstack::Stage::layer_metadata_anchored`] and any other read
    /// that anchors an asset path to its layer.
    #[must_use]
    pub fn layer_locations(&self) -> LayerLocations<'_, B> {
        LayerLocations {
            catalog: &self.catalog,
            storage: &self.storage,
        }
    }

    /// The composed stage flattened into one layer
    /// ([`layerstack::Stage::flatten`], OpenUSD's `UsdStage::Flatten`),
    /// meeting `requirements`, with its asset paths anchored through
    /// [`Self::layer_locations`] unless the requirements keep them as
    /// authored ([`AssetPaths::AsAuthored`]). The default requirements
    /// write the asset paths OpenUSD's flatten writes for a stage read
    /// from the same files.
    ///
    /// The flattened layer takes an id no layer of the document has or
    /// will take; it is not added to the document. Write it with
    /// [`Self::encode_layer`].
    ///
    /// # Errors
    ///
    /// [`FlattenFailure::Flatten`] as [`layerstack::Stage::flatten`]
    /// fails: with every unmet requirement, such as an asset path relative
    /// to a layer with no location, which a host may accept
    /// ([`FlattenRequirements::accepting`]); [`FlattenFailure::Io`] when
    /// the document has no free layer id
    /// ([`IoErrorKind::IdentityExhausted`]).
    ///
    /// Spec: AOUSD Core §11 (stage population), §12 (value resolution).
    ///
    /// [`AssetPaths::AsAuthored`]: layerstack::stage::flatten::AssetPaths::AsAuthored
    /// [`IoErrorKind::IdentityExhausted`]: crate::IoErrorKind::IdentityExhausted
    #[doc(alias = "UsdStage::Flatten")]
    #[doc(alias = "Flatten")]
    pub fn flatten(
        &mut self,
        requirements: &FlattenRequirements,
    ) -> Result<Flattened, FlattenFailure> {
        let id = self.catalog.allocate().map_err(FlattenFailure::Io)?;
        let root = self.root_layer();
        let locations = LayerLocations {
            catalog: &self.catalog,
            storage: &self.storage,
        };
        self.stage
            .stage()
            .flatten(&mut self.store, &locations, root, id, requirements)
            .map_err(FlattenFailure::Flatten)
    }
}

/// Why [`StageDocument::flatten`] returned no layer.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum FlattenFailure {
    /// The flatten was refused, or the root layer is missing.
    Flatten(FlattenError),
    /// The document has no free layer id for the flattened layer.
    Io(IoError),
}

impl core::fmt::Display for FlattenFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Flatten(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl core::error::Error for FlattenFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Flatten(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}
