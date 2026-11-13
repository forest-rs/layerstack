// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Releasing the layers a document's stage no longer uses, so their memory
//! comes back and a later demand reads their files again.
//!
//! OpenUSD frees a layer once nothing holds it: `SdfLayer`'s registry holds
//! layers weakly (`pxr/usd/sdf/layerRegistry.h`), a stage holds its layer
//! stacks through its prim indexes (`pxr/usd/pcp/primIndex_Graph.h`), and a
//! layer stack holds its layers (`pxr/usd/pcp/layerStack.h`). Unloading a
//! payload, muting a layer or removing the arc or sublayer naming it drops
//! the last holder, and loading, unmuting or authoring it again opens the
//! file anew. A document keeps every layer it read until the host calls
//! [`StageDocument::release_unused_layers`]: releasing is explicit, and
//! returns what it freed.
//!
//! Spec: AOUSD Core does not define layer lifetime; composition (§10.3)
//! reads the layers its arcs name. This follows OpenUSD's observable
//! behaviour, except that a layer with unsaved edits is kept and reported
//! where OpenUSD frees it and loses them.

use super::*;

/// What [`StageDocument::release_unused_layers`] released and kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReleasedLayers {
    /// The layers released, in id order.
    pub released: Vec<ReleasedLayer>,
    /// The layers nothing uses that were kept because they hold edits not
    /// saved, anonymous layers among them, in id order. Save them
    /// ([`StageDocument::save_layers`]) or discard their edits
    /// ([`StageDocument::reload_layers`] with
    /// [`crate::ReloadPolicy::DiscardDirty`]), and a later release frees them.
    pub kept_dirty: Vec<LayerId>,
}

impl ReleasedLayers {
    /// Whether the call released nothing and kept no dirty layer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.released.is_empty() && self.kept_dirty.is_empty()
    }

    /// The encoded bytes the released layers were read from, summed over
    /// those whose size is known ([`ReleasedLayer::source_bytes`]).
    #[must_use]
    pub fn source_bytes(&self) -> u64 {
        self.released
            .iter()
            .filter_map(|layer| layer.source_bytes)
            .sum()
    }
}

/// One layer [`StageDocument::release_unused_layers`] released.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReleasedLayer {
    /// The layer's id, which a later read of [`Self::identifier`] takes
    /// again.
    pub layer: LayerId,
    /// The identifier it was read from ([`StageDocument::identifier`]).
    pub identifier: String,
    /// The encoded bytes it was read from; `None` when the document did
    /// not read it, such as a layer the host inserted. The memory the
    /// layer held is larger: decoded specs, values and indexes.
    pub source_bytes: Option<u64>,
    /// Whether the stage mutes it: arcs and sublayers naming it stay bound
    /// to its id and compose muted, and unmuting it reads it again.
    pub muted: bool,
}

impl<B: Storage> StageDocument<B> {
    /// Releases the clean layers the stage does not use, and reports them
    /// and the unused dirty layers it kept. A layer is used when it is
    ///
    /// - the root or session layer, or in their layer stacks
    ///   ([`layerstack::Stage::layer_stack`]);
    /// - in a layer stack composition reaches, a loaded payload's included
    ///   ([`layerstack::Stage::used_layers`]);
    /// - a value-clip layer the composed clips hold for queries
    ///   ([`Self::prepare_clip_query`]);
    /// - in `keep`: what the host holds, such as the layer of an edit
    ///   target it will author to, a layer it shows, or the layers another
    ///   stage composing this store uses. A package's layers are released
    ///   together, so a package is kept whole while any of its layers is.
    ///
    /// A layer with no source to read it from again, such as an anonymous
    /// layer not yet edited, is kept too, and not reported.
    ///
    /// The stage composes first ([`Self::synchronize`]), so a layer read
    /// and not yet composed counts as composition will use it. Nothing
    /// composed changes and no notice follows: a released layer
    /// contributed nothing.
    ///
    /// A released layer leaves the store with the bindings of its assets;
    /// the identifier keeps its id ([`Self::layer_named`]), and storage
    /// may drop the source's bytes ([`Storage::release_source`]). When the stage
    /// next asks for it (a payload loaded again, a sublayer or arc
    /// authored again), it reads the file again through storage as a new
    /// layer, and the stage hears it as any layer newly read. A muted
    /// layer released stays muted, as one muted before it was read: arcs
    /// naming it compose muted, and unmuting reads it.
    ///
    /// OpenUSD frees such a layer once its last holder goes, with any
    /// unsaved edits. This keeps an unused dirty layer instead, and lists
    /// it in [`ReleasedLayers::kept_dirty`]: a host that wants OpenUSD's
    /// behaviour discards those edits and calls this again.
    ///
    /// OpenUSD: `SdfLayer` lifetime under `UsdStage::Unload`,
    /// `UsdStage::MuteLayer` and sublayer or arc removal
    /// (`pxr/usd/pcp/layerStackRegistry.cpp` holds layer stacks weakly).
    #[doc(alias = "SdfLayer::Find")]
    pub fn release_unused_layers(&mut self, keep: &[LayerId]) -> ReleasedLayers {
        self.synchronize();
        let stage = self.stage.stage();
        let mut held = stage.used_layers(true);
        held.extend(stage.layer_stack().iter().copied());
        held.extend(stage.root_layer());
        held.extend(stage.session_layer());
        held.extend(keep.iter().copied());
        let muted = &self.stage.options().muted_layers;
        // A package's layers go together: held or dirty, one keeps all.
        let group = |id: LayerId| {
            self.catalog
                .source(id)
                .and_then(|s| s.package)
                .unwrap_or(id)
        };
        let mut kept_groups = BTreeSet::new();
        let mut kept_dirty = Vec::new();
        let mut candidates = Vec::new();
        for id in self.store.layer_ids() {
            if held.contains(&id) {
                kept_groups.insert(group(id));
            } else if self.is_dirty(id) {
                kept_groups.insert(group(id));
                kept_dirty.push(id);
            } else if self.catalog.source(id).is_none() {
                // Nothing could read it again: an anonymous layer, clean
                // since it was created.
                kept_groups.insert(group(id));
            } else {
                candidates.push(id);
            }
        }
        let mut released = Vec::new();
        let mut ids = Vec::new();
        for id in candidates {
            if kept_groups.contains(&group(id)) {
                continue;
            }
            let record = self.catalog.record(id);
            released.push(ReleasedLayer {
                layer: id,
                identifier: self.identifier(id).map(String::from).unwrap_or_default(),
                source_bytes: record.and_then(|record| record.source_bytes),
                muted: muted.contains(&id),
            });
            ids.push(id);
        }
        let mut sources: Vec<String> = Vec::new();
        for layer in &released {
            if let Some(source) = self.catalog.source(layer.layer) {
                let identifier = FormatArgs::strip(&source.identifier);
                let outer = identifier
                    .split_once('[')
                    .map_or(identifier, |(outer, _)| outer);
                if !sources.iter().any(|known| known == outer) {
                    sources.push(String::from(outer));
                }
            }
        }
        // The layers drop their views of the bytes first.
        drop(self.store.release_layers(&ids));
        for source in &sources {
            self.storage.release_source(source);
        }
        for layer in &released {
            let id = layer.layer;
            // A muted layer's bindings stay, so arcs naming it compose
            // muted and unmuting asks for it again
            // (`StageDocument::mute_and_unmute_layers`).
            if !layer.muted {
                self.store.forget_unused_assets_bound_to(id);
            }
            self.catalog.clear_state(id);
            if let Some(record) = self.catalog.layers.get_mut(&id) {
                record.saved = None;
                record.package = None;
                record.package_root = None;
                record.source_bytes = None;
                record.released = true;
            }
            #[cfg(feature = "std")]
            self.catalog.retained_values.remove(&id);
        }
        let gone = |anchor: &LayerId| ids.binary_search(anchor).is_ok();
        self.catalog.failures.retain(|(anchor, _), _| !gone(anchor));
        self.unavailable.retain(|(anchor, _), _| !gone(anchor));
        self.waiting.retain(|(anchor, _), _| !gone(anchor));
        self.stage.forget_released_layers(&ids);
        ReleasedLayers {
            released,
            kept_dirty,
        }
    }
}

impl<B: Storage> StageDocument<B> {
    /// Reads the released layer `layer` again from `identifier`, its
    /// source, under its id, without composing: the arcs and sublayers
    /// bound to its id compose it at the next synchronization. Returns
    /// what it read, which the open's [`Self::load_report`] does not take;
    /// a failure is the caller's to fall back from.
    pub(crate) fn read_released(
        &mut self,
        layer: LayerId,
        identifier: &str,
    ) -> Result<LoadReport, IoError> {
        let mut catalog = self.catalog.clone();
        catalog.reserved.extend(self.store.layer_ids());
        let mut reader = Reader::new(
            &mut self.storage,
            &mut catalog,
            self.import_policy,
            self.usdc_options,
            self.package_limits,
            self.stage.options().schemas.clone(),
        );
        reader.variant_arcs = self.variant_arcs;
        reader.defers_payloads = excludes_payloads(&self.stage.options().load_rules);
        reader.formats = self.formats.clone();
        reader.log = ReadLog::new(self.clock.clone());
        reader.muted.clone_from(&self.stage.options().muted_layers);
        let resolved = reader.load(identifier, None, self.store.tokens(), self.store.paths())?;
        if resolved.layer_id != layer {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                "the released layer's source names another layer now",
            )
            .at(identifier));
        }
        if let Some(read) = resolved.layer {
            reader.pending.push(read);
        }
        reader.resolve_expressions(
            &mut self.store,
            layerstack::LayerStackIdentifier {
                root: self.stage.stage().root_layer().expect("document root"),
                session: self.stage.stage().session_layer(),
            },
        )?;
        let report = reader.commit(&mut self.store);
        self.catalog = catalog;
        Ok(report)
    }
}
