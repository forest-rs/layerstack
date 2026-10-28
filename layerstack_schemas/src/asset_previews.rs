// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An asset's default thumbnails, read and authored as OpenUSD's
//! `UsdMediaAssetPreviewsAPI` keeps them: the `defaultImage` asset of the
//! `assetInfo` dictionary entry `previews:thumbnails:default`.
//!
//! [`asset_default_previews`] reads them from a layer's default prim
//! without composing the rest of the asset, for asset browsers and pickers.

use crate::{
    MetadataError, PrimView, Scene, SchemaEdit,
    assets::AssetReference,
    usd_media::{AssetPreviewsApi, AssetPreviewsApiEdit},
};
use alloc::{string::String, sync::Arc, vec, vec::Vec};
use layerstack::{
    FieldValue, LayerId, LayerStore, OpinionValue, Path, PathId, PopulationMask, Provenance,
    SchemaRegistry, Stage, StageOptions, Value,
};

const ASSET_INFO: &str = "assetInfo";
const DEFAULT_THUMBNAILS: &str = "previews:thumbnails:default";
const DEFAULT_IMAGE: &str = "defaultImage";

/// A prim's default thumbnails.
///
/// OpenUSD: `UsdMediaAssetPreviewsAPI::Thumbnails`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Thumbnails {
    /// The `defaultImage` asset, with the layer whose `assetInfo` opinion
    /// authors it, to anchor it by ([`AssetReference::anchor`]).
    pub default_image: AssetReference,
}

impl AssetPreviewsApi<'_> {
    /// The prim's default thumbnails: the composed `assetInfo` entry
    /// `previews:thumbnails:default:defaultImage`, when the prim has the
    /// schema applied and the entry is an asset path (a `string` reads as
    /// none). The entry's source is the strongest opinion that authors it,
    /// so a weaker layer's thumbnail anchors at that layer.
    ///
    /// OpenUSD: `UsdMediaAssetPreviewsAPI::GetDefaultThumbnails`.
    #[doc(alias = "GetDefaultThumbnails")]
    #[must_use]
    pub fn default_thumbnails(&self) -> Option<Thumbnails> {
        let scene = self.scene();
        if !scene.has_api(self.path(), Self::SCHEMA, None) {
            return None;
        }
        let key_path = [DEFAULT_THUMBNAILS, DEFAULT_IMAGE].join(":");
        let Value::Asset(path) = self.metadata_by_key(ASSET_INFO, &key_path)? else {
            return None;
        };
        let source = self.prim().and_then(|prim| {
            prim.metadata_opinions(ASSET_INFO)
                .iter()
                .find_map(|opinion| {
                    let OpinionValue::Field(FieldValue::Value(dictionary)) = opinion.value() else {
                        return None;
                    };
                    crate::metadata::dictionary_entry(dictionary, &key_path)?;
                    Some(Provenance {
                        layer: opinion.layer_id(),
                        spec_path: opinion.spec_path(),
                        field: opinion.field(),
                    })
                })
        });
        Some(Thumbnails {
            default_image: AssetReference {
                asset_path: String::from(&*path),
                source,
            },
        })
    }
}

impl AssetPreviewsApiEdit {
    /// Authors `default_image` as the prim's default thumbnail: the edit
    /// target's `assetInfo` entry `previews:thumbnails:default` becomes the
    /// dictionary `{defaultImage: default_image}` as a whole, so a key a
    /// weaker layer authors beside it still composes in. The schema need not
    /// be applied, as in C++.
    ///
    /// OpenUSD: `UsdMediaAssetPreviewsAPI::SetDefaultThumbnails`.
    ///
    /// # Errors
    ///
    /// As [`crate::PrimEdit::write_metadata_by_key`]: no prim, or one that
    /// cannot be edited; nothing is authored.
    #[doc(alias = "SetDefaultThumbnails")]
    pub fn set_default_thumbnails(
        &self,
        edit: &mut SchemaEdit<'_>,
        default_image: &str,
    ) -> Result<&Self, MetadataError> {
        let thumbnails = Value::Dictionary(vec![(
            Arc::from(DEFAULT_IMAGE),
            Value::Asset(Arc::from(default_image)),
        )]);
        self.write_metadata_by_key(edit, ASSET_INFO, DEFAULT_THUMBNAILS, thumbnails)?;
        Ok(self)
    }

    /// Erases the edit target's `previews:thumbnails:default` entry of
    /// `assetInfo`, so a weaker layer's thumbnail shows through; an
    /// `assetInfo` left empty is cleared.
    ///
    /// OpenUSD: `UsdMediaAssetPreviewsAPI::ClearDefaultThumbnails`.
    ///
    /// # Errors
    ///
    /// As [`Self::set_default_thumbnails`].
    #[doc(alias = "ClearDefaultThumbnails")]
    pub fn clear_default_thumbnails(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<&Self, MetadataError> {
        self.clear_metadata_by_key(edit, ASSET_INFO, DEFAULT_THUMBNAILS)?;
        Ok(self)
    }
}

/// A layer's default prim on a stage composed only that deep, from
/// [`asset_default_previews`].
#[derive(Debug)]
pub struct AssetDefaultPreviews {
    /// The stage, masked to the default prim: its children, references
    /// and payloads below it are not composed.
    pub stage: Stage,
    /// The layer's default prim.
    pub prim: PathId,
}

impl AssetDefaultPreviews {
    /// The default prim's thumbnails ([`AssetPreviewsApi::default_thumbnails`]).
    #[must_use]
    pub fn default_thumbnails(&self, store: &dyn LayerStore) -> Option<Thumbnails> {
        let scene = Scene::new(&self.stage, store);
        AssetPreviewsApi::from_prim_unchecked(PrimView::new(scene, self.prim)).default_thumbnails()
    }
}

/// The default previews of the asset whose root layer is `layer`: its
/// default prim (`defaultPrim`, a root prim name or an absolute prim path)
/// on a stage masked to `<defaultPrim>/__No_Such_Prim__`, so only the
/// default prim and its ancestors are populated, not the asset's contents.
/// `None` when the layer is not in `store` or names no default prim.
///
/// `schemas` must be built on `store`'s tokens, as for any stage.
///
/// OpenUSD: `UsdMediaAssetPreviewsAPI::GetAssetDefaultPreviews`.
#[doc(alias = "GetAssetDefaultPreviews")]
pub fn asset_default_previews(
    store: &mut dyn LayerStore,
    layer: LayerId,
    schemas: Arc<SchemaRegistry>,
) -> Option<AssetDefaultPreviews> {
    let name = store.layer(layer)?.default_prim()?;
    let text = String::from(store.tokens().resolve(name));
    let path = if text.starts_with('/') {
        Path::parse_absolute(&text, store.tokens()).ok()?
    } else {
        Path::root().join(&[name])
    };
    if path.depth() == 0 {
        return None;
    }
    let no_such_prim = store.tokens().intern("__No_Such_Prim__");
    let mask = store.paths().intern(path.join(&[no_such_prim]));
    let prim = store.paths().intern(path);
    let stage = Stage::compose(
        store,
        layer,
        StageOptions {
            mask: Some(PopulationMask {
                include: Vec::from([mask]),
            }),
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    Some(AssetDefaultPreviews { stage, prim })
}
