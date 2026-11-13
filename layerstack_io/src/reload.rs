// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reloading layers from their sources: only the sources that changed are
//! read, and only what they changed is published.

use super::*;
use layerstack::Transaction;

/// A reload read and held apart from the published document until its
/// explicit commit.
///
/// Preparation reads the selected sources that changed since the document
/// read them (or that are dirty, or forced), with the layers they newly
/// reach, and retries the assets recorded missing; it publishes nothing.
/// Inspect [`Self::reloaded_layers`] and [`Self::load_report`], or compose
/// the scene the reload would publish with [`Self::candidate`], to validate
/// geometry and host budgets. Dropping this value rejects the reload: no
/// layer, source binding, dirty generation or notice is published. The
/// exclusive document borrow prevents edits, saves and other reloads
/// between preparation and commit. Source transport reads cannot be rolled
/// back, and the tokens and paths the read layers name are interned in the
/// document's interners, where they stay after a rejection (interned names
/// change no scene).
///
/// [`Self::commit`] publishes each layer read again as the per-spec
/// difference from its current content ([`Transaction::transfer_content`]),
/// in one transaction, so the stage's notices name only the specs that
/// changed, and those of a layer read again
/// ([`Transaction::mark_reloaded`]). A reload with nothing changed reads
/// no source whose storage reports versions ([`Storage::version`]) and
/// publishes nothing. This is a host-controlled extension of
/// `UsdStage::Reload`, which reads and publishes in one step.
#[must_use = "dropping a prepared reload rejects its candidate"]
pub struct PreparedReload<'a, B> {
    pub(super) document: &'a mut StageDocument<B>,
    pub(super) catalog: Catalog,
    /// What was read, not yet published.
    batch: ReadBatch,
    /// The sources read again, by layer, package members included, in id
    /// order.
    reloaded: Vec<LayerId>,
    /// The layers whose new identifier the reload publishes: a search path
    /// now resolving elsewhere.
    moved: Vec<LayerId>,
    /// Whether a reloaded layer is reported as read again even when its
    /// content is unchanged: its source changed or was forced, rather than
    /// read only to compare its bytes.
    marked: BTreeSet<LayerId>,
    /// The layers read only so that their arcs resolve the assets
    /// [`Self::found`] lists, their sources unchanged: never reported as
    /// read again.
    rebound_only: BTreeSet<LayerId>,
    /// The assets recorded missing that now resolve: anchor, asset path,
    /// layer.
    found: Vec<(LayerId, Arc<str>, LayerId)>,
    report: LoadReport,
    /// The composition [`Self::candidate`] made, on first request.
    candidate: Option<(InMemoryStore, LiveStage)>,
}
impl<B> core::fmt::Debug for PreparedReload<'_, B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PreparedReload")
            .field("reloaded", &self.reloaded)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}
impl<B> PreparedReload<'_, B> {
    /// The layers whose sources the reload read again, package members
    /// included, in id order: those whose source changed since it was read,
    /// the dirty ones the policy discards, and those forced. Empty when
    /// nothing changed.
    pub fn reloaded_layers(&self) -> &[LayerId] {
        &self.reloaded
    }
    /// The content a layer of [`Self::reloaded_layers`] (or a layer the
    /// reload newly reaches) will have, as read; `None` for any other
    /// layer.
    pub fn layer(&self, layer: LayerId) -> Option<&Layer> {
        self.batch.pending.iter().find(|read| read.id() == layer)
    }
    /// Whether the reload would publish nothing: no layer changed, and no
    /// missing asset now resolves.
    pub fn is_empty(&self) -> bool {
        self.batch.pending.is_empty() && self.found.is_empty()
    }
    /// The scene the reload would publish, composed in full: a copy of the
    /// document's layers with the layers read in their place. Composed on
    /// the first call, a whole composition publishing does not need; the
    /// copy is an independent store ([`InMemoryStore::snapshot`]) that the
    /// document's ids name too.
    pub fn candidate(&mut self) -> (&InMemoryStore, &LiveStage) {
        if self.candidate.is_none() {
            let mut store = self.document.store.snapshot();
            let mut catalog = self.catalog.clone();
            for layer in &self.batch.pending {
                store.insert_layer(layer.clone());
            }
            self.batch.clone().commit(&mut store, &mut catalog);
            for (anchor, asset, layer) in &self.found {
                store.insert_asset_layer(*anchor, asset, *layer);
            }
            let root = self
                .document
                .stage
                .stage()
                .root_layer()
                .expect("document root");
            let options = self.document.stage.options().clone();
            let stage = LiveStage::compose(&mut store, root, options);
            self.candidate = Some((store, stage));
        }
        let (store, stage) = self.candidate.as_ref().expect("composed");
        (store, stage)
    }
    /// Layers read, diagnostics and asset failures for this reload.
    pub fn load_report(&self) -> &LoadReport {
        &self.report
    }
    /// Candidate source identifier; package members use `package[member]`.
    pub fn identifier(&self, layer: LayerId) -> Option<&str> {
        self.catalog.source(layer).map(|s| &*s.identifier)
    }
    /// Candidate retained-array statistics without materializing numeric data.
    #[cfg(feature = "std")]
    pub fn retained_values(&self, layer: LayerId) -> Option<&layerstack_usdc::RetainedValues> {
        self.catalog.retained_values.get(&layer)
    }
    /// Candidate package construction work and encoded backing size, with the
    /// same units and exclusions as [`StageDocument::package_stats`].
    pub fn package_stats(&self, layer: LayerId) -> Option<PackageStats> {
        self.catalog.package_stats(layer)
    }
    /// Candidate-local transport/index work and current package owner footprint.
    pub fn package_cache_stats(&self) -> PackageCacheStats {
        self.catalog.cache_stats()
    }
    /// Publishes the reload, reading no source again and composing only
    /// what changed.
    ///
    /// Layers the reload newly reaches are added, then every layer read
    /// again takes its new content as one transaction of per-spec edits
    /// ([`Transaction::transfer_content`]), each layer reported as read
    /// again ([`Transaction::mark_reloaded`]). The stage reports the
    /// changes, through the document's change cursors and subscriptions,
    /// as it reports an edit's: a changed value info-only, a prim added or
    /// removed resynced, the pseudo-root info-only for a layer of the
    /// stage's own layer stack, and a sublayer added or removed as the
    /// specs it brings or takes, wherever they compose. A layer naming an asset recorded missing
    /// that now resolves was read again with it, so its arc resolves in
    /// the same transaction and resyncs its prim alone; a deferred arc
    /// naming one resyncs as the asset is bound, in a second notice.
    /// Every layer read is clean afterwards. As with
    /// ordinary stage notices, callback panics propagate after publication.
    ///
    /// OpenUSD: `UsdStage::Reload` (`pxr/usd/usd/stage.cpp`), with
    /// `SdfLayer::Reload` publishing each changed layer through
    /// `SdfLayer::_SetData`'s per-spec difference, and `PcpCache::Reload`
    /// resyncing the arcs whose asset now resolves
    /// (`PcpChanges::DidMaybeFixAsset`), in one change block. Spec: AOUSD
    /// Core §9 (asset resolution), §12 (composed scene observation).
    pub fn commit(self) -> LoadReport {
        let Self {
            document,
            catalog,
            mut batch,
            reloaded,
            moved,
            marked,
            rebound_only,
            found,
            report,
            candidate: _,
        } = self;
        document.catalog = catalog;
        // The layers read again leave the batch; the rest are new.
        let mut again: Vec<Layer> = Vec::new();
        let mut seen = HashSet::new();
        batch.pending.retain_mut(|layer| {
            let id = layer.id();
            if !document.store.contains_layer(id) {
                return true;
            }
            if seen.insert(id) {
                again.push(core::mem::replace(layer, Layer::new(id)));
            }
            false
        });
        // Bindings of layers read again are published after their content,
        // so that a binding that changed resyncs what names it.
        let rebound: Vec<(LayerId, HashMap<Arc<str>, LayerId>)> = again
            .iter()
            .filter_map(|layer| batch.bindings.remove_entry(&layer.id()))
            .collect();
        let anchors: BTreeSet<LayerId> = rebound
            .iter()
            .map(|(anchor, _)| *anchor)
            .chain(found.iter().map(|(anchor, _, _)| *anchor))
            .collect();
        let clean: Vec<LayerId> = anchors
            .into_iter()
            .filter(|anchor| {
                document
                    .store
                    .layer(*anchor)
                    .is_some_and(|layer| document.catalog.is_clean(*anchor, layer.generation()))
            })
            .collect();
        batch.commit(&mut document.store, &mut document.catalog);
        // A muted layer composes nothing: its content changes with no
        // notice, and composes when it is unmuted.
        let muted = &document.stage.options().muted_layers;
        let (quiet, composed): (Vec<Layer>, Vec<Layer>) = again
            .into_iter()
            .partition(|layer| muted.contains(&layer.id()));
        let mut edit = Transaction::new();
        for layer in &composed {
            let id = layer.id();
            let content =
                Transaction::transfer_content(&document.store, id, layer).expect("a held layer");
            let changed = !content.is_empty();
            edit.append(content);
            if marked.contains(&id) || (changed && !rebound_only.contains(&id)) {
                edit.mark_reloaded(id);
            }
        }
        if !edit.is_empty() && document.stage.apply(&mut document.store, &edit).is_err() {
            // Only an edit of the layers since preparation stops it, which
            // the borrow rules out; publish the layers whole then.
            for layer in composed {
                document.store.insert_layer(layer);
            }
        }
        for layer in quiet {
            let id = layer.id();
            let content =
                Transaction::transfer_content(&document.store, id, &layer).expect("a held layer");
            if content.apply(&mut document.store).is_err() {
                document.store.insert_layer(layer);
            }
        }
        for id in moved {
            if let Some(source) = document.catalog.source(id) {
                let identifier = Arc::clone(&source.identifier);
                document.store.set_layer_identifier(id, identifier);
            }
        }
        for (anchor, assets) in rebound {
            document.store.insert_published_asset_layers(anchor, assets);
        }
        for (anchor, asset, layer) in found {
            document
                .store
                .insert_published_asset_layers(anchor, [(Arc::clone(&asset), layer)]);
            document
                .catalog
                .failures
                .remove(&(anchor, String::from(&*asset)));
        }
        document.stage.synchronize(&mut document.store);
        for id in reloaded.into_iter().chain(clean) {
            if let Some(layer) = document.store.layer(id) {
                document.catalog.set_saved(id, layer.generation());
            }
        }
        report
    }
}
impl<B: Storage> StageDocument<B> {
    /// Prepares reload of resident used sources, excluding anonymous/session layers.
    ///
    /// Selection uses the current composed snapshot, and the clean layers
    /// it mutes, so that unmuting one composes its source as it is now;
    /// synchronize direct authored edits first if they change its used
    /// layers. A clean layer whose source is unchanged ([`Storage::version`])
    /// is not read. Preparation itself publishes no notices. Drop the
    /// returned value to reject, or call its explicit `commit`.
    ///
    /// OpenUSD: `UsdStage::Reload` reloads the layers its composition uses
    /// (`PcpCache::Reload`, `pxr/usd/pcp/cache.cpp`), session layers aside,
    /// and discards the edits of dirty ones; [`ReloadPolicy::PreserveDirty`]
    /// refuses those instead.
    pub fn prepare_reload(
        &mut self,
        policy: ReloadPolicy,
    ) -> Result<PreparedReload<'_, B>, IoError> {
        let sessions = self.session_layers();
        let muted = self.stage.options().muted_layers.iter().copied();
        let muted: Vec<LayerId> = muted
            .filter(|id| self.store.contains_layer(*id) && !self.is_dirty(*id))
            .collect();
        let ids: Vec<_> = self
            .stage
            .stage()
            .used_layers(true)
            .difference(&sessions)
            .copied()
            .chain(muted)
            .filter(|id| self.catalog.source(*id).is_some())
            .collect();
        self.prepare_reload_internal(&ids, policy, Reading::Changed, true)
    }
    /// Prepares a selected reload without publishing its layers or source catalog.
    ///
    /// Selecting a package member reloads its whole package; dirty protection
    /// covers every resident member. A clean selected layer whose source is
    /// unchanged is not read; [`Self::prepare_force_reload_layers`] reads
    /// it anyway. A layer that fails to read or parse again keeps its
    /// content, dirty state and recorded version, and the failure is in
    /// [`PreparedReload::load_report`]'s `asset_failures` (with no anchor),
    /// while the others reload, as `SdfLayer::ReloadLayers` reloads each
    /// layer apart. Rejecting the candidate leaves authored layers, dirty
    /// cursors and dependency freshness intact.
    /// AOUSD Core §9 (source resolution), §16
    /// (format import); host-controlled counterpart of `SdfLayer::ReloadLayers`.
    pub fn prepare_reload_layers(
        &mut self,
        layers: &[LayerId],
        policy: ReloadPolicy,
    ) -> Result<PreparedReload<'_, B>, IoError> {
        self.prepare_reload_internal(layers, policy, Reading::Changed, false)
    }
    /// [`Self::prepare_reload_layers`], reading each selected source even
    /// when it is unchanged; each is reported as read again, its changes
    /// aside.
    ///
    /// OpenUSD: `SdfLayer::Reload(force = true)`.
    pub fn prepare_force_reload_layers(
        &mut self,
        layers: &[LayerId],
        policy: ReloadPolicy,
    ) -> Result<PreparedReload<'_, B>, IoError> {
        self.prepare_reload_internal(layers, policy, Reading::Forced, false)
    }
    /// Refreshes the authored root and its freshly reachable dependencies once.
    /// Unlike explicit selection, obsolete resident sources and package members
    /// are not forced to load. Existing source IDs and unused authored layers are
    /// retained; required dirty sources reject under `PreserveDirty`.
    /// Session opinions are preserved, and session-only dependencies are outside
    /// this root-file scope. Every reachable source is read, changed or not;
    /// only the layers whose content changed are reported as read again.
    /// Candidate rejection publishes no freshness changes.
    /// AOUSD Core §9, §16; explicit host root/dependency reload policy.
    pub fn prepare_reload_root(
        &mut self,
        policy: ReloadPolicy,
    ) -> Result<PreparedReload<'_, B>, IoError> {
        let root = self.stage.stage().root_layer().expect("document root");
        self.prepare_reload_internal(&[root], policy, Reading::Reachable, false)
    }
    /// Prepares the reload of `layers`; `whole_stage` for a reload of the
    /// stage ([`Self::prepare_reload`]), whose root layer's asset path is
    /// resolved again where the root is found first.
    fn prepare_reload_internal(
        &mut self,
        layers: &[LayerId],
        policy: ReloadPolicy,
        reading: Reading,
        whole_stage: bool,
    ) -> Result<PreparedReload<'_, B>, IoError> {
        let root_closure = reading == Reading::Reachable;
        let roots: BTreeSet<_> = layers
            .iter()
            .map(|id| {
                self.catalog
                    .source(*id)
                    .map(|s| s.package.unwrap_or(*id))
                    .ok_or_else(|| {
                        IoError::new(
                            IoErrorKind::MissingLayer,
                            "selected layer has no reloadable source",
                        )
                    })
            })
            .collect::<Result<_, _>>()?;
        let members = |catalog: &Catalog, roots: &BTreeSet<LayerId>| -> BTreeSet<LayerId> {
            catalog
                .sources()
                .filter_map(|(id, s)| {
                    (roots.contains(&id) || s.package.is_some_and(|p| roots.contains(&p)))
                        .then_some(id)
                })
                .collect()
        };
        let selected = members(&self.catalog, &roots);
        if !root_closure
            && policy == ReloadPolicy::PreserveDirty
            && selected.iter().any(|id| self.is_dirty(*id))
        {
            return Err(IoError::new(
                IoErrorKind::DirtyReload,
                "reload would overwrite dirty selected layers",
            ));
        }
        let mut catalog = self.catalog.clone();
        catalog.reserved.extend(self.store.layer_ids());
        // Which sources to read, and which moved: a source asked for by a
        // search path that now resolves to another file is read from there
        // (`SdfLayer::Reload` re-resolves the identifier first,
        // `pxr/usd/sdf/layer.cpp`). AOUSD Core §9.
        let mut moved = Vec::new();
        let mut marked = BTreeSet::new();
        let mut read: BTreeSet<LayerId> = BTreeSet::new();
        let mut compare = HashMap::new();
        // The roots with a dirty layer: the layer, or a member of its package.
        let dirty_roots: BTreeSet<LayerId> = selected
            .iter()
            .filter(|id| self.is_dirty(**id))
            .map(|id| {
                catalog
                    .source(*id)
                    .map_or(*id, |s| s.package.unwrap_or(*id))
            })
            .collect();
        // The asset paths bound to each layer, by anchor, to ask storage
        // where each resolves now; the root layer's by the asset path it
        // was opened by. A stage resolves its root where the root is first,
        // as `UsdStage::Reload` binds the stage's resolver context, which
        // `ArDefaultResolver` makes of the root layer's directory; a layer
        // reload resolves it with no anchor, as `SdfLayer::Reload` does
        // with no context bound.
        let mut bound_to: BoundAssets = HashMap::new();
        if !root_closure {
            for (anchor, asset, layer) in self.store.asset_bindings() {
                if roots.contains(&layer) {
                    bound_to
                        .entry(layer)
                        .or_default()
                        .push((Some(anchor), asset));
                }
            }
            let root = self.stage.stage().root_layer().expect("document root");
            if roots.contains(&root) {
                let anchor = whole_stage.then_some(root);
                bound_to
                    .entry(root)
                    .or_default()
                    .push((anchor, Arc::clone(&self.root_asset)));
            }
        }
        for &root in &roots {
            if root_closure {
                read.insert(root);
                continue;
            }
            if let Some(identifier) = self.moved_identifier(root, &catalog, &bound_to) {
                let old = Arc::clone(&catalog.source(root).expect("source").identifier);
                catalog.ids.remove(&old);
                let identifier: Arc<str> = Arc::from(identifier);
                catalog.ids.insert(Arc::clone(&identifier), root);
                catalog.source_mut(root).expect("source").identifier = identifier;
                moved.push(root);
                marked.insert(root);
                read.insert(root);
                continue;
            }
            let dirty = dirty_roots.contains(&root);
            let source = catalog.source(root).expect("source");
            let outer = FormatArgs::strip(&source.identifier);
            let reads_source = self
                .formats
                .for_identifier(&source.identifier)
                .is_none_or(FileFormat::reads_source);
            let freshness = catalog.record(root).and_then(|r| r.freshness.as_ref());
            let forced = reading == Reading::Forced || dirty;
            match freshness {
                _ if forced => {
                    marked.insert(root);
                    read.insert(root);
                }
                // Built from its format arguments alone: nothing to compare.
                _ if !reads_source => {}
                Some(Freshness::Version(version)) => {
                    if freshness::version_changed(&self.storage, outer, version) {
                        marked.insert(root);
                        read.insert(root);
                    }
                }
                // Read, and left as it is if its bytes are as they were.
                Some(Freshness::Digest { .. }) => {
                    compare.insert(root, freshness.cloned().expect("a digest"));
                    read.insert(root);
                }
                // Unknown: read, and published only if it differs.
                None => {
                    read.insert(root);
                }
            }
        }
        let selected = members(&catalog, &read);
        // A package read again for a reason of its own reads its members
        // for it too.
        let marked_members = members(&catalog, &marked);
        marked.extend(marked_members);
        if root_closure {
            catalog.clear_states();
            #[cfg(feature = "std")]
            catalog.retained_values.clear();
        }
        for id in &selected {
            catalog.clear_state(*id);
            // A source compared by its bytes keeps its values if unchanged;
            // read again, its new ones replace them.
            #[cfg(feature = "std")]
            {
                let package = catalog.source(*id).and_then(|s| s.package);
                if !compare.contains_key(id) && !package.is_some_and(|p| compare.contains_key(&p)) {
                    catalog.retained_values.remove(id);
                }
            }
        }
        for root in &read {
            let outer = catalog.source(*root).expect("source").identifier.clone();
            catalog
                .ids
                .retain(|identifier, id| *id != *root || *identifier == outer);
        }
        let dirty: BTreeSet<_> = if root_closure && policy == ReloadPolicy::PreserveDirty {
            self.store
                .layer_ids()
                .into_iter()
                .filter(|&id| self.is_dirty(id))
                .collect()
        } else {
            BTreeSet::new()
        };
        // The assets recorded missing, retried as
        // `PcpCache::Reload` retries every invalid asset: one that now
        // resolves is bound, and the arcs naming it resync
        // (`PcpChanges::DidMaybeFixAsset`). AOUSD Core §9, §10.6.
        let used = self.stage.stage().used_layers(true);
        let dirty_layers: BTreeSet<LayerId> = used
            .iter()
            .copied()
            .filter(|id| self.is_dirty(*id))
            .collect();
        let missing: Vec<(LayerId, Arc<str>)> = self
            .store
            .asset_bindings()
            .into_iter()
            .filter(|(anchor, _, layer)| *layer == LayerId::UNRESOLVED && used.contains(anchor))
            .map(|(anchor, asset, _)| (anchor, asset))
            .collect();
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
        reader.reachable_only = root_closure;
        reader.muted.clone_from(&self.stage.options().muted_layers);
        reader.unchanged = compare;
        // Muted layers selected are read as any other: they compose
        // nothing until unmuted.
        for id in &read {
            reader.muted.remove(id);
        }
        let (tokens, paths) = (self.store.tokens(), self.store.paths());
        // An asset found now that a layer not read again names is read,
        // and so is that layer, clean, so that its arcs resolve to it: a
        // reference or sublayer that failed holds no layer, which only
        // reading the layer again gives it.
        let mut found = Vec::new();
        let mut again: BTreeSet<LayerId> = BTreeSet::new();
        let mut decoded = HashMap::new();
        let package_of = |catalog: &Catalog, id: LayerId| {
            catalog.source(id).map_or(id, |s| s.package.unwrap_or(id))
        };
        let (before, after): (Vec<_>, Vec<_>) = missing.into_iter().partition(|(anchor, _)| {
            !selected.contains(anchor)
                || reader
                    .unchanged
                    .contains_key(&package_of(reader.catalog, *anchor))
        });
        for (anchor, asset) in before {
            if let Ok(resolved) = reader.load(&asset, Some(anchor), tokens, paths) {
                found.push((anchor, asset, resolved.layer_id));
                if let Some(layer) = resolved.layer {
                    reader.pending.push(layer);
                }
                let root = package_of(reader.catalog, anchor);
                // Compared by its bytes, it is decoded whatever they are.
                if let Some(digest) = reader.unchanged.remove(&root) {
                    decoded.insert(root, digest);
                } else if !read.contains(&root) && !dirty_layers.contains(&root) {
                    again.insert(root);
                }
            }
        }
        for &id in &again {
            let members: Vec<LayerId> = reader
                .catalog
                .sources()
                .filter(|(member, s)| *member == id || s.package == Some(id))
                .map(|(member, _)| member)
                .collect();
            for member in members {
                reader.catalog.clear_state(member);
                #[cfg(feature = "std")]
                reader.catalog.retained_values.remove(&member);
            }
        }
        // A layer that cannot be read again keeps its content while the
        // others reload, as `SdfLayer::ReloadLayers` reloads each layer
        // apart (`pxr/usd/sdf/layer.cpp`); the failure is reported.
        let mut failed: Vec<(LayerId, Arc<str>, IoError)> = Vec::new();
        for &id in read.iter().chain(&again) {
            let identifier = reader
                .catalog
                .source(id)
                .expect("source")
                .identifier
                .clone();
            match reader.load(&identifier, None, tokens, paths) {
                Ok(result) => {
                    if let Some(layer) = result.layer {
                        reader.pending.push(layer);
                    }
                }
                Err(error) if !root_closure && self.store.contains_layer(id) => {
                    failed.push((id, identifier, error));
                }
                Err(error) => return Err(error),
            }
        }
        for (anchor, asset) in after {
            // A layer read again resolved the arcs it does not defer.
            let bound = reader.bindings.get(&anchor);
            if bound.is_some_and(|assets| assets.contains_key(&asset)) {
                continue;
            }
            if let Ok(resolved) = reader.load(&asset, Some(anchor), tokens, paths) {
                found.push((anchor, asset, resolved.layer_id));
                if let Some(layer) = resolved.layer {
                    reader.pending.push(layer);
                }
            }
        }
        reader.resolve_expressions(
            &mut self.store,
            layerstack::LayerStackIdentifier {
                root: self.stage.stage().root_layer().expect("document root"),
                session: self.stage.stage().session_layer(),
            },
        )?;
        if root_closure
            && policy == ReloadPolicy::PreserveDirty
            && reader
                .pending
                .iter()
                .any(|layer| dirty.contains(&layer.id()))
        {
            return Err(IoError::new(
                IoErrorKind::DirtyReload,
                "reload would overwrite a dirty reachable source",
            ));
        }
        let (mut batch, read_catalog) = reader.finish();
        let failed_roots: BTreeSet<LayerId> = failed.iter().map(|(id, _, _)| *id).collect();
        let failed_members = members(read_catalog, &failed_roots);
        batch
            .pending
            .retain(|layer| !failed_members.contains(&layer.id()));
        for &id in &failed_members {
            // As it was: read, with the version, values and identifier it
            // was read with.
            read_catalog.set_state(id, State::Loaded);
            let freshness = self.catalog.record(id).and_then(|r| r.freshness.clone());
            read_catalog.record_mut(id).freshness = freshness;
            #[cfg(feature = "std")]
            if let Some(values) = self.catalog.retained_values.get(&id) {
                read_catalog.retained_values.insert(id, values.clone());
            }
            if let Some(source) = self.catalog.source(id).cloned() {
                read_catalog.ids.retain(|_, held| *held != id);
                for (identifier, held) in &self.catalog.ids {
                    if *held == id {
                        read_catalog.ids.insert(Arc::clone(identifier), id);
                    }
                }
                read_catalog.insert_source(id, source);
            }
        }
        moved.retain(|id| !failed_roots.contains(id));
        marked.retain(|id| !failed_members.contains(id));
        for (_, identifier, error) in failed {
            batch.failures.push(AssetFailure {
                asset_path: String::from(&*identifier),
                anchor: None,
                error,
            });
        }
        // Read only to resolve the arcs of an asset found now: not
        // reported as read again, as its source did not change.
        let mut rebound_only = again;
        rebound_only.extend(decoded.into_iter().filter_map(|(root, digest)| {
            let record = read_catalog.record(root)?;
            (record.freshness.as_ref() == Some(&digest)).then_some(root)
        }));
        let rebound_members = members(read_catalog, &rebound_only);
        rebound_only.extend(rebound_members);
        // A package left as it was keeps its members as they were.
        for id in &selected {
            if read_catalog.state(*id).is_none() && self.store.contains_layer(*id) {
                read_catalog.set_state(*id, State::Loaded);
            }
        }
        // A clean source read only to compare its bytes, unchanged, is not
        // published: its layer stays as it is, as an unchanged file would.
        let unchanged: BTreeSet<LayerId> = batch
            .pending
            .iter()
            .filter(|layer| {
                let id = layer.id();
                !marked.contains(&id)
                    && !self.is_dirty(id)
                    && self.store.layer(id).is_some_and(|held| held == *layer)
            })
            .map(Layer::id)
            .collect();
        batch
            .pending
            .retain(|layer| !unchanged.contains(&layer.id()));
        let mut reloaded: Vec<LayerId> = batch
            .pending
            .iter()
            .map(Layer::id)
            .filter(|id| self.store.contains_layer(*id))
            .collect();
        sort::sort_unstable(&mut reloaded);
        reloaded.dedup();
        if root_closure {
            // Every reachable source was read; only those that changed
            // are reported as read again.
            marked.clear();
        }
        let report = batch.report();
        Ok(PreparedReload {
            document: self,
            catalog,
            batch,
            reloaded,
            moved,
            marked,
            rebound_only,
            found,
            report,
            candidate: None,
        })
    }
    /// The identifier the source of `layer` would have if read now, when
    /// every asset path bound to it resolves to that one other identifier,
    /// which no other layer has: a search path that now finds another file
    /// first. `None` for a source that has not moved, a package or package
    /// member, and one read with format arguments.
    fn moved_identifier(
        &self,
        layer: LayerId,
        catalog: &Catalog,
        bound_to: &BoundAssets,
    ) -> Option<String> {
        let source = catalog.source(layer)?;
        if source.package.is_some()
            || source.identifier.contains(FORMAT_ARGS_DELIMITER)
            || source.identifier.contains('[')
        {
            return None;
        }
        let mut moved: Option<String> = None;
        for (anchor, asset) in bound_to.get(&layer)? {
            if asset.contains('[') || asset.contains(FORMAT_ARGS_DELIMITER) {
                continue;
            }
            let anchor = anchor.and_then(|anchor| catalog.source(anchor)).map(|s| {
                let identifier = FormatArgs::strip(&s.identifier);
                String::from(
                    identifier
                        .split_once('[')
                        .map_or(identifier, |(outer, _)| outer),
                )
            });
            let identifier = self.storage.identify(asset, anchor.as_deref()).ok()?;
            if identifier == *source.identifier {
                return None;
            }
            match &moved {
                Some(found) if *found != identifier => return None,
                _ => moved = Some(identifier),
            }
        }
        let moved = moved?;
        (!catalog.ids.contains_key(moved.as_str())).then_some(moved)
    }
}

/// The asset paths each layer is bound by, with the layer anchoring each
/// (none for the root layer's own asset path resolved as it was opened).
type BoundAssets = HashMap<LayerId, Vec<(Option<LayerId>, Arc<str>)>>;

/// Which selected sources a reload reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    /// Those changed since they were read, and the dirty ones.
    Changed,
    /// Every one, changed or not.
    Forced,
    /// The root and every source it reaches, read afresh.
    Reachable,
}
