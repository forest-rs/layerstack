// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Candidate reload publication with a borrow guarding the source document.

use super::*;

/// Imported candidate held apart from the published document until explicit commit.
///
/// Inspect [`Self::store`], [`Self::stage`] and [`Self::load_report`] to validate
/// geometry and host budgets. Dropping this value rejects the candidate without
/// publishing layers, source bindings, dirty generations or composed notices.
/// The exclusive document borrow prevents edits, saves and other reloads between
/// preparation and commit. Source transport reads cannot be rolled back.
///
/// The candidate uses an independent interner domain with existing IDs preserved.
/// Rejection (including a leaked candidate) leaves the published domain intact.
/// Numeric buffers and package bytes share ownership; authored layer maps,
/// interner indexes and the candidate composition are copied.
/// Commit reuses that composition and preserves the document's change observers.
/// No automatic validation of renderer budgets or external texture contents is
/// performed. This is a host-controlled extension of `UsdStage::Reload`.
#[must_use = "dropping a prepared reload rejects its candidate"]
pub struct PreparedReload<'a, B> {
    pub(super) document: &'a mut StageDocument<B>,
    store: InMemoryStore,
    stage: LiveStage,
    pub(super) catalog: Catalog,
    report: LoadReport,
}
impl<B> core::fmt::Debug for PreparedReload<'_, B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PreparedReload")
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}
impl<B> PreparedReload<'_, B> {
    /// Candidate authored layers and shared interners. This view is read-only.
    pub fn store(&self) -> &InMemoryStore {
        &self.store
    }
    /// Candidate retained stage, already composed from the candidate sources.
    pub fn stage(&self) -> &LiveStage {
        &self.stage
    }
    /// Imported layers, diagnostics and asset failures for this candidate.
    pub fn load_report(&self) -> &LoadReport {
        &self.report
    }
    /// Candidate source identifier; package members use `package[member]`.
    pub fn identifier(&self, layer: LayerId) -> Option<&str> {
        self.catalog
            .sources
            .get(&layer)
            .map(|s| s.identifier.as_str())
    }
    /// Candidate retained-array statistics without materializing numeric data.
    #[cfg(feature = "std")]
    pub fn retained_values(&self, layer: LayerId) -> Option<&layerstack_usdc::RetainedValues> {
        self.catalog.retained_values.get(&layer)
    }
    /// Publishes the validated candidate store, source catalog and composition.
    ///
    /// No sources are reread and no second composition is performed. Publication
    /// emits a root resync through the document's existing change cursors and
    /// subscriptions. Other retained clients must synchronize against its store.
    /// As with ordinary stage notices, callback panics propagate after publication.
    pub fn commit(self) -> LoadReport {
        self.document.store = self.store;
        self.document.catalog = self.catalog;
        self.document
            .stage
            .replace_with(self.stage, &mut self.document.store);
        self.report
    }
}
impl<B: Storage> StageDocument<B> {
    /// Prepares reload of resident used sources, excluding anonymous/session layers.
    ///
    /// Selection uses the current composed snapshot; synchronize direct authored
    /// edits first if they change its used layers. Preparation itself publishes no
    /// notices. Drop the returned value to reject, or call its explicit `commit`.
    pub fn prepare_reload(
        &mut self,
        policy: ReloadPolicy,
    ) -> Result<PreparedReload<'_, B>, IoError> {
        let sessions = self.session_layers();
        let ids: Vec<_> = self
            .stage
            .stage()
            .used_layers(true)
            .difference(&sessions)
            .copied()
            .filter(|id| self.catalog.sources.contains_key(id))
            .collect();
        self.prepare_reload_layers(&ids, policy)
    }
    /// Prepares a selected reload without publishing its layers or source catalog.
    ///
    /// Selecting a package member reloads its whole package; dirty protection
    /// covers every resident member. Read/parse failure and candidate rejection
    /// leave authored layers, dirty cursors and dependency freshness intact.
    /// Candidate-only token/path IDs must not be used on a rejected document.
    /// AOUSD Core §9 (source resolution), §16
    /// (format import); host-controlled counterpart of `UsdStage::Reload`.
    pub fn prepare_reload_layers(
        &mut self,
        layers: &[LayerId],
        policy: ReloadPolicy,
    ) -> Result<PreparedReload<'_, B>, IoError> {
        let roots: BTreeSet<_> = layers
            .iter()
            .map(|id| {
                self.catalog
                    .sources
                    .get(id)
                    .map(|s| s.package.unwrap_or(*id))
                    .ok_or_else(|| {
                        IoError::new(
                            IoErrorKind::MissingLayer,
                            "selected layer has no reloadable source",
                        )
                    })
            })
            .collect::<Result<_, _>>()?;
        let selected: BTreeSet<_> = self
            .catalog
            .sources
            .iter()
            .filter_map(|(&id, s)| {
                (roots.contains(&id) || s.package.is_some_and(|p| roots.contains(&p))).then_some(id)
            })
            .collect();
        if policy == ReloadPolicy::PreserveDirty && selected.iter().any(|id| self.is_dirty(*id)) {
            return Err(IoError::new(
                IoErrorKind::DirtyReload,
                "reload would overwrite dirty selected layers",
            ));
        }
        let mut store = self.store.snapshot();
        let mut catalog = self.catalog.clone();
        catalog.reserved.extend(self.store.layers.keys().copied());
        for id in &selected {
            catalog.states.remove(id);
            #[cfg(feature = "std")]
            catalog.retained_values.remove(id);
        }
        for root in &roots {
            let outer = catalog.sources[root].identifier.clone();
            catalog
                .ids
                .retain(|identifier, id| *id != *root || *identifier == outer);
        }
        let mut reader = Reader::new(
            &mut self.storage,
            &mut catalog,
            self.import_policy,
            self.usdc_options,
        );
        for id in roots {
            let identifier = reader.catalog.sources[&id].identifier.clone();
            let result = reader.load(&identifier, None, &mut store.tokens, &mut store.paths)?;
            if let Some(layer) = result.layer {
                reader.pending.push(layer);
            }
        }
        reader.resolve_expressions(
            &mut store,
            layerstack::LayerStackIdentifier {
                root: self.stage.stage().root_layer().expect("document root"),
                session: self.stage.stage().session_layer(),
            },
        )?;
        let report = reader.commit(&mut store);
        let root = self.stage.stage().root_layer().expect("document root");
        let options = self.stage.options().clone();
        let stage = LiveStage::compose(&mut store, root, options);
        Ok(PreparedReload {
            document: self,
            store,
            stage,
            catalog,
            report,
        })
    }
}
