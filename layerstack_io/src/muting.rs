// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Muting a document's layers, by id or by identifier, as OpenUSD's stage
//! mutes them: a layer may be muted before anything reads it, and an arc
//! to it then composes muted without reading it.
//!
//! The live stage mutes layer ids ([`layerstack::LiveStage::mute_and_unmute_layers`]).
//! An identifier names the id of the layer read from it, or, for one the
//! document has not read, the id it reserves for it: the id a later read
//! of that identifier takes. The reads of a document skip a muted layer
//! they have not read (`Reader::unread_muted`) and bind the assets naming
//! it to its id, so their arcs compose muted
//! ([`layerstack::CompositionError::MutedAsset`]); unmuting it forgets
//! those bindings, and the stage asks for the layer again.
//!
//! Spec: AOUSD Core composes the layer stacks (§10.3.1) of the layers it
//! is given and has no muting; this follows OpenUSD's
//! `UsdStage::MuteAndUnmuteLayers` and `Pcp_MutedLayers`
//! (`pxr/usd/pcp/layerStackRegistry.cpp`).

use alloc::{string::String, sync::Arc, vec::Vec};

use layerstack::{
    LayerId, LayerMuteError, LayerStore,
    asset::{FormatArgs, PackagePath},
    sort,
};

use crate::{
    Format, IoError, IoErrorKind, Source, StageDocument, Storage, format, source_identifier,
};

impl<B: Storage> StageDocument<B> {
    /// The identifier `identifier` names a layer by once anchored to the
    /// root layer, as OpenUSD anchors a muted identifier
    /// (`Pcp_MutedLayers::_GetCanonicalLayerId` creates its identifier
    /// relative to the root layer's): `./lamp.usda`, `lamp.usda` and
    /// `parts/../lamp.usda` beside a root `set/scene.usda` all name
    /// `set/lamp.usda` with storage that anchors as the browser bindings'
    /// does. It is the identifier an arc authored on the root to the same
    /// asset path resolves to ([`Storage::identify`]), so an arc anchored
    /// to another layer names the same layer when it resolves to the same
    /// identifier, as in OpenUSD, which anchors the asset path of an arc to
    /// the layer authoring it before it checks muting.
    ///
    /// # Errors
    ///
    /// The [`IoError`] [`Storage::identify`] returns for an identifier it
    /// cannot anchor, and [`IoErrorKind::Rejected`] for a malformed
    /// package path or file format arguments.
    pub fn anchored_identifier(&self, identifier: &str) -> Result<String, IoError> {
        let anchor = self.identifier(self.root_layer()).map(|root| {
            let root = FormatArgs::strip(root);
            String::from(root.split_once('[').map_or(root, |(outer, _)| outer))
        });
        let (asset, args) = format::split_args(identifier)?;
        let path = PackagePath::parse(asset, 32)
            .map_err(|reason| IoError::new(IoErrorKind::Rejected, reason).at(identifier))?;
        let outer = self
            .storage
            .identify(path.outer(), anchor.as_deref())
            .map_err(|error| error.at(identifier))?;
        Ok(source_identifier(&self.formats, &path, &outer, args))
    }

    /// The layer `identifier`, anchored to the root layer
    /// ([`Self::anchored_identifier`]), names, if the document has read it
    /// or reserved its id ([`Self::layer_named`]).
    ///
    /// # Errors
    ///
    /// As [`Self::anchored_identifier`].
    pub fn find_layer_named(&self, identifier: &str) -> Result<Option<LayerId>, IoError> {
        let anchored = self.anchored_identifier(identifier)?;
        Ok(self.catalog.ids.get(anchored.as_str()).copied())
    }

    /// The id of the layer `identifier`, anchored to the root layer
    /// ([`Self::anchored_identifier`]), names: the id of the layer read
    /// from it, or for a layer the document has not read, the id it
    /// reserves for it now, which a later read of that identifier takes.
    /// Pass it to [`Self::mute_and_unmute_layers`] to mute a layer before
    /// anything reads it, as `UsdStage::MuteLayer` mutes an identifier.
    /// Reserving reads nothing, and the store holds no layer for the id
    /// until a read of it, as OpenUSD opens no layer for a muted
    /// identifier (`SdfLayer::Find` gives none); it names the id by
    /// `identifier` ([`layerstack::LayerStore::layer_identifier`]), so
    /// diagnostics write `@lamp.usda@` rather than the id.
    ///
    /// # Errors
    ///
    /// As [`Self::anchored_identifier`], and
    /// [`IoErrorKind::IdentityExhausted`] when no id is left.
    pub fn layer_named(&mut self, identifier: &str) -> Result<LayerId, IoError> {
        let anchored = self.anchored_identifier(identifier)?;
        if let Some(&id) = self.catalog.ids.get(anchored.as_str()) {
            // An asset the stage asked for has an id but no layer yet.
            if self.store.layer_identifier(id).is_none() {
                self.store.set_layer_identifier(id, anchored);
            }
            return Ok(id);
        }
        self.catalog.reserved.extend(self.store.layer_ids());
        let anchored: Arc<str> = Arc::from(anchored);
        let id = self.catalog.allocate_for(&anchored)?;
        self.catalog.ids.insert(Arc::clone(&anchored), id);
        // Diagnostics name the layer by its identifier before any read, as
        // OpenUSD's `PcpErrorMutedAssetPath` names the muted asset.
        self.store.set_layer_identifier(id, Arc::clone(&anchored));
        self.catalog.insert_source(
            id,
            Source {
                identifier: anchored,
                // A read finds the format; nothing reads it before then.
                format: Format::Usda,
                package: None,
            },
        );
        Ok(id)
    }

    /// Mutes the layers of `mute`, then unmutes those of `unmute`, as one
    /// request, and recomposes once: `UsdStage::MuteAndUnmuteLayers`.
    /// Returns whether the muted set changed. A layer in both stays muted,
    /// as OpenUSD drops an unmute request a mute request repeats
    /// ([`layerstack::LiveStage::mute_and_unmute_layers`]).
    ///
    /// A layer may be muted before the document reads it
    /// ([`Self::layer_named`]): the document then does not read it, an
    /// arc or sublayer naming it composes muted
    /// ([`layerstack::CompositionError::MutedAsset`] for an arc), and an
    /// asset the stage asks for that storage could not supply yet
    /// ([`Self::unavailable_assets`]) that names it is no longer asked
    /// for. Unmuting a layer the document has not read asks for it again:
    /// [`Self::load_requested_assets`] reads it, as OpenUSD opens an
    /// unmuted layer when it recomposes.
    ///
    /// # Errors
    ///
    /// [`LayerMuteError::RootLayer`] when `mute` names the root layer;
    /// nothing changes.
    #[doc(alias = "UsdStage::MuteAndUnmuteLayers")]
    pub fn mute_and_unmute_layers(
        &mut self,
        mute: &[LayerId],
        unmute: &[LayerId],
    ) -> Result<bool, LayerMuteError> {
        let changed = self.stage.mute_and_unmute_layers(mute, unmute)?;
        let muted = &self.stage.options().muted_layers;
        // An unmuted layer not read: the assets bound to its id ask again.
        for &layer in unmute {
            if !muted.contains(&layer) && self.store.layer(layer).is_none() {
                self.store.forget_assets_bound_to(layer);
            }
        }
        // A muted layer storage could not supply: bound, so its arcs
        // compose muted, and no longer asked for.
        let bound: Vec<((LayerId, String), LayerId)> = self
            .unavailable
            .iter()
            .filter_map(|(key, identifier)| {
                let id = *self.catalog.ids.get(identifier.as_str())?;
                (muted.contains(&id) && self.store.layer(id).is_none()).then(|| (key.clone(), id))
            })
            .collect();
        for ((anchor, asset), id) in bound {
            self.unavailable.remove(&(anchor, asset.clone()));
            self.store.insert_asset_layer(anchor, &asset, id);
        }
        self.synchronize();
        Ok(changed)
    }

    /// The identifiers of the layers the stage mutes, sorted, as
    /// `UsdStage::GetMutedLayers` lists them: each layer's identifier
    /// ([`Self::identifier`]), whether the document has read it or only
    /// reserved its id ([`Self::layer_named`]). A muted layer with no
    /// identifier, an anonymous one, is not listed;
    /// [`layerstack::Stage::muted_layers`] lists every muted id.
    #[must_use]
    #[doc(alias = "UsdStage::GetMutedLayers")]
    pub fn muted_identifiers(&self) -> Vec<&str> {
        let mut identifiers: Vec<&str> = self
            .stage
            .options()
            .muted_layers
            .iter()
            .filter_map(|&id| self.identifier(id))
            .collect();
        sort::sort_unstable(&mut identifiers);
        identifiers
    }
}
