// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Incremental recomposition via `invalidation`.
//!
//! [`LiveStage`] wraps a composed [`Stage`] and owns an
//! [`InvalidationTracker`] to support scoped recomposition: when a layer's
//! opinions change, only the transitively affected prims are recomposed.
//!
//! Callers mark roots, and propagation to transitive dependents happens at
//! drain time during [`LiveStage::recompose`].

use alloc::{collections::BTreeSet, vec::Vec};

mod controls;
mod mask;
pub use mask::MaskExpansionReport;
mod discovery;
pub use controls::RecompositionWork;
mod local;
mod notices;
pub use notices::{ChangeCursor, ChangeHistoryError, ChangeNotice, ChangeSubscription};

use hashbrown::{HashMap, HashSet};
use invalidation::{Channel, CycleHandling, InvalidationTracker, TraversalScratch};

use crate::{
    dependency_map::{ArcDependency, CompositionDeps},
    doc::{LayerId, LayerStore},
    edit::{Applied, Changes, EditError, Transaction},
    expression_variables::VariableReads,
    path::PathId,
    stage::{PopulationMask, Stage, StageOptions},
};

/// Invalidation channel for opinion (field value) edits.
pub const OPINION_EDIT: Channel = Channel::new(0);

/// Invalidation channel for structural changes (prims added/removed, arcs changed).
///
/// Structural changes fall back to a full rebuild.
pub const STRUCTURAL: Channel = Channel::new(1);

/// A layer's [`Layer::generation`](crate::Layer::generation) and
/// [`Layer::structural_generation`](crate::Layer::structural_generation).
type LayerGenerations = (u64, u64);

/// The scopes of the structural changes of `layer` since `generation`, and
/// the changed prims whose specs only support existing children; None when
/// its history does not reach back that far.
fn source_changes(
    store: &dyn LayerStore,
    layer: LayerId,
    generation: u64,
) -> Option<(Vec<PathId>, Vec<PathId>)> {
    let layer = store.layer(layer)?;
    Some((
        layer.changed_scopes_since(generation)?,
        layer.changed_supports_since(generation)?,
    ))
}

fn generations_of(store: &dyn LayerStore, layer: LayerId) -> Option<LayerGenerations> {
    store
        .layer(layer)
        .map(|found| (found.generation(), found.structural_generation()))
}

fn is_beneath(store: &dyn LayerStore, path: PathId, roots: &[PathId]) -> bool {
    roots.iter().any(|root| {
        store
            .paths()
            .resolve(path)
            .strip_prefix(store.paths().resolve(*root))
            .is_some()
    })
}

/// Whether `path` is one of `roots` or beneath one, as [`is_beneath`]
/// tells, walking `path`'s ancestors: the cost is its depth, not the
/// number of roots, which a bulk edit makes as many as the prims it
/// creates.
fn is_beneath_set(store: &dyn LayerStore, path: PathId, roots: &HashSet<PathId>) -> bool {
    let mut current = Some(path);
    while let Some(at) = current {
        if roots.contains(&at) {
            return true;
        }
        current = store.paths().parent(at);
    }
    false
}

fn minimal_roots(store: &dyn LayerStore, paths: &[PathId]) -> Vec<PathId> {
    let set: HashSet<_> = paths.iter().copied().collect();
    let mut roots: Vec<_> = set
        .iter()
        .copied()
        .filter(|path| {
            let mut current = store.paths().resolve(*path).parent();
            while let Some(parent) = current {
                if store
                    .paths()
                    .lookup(&parent)
                    .is_some_and(|id| set.contains(&id))
                {
                    return false;
                }
                current = parent.parent();
            }
            true
        })
        .collect();
    roots.sort_unstable();
    roots
}

fn include_resyncs(store: &dyn LayerStore, changes: &mut Changes, paths: &[PathId]) {
    if paths.is_empty() {
        return;
    }
    changes.resynced.extend_from_slice(paths);
    changes.resynced = minimal_roots(store, &changes.resynced);
    let resynced: HashSet<PathId> = changes.resynced.iter().copied().collect();
    changes
        .changed_info_only
        .retain(|path| !is_beneath_set(store, *path, &resynced));
}

/// Every layer a stage rooted at `root` reads or would read: the root's
/// layer stack, and, transitively, the layer stacks every reference and
/// payload authored in them targets, whether or not they contribute
/// opinions yet. Layers the store does not hold are included too.
///
/// Spec: AOUSD Core §9 (layer stacks), §10.3.2.1 and §10.3.2.2
/// (references and payloads).
fn participating_layers(
    store: &dyn LayerStore,
    root: LayerId,
    muted: &BTreeSet<LayerId>,
) -> HashSet<LayerId> {
    fn arc_layers(
        store: &dyn LayerStore,
        references: &crate::ListOp<crate::Reference>,
        payloads: &crate::ListOp<crate::Reference>,
        pending: &mut Vec<LayerId>,
        expressions: &mut bool,
    ) {
        for list in [references, payloads] {
            for arc in list.inserted_items() {
                *expressions |= arc.is_expression();
                pending.extend(arc.stored_target_layer(store));
            }
        }
    }
    let mut seen = HashSet::new();
    let mut expressions = false;
    let mut pending = alloc::vec![root];
    while let Some(id) = pending.pop() {
        if id == LayerId::UNRESOLVED || muted.contains(&id) || !seen.insert(id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            continue;
        };
        for entry in &layer.sublayers {
            expressions |= entry.is_expression();
            pending.push(entry.layer);
        }
        for spec in layer
            .prims
            .values()
            .chain(layer.variant_prims.values().flatten())
        {
            arc_layers(
                store,
                &spec.references,
                &spec.payloads,
                &mut pending,
                &mut expressions,
            );
            for branch in spec.variant_branches() {
                arc_layers(
                    store,
                    &branch.spec.references,
                    &branch.spec.payloads,
                    &mut pending,
                    &mut expressions,
                );
            }
        }
    }
    // The layers asset paths authored as variable expressions reach, with
    // the variables of each layer stack reaching them.
    if expressions {
        for walked in crate::expression_variables::walk(store, root).stacks {
            if walked.stack.chains.iter().any(|chain| {
                chain.iter().any(|id| {
                    muted.contains(&id.root) || id.session.is_some_and(|s| muted.contains(&s))
                })
            }) {
                continue;
            }
            seen.extend(
                walked
                    .stack
                    .layers
                    .into_iter()
                    .filter(|id| !muted.contains(id)),
            );
        }
    }
    seen
}

/// A mutable composition stage that supports incremental recomposition.
///
/// `LiveStage` owns a fully composed [`Stage`] and an
/// [`InvalidationTracker`] (the single source of truth for dependency
/// topology and dirty prim state).
///
/// Notifications use lazy propagation: callers mark roots via
/// [`notify_layer_edit`](Self::notify_layer_edit) or
/// [`notify_prim_edit`](Self::notify_prim_edit), and transitive dependents
/// are expanded at drain time during [`recompose`](Self::recompose).
#[derive(Debug)]
#[doc(alias = "UsdStage")]
pub struct LiveStage {
    stage: Stage,
    local_namespace: Option<local::LocalNamespace>,
    /// Tracks dependency topology and dirty prim state.
    tracker: InvalidationTracker<PathId>,
    /// Reused traversal state for lazy invalidation expansion.
    traversal_scratch: TraversalScratch<PathId>,
    /// Arc metadata for incremental edge updates and diagnostics.
    arc_metadata: HashSet<ArcDependency>,
    /// Layer → prims that receive opinions from that layer.
    layer_to_prims: HashMap<LayerId, HashSet<PathId>>,
    /// Prim → layers that contribute opinions to it.
    prim_to_layers: crate::dependency_map::LayerSets,
    /// Source site `(layer, prim path in that layer)` → composed prims whose
    /// prim index draws specs or opinions from it.
    source_to_prims: HashMap<(LayerId, PathId), HashSet<PathId>>,
    /// Composed prim → source sites recorded in `source_to_prims`.
    prim_to_sources: HashMap<PathId, Vec<(LayerId, PathId)>>,
    /// Site `(layer, prim path)` → composed prims whose dynamic payload
    /// arguments were composed from its specs ([`Stage::dynamic_dependency`]).
    dynamic_to_prims: HashMap<(LayerId, PathId), HashSet<PathId>>,
    /// Composed prim → sites recorded in `dynamic_to_prims`.
    prim_to_dynamic: HashMap<PathId, Vec<(LayerId, PathId)>>,
    /// Layer → composed prims with a reference or payload that targets that
    /// layer's `defaultPrim`, resolved or not.
    default_prim_dependents: HashMap<LayerId, HashSet<PathId>>,
    /// Layers whose `layerRelocates` the last full composition consulted.
    relocation_layers: HashSet<LayerId>,
    /// The expression variables composition read, with the values found.
    expression_variables: VariableReads,
    /// The generation and structural generation of every layer the stage
    /// reads, as this stage last saw them; `None` for a layer the store
    /// did not hold (see [`LiveStage::notify_changed_layers`]).
    generations: HashMap<LayerId, Option<LayerGenerations>>,
    root: LayerId,
    options: StageOptions,
    needs_full_rebuild: bool,
    controls_pending: bool,
    structural_roots: Vec<PathId>,
    structural_sources: Vec<(LayerId, PathId)>,
    /// Changed source prims whose specs only support existing children
    /// ([`crate::Layer::changed_supports_since`]): their prim indexes need
    /// refreshing, not the namespace beneath them.
    support_sources: Vec<(LayerId, PathId)>,
    layer_discovery: discovery::LayerDiscovery,
    /// The child specs of layers that do not list them, kept across
    /// compositions so an edit re-indexes only the specs it adds.
    child_index: crate::prim_indexing::ChildIndex,
    work: RecompositionWork,
    /// [`Self::child_index`]'s work when [`Self::work`] was last reset.
    child_index_mark: crate::prim_indexing::ChildIndexWork,
    notices: notices::Journal,
}

impl LiveStage {
    /// Performs an initial full composition and builds the dependency graph.
    pub fn compose(store: &mut dyn LayerStore, root: LayerId, mut options: StageOptions) -> Self {
        options.muted_layers.remove(&root);
        let opts = StageOptions {
            with_dependencies: true,
            ..options.clone()
        };
        let child_index = crate::prim_indexing::ChildIndex::default();
        let mut stage =
            Stage::compose_retained(store, root, opts, false, &HashSet::new(), &child_index);
        let deps = stage.take_deps().unwrap_or_default();
        let tracker =
            InvalidationTracker::from_graph_with_cycle_handling(deps.graph, CycleHandling::Ignore);

        let mut live = Self {
            stage,
            local_namespace: local::LocalNamespace::build(store, root, &options),
            tracker,
            traversal_scratch: TraversalScratch::new(),
            arc_metadata: deps.arcs,
            layer_to_prims: deps.layer_to_prims,
            prim_to_layers: deps.prim_to_layers,
            source_to_prims: HashMap::new(),
            prim_to_sources: HashMap::new(),
            dynamic_to_prims: HashMap::new(),
            prim_to_dynamic: HashMap::new(),
            default_prim_dependents: deps.default_prim_dependents,
            relocation_layers: deps.relocation_layers,
            expression_variables: deps.expression_variables,
            generations: HashMap::new(),
            root,
            options,
            needs_full_rebuild: false,
            controls_pending: false,
            structural_roots: Vec::new(),
            structural_sources: Vec::new(),
            support_sources: Vec::new(),
            layer_discovery: discovery::LayerDiscovery::default(),
            child_index_mark: child_index.work(),
            child_index,
            work: RecompositionWork::default(),
            notices: notices::Journal::default(),
        };
        live.reindex_all_sources();
        live.record_generations(store);
        live
    }

    /// Pending stage-local controls, reflected in the snapshot after
    /// [`Self::synchronize`] or [`Self::recompose_changes`].
    #[must_use]
    pub fn options(&self) -> &StageOptions {
        &self.options
    }

    /// Publishes an already composed replacement without composing it again.
    ///
    /// Preserves this stage's change cursors, subscriptions and history budget;
    /// publishes a root resync with created/removed paths. The replacement's own
    /// observers are discarded. Its retained dependencies and controls become
    /// this stage's, so subsequent edits continue to synchronize normally.
    ///
    /// The caller must publish the replacement's matching source store first;
    /// old token/path IDs must retain their meanings (for example, through
    /// [`crate::InMemoryStore::snapshot`]). Do not change replacement inputs between
    /// composing and publishing. This is the commit seam for host-owned staged
    /// reloads, not a reload or freshness check. AOUSD Core §12 (composed scene
    /// observation); OpenUSD `UsdNotice::ObjectsChanged` root resync semantics.
    pub fn replace_with(&mut self, mut replacement: Self, store: &mut dyn LayerStore) -> Changes {
        let old: HashSet<_> = self.stage.prim_paths().collect();
        let mut changes = Changes {
            created: replacement
                .stage
                .prim_paths()
                .filter(|p| !old.contains(p))
                .collect(),
            removed: old
                .into_iter()
                .filter(|p| !replacement.stage.has_prim(*p))
                .collect(),
            resynced: alloc::vec![store.paths_mut().intern(crate::Path::root())],
            ..Default::default()
        };
        replacement.notices = core::mem::take(&mut self.notices);
        *self = replacement;
        self.notices.publish(&mut changes, &self.stage, store);
        changes
    }

    /// Queue an atomic mute/unmute batch. Muting never edits or evicts a layer.
    /// Root muting and conflicting requests fail without changing any controls.
    /// A changed batch queues a structural recomposition and its usual notices.
    /// OpenUSD: `UsdStage::MuteAndUnmuteLayers`.
    #[doc(alias = "UsdStage::MuteAndUnmuteLayers")]
    #[doc(alias = "MuteAndUnmuteLayers")]
    pub fn mute_and_unmute_layers(
        &mut self,
        mute: &[LayerId],
        unmute: &[LayerId],
    ) -> Result<bool, crate::LayerMuteError> {
        if mute.contains(&self.root) {
            return Err(crate::LayerMuteError::RootLayer);
        }
        if mute.iter().any(|l| unmute.contains(l)) {
            return Err(crate::LayerMuteError::ConflictingRequest);
        }
        let mut changed = false;
        for &layer in mute {
            changed |= self.options.muted_layers.insert(layer);
        }
        for layer in unmute {
            changed |= self.options.muted_layers.remove(layer);
        }
        self.controls_pending |= changed;
        Ok(changed)
    }

    /// Queue exclusion of a layer and its sublayers from this stage.
    #[doc(alias = "UsdStage::MuteLayer")]
    #[doc(alias = "MuteLayer")]
    pub fn mute_layer(&mut self, layer: LayerId) -> Result<bool, crate::LayerMuteError> {
        self.mute_and_unmute_layers(&[layer], &[])
    }

    /// Queue restoring a muted layer. Returns false for an already unmuted layer.
    #[doc(alias = "UsdStage::UnmuteLayer")]
    #[doc(alias = "UnmuteLayer")]
    pub fn unmute_layer(&mut self, layer: LayerId) -> bool {
        self.mute_and_unmute_layers(&[], &[layer])
            .expect("unmuting cannot fail")
    }

    /// Select a different host-owned session root, or detach it with `None`.
    /// Synchronization is explicit, as for other stage controls. Switching a
    /// session changes the local stack globally and requires a full rebuild.
    pub fn set_session_layer(&mut self, session: Option<LayerId>) -> bool {
        if self.options.session_layer == session {
            return false;
        }
        self.options.session_layer = session;
        self.notify_structural_change();
        true
    }

    /// Queue a new subtree population mask. None includes the full namespace.
    #[doc(alias = "UsdStage::SetPopulationMask")]
    #[doc(alias = "SetPopulationMask")]
    pub fn set_population_mask(&mut self, mask: Option<PopulationMask>) -> bool {
        if self.options.mask == mask {
            return false;
        }
        self.options.mask = mask;
        self.notify_structural_change();
        true
    }

    /// Queue replacement payload inclusion rules without changing stored assets.
    #[doc(alias = "UsdStage::SetLoadRules")]
    #[doc(alias = "SetLoadRules")]
    pub fn set_load_rules(&mut self, rules: crate::PayloadLoadRules) -> bool {
        if self.options.load_rules == rules {
            return false;
        }
        self.options.load_rules = rules;
        self.controls_pending = true;
        true
    }

    /// Queue loading a payload prim with or without its descendants.
    /// Required ancestor payloads are included by the effective-rule fold.
    pub fn load(
        &mut self,
        store: &dyn LayerStore,
        path: PathId,
        policy: crate::LoadPolicy,
    ) -> bool {
        let mut rules = self.options.load_rules.clone();
        let path = store.paths().resolve(path).clone();
        if policy == crate::LoadPolicy::WithDescendants {
            rules.load_with_descendants(path);
        } else {
            rules.load_without_descendants(path);
        }
        self.set_load_rules(rules)
    }

    /// Queue unloading payloads at and beneath the composed path.
    pub fn unload(&mut self, store: &dyn LayerStore, path: PathId) -> bool {
        let mut rules = self.options.load_rules.clone();
        rules.unload(store.paths().resolve(path).clone());
        self.set_load_rules(rules)
    }

    /// Queue one batch, applying unloads before loads as in OpenUSD.
    /// Recomposition occurs once when the caller synchronizes the stage.
    pub fn load_and_unload(
        &mut self,
        store: &dyn LayerStore,
        load: &[PathId],
        unload: &[PathId],
        policy: crate::LoadPolicy,
    ) -> bool {
        let mut rules = self.options.load_rules.clone();
        for p in unload {
            rules.unload(store.paths().resolve(*p).clone());
        }
        for p in load {
            let path = store.paths().resolve(*p).clone();
            if policy == crate::LoadPolicy::WithDescendants {
                rules.load_with_descendants(path);
            } else {
                rules.load_without_descendants(path);
            }
        }
        self.set_load_rules(rules)
    }

    /// Applies `txn` to the layers of `store` (see [`Transaction::apply`])
    /// and updates the prims it affects: the entry point for authoring a
    /// live stage.
    ///
    /// Edits of opinions on existing specs are notified as edits of their
    /// source sites ([`LiveStage::notify_layer_prim_edits`]), so only the
    /// prims drawing on those specs are updated. Existing attribute defaults
    /// and samples refresh their cached opinions without rebuilding prim
    /// graphs when source mappings are exact and no other changes are pending.
    /// Local prim creation and removal recompose the affected subtrees when
    /// the stage has one layer, all authored ancestors, no population mask,
    /// and no arcs, variants, relocates or instanceable specs. The work also
    /// includes ancestor composition and the changed boundary child lists to
    /// preserve ordering and pruning. Unchanged sibling indexes are retained;
    /// processing a wide boundary list still costs work proportional to it.
    /// Shared/session and referenced structural edits map retained authored
    /// change paths through their source sites and replace complete dependent
    /// subtrees. Unknown edits, expired source history, relocations and global
    /// composition errors conservatively rebuild the stage.
    ///
    /// [`Applied::changes`] separates exact created/removed prim inventories,
    /// subtree resync roots and changes that invalidate only the named prim.
    /// Pending notifications are included in that report. The conservative
    /// full-rebuild path reports a pseudo-root resync.
    ///
    /// Stage addresses take the declared type of an attribute they create
    /// from this stage's composed declaration of the property
    /// ([`crate::Stage::resolve_property_declaration`]), as
    /// `UsdAttribute::Set` does.
    ///
    /// On error nothing was applied and nothing is recomposed.
    pub fn apply(
        &mut self,
        store: &mut dyn LayerStore,
        txn: &Transaction,
    ) -> Result<Applied, EditError> {
        let pending_opinions = self.tracker.has_invalidated(OPINION_EDIT);
        let pending_recomposition = self.needs_full_rebuild
            || pending_opinions
            || self.controls_pending
            || !self.structural_roots.is_empty()
            || !self.structural_sources.is_empty()
            || !self.support_sources.is_empty();
        // Precision is valid only against the source generations we composed.
        // Unknown external edits must not be hidden behind this transaction.
        let sources_current = self
            .generations
            .iter()
            .all(|(&layer, seen)| generations_of(store, layer) == *seen);
        let local_current = !pending_recomposition
            && self
                .local_namespace
                .as_ref()
                .is_some_and(|index| index.current(store));
        let namespace = local_current
            .then_some(self.local_namespace.as_ref())
            .flatten()
            .map(|index| index as &dyn crate::edit::SourceNamespace);
        let outcome = crate::edit::apply(store, txn, Some(&self.stage), namespace)?;
        self.reset_work();
        // Prims whose dynamic payload arguments the edits may change are
        // recomposed, though no value refresh or source mapping finds them.
        let argument_dependents = if sources_current {
            self.argument_dependents(store, &outcome.argument_inputs)
        } else {
            Vec::new()
        };
        let mut property_changes = alloc::collections::BTreeMap::new();
        if sources_current
            && !pending_recomposition
            && let Some(properties) = &outcome.properties
        {
            for &(layer, source, field) in properties {
                for prim in self.prims_for_source(layer, source) {
                    property_changes
                        .entry(prim)
                        .or_insert_with(Vec::new)
                        .push(field);
                }
            }
        }
        let declaration_resyncs: Vec<_> = outcome
            .resync_sites
            .iter()
            .flat_map(|&(layer, path)| self.prims_for_source(layer, path))
            .collect();
        if outcome.structural
            && local_current
            && outcome.layers.iter().all(|layer| *layer == self.root)
            && let Some(paths) = &outcome.local_structure
            && !paths.is_empty()
            && let Some((mut recomposed, mut changes)) =
                self.refresh_local_structure(store, paths, &outcome.touched)
        {
            include_resyncs(store, &mut changes, &declaration_resyncs);
            self.notices.publish(&mut changes, &self.stage, store);
            store.paths().sort_paths(&mut recomposed, store.tokens());
            return Ok(Applied {
                inverse: outcome.inverse,
                recomposed,
                changes,
            });
        }
        // An unsupported or externally changed namespace cannot reuse its
        // source index until a full composition establishes eligibility again.
        if !local_current || outcome.structural || outcome.local_structure.is_none() {
            self.local_namespace = None;
        } else if let Some(index) = &mut self.local_namespace
            && let Some(layer) = store.layer(self.root)
        {
            index.generation = layer.generation();
        }
        // Property values do not change prim indexes, dependency edges or
        // namespace. Do not rediscover population to refresh those slots.
        let refreshed = outcome.values.as_ref().and_then(|edits| {
            if pending_recomposition || !argument_dependents.is_empty() {
                return None;
            }
            let dependents: Vec<Vec<PathId>> = edits
                .iter()
                .map(|edit| {
                    self.prims_for_source(edit.layer, edit.path.prim_path())
                        .collect()
                })
                .collect();
            let mut affected: Vec<PathId> = dependents.iter().flatten().copied().collect();
            affected.sort_unstable();
            affected.dedup();
            // An unnotified source edit may have changed more than this
            // transaction's slots. Leave it to ordinary recomposition.
            for prim in &affected {
                for layer in self
                    .prim_to_layers
                    .get(prim)
                    .into_iter()
                    .flat_map(|layers| layers.iter())
                {
                    let Some(Some(seen)) = self.generations.get(layer) else {
                        return None;
                    };
                    let expected = (seen.0 + u64::from(outcome.layers.contains(layer)), seen.1);
                    if generations_of(store, *layer) != Some(expected) {
                        return None;
                    }
                }
            }
            self.stage.refresh_values(store, edits, &dependents)
        });
        if refreshed.is_none() {
            if outcome.structural
                && sources_current
                && !pending_opinions
                && !self.needs_full_rebuild
            {
                for &layer in &outcome.layers {
                    let changes = self
                        .generations
                        .get(&layer)
                        .copied()
                        .flatten()
                        .and_then(|seen| source_changes(store, layer, seen.0));
                    match changes {
                        Some((paths, supports)) => {
                            self.queue_source_structure(store, layer, &paths, &supports);
                        }
                        None => self.notify_structural_change(),
                    }
                    self.generations.insert(layer, generations_of(store, layer));
                }
            } else if outcome.structural {
                self.notify_structural_change();
            } else if sources_current {
                for &(layer, prim) in &outcome.touched {
                    self.mark_source_prims(layer, &[prim]);
                }
                for &prim in &argument_dependents {
                    self.tracker.mark(prim, OPINION_EDIT);
                }
            } else {
                for &(layer, prim) in &outcome.touched {
                    self.notify_layer_prim_edits(layer, &[prim]);
                }
            }
        }
        // A value-only transaction moved each layer's generation once, and
        // it is notified; an earlier unnotified edit of the layer stays
        // visible to `notify_changed_layers`. A structural one rebuilds,
        // which sees every layer afresh.
        for layer in &outcome.layers {
            if let (Some(Some(seen)), Some(found)) = (
                self.generations.get_mut(layer),
                generations_of(store, *layer),
            ) && (seen.0 + 1, seen.1) == found
            {
                *seen = found;
            }
        }
        let mut changes = Changes::default();
        let mut recomposed = match refreshed {
            Some(paths) => {
                changes.changed_info_only = paths.clone();
                paths
            }
            None => self.recompose_report(store, Some(&mut changes)),
        };
        include_resyncs(store, &mut changes, &declaration_resyncs);
        // External notifications identify affected prims but do not describe
        // their edits. They may have changed declarations or schema identity;
        // only transaction-known value edits can promise info-only changes.
        if pending_opinions {
            include_resyncs(store, &mut changes, &recomposed);
        }
        changes.property_changes = property_changes
            .into_iter()
            .filter(|(prim, _)| changes.changed_info_only.contains(prim))
            .map(|(prim, fields)| crate::PrimPropertyChanges { prim, fields })
            .collect();
        self.notices.publish(&mut changes, &self.stage, store);
        store.paths().sort_paths(&mut recomposed, store.tokens());
        Ok(Applied {
            inverse: outcome.inverse,
            recomposed,
            changes,
        })
    }

    /// Updates local namespace regions using the source index. Composition of
    /// each candidate still uses the same graph construction and finalization.
    fn refresh_local_structure(
        &mut self,
        store: &mut dyn LayerStore,
        paths: &[PathId],
        touched: &[(LayerId, PathId)],
    ) -> Option<(Vec<PathId>, Changes)> {
        let source_empty = store.layer(self.root)?.prims.is_empty();
        // Ordinary population of an empty layer has no pseudo-root either.
        // Include its old index in the removal batch instead of retaining a
        // synthetic root from the supporting ancestor seed.
        let roots = if source_empty {
            alloc::vec![store.paths_mut().intern(crate::Path::root())]
        } else {
            minimal_roots(store, paths)
        };
        let mut old = HashSet::new();
        for root in &roots {
            if self.stage.has_prim(*root) {
                old.extend(self.stage.traverse(*root));
            }
        }
        let index = self.local_namespace.as_mut()?;
        if !index.reconcile(store, paths) {
            return None;
        }
        let mut affected = old;
        for root in &roots {
            affected.extend(index.subtree(*root));
        }
        let hierarchy: Vec<_> = affected.iter().copied().collect();
        let in_hierarchy = affected.clone();
        affected.extend(touched.iter().map(|(_, path)| *path));
        let mut parents: Vec<PathId> = roots
            .iter()
            .filter_map(|root| {
                store
                    .paths()
                    .resolve(*root)
                    .parent()
                    .and_then(|p| store.paths().lookup(&p))
            })
            .filter(|parent| !in_hierarchy.contains(parent))
            .collect();
        parents.sort_unstable();
        parents.dedup();
        // Supporting ancestors supply activation context. A boundary parent
        // keeps its retained index, which a sibling added or removed does
        // not change; the partial composition composes its child names
        // once, for its whole child list.
        let boundaries: HashSet<PathId> = parents.iter().copied().collect();
        let mut seed: BTreeSet<PathId> = affected.iter().copied().collect();
        for &path in &affected {
            let mut current = store.paths().resolve(path).parent();
            while let Some(parent) = current {
                let parent_id = store.paths_mut().intern(parent.clone());
                seed.insert(parent_id);
                current = parent.parent();
            }
        }
        // Deleted specs are removal candidates, not population candidates.
        // Seeding them would leave an empty child-list entry on a parent
        // whose clean population has no child-list entry at all.
        let pseudo_root = store.paths_mut().intern(crate::Path::root());
        seed.retain(|path| {
            *path == pseudo_root
                || store
                    .layer(self.root)
                    .is_some_and(|l| l.prims.contains_key(path))
        });
        seed.insert(pseudo_root);
        if source_empty {
            seed.clear();
        }
        let options = StageOptions {
            with_dependencies: true,
            ..self.options.clone()
        };
        let mut partial = Stage::compose_local_paths(
            store,
            self.root,
            options,
            seed,
            &boundaries,
            &self.child_index,
        );
        self.work.composed_prim_indexes += partial.composition_work().composed_prim_indexes;
        // A parent the partial composition did not index keeps the source
        // children it holds, ordered against its retained index.
        let boundary_children: Vec<_> = parents
            .iter()
            .map(|&parent| {
                let given = if partial.has_boundary_children(parent) {
                    Vec::new()
                } else {
                    self.local_namespace
                        .as_ref()
                        .map_or_else(Vec::new, |index| index.children(parent).to_vec())
                };
                (parent, given)
            })
            .collect();
        let deps = partial.take_deps().unwrap_or_default();
        let mut changes = Changes::default();
        for path in &affected {
            match (self.stage.has_prim(*path), partial.has_prim(*path)) {
                (false, true) => changes.created.push(*path),
                (true, false) => changes.removed.push(*path),
                _ => {}
            }
        }
        changes.resynced = roots
            .into_iter()
            .filter(|root| self.stage.has_prim(*root) || partial.has_prim(*root))
            .collect();
        let resynced: HashSet<PathId> = changes.resynced.iter().copied().collect();
        changes.changed_info_only = parents
            .iter()
            .copied()
            .chain(touched.iter().map(|(_, path)| *path))
            .filter(|path| partial.has_prim(*path) && !is_beneath_set(store, *path, &resynced))
            .collect();
        let mut affected: Vec<_> = affected.into_iter().collect();
        affected.sort_unstable();
        self.work.replaced_prim_indexes = affected.len();
        let orders = self.stage.merge_local_subtrees(
            store,
            partial,
            &affected,
            &hierarchy,
            boundary_children,
            &affected,
            &self.child_index,
        );
        self.record_boundary_orders(orders);
        self.update_prim_edges(&affected, &deps);
        for &path in &affected {
            if !self.stage.has_prim(path) {
                self.tracker.remove_key(path);
            }
            self.reindex_sources(path);
        }
        self.generations
            .insert(self.root, generations_of(store, self.root));
        let mut recomposed = affected;
        recomposed.extend(parents);
        recomposed.sort_unstable();
        recomposed.dedup();
        Some((recomposed, changes))
    }

    /// Notifies the edits of every layer the stage reads whose
    /// [`Layer::generation`](crate::Layer::generation) moved since this
    /// stage last saw it, and returns those layers, sorted.
    ///
    /// A layer whose [`Layer::structural_generation`](crate::Layer::structural_generation)
    /// moved too may have gained or lost specs, children, arcs or variant
    /// sets. Layer methods and transactions retain prim paths for the most
    /// recent 64 authored batches; complete history maps structural changes to
    /// dependent subtrees and opinion changes to their contributing prims.
    /// Unknown edits, expired history, and layer arrival/removal use conservative
    /// layer notifications or full structural rebuilds.
    ///
    /// The layers the stage reads are its root layer stack and every layer
    /// stack a reference or payload authored in them targets, including
    /// layers that contribute no opinions yet, such as an empty sublayer.
    /// The stage sees their generations when it composes or rebuilds, and
    /// the generations its own [`apply`](Self::apply) moves. So after edits
    /// made through [`Layer`](crate::Layer) methods by code that does not
    /// notify the stage, this finds the layers they changed, and the next
    /// [`recompose`](Self::recompose) stops serving what those layers no
    /// longer hold.
    ///
    /// Each stage keeps its own generation cursor; polling one client does
    /// not consume another client's authored evidence. A host that separately
    /// notified an edit can still see its layer reported here. Every
    /// mutating [`Layer`](crate::Layer) method records its edit, so no
    /// write goes unseen.
    ///
    /// OpenUSD: `UsdStage` handles `SdfNotice::LayersDidChange`, resyncing
    /// the prims of significant changes and updating the info of the rest.
    pub fn notify_changed_layers(&mut self, store: &dyn LayerStore) -> Vec<LayerId> {
        let mut changed: Vec<(LayerId, Option<LayerGenerations>)> = self
            .generations
            .iter()
            .map(|(layer, seen)| (*layer, *seen, generations_of(store, *layer)))
            .filter(|(_, seen, found)| seen != found)
            .map(|(layer, _, found)| (layer, found))
            .collect();
        changed.sort_unstable_by_key(|(layer, _)| *layer);
        for &(layer, found) in &changed {
            let seen = self.generations.insert(layer, found).flatten();
            match (seen, found) {
                (Some(seen), Some(found)) => match source_changes(store, layer, seen.0) {
                    Some((paths, _)) if seen.1 == found.1 => {
                        self.notify_layer_prim_edits(layer, &paths);
                    }
                    Some((paths, supports)) => {
                        self.queue_source_structure(store, layer, &paths, &supports);
                    }
                    None if seen.1 == found.1 => self.notify_layer_edit(layer),
                    None => self.notify_structural_change(),
                },
                _ => self.notify_structural_change(),
            }
        }
        changed.into_iter().map(|(layer, _)| layer).collect()
    }

    /// Records the generations of every layer the stage reads.
    fn record_generations(&mut self, store: &dyn LayerStore) {
        let mut layers =
            self.layer_discovery
                .participating(store, self.root, &self.options.muted_layers);
        if let Some(session) = self.options.session_layer {
            layers.extend(self.layer_discovery.participating(
                store,
                session,
                &self.options.muted_layers,
            ));
            if self.layer_discovery.has_expressions() {
                for walked in crate::expression_variables::walk_identifier(
                    store,
                    crate::LayerStackIdentifier {
                        root: self.root,
                        session: Some(session),
                    },
                )
                .stacks
                {
                    if walked.stack.chains.iter().any(|chain| {
                        chain.iter().any(|id| {
                            self.options.muted_layers.contains(&id.root)
                                || id
                                    .session
                                    .is_some_and(|s| self.options.muted_layers.contains(&s))
                        })
                    }) {
                        continue;
                    }
                    layers.extend(
                        walked
                            .stack
                            .layers
                            .into_iter()
                            .filter(|id| !self.options.muted_layers.contains(id)),
                    );
                }
            }
        }
        layers.extend(self.stage.clip_layers());
        // A dynamic payload's layer is bound by the identifier its composed
        // arguments form, which no authored arc spells: watch it, and what
        // it reads, as composition found them.
        for layer in self.stage.dynamic_layers() {
            layers.extend(self.layer_discovery.participating(
                store,
                layer,
                &self.options.muted_layers,
            ));
        }
        self.generations = layers
            .into_iter()
            .map(|layer| (layer, generations_of(store, layer)))
            .collect();
    }

    /// Notifies that opinions in `layer` have been edited.
    ///
    /// Marks all prims that receive opinions from this layer, or that a
    /// reference or payload authored in it reaches, as invalidation roots.
    /// Propagation to transitive dependents is deferred to
    /// [`recompose`](Self::recompose).
    ///
    /// An edit to the offset or scale a layer stack reads the layer with
    /// (its entry in a parent's sublayers) retimes those same prims; notify
    /// it here for the layer and for each of its own sublayers.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (sublayer offsets apply to the arcs the
    /// layer authors).
    pub fn notify_layer_edit(&mut self, layer: LayerId) {
        if self.options.muted_layers.contains(&layer) {
            return;
        }
        if let Some(prims) = self.layer_to_prims.get(&layer) {
            for &prim in prims {
                self.tracker.mark(prim, OPINION_EDIT);
            }
        }
    }

    /// Notifies that the prim specs at `prims` within `layer` have had
    /// opinions edited.
    ///
    /// `prims` are source paths in `layer`'s own namespace, not composed
    /// stage paths. Each is mapped through the composed prim indexes to every
    /// composed prim that draws a spec or opinion from that site (see
    /// [`composed_prims_for_source`](Self::composed_prims_for_source)); those
    /// prims are marked dirty, and their transitive dependents are expanded
    /// at drain time. For example, editing `/Source` in a library layer that
    /// is referenced at `/A` marks `/A`.
    ///
    /// This is more precise than [`notify_layer_edit`](Self::notify_layer_edit).
    /// Source paths that no composed prim currently draws on are ignored:
    /// adding a spec there, or editing composition arcs, is a structural
    /// change and must be reported with
    /// [`notify_structural_change`](Self::notify_structural_change).
    ///
    /// A prim whose dynamic payload arguments were composed from one of the
    /// specs is marked too ([`Stage::dynamic_dependency`]): the edit may have
    /// changed them, and so the layer its payload targets.
    pub fn notify_layer_prim_edits(&mut self, layer: LayerId, prims: &[PathId]) {
        self.mark_source_prims(layer, prims);
        if self.options.muted_layers.contains(&layer) {
            return;
        }
        for &source in prims {
            if let Some(dests) = self.dynamic_to_prims.get(&(layer, source)) {
                for &prim in dests {
                    self.tracker.mark(prim, OPINION_EDIT);
                }
            }
        }
    }

    /// Marks the composed prims drawing specs or opinions from `prims`
    /// of `layer` ([`Self::notify_layer_prim_edits`] without the dynamic
    /// argument dependents).
    fn mark_source_prims(&mut self, layer: LayerId, prims: &[PathId]) {
        if self.options.muted_layers.contains(&layer) {
            return;
        }
        for &source in prims {
            if let Some(dests) = self.source_to_prims.get(&(layer, source)) {
                for &prim in dests {
                    self.tracker.mark(prim, OPINION_EDIT);
                }
            }
        }
    }

    /// The composed prims whose dynamic payload arguments `inputs` may
    /// change: those that composed them from an edited spec, for which the
    /// edited field or attribute default is relevant and the format says
    /// the change may affect them ([`crate::DynamicDependency::can_field_change_affect_args`]).
    ///
    /// OpenUSD: `Pcp_DoesInfoChangeAffectFileFormatArguments` and
    /// `Pcp_DoesAttributeChangeAffectFileFormatArguments`
    /// (`pxr/usd/pcp/changes.cpp`).
    fn argument_dependents(
        &self,
        store: &dyn LayerStore,
        inputs: &[crate::edit::ArgumentInput],
    ) -> Vec<PathId> {
        use crate::edit::ArgumentName;
        let mut out = Vec::new();
        for input in inputs {
            if self.options.muted_layers.contains(&input.layer) {
                continue;
            }
            let Some(dests) = self.dynamic_to_prims.get(&(input.layer, input.prim)) else {
                continue;
            };
            for &prim in dests {
                let Some(dependency) = self.stage.dynamic_dependency(prim) else {
                    continue;
                };
                let (old, new) = (input.old.as_ref(), input.new.as_ref());
                let affects = match input.name {
                    ArgumentName::Field(field) => dependency.can_field_change_affect_args(
                        store.tokens().resolve(field),
                        old,
                        new,
                    ),
                    ArgumentName::Default(name) => dependency
                        .can_attribute_default_change_affect_args(
                            store.tokens().resolve(name),
                            old,
                            new,
                        ),
                };
                if affects {
                    out.push(prim);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Returns the composed prims whose prim index draws specs or opinions
    /// from the prim spec at `source` within `layer`, in namespace order
    /// ([`crate::PathInterner::cmp_paths`]).
    ///
    /// The mapping is taken from the composed prim indexes (local opinions,
    /// variants, and every arc, including nested ones), so a library prim
    /// referenced from several places maps to each referencing prim. It
    /// reflects the last composition; it is updated by
    /// [`recompose`](Self::recompose).
    #[must_use]
    pub fn composed_prims_for_source(
        &self,
        layer: LayerId,
        source: PathId,
        store: &dyn LayerStore,
    ) -> Vec<PathId> {
        let mut prims: Vec<PathId> = self.prims_for_source(layer, source).collect();
        store.paths().sort_paths(&mut prims, store.tokens());
        prims
    }

    /// The prims of [`Self::composed_prims_for_source`], in no order.
    fn prims_for_source(&self, layer: LayerId, source: PathId) -> impl Iterator<Item = PathId> {
        self.source_to_prims
            .get(&(layer, source))
            .into_iter()
            .flat_map(|set| set.iter().copied())
    }

    /// Notifies that a specific prim's opinions have changed (any layer).
    ///
    /// This is more precise than [`notify_layer_edit`](Self::notify_layer_edit):
    /// only the named prim is marked dirty (plus its transitive dependents at
    /// drain time), rather than every prim that receives opinions from the layer.
    pub fn notify_prim_edit(&mut self, prim: PathId) {
        self.tracker.mark(prim, OPINION_EDIT);
    }

    /// Batch-marks multiple prims as dirty (any layer).
    ///
    /// Equivalent to calling [`notify_prim_edit`](Self::notify_prim_edit) for
    /// each prim, but more convenient for bulk edits.
    pub fn notify_prim_edits(&mut self, prims: &[PathId]) {
        for &prim in prims {
            self.tracker.mark(prim, OPINION_EDIT);
        }
    }

    /// Notifies that the `defaultPrim` of `layer` was authored, changed or
    /// cleared.
    ///
    /// A reference or payload with no authored prim path targets the prim
    /// `defaultPrim` names, so the change retargets every such arc to
    /// `layer`: the prims it composes into gain and lose children. When any
    /// composed prim has such an arc (see
    /// [`prims_using_default_prim`](Self::prims_using_default_prim)), including
    /// one that did not resolve, the next [`recompose`](Self::recompose)
    /// rebuilds the whole stage, as for
    /// [`notify_structural_change`](Self::notify_structural_change).
    /// Otherwise nothing depends on the change and nothing is recomposed.
    ///
    /// Spec: AOUSD Core §10.3.2.1 (an omitted prim path assumes the
    /// `defaultPrim` of the target layer). OpenUSD treats a `defaultPrim`
    /// change as a resync of the prims that depend on it (`PcpChanges::DidChange`
    /// in `pxr/usd/pcp/changes.cpp`).
    pub fn notify_default_prim_edit(&mut self, layer: LayerId) {
        if self
            .default_prim_dependents
            .get(&layer)
            .is_some_and(|prims| !prims.is_empty())
        {
            self.needs_full_rebuild = true;
        }
    }

    /// Returns the composed prims with a reference or payload, authored or
    /// reached through other arcs, that targets the `defaultPrim` of `layer`,
    /// sorted by [`PathId`]. Prims whose arc did not resolve are included.
    ///
    /// These are the prims a
    /// [`notify_default_prim_edit`](Self::notify_default_prim_edit) of
    /// `layer` recomposes. The set reflects the last composition.
    #[must_use]
    pub fn prims_using_default_prim(&self, layer: LayerId, store: &dyn LayerStore) -> Vec<PathId> {
        let mut prims: Vec<PathId> = self
            .default_prim_dependents
            .get(&layer)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        store.paths().sort_paths(&mut prims, store.tokens());
        prims
    }

    /// Notifies that the `layerRelocates` metadata of `layer` was authored,
    /// changed or cleared.
    ///
    /// Relocates move prims within the namespace of every layer stack
    /// holding the layer, and of every namespace those layer stacks are
    /// mapped into, so when the stage composes any layer stack holding
    /// `layer` the next [`recompose`](Self::recompose) rebuilds the whole
    /// stage, as for
    /// [`notify_structural_change`](Self::notify_structural_change).
    /// Otherwise nothing depends on the change and nothing is recomposed.
    ///
    /// Opinion edits at relocation sources and targets need no such
    /// notification: [`notify_layer_prim_edits`](Self::notify_layer_prim_edits)
    /// recomposes the relocated prims that read them.
    ///
    /// Spec: AOUSD Core §10.3.2.6. OpenUSD treats a relocates change as a
    /// significant change of the layer stack (`PcpChanges::DidChange` in
    /// `pxr/usd/pcp/changes.cpp`).
    pub fn notify_relocates_edit(&mut self, layer: LayerId) {
        if self.relocation_layers.contains(&layer) {
            self.needs_full_rebuild = true;
        }
    }

    /// Notifies that the `expressionVariables` metadata of `layer` was
    /// authored, changed or cleared.
    ///
    /// Composition evaluates the variable expressions of asset paths and
    /// variant selections with the expression variables of the layer
    /// stacks that author them, which the root layers of those layer
    /// stacks, and of the layer stacks referencing them, provide (see
    /// [`crate::variable_expression`]). When a variable composition read
    /// from `layer` now has another value in `store`, or is now set where
    /// composition found it unset, the next [`recompose`](Self::recompose)
    /// rebuilds the whole stage, as for
    /// [`notify_structural_change`](Self::notify_structural_change).
    /// Otherwise nothing depends on the change and nothing is recomposed.
    ///
    /// OpenUSD records the variables each layer stack's expressions use
    /// (`PcpExpressionVariablesDependencyData`) and resyncs the prims that
    /// use a changed one (`PcpChanges::DidChange` in
    /// `pxr/usd/pcp/changes.cpp`).
    pub fn notify_expression_variables_edit(&mut self, store: &dyn LayerStore, layer: LayerId) {
        if self.expression_variables.changed(store, layer) {
            self.needs_full_rebuild = true;
        }
    }

    /// Notifies that a structural change occurred (prims added/removed, arcs changed).
    ///
    /// This forces a full rebuild on the next [`recompose`](Self::recompose)
    /// call, which also indexes the child specs of every layer anew rather
    /// than trusting the generations it indexed them at.
    pub fn notify_structural_change(&mut self) {
        self.needs_full_rebuild = true;
        self.child_index.clear();
    }

    /// Recomposes affected prims and returns the set of prims that were updated.
    ///
    /// - If a structural change was notified, performs a full rebuild and
    ///   returns every prim path of the new stage plus every path that the
    ///   previous stage had and the new one lacks (removed prims), in
    ///   namespace order ([`crate::PathInterner::cmp_paths`]). Removal is computed as a before/after difference; callers
    ///   can tell removed paths apart with [`Stage::has_prim`]. Surviving
    ///   paths are reported whether or not they changed: the rebuild does not
    ///   classify value, hierarchy, or asset changes per path.
    /// - If no prims are invalidated, returns an empty vec.
    /// - Otherwise, drains the invalidation set (expanding lazy roots to all
    ///   transitive dependents), performs a scoped recomposition, and updates
    ///   the dependency graph incrementally for the affected prims. Only the
    ///   affected prims' indexes are replaced; the hierarchy is kept.
    /// - If the scoped recomposition changes an affected prim's contributing
    ///   specs (for example another variant branch is selected), its
    ///   descendants and their dependents are recomposed too, and returned
    ///   with the affected prims.
    /// - If the scoped recomposition shows that an affected prim appeared,
    ///   disappeared, or changed its children (for example `active` or child
    ///   reordering edits), replaces complete affected subtrees. Unknown scope,
    ///   relocations and global composition errors require a full rebuild.
    ///
    /// Edits that introduce prims this stage has never populated (new specs,
    /// new arcs, a variant selection that adds children) are not visible to a
    /// scoped recomposition and must be reported with
    /// [`notify_structural_change`](Self::notify_structural_change).
    /// [`LiveStage::apply`] reports its edits itself, and
    /// [`notify_changed_layers`](Self::notify_changed_layers) finds layers
    /// edited without a notification.
    pub fn recompose(&mut self, store: &mut dyn LayerStore) -> Vec<PathId> {
        let mut changes = Changes::default();
        let mut paths = self.recompose_report(store, Some(&mut changes));
        let info_only = core::mem::take(&mut changes.changed_info_only);
        include_resyncs(store, &mut changes, &info_only);
        self.notices.publish(&mut changes, &self.stage, store);
        store.paths().sort_paths(&mut paths, store.tokens());
        paths
    }

    /// Recomposes pending external edits and returns conservative change evidence.
    ///
    /// Call `notify_changed_layers` or an explicit notification first. External
    /// edits do not carry field inventories, so affected prims are resynced.
    /// Unlike `recompose`, this distinguishes removals and subtree invalidation.
    pub fn recompose_changes(&mut self, store: &mut dyn LayerStore) -> Changes {
        let mut changes = Changes::default();
        self.recompose_report(store, Some(&mut changes));
        let info_only = core::mem::take(&mut changes.changed_info_only);
        include_resyncs(store, &mut changes, &info_only);
        self.notices.publish(&mut changes, &self.stage, store);
        changes
    }

    /// Detects source edits and recomposes before exposing a current scene.
    ///
    /// Scans participating layer generations once per call. Layer methods and
    /// transactions need no observer-specific authoring session. Every
    /// mutating [`crate::Layer`] method records its edit. Unknown source
    /// changes produce conservative resyncs. Like OpenUSD's layer notices,
    /// evidence belongs to the stage, independently of its consumers; unlike
    /// OpenUSD's synchronous dispatch, synchronization here is explicit.
    pub fn synchronize(&mut self, store: &mut dyn LayerStore) {
        self.notify_changed_layers(store);
        self.recompose_changes(store);
    }

    /// Registers a callback after each completed composed change batch.
    ///
    /// Runs synchronously after `apply`, `recompose`, `recompose_changes` or
    /// `synchronize` updates the stage. Source edits outside the stage are
    /// discovered at synchronization, not at mutation time. Callbacks may inspect
    /// the notice's stage/store but cannot mutate them while borrowed. Queue
    /// follow-up edits for after this call returns. Callbacks run in registration
    /// order; a panic propagates and skips later callbacks, but history is already
    /// recorded. Callbacks remain installed until unsubscribed or the stage drops.
    /// Send + Sync preserve `LiveStage`'s thread-transfer traits; dispatch itself
    /// is synchronous and never starts threads.
    pub fn subscribe_changes(
        &mut self,
        callback: impl FnMut(ChangeNotice<'_>) + Send + Sync + 'static,
    ) -> ChangeSubscription {
        self.notices.subscribe(callback)
    }

    /// Removes a callback. Returns false for an absent or foreign subscription.
    pub fn unsubscribe_changes(&mut self, subscription: &ChangeSubscription) -> bool {
        self.notices.unsubscribe(subscription)
    }

    /// Starts an independent observer at the current composed revision.
    ///
    /// Reports are retained only after the first cursor is created. The default
    /// budget is 64 batches and 1 MiB. Missing evidence causes an explicit history
    /// error requiring derived-state recovery. No callbacks are registered.
    pub fn change_cursor(&mut self) -> ChangeCursor {
        self.notices.cursor()
    }

    /// Changes composed-report retention and immediately evicts excess evidence.
    /// This does not change source layers' independent authored-evidence budgets.
    /// Oversized reports still notify callbacks but cannot be replayed by cursors.
    pub fn set_change_history_budget(&mut self, budget: crate::ChangeHistoryBudget) {
        self.notices.set_budget(budget);
    }
    /// Current composed-report retention budget.
    pub fn change_history_budget(&self) -> crate::ChangeHistoryBudget {
        self.notices.budget()
    }
    /// Report-storage memory and cumulative retention work. No report clones
    /// are retained before the first change cursor enables replay.
    pub fn change_history_stats(&self) -> crate::ChangeHistoryStats {
        self.notices.stats()
    }

    /// Borrows reports since this cursor and advances it to the current revision.
    ///
    /// Reading never consumes another observer's reports. An expired cursor is
    /// advanced too: the caller must rebuild before using it again. A cursor
    /// from another stage is rejected without modification. The returned
    /// iterator must be processed in full; dropping it discards the remainder
    /// for this cursor. Call [`Self::synchronize`] first to detect source edits.
    pub fn changes_since<'a>(
        &'a self,
        cursor: &mut ChangeCursor,
    ) -> Result<impl Iterator<Item = &'a Changes> + use<'a>, ChangeHistoryError> {
        self.notices.read(cursor)
    }

    fn recompose_report(
        &mut self,
        store: &mut dyn LayerStore,
        mut changes: Option<&mut Changes>,
    ) -> Vec<PathId> {
        self.reset_work();
        // Store snapshots preserve old numeric IDs but have independent future
        // interning domains. Layer generations alone cannot detect publication
        // of that snapshot; refresh all retained affinity and dependency evidence.
        self.needs_full_rebuild |= self.stage.store_identity() != Some(&store.identity());
        if self.needs_full_rebuild {
            return self.full_rebuild(store, changes);
        }
        if self.controls_pending
            || !self.structural_roots.is_empty()
            || !self.structural_sources.is_empty()
            || !self.support_sources.is_empty()
        {
            return self.recompose_controls(store, changes);
        }

        if !self.tracker.has_invalidated(OPINION_EDIT) {
            return Vec::new();
        }

        // Drain with lazy expansion: roots → all transitive dependents.
        let mut affected: Vec<PathId> = self
            .tracker
            .drain(OPINION_EDIT)
            .affected()
            .scratch(&mut self.traversal_scratch)
            .run()
            .collect();
        let mut resynced_roots = Vec::new();
        let mut partial = loop {
            let partial = self.compose_scoped(store, &affected);
            self.work.composed_prim_indexes += partial.composition_work().composed_prim_indexes;

            // An opinion edit that turns out to change hierarchy (activation,
            // child ordering, a dynamic payload's arguments, ...) cannot be
            // patched from a masked composition, whose child lists are
            // partial by construction.
            if self.stage.hierarchy_diverges(&partial, &affected) {
                return self.recompose_diverging(store, partial, affected, changes);
            }

            // A prim whose contributing specs change (another variant
            // branch selected) changes its descendants' prim indexes too:
            // recompose them, and what depends on them, as well.
            if changes.is_some() {
                resynced_roots.extend(
                    affected
                        .iter()
                        .copied()
                        .filter(|path| self.stage.sources_changed(&partial, *path)),
                );
            }
            let resynced = self.stage.resynced_descendants(&partial, &affected);
            if resynced.is_empty() {
                break partial;
            }
            for prim in resynced {
                self.tracker.mark(prim, OPINION_EDIT);
            }
            affected.extend(
                self.tracker
                    .drain(OPINION_EDIT)
                    .affected()
                    .scratch(&mut self.traversal_scratch)
                    .run(),
            );
            affected.sort_unstable();
            affected.dedup();
        };

        // Extract partial dependency data before merging the stage.
        let mut partial_deps = partial.take_deps().unwrap_or_default();
        self.expression_variables
            .extend(core::mem::take(&mut partial_deps.expression_variables));

        self.work.replaced_prim_indexes = affected.len();
        // Replace only the recomposed prim indexes; hierarchy is unchanged.
        self.stage
            .merge_prims_from(store, partial, &affected, &affected);
        // A metadata-only clip edit can introduce an already resident layer.
        // Start watching it without acknowledging unrelated, unnotified edits
        // to layers whose generations were already tracked.
        for layer in self.stage.clip_layers() {
            self.generations
                .entry(layer)
                .or_insert_with(|| generations_of(store, layer));
        }

        self.update_prim_edges(&affected, &partial_deps);
        for &prim in &affected {
            self.reindex_sources(prim);
        }

        if let Some(changes) = changes.as_mut() {
            changes.resynced = minimal_roots(store, &resynced_roots);
            let resynced: HashSet<PathId> = changes.resynced.iter().copied().collect();
            changes.changed_info_only = affected
                .iter()
                .copied()
                .filter(|path| !is_beneath_set(store, *path, &resynced))
                .collect();
        }
        affected
    }

    /// Recomposes `affected`, composed as `partial`, when some of them
    /// change hierarchy or contributing specs: those are resynced with
    /// their namespace ([`Self::recompose_controls`]); the rest, outside
    /// their subtrees, keep the indexes `partial` composed for them, as an
    /// opinion edit does, so a batch's other edits are not resynced with
    /// them.
    fn recompose_diverging(
        &mut self,
        store: &mut dyn LayerStore,
        mut partial: Stage,
        affected: Vec<PathId>,
        mut changes: Option<&mut Changes>,
    ) -> Vec<PathId> {
        let (diverging, rest): (Vec<PathId>, Vec<PathId>) =
            affected.iter().copied().partition(|prim| {
                self.stage.hierarchy_diverges(&partial, &[*prim])
                    || self.stage.sources_changed(&partial, *prim)
            });
        let rest: Vec<PathId> = rest
            .into_iter()
            .filter(|prim| {
                !is_beneath(store, *prim, &diverging)
                    && !diverging
                        .iter()
                        .any(|root| is_beneath(store, *root, &[*prim]))
            })
            .collect();
        if rest.is_empty() {
            self.structural_roots.extend(affected);
            return self.recompose_controls(store, changes);
        }
        let mut partial_deps = partial.take_deps().unwrap_or_default();
        self.expression_variables
            .extend(core::mem::take(&mut partial_deps.expression_variables));
        self.stage.merge_prims_from(store, partial, &rest, &rest);
        self.update_prim_edges(&rest, &partial_deps);
        for &prim in &rest {
            self.reindex_sources(prim);
        }
        let replaced = rest.len();
        self.structural_roots.extend(diverging);
        let mut recomposed = self.recompose_controls(store, changes.as_deref_mut());
        self.work.replaced_prim_indexes += replaced;
        if let Some(changes) = changes {
            changes.changed_info_only.extend(
                rest.iter()
                    .copied()
                    .filter(|prim| !is_beneath(store, *prim, &changes.resynced)),
            );
        }
        recomposed.extend(rest);
        recomposed.sort_unstable();
        recomposed.dedup();
        recomposed
    }

    /// Physical sharing of this retained client's layer-contributor sets.
    /// Excludes reverse indexes, invalidation topology and allocator overhead.
    #[must_use]
    pub fn layer_dependency_storage(&self) -> crate::LayerDependencyStorage {
        self.prim_to_layers.storage()
    }

    /// Returns a reference to the underlying composed stage.
    #[must_use]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// The composed stage bound to `store`, which holds the layers it was
    /// composed from (see [`crate::StageView`]).
    #[must_use]
    pub fn view<'a>(&'a self, store: &'a dyn LayerStore) -> crate::StageView<'a> {
        self.stage.view(store)
    }

    /// Replaces dependency data for the affected batch, leaving other
    /// dependents intact. Scan global metadata once, not once per prim.
    fn update_prim_edges(&mut self, affected: &[PathId], partial: &CompositionDeps) {
        // Relocation tables are consulted even when currently empty. A newly
        // introduced stack must receive future table-change notifications too;
        // retain conservative watches until a full rebuild re-establishes them.
        self.relocation_layers
            .extend(partial.relocation_layers.iter().copied());
        let affected_set: HashSet<_> = affected.iter().copied().collect();
        self.arc_metadata
            .retain(|arc| !affected_set.contains(&arc.target));
        let mut new_arcs: Vec<_> = partial
            .arcs
            .iter()
            .filter(|arc| affected_set.contains(&arc.target))
            .copied()
            .collect();
        new_arcs.sort_unstable_by_key(|arc| arc.target);

        for &prim in affected {
            let start = new_arcs.partition_point(|arc| arc.target < prim);
            let end = new_arcs.partition_point(|arc| arc.target <= prim);
            self.update_prim_graph_edges(prim, partial, &new_arcs[start..end]);
        }
        self.arc_metadata.extend(new_arcs);

        // Scoped composition also holds supporting source prims. Only
        // replace dependencies for prims whose indexes were merged.
        for dependents in self.default_prim_dependents.values_mut() {
            dependents.retain(|prim| !affected_set.contains(prim));
        }
        for (layer, dependents) in &partial.default_prim_dependents {
            for prim in dependents
                .iter()
                .filter(|&prim| affected_set.contains(prim))
            {
                self.default_prim_dependents
                    .entry(*layer)
                    .or_default()
                    .insert(*prim);
            }
        }
        self.default_prim_dependents
            .retain(|_, dependents| !dependents.is_empty());
    }

    /// Replaces one dependent's graph and layer edges from the partial
    /// composition, using the batch's already grouped arc metadata.
    fn update_prim_graph_edges(
        &mut self,
        prim: PathId,
        partial: &CompositionDeps,
        new_arcs: &[ArcDependency],
    ) {
        // Value edits normally keep the same topology. Let invalidation
        // preserve those edges, and cycle-check only genuinely new ones.
        let _ = self.tracker.replace_dependencies(
            prim,
            OPINION_EDIT,
            new_arcs.iter().map(|arc| arc.source),
        );

        if self.prim_to_layers.get(&prim) == partial.prim_to_layers.get(&prim) {
            return;
        }

        // Remove old layer-opinion edges for this prim.
        if let Some(old_layers) = self.prim_to_layers.remove(&prim) {
            for layer in old_layers.iter() {
                if let Some(prim_set) = self.layer_to_prims.get_mut(layer) {
                    prim_set.remove(&prim);
                }
            }
        }

        // Add new layer-opinion edges from the partial composition.
        if let Some(partial_layers) = partial.prim_to_layers.get(&prim) {
            for &layer in partial_layers.iter() {
                self.layer_to_prims.entry(layer).or_default().insert(prim);
            }
            self.prim_to_layers.set_shared(prim, partial_layers);
        }
    }

    /// Composes `affected` with a population mask that also holds the arc
    /// sources they draw on and their current children.
    ///
    /// The arc sources let composition read inherit and reference targets;
    /// the children make the masked composition's child lists for
    /// `affected` complete, so [`Stage::hierarchy_diverges`] can detect
    /// hierarchy changes.
    fn compose_scoped(&mut self, store: &mut dyn LayerStore, affected: &[PathId]) -> Stage {
        let mut mask_set: HashSet<PathId> = HashSet::from_iter(affected.iter().copied());
        for &prim in affected {
            for dep in self.tracker.graph().dependencies(prim, OPINION_EDIT) {
                mask_set.insert(dep);
            }
            mask_set.extend(self.stage.children_of(prim).unwrap_or(&[]).iter().copied());
        }
        let mask_vec: Vec<PathId> = mask_set.into_iter().collect();
        let scoped_opts = StageOptions {
            session_layer: self.options.session_layer,
            mask: Some(PopulationMask { include: mask_vec }),
            with_provenance: self.options.with_provenance,
            with_dependencies: true,
            variant_fallbacks: self.options.variant_fallbacks.clone(),
            schemas: self.options.schemas.clone(),
            load_rules: self.options.load_rules.clone(),
            muted_layers: self.options.muted_layers.clone(),
            threads: self.options.threads,
            dynamic_formats: self.options.dynamic_formats.clone(),
        };
        Stage::compose_retained(
            store,
            self.root,
            scoped_opts,
            true,
            &HashSet::new(),
            &self.child_index,
        )
    }

    /// Recomposes the whole stage and returns every path in the new stage
    /// plus every path removed relative to the old one (see
    /// [`recompose`](Self::recompose)), sorted by [`PathId`].
    fn full_rebuild(
        &mut self,
        store: &mut dyn LayerStore,
        changes: Option<&mut Changes>,
    ) -> Vec<PathId> {
        self.needs_full_rebuild = false;
        self.controls_pending = false;
        self.structural_roots.clear();
        self.structural_sources.clear();
        self.support_sources.clear();
        self.layer_discovery = discovery::LayerDiscovery::default();
        self.tracker.clear(OPINION_EDIT);
        let old_prims: HashSet<PathId> = self.stage.prim_paths().collect();

        let opts = StageOptions {
            with_dependencies: true,
            ..self.options.clone()
        };
        let mut stage = Stage::compose_retained(
            store,
            self.root,
            opts,
            false,
            &HashSet::new(),
            &self.child_index,
        );
        let deps = stage.take_deps().unwrap_or_default();

        self.work.full_rebuild = true;
        self.work.composed_prim_indexes += stage.composition_work().composed_prim_indexes;
        self.work.replaced_prim_indexes = old_prims.len()
            + stage
                .prim_paths()
                .filter(|p| !old_prims.contains(p))
                .count();
        self.stage = stage;
        self.tracker =
            InvalidationTracker::from_graph_with_cycle_handling(deps.graph, CycleHandling::Ignore);
        self.arc_metadata = deps.arcs;
        self.layer_to_prims = deps.layer_to_prims;
        self.prim_to_layers = deps.prim_to_layers;
        self.default_prim_dependents = deps.default_prim_dependents;
        self.relocation_layers = deps.relocation_layers;
        self.expression_variables = deps.expression_variables;
        self.reindex_all_sources();
        self.record_generations(store);
        self.local_namespace = local::LocalNamespace::build(store, self.root, &self.options);
        if let Some(changes) = changes {
            changes.created = self
                .stage
                .prim_paths()
                .filter(|path| !old_prims.contains(path))
                .collect();
            changes.removed = old_prims
                .iter()
                .copied()
                .filter(|path| !self.stage.has_prim(*path))
                .collect();
            changes.resynced = alloc::vec![store.paths_mut().intern(crate::Path::root())];
        }

        // A before/after difference, not an edit log: removed paths are those
        // the old stage had and the new one lacks.
        let mut changed: Vec<PathId> = self.stage.prim_paths().collect();
        changed.extend(old_prims.into_iter().filter(|p| !self.stage.has_prim(*p)));
        changed.sort_unstable();
        changed
    }

    /// Rebuilds the source-site index from the whole stage.
    fn reindex_all_sources(&mut self) {
        self.source_to_prims.clear();
        self.prim_to_sources.clear();
        self.dynamic_to_prims.clear();
        self.prim_to_dynamic.clear();
        let prims: Vec<PathId> = self.stage.prim_paths().collect();
        for prim in prims {
            self.reindex_sources(prim);
        }
    }

    /// Replaces the source-site index entries for `prim` with those of its
    /// current prim index.
    fn reindex_sources(&mut self, prim: PathId) {
        let dynamic = self.stage.dynamic_sites(prim);
        reindex(
            &mut self.dynamic_to_prims,
            &mut self.prim_to_dynamic,
            prim,
            dynamic,
        );
        let sites = self.stage.source_sites(prim);
        if self
            .prim_to_sources
            .get(&prim)
            .is_some_and(|old| *old == sites)
        {
            return;
        }

        if let Some(old) = self.prim_to_sources.remove(&prim) {
            for site in old {
                if let Some(dests) = self.source_to_prims.get_mut(&site) {
                    dests.remove(&prim);
                    if dests.is_empty() {
                        self.source_to_prims.remove(&site);
                    }
                }
            }
        }
        for &site in &sites {
            self.source_to_prims.entry(site).or_default().insert(prim);
        }
        if !sites.is_empty() {
            self.prim_to_sources.insert(prim, sites);
        }
    }
}

/// Replaces `prim`'s entries in a site → prims index and its inverse with
/// `sites`.
fn reindex(
    site_to_prims: &mut HashMap<(LayerId, PathId), HashSet<PathId>>,
    prim_to_sites: &mut HashMap<PathId, Vec<(LayerId, PathId)>>,
    prim: PathId,
    sites: Vec<(LayerId, PathId)>,
) {
    if prim_to_sites.get(&prim).is_some_and(|old| *old == sites) {
        return;
    }
    if let Some(old) = prim_to_sites.remove(&prim) {
        for site in old {
            if let Some(dests) = site_to_prims.get_mut(&site) {
                dests.remove(&prim);
                if dests.is_empty() {
                    site_to_prims.remove(&site);
                }
            }
        }
    }
    for &site in &sites {
        site_to_prims.entry(site).or_default().insert(prim);
    }
    if !sites.is_empty() {
        prim_to_sites.insert(prim, sites);
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec;

    use super::*;
    use crate::{
        FieldValue, HashMap, Layer, PrimSpec, PropertySpec, Reference, SublayerEntry, Value,
        doc::InMemoryStore,
        interner::TokenId,
        path::{Path, PropertyPath},
    };

    fn p(store: &mut InMemoryStore, s: &str) -> PathId {
        let path = Path::parse_absolute(s, &store.tokens).expect("valid path");
        store.paths.intern(path)
    }

    /// An attribute spec authoring only the default `value`.
    fn attr(value: i64) -> PropertySpec {
        PropertySpec::attribute().with_default(value)
    }

    /// Layer 1 references `/R` of layer 2 from `/A` and relocates `/A/B`
    /// to `/A/C`; layer 2's `/R/B` authors `x = 1`.
    fn relocated_scene(store: &mut InMemoryStore) -> (PathId, PathId, TokenId) {
        let field_x = store.tokens.intern("x");
        let (a, b, c) = (p(store, "/A"), p(store, "/A/B"), p(store, "/A/C"));
        let (r, r_b) = (p(store, "/R"), p(store, "/R/B"));
        let mut root = Layer::new(LayerId(1));
        root.relocates = vec![crate::Relocate {
            source: b,
            target: Some(c),
        }];
        let mut referencing = PrimSpec::def();
        referencing.references.explicit = Some(vec![Reference::new(LayerId(2), r)]);
        root.insert_prim(a, referencing);
        store.insert_layer(root);
        let mut referenced = Layer::new(LayerId(2));
        referenced.insert_prim(r, PrimSpec::def());
        let mut child = PrimSpec::def();
        child.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        referenced.insert_prim(r_b, child);
        store.insert_layer(referenced);
        (b, c, field_x)
    }

    #[test]
    fn layer_contributor_sharing_survives_incremental_namespace_growth() {
        use crate::{EditTarget, Specifier};
        let mut store = InMemoryStore::default();
        let world = store.path("/World");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(world, PrimSpec::def());
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        for i in 0..32 {
            let child = store.path(&alloc::format!("/World/C{i}"));
            let mut transaction = Transaction::new();
            transaction.create_prim(
                EditTarget::for_layer(LayerId(1)).prim(child),
                Specifier::Def,
                None,
            );
            live.apply(&mut store, &transaction).unwrap();
            assert!(live.stage().has_prim(child));
            let storage = live.layer_dependency_storage();
            assert_eq!(storage.unique_layer_sets, 1);
            assert_eq!(storage.physical_layer_references, 1);
            assert_eq!(storage.logical_layer_references, storage.prims);
        }
        let fresh = Stage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                with_dependencies: true,
                ..Default::default()
            },
        );
        assert_eq!(
            live.layer_dependency_storage(),
            fresh.layer_dependency_storage().unwrap()
        );
    }

    /// Asserts that `live` composes what a fresh composition of `store`
    /// does: every prim, its prim stack, its children and `value`.
    fn assert_fresh(live: &LiveStage, store: &mut InMemoryStore, value: TokenId) {
        let fresh = Stage::compose(store, LayerId(1), StageOptions::default());
        let mut prims: Vec<_> = fresh.prim_paths().collect();
        let mut live_prims: Vec<_> = live.stage().prim_paths().collect();
        prims.sort_unstable();
        live_prims.sort_unstable();
        assert_eq!(live_prims, prims);
        for prim in prims {
            assert_eq!(
                live.stage().prim_stack(prim),
                fresh.prim_stack(prim),
                "{prim:?}"
            );
            assert_eq!(
                live.stage().children_of(prim),
                fresh.children_of(prim),
                "{prim:?}"
            );
            let property = PropertyPath::new(prim, value);
            assert_eq!(
                live.stage().resolve_field_path(property).map(|r| r.value),
                fresh.resolve_field_path(property).map(|r| r.value),
                "{prim:?}"
            );
        }
    }

    /// Authoring at a deep prim creates an inert `over` for each ancestor
    /// the layer has no spec for; those change the ancestors' spec stacks,
    /// not their namespace, so only the edited prim's subtree recomposes.
    ///
    /// Spec: AOUSD Core §10 (namespace composition), §10.3.2.5 (variant
    /// selections). OpenUSD treats an added inert prim spec as a spec stack
    /// change of that prim (`PcpChanges::DidChange`).
    #[test]
    fn authoring_beneath_referenced_ancestors_recomposes_only_the_edited_subtree() {
        use crate::{EditTarget, VariantSetSpec, VariantSpec};
        let mut store = InMemoryStore::default();
        let value = store.tokens.intern("value");
        let look = store.tokens.intern("look");
        let (a, b) = (store.tokens.intern("a"), store.tokens.intern("b"));
        let set = p(&mut store, "/Set");
        let mut asset = Layer::new(LayerId(2));
        let names = ["Other", "Model", "Last"];
        let children: Vec<_> = names.iter().map(|n| store.tokens.intern(n)).collect();
        asset.insert_prim(
            set,
            PrimSpec::def().with_children(vec![
                store.tokens.intern("Shelf"),
                store.tokens.intern("Side"),
            ]),
        );
        let shelf = p(&mut store, "/Set/Shelf");
        asset.insert_prim(shelf, PrimSpec::def().with_children(children));
        for name in ["Other", "Last"] {
            let path = p(&mut store, &alloc::format!("/Set/Shelf/{name}"));
            asset.insert_prim(path, PrimSpec::def().with_property(value, attr(0)));
        }
        let side = p(&mut store, "/Set/Side");
        asset.insert_prim(side, PrimSpec::def().with_property(value, attr(0)));
        let model = p(&mut store, "/Set/Shelf/Model");
        let branch = |v: i64| VariantSpec {
            properties: vec![crate::property::PropertyEntry {
                name: value,
                spec: Arc::new(attr(v)),
            }],
            ..VariantSpec::default()
        };
        let mut spec = PrimSpec::def();
        spec.variant_sets.insert(
            look,
            VariantSetSpec {
                variants: [(a, branch(1)), (b, branch(2))].into_iter().collect(),
            },
        );
        spec.variant_set_names.prepend.push(look);
        spec.variant_selections.insert(look, a);
        asset.insert_prim(model, spec);
        store.insert_layer(asset);
        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            set,
            PrimSpec::def().with_reference(Reference::with_asset(LayerId(2), set, "asset.usda")),
        );
        store.insert_layer(root);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        let mut edit = Transaction::new();
        edit.set_variant_selection(EditTarget::for_layer(LayerId(1)).prim(model), look, Some(b));
        let applied = live.apply(&mut store, &edit).unwrap();
        assert!(
            store.layers[&LayerId(1)].prims.contains_key(&shelf),
            "an over supports /Set/Shelf/Model"
        );
        let work = live.recomposition_work();
        assert!(!work.full_rebuild, "{work:?}");
        for sibling in ["/Set/Side", "/Set/Shelf/Other", "/Set/Shelf/Last"] {
            let sibling = p(&mut store, sibling);
            assert!(
                !applied.recomposed.contains(&sibling),
                "{sibling:?} in {:?}",
                applied.recomposed
            );
        }
        assert_eq!(applied.changes.resynced, [model]);
        assert_eq!(applied.changes.changed_info_only, [shelf]);
        assert_fresh(&live, &mut store, value);
        // The stronger `over` lists only `Model`; the composed order stays
        // the weaker layer's, as in OpenUSD 26.08.
        let order: Vec<_> = names
            .iter()
            .map(|n| p(&mut store, &alloc::format!("/Set/Shelf/{n}")))
            .collect();
        assert_eq!(live.stage().children_of(shelf), Some(order.as_slice()));

        // Undoing removes the supporting `over` again.
        let undone = live.apply(&mut store, &applied.inverse).unwrap();
        assert!(!store.layers[&LayerId(1)].prims.contains_key(&shelf));
        assert!(!live.recomposition_work().full_rebuild);
        for sibling in ["/Set/Side", "/Set/Shelf/Other", "/Set/Shelf/Last"] {
            let sibling = p(&mut store, sibling);
            assert!(
                !undone.recomposed.contains(&sibling),
                "{sibling:?} in {:?}",
                undone.recomposed
            );
        }
        assert_fresh(&live, &mut store, value);
    }

    #[test]
    fn relocated_opinion_edits_recompose_the_relocated_prim() {
        // Spec: AOUSD Core §10.3.2.6. `/A/C` reads `/R/B` through the
        // relocation; an edit there recomposes it in scope.
        let mut store = InMemoryStore::default();
        let (_, c, field_x) = relocated_scene(&mut store);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let r_b = p(&mut store, "/R/B");
        assert_eq!(live.composed_prims_for_source(LayerId(2), r_b, &store), [c]);

        let spec = store
            .layers
            .get_mut(&LayerId(2))
            .and_then(|layer| layer.prims.get_mut(&r_b))
            .expect("spec");
        spec.set_field(field_x, FieldValue::Value(Value::Int64(2)));
        live.notify_layer_prim_edits(LayerId(2), &[r_b]);
        assert_eq!(live.recompose(&mut store), [c]);
        assert_eq!(
            live.stage().resolve_field(c, field_x).map(|r| r.value),
            Some(Value::Int64(2))
        );
    }

    #[test]
    fn relocates_edits_rebuild_stages_holding_the_layer() {
        let mut store = InMemoryStore::default();
        let (b, c, _) = relocated_scene(&mut store);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert!(live.stage().has_prim(c) && !live.stage().has_prim(b));

        store
            .layers
            .get_mut(&LayerId(1))
            .expect("root layer")
            .relocates
            .clear();
        // No composed layer stack holds layer 3.
        live.notify_relocates_edit(LayerId(3));
        assert!(live.recompose(&mut store).is_empty());

        live.notify_relocates_edit(LayerId(1));
        live.recompose(&mut store);
        assert!(live.stage().has_prim(b) && !live.stage().has_prim(c));
    }

    /// Relocates cleared through the crate-internal fields and `touch` are
    /// found by polling, which rebuilds the namespace they moved.
    #[test]
    fn polled_relocates_edit_rebuilds_namespace() {
        let mut store = InMemoryStore::default();
        let (b, c, field_x) = relocated_scene(&mut store);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let root = store.layers.get_mut(&LayerId(1)).expect("root layer");
        root.relocates.clear();
        root.touch();
        assert_eq!(live.notify_changed_layers(&store), [LayerId(1)]);
        live.recompose(&mut store);
        assert!(live.stage().has_prim(b) && !live.stage().has_prim(c));
        assert_matches_fresh(&live, &mut store, &[field_x]);
    }

    /// Edits through `Layer` methods move generations, which
    /// `notify_changed_layers` turns into layer notifications; writes into
    /// the crate-internal fields are not seen.
    #[test]
    fn changed_layers_are_found_by_generation() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let (a, b) = (p(&mut store, "/A"), p(&mut store, "/B"));
        let mut root = Layer::new(LayerId(1));
        root.sublayers.push(SublayerEntry::new(LayerId(2)));
        root.insert_prim(a, PrimSpec::def().with_property(field_x, attr(1)));
        store.insert_layer(root);
        let mut sub = Layer::new(LayerId(2));
        sub.insert_prim(b, PrimSpec::def().with_property(field_x, attr(1)));
        store.insert_layer(sub);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(live.notify_changed_layers(&store), [], "nothing changed");

        let layer = store.layers.get_mut(&LayerId(2)).unwrap();
        layer.set_property(PropertyPath::new(b, field_x), attr(2));
        assert_eq!(live.notify_changed_layers(&store), [LayerId(2)]);
        assert_eq!(
            live.recompose(&mut store),
            [b],
            "the layer's prims recompose"
        );
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_eq!(live.notify_changed_layers(&store), [], "reported once");

        let layer = store.layers.get_mut(&LayerId(1)).unwrap();
        Arc::make_mut(&mut layer.prims.get_mut(&a).unwrap().properties[0].spec).default =
            Some(Value::Int64(3));
        assert_eq!(
            live.notify_changed_layers(&store),
            [],
            "a field write moves no generation"
        );
        layer_touch(&mut store, LayerId(1));
        assert_eq!(live.notify_changed_layers(&store), [LayerId(1)]);
        live.recompose(&mut store);
        assert_matches_fresh(&live, &mut store, &[field_x]);
    }

    /// A scene whose root layer has an empty sublayer (3) and references
    /// `/Rock` of an asset layer (2) at `/World/Rock`, and the live stage
    /// composed from it.
    fn rock_scene() -> (InMemoryStore, LiveStage, TokenId) {
        let mut store = InMemoryStore::default();
        let size = store.tokens.intern("size");
        let (world, world_rock, rock) = (
            p(&mut store, "/World"),
            p(&mut store, "/World/Rock"),
            p(&mut store, "/Rock"),
        );
        let (world_name, rock_name) = (store.tokens.intern("World"), store.tokens.intern("Rock"));
        let root_path = p(&mut store, "/");
        let mut root = Layer::new(LayerId(1));
        root.sublayers.push(SublayerEntry::new(LayerId(3)));
        root.insert_prim(
            root_path,
            PrimSpec::default().with_children(vec![world_name]),
        );
        root.insert_prim(world, PrimSpec::def().with_children(vec![rock_name]));
        root.insert_prim(
            world_rock,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), rock)),
        );
        store.insert_layer(root);
        let mut asset = Layer::new(LayerId(2));
        asset.insert_prim(rock, PrimSpec::def().with_property(size, attr(1)));
        store.insert_layer(asset);
        store.insert_layer(Layer::new(LayerId(3)));
        let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        (store, live, size)
    }

    /// Polls, recomposes and checks the stage against a fresh composition.
    fn poll(live: &mut LiveStage, store: &mut InMemoryStore, expected: &[LayerId], size: TokenId) {
        assert_eq!(live.notify_changed_layers(store), expected);
        live.recompose(store);
        assert_matches_fresh(live, store, &[size]);
        assert_eq!(live.notify_changed_layers(store), [], "reported once");
    }

    /// A prim spec added to a referenced layer adds a composed prim: the
    /// poll rebuilds the namespace, not only the prims drawing on the
    /// layer.
    #[test]
    fn polled_spec_insertion_rebuilds_namespace() {
        let (mut store, mut live, size) = rock_scene();
        let pebble = p(&mut store, "/Rock/Pebble");
        let pebble_name = store.tokens.intern("Pebble");
        let rock = p(&mut store, "/Rock");
        let asset = store.layers.get_mut(&LayerId(2)).unwrap();
        asset.insert_prim(pebble, PrimSpec::def().with_property(size, attr(2)));
        asset
            .prims
            .get_mut(&rock)
            .unwrap()
            .authored_children
            .push(pebble_name);
        poll(&mut live, &mut store, &[LayerId(2)], size);
        let world_pebble = p(&mut store, "/World/Rock/Pebble");
        assert!(live.stage().has_prim(world_pebble));
    }

    /// A prim spec removed, through the crate-internal fields and `touch`,
    /// removes its composed prim.
    #[test]
    fn polled_spec_removal_rebuilds_namespace() {
        let (mut store, mut live, size) = rock_scene();
        let world_rock = p(&mut store, "/World/Rock");
        let rock_name = store.tokens.intern("Rock");
        let world = p(&mut store, "/World");
        let root = store.layers.get_mut(&LayerId(1)).unwrap();
        root.prims.remove(&world_rock);
        root.prims
            .get_mut(&world)
            .unwrap()
            .authored_children
            .retain(|c| *c != rock_name);
        root.touch();
        poll(&mut live, &mut store, &[LayerId(1)], size);
        assert!(!live.stage().has_prim(world_rock));
    }

    /// A reference retargeted by replacing the prim spec that authors it
    /// changes the arc, and the prim's opinions follow it.
    #[test]
    fn polled_arc_change_recomposes_through_the_new_target() {
        let (mut store, mut live, size) = rock_scene();
        let (world_rock, boulder) = (p(&mut store, "/World/Rock"), p(&mut store, "/Boulder"));
        let asset = store.layers.get_mut(&LayerId(2)).unwrap();
        asset.insert_prim(boulder, PrimSpec::def().with_property(size, attr(5)));
        store.layers.get_mut(&LayerId(1)).unwrap().insert_prim(
            world_rock,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), boulder)),
        );
        poll(&mut live, &mut store, &[LayerId(1), LayerId(2)], size);
        let value = live
            .stage()
            .resolve_property_path(PropertyPath::new(world_rock, size))
            .unwrap()
            .value;
        assert_eq!(value, crate::ResolvedValue::Scalar(Value::Int64(5)));
    }

    /// A sublayer that is empty when the stage composes is still read by
    /// it: a prim spec added there later is found.
    #[test]
    fn polled_initially_empty_sublayer_is_tracked() {
        let (mut store, mut live, size) = rock_scene();
        let (added, added_name) = (p(&mut store, "/Added"), store.tokens.intern("Added"));
        let root_path = p(&mut store, "/");
        let sub = store.layers.get_mut(&LayerId(3)).unwrap();
        sub.insert_prim(
            root_path,
            PrimSpec::default().with_children(vec![added_name]),
        );
        sub.insert_prim(added, PrimSpec::def().with_property(size, attr(3)));
        poll(&mut live, &mut store, &[LayerId(3)], size);
        assert!(live.stage().has_prim(added));
    }

    /// A layer a reference targets that the store does not hold yet is
    /// tracked too, and its arrival rebuilds.
    #[test]
    fn polled_layer_arrival_rebuilds() {
        let (mut store, mut live, size) = rock_scene();
        let (world_rock, rock) = (p(&mut store, "/World/Rock"), p(&mut store, "/Rock"));
        let root = store.layers.get_mut(&LayerId(1)).unwrap();
        root.insert_prim(
            world_rock,
            PrimSpec::def()
                .with_reference(Reference::new(LayerId(2), rock))
                .with_reference(Reference::new(LayerId(4), rock)),
        );
        poll(&mut live, &mut store, &[LayerId(1)], size);
        let mut late = Layer::new(LayerId(4));
        late.insert_prim(rock, PrimSpec::def().with_property(size, attr(9)));
        store.insert_layer(late);
        poll(&mut live, &mut store, &[LayerId(4)], size);
    }

    fn layer_touch(store: &mut InMemoryStore, id: LayerId) {
        store.layers.get_mut(&id).unwrap().touch();
    }

    #[test]
    fn live_stage_matches_full_compose() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim = p(&mut store, "/P");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut spec = PrimSpec::default();
        spec.set_field(field_x, FieldValue::Value(Value::Int64(42)));
        layer.insert_prim(prim, spec);
        store.insert_layer(layer);

        let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let full = Stage::compose(&mut store, LayerId(1), StageOptions::default());

        assert_eq!(
            live.stage().resolve_field(prim, field_x).unwrap().value,
            full.resolve_field(prim, field_x).unwrap().value,
        );
    }

    #[test]
    fn noop_recompose_returns_empty() {
        let mut store = InMemoryStore::default();
        let prim = p(&mut store, "/P");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        layer.insert_prim(prim, PrimSpec::default());
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let updated = live.recompose(&mut store);
        assert!(updated.is_empty(), "no changes should mean no updates");
    }

    #[test]
    fn opinion_edit_recomposes_affected_prim() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim = p(&mut store, "/P");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut spec = PrimSpec::default();
        spec.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        layer.insert_prim(prim, spec);
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(
            live.stage().resolve_field(prim, field_x).unwrap().value,
            Value::Int64(1)
        );

        // Mutate the layer in the store.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(99)));
        }

        // Notify and recompose.
        live.notify_layer_edit(LayerId(1));
        let updated = live.recompose(&mut store);

        assert!(updated.contains(&prim), "affected prim should be returned");
        assert_eq!(
            live.stage().resolve_field(prim, field_x).unwrap().value,
            Value::Int64(99)
        );
    }

    #[test]
    fn scoped_recompose_keeps_arc_cycle_errors() {
        // `/P/C` inherits its parent: an arc cycle reported on `/P/C`.
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let parent = p(&mut store, "/P");
        let child = p(&mut store, "/P/C");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(parent, PrimSpec::def());
        layer.insert_prim(child, PrimSpec::def().with_inherit(parent));
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let errors = live.stage().composition_errors().to_vec();
        assert_eq!(errors.len(), 1, "one cycle: {errors:?}");

        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&child).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        }
        live.notify_prim_edit(child);
        let updated = live.recompose(&mut store);

        assert!(updated.contains(&child), "the edited prim is recomposed");
        assert_eq!(live.stage().composition_errors(), errors.as_slice());
    }

    #[test]
    fn structural_change_triggers_full_rebuild() {
        let mut store = InMemoryStore::default();
        let prim = p(&mut store, "/P");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        layer.insert_prim(prim, PrimSpec::default());
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Add a new prim to the store.
        let prim_q = p(&mut store, "/Q");
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            layer.insert_prim(prim_q, PrimSpec::default());
        }

        live.notify_structural_change();
        let updated = live.recompose(&mut store);

        assert!(!updated.is_empty(), "full rebuild should return prims");
        assert!(
            live.stage().has_prim(prim_q),
            "new prim should be in the stage"
        );
    }

    /// A structural rebuild reports removed prims as well as the new stage's
    /// prims, computed as a before/after difference.
    #[test]
    fn structural_rebuild_reports_removed_prims() {
        let mut store = InMemoryStore::default();
        let a = p(&mut store, "/A");
        let b = p(&mut store, "/B");
        let b_child = p(&mut store, "/B/Child");
        let c = p(&mut store, "/C");
        let root = p(&mut store, "/");

        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(a, PrimSpec::def());
        layer.insert_prim(b, PrimSpec::def());
        layer.insert_prim(b_child, PrimSpec::def());
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            layer.prims.remove(&b);
            layer.prims.remove(&b_child);
            layer.insert_prim(c, PrimSpec::def());
        }
        live.notify_structural_change();
        let updated = live.recompose(&mut store);

        let mut expected = vec![root, a, b, b_child, c];
        store.paths.sort_paths(&mut expected, &store.tokens);
        assert_eq!(updated, expected, "new-stage paths plus removed paths");
        let removed: Vec<PathId> = updated
            .iter()
            .copied()
            .filter(|path| !live.stage().has_prim(*path))
            .collect();
        let expected_removed = vec![b, b_child];
        assert_eq!(removed, expected_removed, "removed paths are identifiable");
    }

    #[test]
    fn arc_dependency_propagates_through_reference() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim_p = p(&mut store, "/P");
        let prim_q = p(&mut store, "/Q");

        // P references Q via LayerId(2).
        let mut root = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut p_spec = PrimSpec::default();
        p_spec.add_reference(Reference::new(LayerId(2), prim_q));
        root.insert_prim(prim_p, p_spec);
        store.insert_layer(root);

        let mut ref_layer = Layer {
            id: LayerId(2),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut q_spec = PrimSpec::default();
        q_spec.set_field(field_x, FieldValue::Value(Value::Int64(10)));
        ref_layer.insert_prim(prim_q, q_spec);
        store.insert_layer(ref_layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(
            live.stage().resolve_field(prim_p, field_x).unwrap().value,
            Value::Int64(10)
        );

        // Edit the referenced layer.
        {
            let layer = store.layers.get_mut(&LayerId(2)).unwrap();
            let spec = layer.prims.get_mut(&prim_q).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(77)));
        }

        live.notify_layer_edit(LayerId(2));
        let updated = live.recompose(&mut store);

        // P should be updated because it depends on Q through the reference arc.
        assert!(
            updated.contains(&prim_p) || updated.contains(&prim_q),
            "arc dependents should be updated"
        );
        assert_eq!(
            live.stage().resolve_field(prim_p, field_x).unwrap().value,
            Value::Int64(77)
        );
    }

    #[test]
    fn multi_layer_opinion_edit_strongest_wins() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim = p(&mut store, "/P");

        // Layer 2 is a sublayer of layer 1. Layer 1 is stronger.
        let mut layer2 = Layer {
            id: LayerId(2),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut spec2 = PrimSpec::default();
        spec2.set_field(field_x, FieldValue::Value(Value::Int64(10)));
        layer2.insert_prim(prim, spec2);
        store.insert_layer(layer2);

        let mut layer1 = Layer {
            id: LayerId(1),
            sublayers: vec![SublayerEntry::new(LayerId(2))],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut spec1 = PrimSpec::default();
        spec1.set_field(field_x, FieldValue::Value(Value::Int64(20)));
        layer1.insert_prim(prim, spec1);
        store.insert_layer(layer1);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        // Layer 1 is stronger, so x = 20.
        assert_eq!(
            live.stage().resolve_field(prim, field_x).unwrap().value,
            Value::Int64(20)
        );

        // Edit the weaker layer — value should stay 20.
        {
            let layer = store.layers.get_mut(&LayerId(2)).unwrap();
            let spec = layer.prims.get_mut(&prim).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(99)));
        }
        live.notify_layer_edit(LayerId(2));
        let updated = live.recompose(&mut store);
        assert!(updated.contains(&prim));
        assert_eq!(
            live.stage().resolve_field(prim, field_x).unwrap().value,
            Value::Int64(20),
            "stronger layer opinion should still win"
        );

        // Now edit the stronger layer.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(55)));
        }
        live.notify_layer_edit(LayerId(1));
        let updated = live.recompose(&mut store);
        assert!(updated.contains(&prim));
        assert_eq!(
            live.stage().resolve_field(prim, field_x).unwrap().value,
            Value::Int64(55),
            "updated stronger opinion should resolve"
        );
    }

    #[test]
    fn inherits_arc_propagation() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let class_c = p(&mut store, "/Class_C");
        let prim_p = p(&mut store, "/P");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        // /Class_C defines x = 42.
        let mut class_spec = PrimSpec::default();
        class_spec.set_field(field_x, FieldValue::Value(Value::Int64(42)));
        layer.insert_prim(class_c, class_spec);
        // /P inherits from /Class_C.
        let mut p_spec = PrimSpec::default();
        p_spec.add_inherit(class_c);
        layer.insert_prim(prim_p, p_spec);
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(
            live.stage().resolve_field(prim_p, field_x).unwrap().value,
            Value::Int64(42),
            "P should inherit x from Class_C"
        );

        // Edit the class prim's opinion.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&class_c).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(100)));
        }
        live.notify_layer_edit(LayerId(1));
        let updated = live.recompose(&mut store);

        assert_eq!(
            live.stage().resolve_field(prim_p, field_x).unwrap().value,
            Value::Int64(100),
            "P should see updated inherited value"
        );
        // Both class_c and prim_p should be in the affected set.
        assert!(
            updated.contains(&class_c),
            "class prim should be in affected set"
        );
    }

    #[test]
    fn batch_notifications_single_recompose() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let field_y = store.tokens.intern("y");
        let prim_a = p(&mut store, "/A");
        let prim_b = p(&mut store, "/B");

        // Two independent prims across two layers.
        let mut layer1 = Layer {
            id: LayerId(1),
            sublayers: vec![SublayerEntry::new(LayerId(2))],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut a_spec = PrimSpec::default();
        a_spec.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        layer1.insert_prim(prim_a, a_spec);
        store.insert_layer(layer1);

        let mut layer2 = Layer {
            id: LayerId(2),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut b_spec = PrimSpec::default();
        b_spec.set_field(field_y, FieldValue::Value(Value::Int64(2)));
        layer2.insert_prim(prim_b, b_spec);
        store.insert_layer(layer2);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit both layers before recomposing.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim_a).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(11)));
        }
        {
            let layer = store.layers.get_mut(&LayerId(2)).unwrap();
            let spec = layer.prims.get_mut(&prim_b).unwrap();
            spec.set_field(field_y, FieldValue::Value(Value::Int64(22)));
        }

        // Batch notify both layers, then recompose once.
        live.notify_layer_edit(LayerId(1));
        live.notify_layer_edit(LayerId(2));
        let updated = live.recompose(&mut store);

        assert!(updated.contains(&prim_a), "A should be updated");
        assert!(updated.contains(&prim_b), "B should be updated");
        assert_eq!(
            live.stage().resolve_field(prim_a, field_x).unwrap().value,
            Value::Int64(11)
        );
        assert_eq!(
            live.stage().resolve_field(prim_b, field_y).unwrap().value,
            Value::Int64(22)
        );
    }

    #[test]
    fn double_recompose_is_idempotent() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim = p(&mut store, "/P");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut spec = PrimSpec::default();
        spec.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        layer.insert_prim(prim, spec);
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit and recompose.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(99)));
        }
        live.notify_layer_edit(LayerId(1));
        let first = live.recompose(&mut store);
        assert!(!first.is_empty());

        // Second recompose with no new notifications should be a no-op.
        let second = live.recompose(&mut store);
        assert!(second.is_empty(), "second recompose should be a no-op");
        assert_eq!(
            live.stage().resolve_field(prim, field_x).unwrap().value,
            Value::Int64(99),
            "value should be stable after idempotent recompose"
        );
    }

    #[test]
    fn recompose_matches_full_compose_after_edit() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim_p = p(&mut store, "/P");
        let prim_q = p(&mut store, "/Q");

        // P references Q.
        let mut root = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut p_spec = PrimSpec::default();
        p_spec.add_reference(Reference::new(LayerId(2), prim_q));
        p_spec.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        root.insert_prim(prim_p, p_spec);
        store.insert_layer(root);

        let mut ref_layer = Layer {
            id: LayerId(2),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut q_spec = PrimSpec::default();
        q_spec.set_field(field_x, FieldValue::Value(Value::Int64(100)));
        ref_layer.insert_prim(prim_q, q_spec);
        store.insert_layer(ref_layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit the referenced layer.
        {
            let layer = store.layers.get_mut(&LayerId(2)).unwrap();
            let spec = layer.prims.get_mut(&prim_q).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(200)));
        }

        live.notify_layer_edit(LayerId(2));
        live.recompose(&mut store);

        // Compare with a fresh full compose.
        let full = Stage::compose(&mut store, LayerId(1), StageOptions::default());

        assert_eq!(
            live.stage().resolve_field(prim_p, field_x).unwrap().value,
            full.resolve_field(prim_p, field_x).unwrap().value,
            "incremental and full compose should agree on P.x"
        );
    }

    #[test]
    fn unaffected_prim_not_in_updated_set() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim_a = p(&mut store, "/A");
        let prim_b = p(&mut store, "/B");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut a_spec = PrimSpec::default();
        a_spec.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        layer.insert_prim(prim_a, a_spec);

        let mut b_spec = PrimSpec::default();
        b_spec.set_field(field_x, FieldValue::Value(Value::Int64(2)));
        layer.insert_prim(prim_b, b_spec);
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Only edit prim A directly.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim_a).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(99)));
        }
        live.notify_prim_edit(prim_a);
        let updated = live.recompose(&mut store);

        assert!(updated.contains(&prim_a), "A should be updated");
        // B has no dependency on A — it should not appear in the updated set
        // unless the population mask causes it to be recomposed. Since
        // notify_prim_edit only marks A, B should be untouched.
        assert!(
            !updated.contains(&prim_b),
            "B should not be affected by an edit to A"
        );
        assert_eq!(
            live.stage().resolve_field(prim_b, field_x).unwrap().value,
            Value::Int64(2),
            "B's value should be unchanged"
        );
    }

    #[test]
    fn notify_layer_prim_edits_only_marks_connected_prims() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim_a = p(&mut store, "/A");
        let prim_b = p(&mut store, "/B");

        // Both prims in layer 1.
        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut a_spec = PrimSpec::default();
        a_spec.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        layer.insert_prim(prim_a, a_spec);

        let mut b_spec = PrimSpec::default();
        b_spec.set_field(field_x, FieldValue::Value(Value::Int64(2)));
        layer.insert_prim(prim_b, b_spec);
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit only prim A in layer 1.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim_a).unwrap();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(99)));
        }

        // Use the targeted notification: only mark prim_a within layer 1.
        live.notify_layer_prim_edits(LayerId(1), &[prim_a]);
        let updated = live.recompose(&mut store);

        assert!(updated.contains(&prim_a), "A should be updated");
        assert!(
            !updated.contains(&prim_b),
            "B should not be updated (not in notification list)"
        );
        assert_eq!(
            live.stage().resolve_field(prim_a, field_x).unwrap().value,
            Value::Int64(99)
        );
        assert_eq!(
            live.stage().resolve_field(prim_b, field_x).unwrap().value,
            Value::Int64(2),
            "B should be unchanged"
        );
    }

    #[test]
    fn notify_layer_prim_edits_ignores_unconnected_prims() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim_a = p(&mut store, "/A");
        let prim_b = p(&mut store, "/B");

        // Prim A in layer 1, prim B in layer 2.
        let mut layer1 = Layer {
            id: LayerId(1),
            sublayers: vec![SublayerEntry::new(LayerId(2))],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut a_spec = PrimSpec::default();
        a_spec.set_field(field_x, FieldValue::Value(Value::Int64(1)));
        layer1.insert_prim(prim_a, a_spec);
        store.insert_layer(layer1);

        let mut layer2 = Layer {
            id: LayerId(2),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        let mut b_spec = PrimSpec::default();
        b_spec.set_field(field_x, FieldValue::Value(Value::Int64(2)));
        layer2.insert_prim(prim_b, b_spec);
        store.insert_layer(layer2);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Try to mark prim_b as dirty in layer 1 — it's not connected.
        live.notify_layer_prim_edits(LayerId(1), &[prim_b]);
        let updated = live.recompose(&mut store);

        assert!(
            updated.is_empty(),
            "prim_b is not connected to layer 1, so nothing should be invalidated"
        );
    }

    #[test]
    fn notify_prim_edits_batch() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim_a = p(&mut store, "/A");
        let prim_b = p(&mut store, "/B");
        let prim_c = p(&mut store, "/C");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            default_prim: None,
            metadata: Vec::new(),
            relocates: Vec::new(),
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
            generation: 0,
            structure: 0,
            change_history: crate::change_history::History::default(),
        };
        for &(prim, val) in &[(prim_a, 1), (prim_b, 2), (prim_c, 3)] {
            let mut spec = PrimSpec::default();
            spec.set_field(field_x, FieldValue::Value(Value::Int64(val)));
            layer.insert_prim(prim, spec);
        }
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit A and B.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            layer
                .prims
                .get_mut(&prim_a)
                .unwrap()
                .set_field(field_x, FieldValue::Value(Value::Int64(10)));
            layer
                .prims
                .get_mut(&prim_b)
                .unwrap()
                .set_field(field_x, FieldValue::Value(Value::Int64(20)));
        }

        live.notify_prim_edits(&[prim_a, prim_b]);
        let updated = live.recompose(&mut store);

        assert!(updated.contains(&prim_a), "A should be updated");
        assert!(updated.contains(&prim_b), "B should be updated");
        assert!(!updated.contains(&prim_c), "C should not be updated");
    }

    /// Asserts that `live` is indistinguishable from a fresh composition:
    /// prim set, traversal order, children, resolved values with provenance,
    /// opinion stacks, and the dependency data used for later invalidation.
    fn assert_matches_fresh(live: &LiveStage, store: &mut InMemoryStore, fields: &[TokenId]) {
        let fresh_opts = StageOptions {
            with_dependencies: true,
            ..live.options.clone()
        };
        let mut fresh = Stage::compose(store, live.root, fresh_opts);
        let fresh_deps = fresh.take_deps().unwrap_or_default();
        let stage = live.stage();
        let root = p(store, "/");

        let mut live_prims: Vec<PathId> = stage.prim_paths().collect();
        let mut fresh_prims: Vec<PathId> = fresh.prim_paths().collect();
        live_prims.sort_unstable();
        fresh_prims.sort_unstable();
        assert_eq!(live_prims, fresh_prims, "prim sets differ");

        let live_order: Vec<PathId> = stage.traverse(root).collect();
        let fresh_order: Vec<PathId> = fresh.traverse(root).collect();
        assert_eq!(live_order, fresh_order, "traversal order differs");

        for &prim in &fresh_prims {
            let live_dependencies: HashSet<_> = live
                .tracker
                .graph()
                .dependencies(prim, OPINION_EDIT)
                .collect();
            let fresh_dependencies: HashSet<_> =
                fresh_deps.graph.dependencies(prim, OPINION_EDIT).collect();
            assert_eq!(
                live_dependencies, fresh_dependencies,
                "invalidation dependencies differ for {prim:?}"
            );
            assert_eq!(
                stage.children_of(prim),
                fresh.children_of(prim),
                "children differ for {prim:?}"
            );
            for &field in fields {
                assert_eq!(
                    stage.resolve_value(prim, field),
                    fresh.resolve_value(prim, field),
                    "value/provenance differs for {prim:?}.{field:?}"
                );
                assert_eq!(
                    stage.explain_field(prim, field),
                    fresh.explain_field(prim, field),
                    "opinion stack differs for {prim:?}.{field:?}"
                );
                let property = PropertyPath::new(prim, field);
                assert_eq!(
                    stage.resolve_property_path(property),
                    fresh.resolve_property_path(property),
                    "property value differs for {prim:?}.{field:?}"
                );
                assert_eq!(
                    stage.explain_property_path(property),
                    fresh.explain_property_path(property),
                    "property opinion stack differs for {prim:?}.{field:?}"
                );
            }
        }

        assert_eq!(live.arc_metadata, fresh_deps.arcs, "arc metadata differs");
        let non_empty = |m: &HashMap<LayerId, HashSet<PathId>>| {
            m.iter()
                .filter(|(_, prims)| !prims.is_empty())
                .map(|(layer, prims)| (*layer, prims.clone()))
                .collect::<HashMap<_, _>>()
        };
        assert_eq!(
            non_empty(&live.layer_to_prims),
            non_empty(&fresh_deps.layer_to_prims),
            "layer → prim dependencies differ"
        );
        assert_eq!(
            live.prim_to_layers, fresh_deps.prim_to_layers,
            "prim → layer dependencies differ"
        );
        assert_eq!(
            non_empty(&live.default_prim_dependents),
            non_empty(&fresh_deps.default_prim_dependents),
            "`defaultPrim` dependencies differ"
        );

        let fresh_live = LiveStage::compose(store, live.root, live.options.clone());
        assert_eq!(
            live.source_to_prims, fresh_live.source_to_prims,
            "source site → prim index differs"
        );
    }

    /// Regression: `notify_layer_prim_edits` documented paths "within the
    /// edited layer" but filtered them against composed destination paths,
    /// so editing a referenced library prim invalidated nothing.
    #[test]
    fn source_edit_in_referenced_layer_updates_referencing_prims() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let source = p(&mut store, "/Source");
        let other = p(&mut store, "/Other");
        let a = p(&mut store, "/A");
        let b = p(&mut store, "/B");
        let c = p(&mut store, "/C");
        let d = p(&mut store, "/D");

        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            a,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), source)),
        );
        root.insert_prim(
            b,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), source)),
        );
        root.insert_prim(c, PrimSpec::def().with_property(field_x, attr(3_i64)));
        root.insert_prim(
            d,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), other)),
        );
        store.insert_layer(root);

        let mut library = Layer::new(LayerId(2));
        library.insert_prim(source, PrimSpec::def().with_property(field_x, attr(1_i64)));
        library.insert_prim(other, PrimSpec::def().with_property(field_x, attr(7_i64)));
        store.insert_layer(library);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        assert_eq!(
            live.composed_prims_for_source(LayerId(2), source, &store),
            [a, b]
        );
        assert_eq!(live.composed_prims_for_source(LayerId(1), c, &store), [c]);
        assert!(
            live.composed_prims_for_source(LayerId(2), a, &store)
                .is_empty(),
            "`/A` is not a path in the library layer"
        );

        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .set_property(PropertyPath::new(source, field_x), attr(2_i64));
        live.notify_layer_prim_edits(LayerId(2), &[source]);
        let updated = live.recompose(&mut store);
        assert_eq!(updated, [a, b], "exactly the referencing prims recompose");
        assert_eq!(
            live.stage()
                .resolve_field_path(PropertyPath::new(a, field_x))
                .unwrap()
                .value,
            Value::Int64(2)
        );
        assert_matches_fresh(&live, &mut store, &[field_x]);

        // Repeat the shared-source batch, then edit the untouched source.
        // Dependency replacement must preserve both future propagation paths.
        for (site, expected) in [(source, vec![a, b]), (other, vec![d])] {
            store
                .layers
                .get_mut(&LayerId(2))
                .unwrap()
                .set_property(PropertyPath::new(site, field_x), attr(11));
            live.notify_layer_prim_edits(LayerId(2), &[site]);
            let updated = live.recompose(&mut store);
            assert_eq!(updated, expected);
            assert_matches_fresh(&live, &mut store, &[field_x]);
        }

        // Destination paths are not source paths of the library layer.
        live.notify_layer_prim_edits(LayerId(2), &[a]);
        assert!(live.recompose(&mut store).is_empty());
    }

    /// A prim authored in both branches of a variant set has one spec per
    /// branch (`/Model{lod=high}Geom` and `/Model{lod=low}Geom`). Editing the
    /// selected branch's spec, reported by its namespace path, recomposes
    /// `/Model/Geom` alone and matches a fresh composition; editing the
    /// unselected branch's spec changes nothing.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs),
    /// §10.3.2.5 (only the selected variant contributes).
    #[test]
    fn source_edit_inside_variant_branch_recomposes_branch_child() {
        use crate::{VariantSetSpec, VariantSpec, spec_path::VariantSelectionSite};

        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let lod = store.tokens.intern("lod");
        let high = store.tokens.intern("high");
        let low = store.tokens.intern("low");
        let geom_tok = store.tokens.intern("Geom");
        let model = p(&mut store, "/Model");
        let geom = p(&mut store, "/Model/Geom");
        let other = p(&mut store, "/Other");
        let site = |variant| VariantSelectionSite {
            host_path: model,
            set: lod,
            variant,
        };

        let mut layer = Layer::new(LayerId(1));
        let mut model_spec = PrimSpec::def();
        let mut set = VariantSetSpec::default();
        for variant in [high, low] {
            set.variants.insert(
                variant,
                VariantSpec {
                    authored_children: vec![geom_tok],
                    ..VariantSpec::default()
                },
            );
        }
        model_spec.variant_sets.insert(lod, set);
        model_spec.variant_set_names.prepend.push(lod);
        model_spec.variant_selections.insert(lod, high);
        layer.insert_prim(model, model_spec);
        for (variant, value) in [(high, 1_i64), (low, 2_i64)] {
            layer.insert_prim(
                geom,
                PrimSpec {
                    outer_variant_sites: vec![site(variant)],
                    ..PrimSpec::def().with_property(field_x, attr(value))
                },
            );
        }
        layer.insert_prim(other, PrimSpec::def().with_property(field_x, attr(5)));
        store.insert_layer(layer);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let x_of = |live: &LiveStage| {
            live.stage()
                .resolve_field_path(PropertyPath::new(geom, field_x))
                .unwrap()
                .value
        };
        assert_eq!(x_of(&live), Value::Int64(1));
        assert_eq!(
            live.composed_prims_for_source(LayerId(1), geom, &store),
            [geom]
        );

        let edit = |store: &mut InMemoryStore, variant, value: i64| {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer
                .prims
                .get_mut(&geom)
                .into_iter()
                .chain(layer.variant_prims.get_mut(&geom).into_iter().flatten())
                .find(|spec| spec.outer_variant_sites == [site(variant)])
                .unwrap();
            spec.set_property(field_x, attr(value));
        };

        edit(&mut store, high, 10);
        live.notify_layer_prim_edits(LayerId(1), &[geom]);
        assert_eq!(live.recompose(&mut store), [geom]);
        assert_eq!(x_of(&live), Value::Int64(10));
        assert_matches_fresh(&live, &mut store, &[field_x]);

        edit(&mut store, low, 20);
        live.notify_layer_prim_edits(LayerId(1), &[geom]);
        assert_eq!(live.recompose(&mut store), [geom]);
        assert_eq!(x_of(&live), Value::Int64(10), "`low` is not selected");
        assert_matches_fresh(&live, &mut store, &[field_x]);
    }

    #[test]
    fn selection_edit_recomposes_the_switched_prims_descendants() {
        // Spec: AOUSD Core §10.3.2.5. `/Model/Geom` draws nothing from the
        // `/Model` spec that authors the selection, but selecting `low`
        // swaps the branch spec it composes: it is recomposed with `/Model`.
        use crate::{VariantSetSpec, VariantSpec, spec_path::VariantSelectionSite};

        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let lod = store.tokens.intern("lod");
        let high = store.tokens.intern("high");
        let low = store.tokens.intern("low");
        let geom_tok = store.tokens.intern("Geom");
        let model = p(&mut store, "/Model");
        let geom = p(&mut store, "/Model/Geom");
        let other = p(&mut store, "/Other");
        let site = |variant| VariantSelectionSite {
            host_path: model,
            set: lod,
            variant,
        };

        let mut layer = Layer::new(LayerId(1));
        let mut model_spec = PrimSpec::def();
        let mut set = VariantSetSpec::default();
        for variant in [high, low] {
            set.variants.insert(
                variant,
                VariantSpec {
                    authored_children: vec![geom_tok],
                    ..VariantSpec::default()
                },
            );
        }
        model_spec.variant_sets.insert(lod, set);
        model_spec.variant_set_names.prepend.push(lod);
        model_spec.variant_selections.insert(lod, high);
        model_spec.authored_children.push(geom_tok);
        layer.insert_prim(model, model_spec);
        layer.insert_prim(geom, PrimSpec::def());
        for (variant, value) in [(high, 1_i64), (low, 2_i64)] {
            layer.insert_prim(
                geom,
                PrimSpec {
                    outer_variant_sites: vec![site(variant)],
                    ..PrimSpec::over().with_property(field_x, attr(value))
                },
            );
        }
        layer.insert_prim(other, PrimSpec::def().with_property(field_x, attr(5)));
        store.insert_layer(layer);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let x_of = |live: &LiveStage| {
            live.stage()
                .resolve_field_path(PropertyPath::new(geom, field_x))
                .unwrap()
                .value
        };
        assert_eq!(x_of(&live), Value::Int64(1));

        for (variant, value) in [(low, 2), (high, 1)] {
            store
                .layers
                .get_mut(&LayerId(1))
                .unwrap()
                .prims
                .get_mut(&model)
                .unwrap()
                .variant_selections
                .insert(lod, variant);
            live.notify_layer_prim_edits(LayerId(1), &[model]);
            assert_eq!(live.recompose(&mut store), [model, geom]);
            assert_eq!(x_of(&live), Value::Int64(value));
            assert_matches_fresh(&live, &mut store, &[field_x]);
        }
    }

    /// `/A` references a library prim that specializes `/Class`, and has a
    /// payload that also authors `x`. The specialized class is weaker than
    /// the payload although the specializes arc is reached through the
    /// stronger reference. Editing the class, then removing the payload's
    /// opinion, recomposes `/A` to match a fresh composition each time.
    ///
    /// Spec: AOUSD Core §10.4.1 (specializes are globally weaker).
    #[test]
    fn specialized_class_edit_recomposes_prims_reaching_it_through_a_reference() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let a = p(&mut store, "/A");
        let other = p(&mut store, "/Other");
        let reference = p(&mut store, "/Ref");
        let class = p(&mut store, "/Class");
        let payload = p(&mut store, "/Payload");

        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            a,
            PrimSpec::def()
                .with_reference(Reference::new(LayerId(2), reference))
                .with_payload(Reference::new(LayerId(2), payload)),
        );
        root.insert_prim(other, PrimSpec::def().with_property(field_x, attr(5)));
        store.insert_layer(root);

        let mut library = Layer::new(LayerId(2));
        library.insert_prim(reference, PrimSpec::over().with_specialize(class));
        library.insert_prim(class, PrimSpec::class().with_property(field_x, attr(1)));
        library.insert_prim(payload, PrimSpec::over().with_property(field_x, attr(2)));
        store.insert_layer(library);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let x_of = |live: &LiveStage| {
            live.stage()
                .resolve_field_path(PropertyPath::new(a, field_x))
                .unwrap()
                .value
        };
        assert_eq!(
            x_of(&live),
            Value::Int64(2),
            "the payload outranks `/Class`"
        );

        let edit = |store: &mut InMemoryStore, prim: PathId, value: Option<i64>| {
            let spec = store
                .layers
                .get_mut(&LayerId(2))
                .unwrap()
                .prims
                .get_mut(&prim)
                .unwrap();
            match value {
                Some(value) => {
                    spec.set_property(field_x, attr(value));
                }
                None => {
                    spec.remove_property(field_x);
                }
            }
        };

        edit(&mut store, class, Some(3));
        live.notify_layer_prim_edits(LayerId(2), &[class]);
        assert_eq!(live.recompose(&mut store), [a]);
        assert_eq!(x_of(&live), Value::Int64(2));
        assert_matches_fresh(&live, &mut store, &[field_x]);

        edit(&mut store, payload, None);
        live.notify_layer_prim_edits(LayerId(2), &[payload]);
        assert_eq!(live.recompose(&mut store), [a]);
        assert_eq!(x_of(&live), Value::Int64(3), "now from `/Class`");
        assert_matches_fresh(&live, &mut store, &[field_x]);
    }

    /// `/A` references `/Parent/Child` of a library whose `/Parent`
    /// references `/Source`, and `/B` references `/Parent/Child` of the
    /// stage's own layer stack, where `/Parent` references `/Source` as
    /// well. Each reaches `/Source/Child` through the ancestral reference of
    /// its subroot target. Editing that site, then an opinion on the
    /// target's parent, recomposes exactly the prims that read it and
    /// matches a fresh composition each time.
    ///
    /// Spec: AOUSD Core §10.2 (a subroot target's ancestral arcs); OpenUSD
    /// `_BuildInitialPrimIndexFromAncestor` in `pxr/usd/pcp/primIndex.cpp`.
    #[test]
    fn ancestral_arc_source_edit_recomposes_subroot_referencing_prims() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let a = p(&mut store, "/A");
        let b = p(&mut store, "/B");
        let parent = p(&mut store, "/Parent");
        let child = p(&mut store, "/Parent/Child");
        let source_child = p(&mut store, "/Source/Child");
        let source = p(&mut store, "/Source");

        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            a,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), child)),
        );
        root.insert_prim(
            b,
            PrimSpec::def().with_reference(Reference::new(LayerId(1), child)),
        );
        root.insert_prim(
            parent,
            PrimSpec::def().with_reference(Reference::new(LayerId(1), source)),
        );
        root.insert_prim(child, PrimSpec::over());
        root.insert_prim(source, PrimSpec::def());
        root.insert_prim(
            source_child,
            PrimSpec::def().with_property(field_x, attr(1)),
        );
        store.insert_layer(root);

        let mut library = Layer::new(LayerId(2));
        library.insert_prim(
            parent,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), source)),
        );
        library.insert_prim(child, PrimSpec::def());
        library.insert_prim(source, PrimSpec::def());
        library.insert_prim(
            source_child,
            PrimSpec::def().with_property(field_x, attr(10)),
        );
        store.insert_layer(library);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let x_of = |live: &LiveStage, prim: PathId| {
            live.stage()
                .resolve_field_path(PropertyPath::new(prim, field_x))
                .unwrap()
                .value
        };
        assert_eq!(x_of(&live, a), Value::Int64(10));
        assert_eq!(x_of(&live, b), Value::Int64(1));
        assert_eq!(
            live.composed_prims_for_source(LayerId(2), source_child, &store),
            [a]
        );

        let edit = |store: &mut InMemoryStore, layer: LayerId, prim: PathId, value: i64| {
            store
                .layers
                .get_mut(&layer)
                .unwrap()
                .prims
                .get_mut(&prim)
                .unwrap()
                .set_property(field_x, attr(value));
        };

        edit(&mut store, LayerId(2), source_child, 20);
        live.notify_layer_prim_edits(LayerId(2), &[source_child]);
        assert_eq!(live.recompose(&mut store), [a]);
        assert_eq!(x_of(&live, a), Value::Int64(20));
        assert_matches_fresh(&live, &mut store, &[field_x]);

        edit(&mut store, LayerId(1), source_child, 2);
        live.notify_layer_prim_edits(LayerId(1), &[source_child]);
        let updated = live.recompose(&mut store);
        assert!(updated.contains(&b), "`/B` reads `/Source/Child`");
        assert_eq!(x_of(&live, b), Value::Int64(2));
        assert_matches_fresh(&live, &mut store, &[field_x]);

        // An opinion on the target's parent is not an opinion of the
        // target: the parent's arcs reach it, its properties do not.
        edit(&mut store, LayerId(2), parent, 30);
        live.notify_layer_prim_edits(LayerId(2), &[parent]);
        assert!(live.recompose(&mut store).is_empty());
        assert_eq!(x_of(&live, a), Value::Int64(20));
        assert_matches_fresh(&live, &mut store, &[field_x]);
    }

    /// `/R` references `/Parent/Child` and `/P` has a payload to it; the
    /// library's `/Parent` references, and has a payload to, the
    /// `defaultPrim` of a third layer, so each reaches `Child` of that
    /// prim through an ancestral arc of its subroot target. Those arcs
    /// depend on the `defaultPrim` as direct arcs do: a missing one is
    /// reported for each destination, and setting and retargeting it
    /// recompose both prims to match a fresh composition.
    ///
    /// Spec: AOUSD Core §10.3.2.1 (an omitted prim path assumes the
    /// `defaultPrim`), §10.2 (a subroot target's ancestral arcs).
    #[test]
    fn default_prim_edit_recomposes_prims_reaching_it_through_ancestral_arcs() {
        use crate::CompositionError;

        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let r = p(&mut store, "/R");
        let pl = p(&mut store, "/P");
        let parent = p(&mut store, "/Parent");
        let child = p(&mut store, "/Parent/Child");
        let first_tok = store.tokens.intern("First");
        let second_tok = store.tokens.intern("Second");

        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            r,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), child)),
        );
        root.insert_prim(
            pl,
            PrimSpec::def().with_payload(Reference::new(LayerId(2), child)),
        );
        store.insert_layer(root);

        let mut library = Layer::new(LayerId(2));
        library.insert_prim(
            parent,
            PrimSpec::def()
                .with_reference(Reference::to_default_prim(LayerId(3)))
                .with_payload(Reference::to_default_prim(LayerId(3))),
        );
        library.insert_prim(child, PrimSpec::def());
        store.insert_layer(library);

        let mut target = Layer::new(LayerId(3));
        for (name, value) in [("First", 1_i64), ("Second", 2_i64)] {
            let prim = p(&mut store, &alloc::format!("/{name}"));
            let prim_child = p(&mut store, &alloc::format!("/{name}/Child"));
            target.insert_prim(prim, PrimSpec::def());
            target.insert_prim(
                prim_child,
                PrimSpec::def().with_property(field_x, attr(value)),
            );
        }
        store.insert_layer(target);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let x_of = |live: &LiveStage, prim: PathId| {
            live.stage()
                .resolve_field_path(PropertyPath::new(prim, field_x))
                .map(|resolved| resolved.value)
        };
        assert_eq!(x_of(&live, r), None);
        assert_eq!(live.prims_using_default_prim(LayerId(3), &store), [pl, r]);
        let unresolved: Vec<PathId> = live
            .stage()
            .composition_errors()
            .iter()
            .filter_map(|error| match error {
                CompositionError::UnresolvedDefaultPrim(error) => {
                    assert_eq!((error.layer, error.path), (LayerId(3), None));
                    Some(error.prim)
                }
                _ => None,
            })
            .collect();
        assert!(unresolved.contains(&r) && unresolved.contains(&pl));
        assert_errors_match_fresh(&live, &mut store);

        for (default_prim, value) in [(first_tok, 1_i64), (second_tok, 2_i64)] {
            store.layers.get_mut(&LayerId(3)).unwrap().default_prim = Some(default_prim);
            live.notify_default_prim_edit(LayerId(3));
            let updated = live.recompose(&mut store);
            assert!(updated.contains(&r) && updated.contains(&pl));
            assert_eq!(x_of(&live, r), Some(Value::Int64(value)));
            assert_eq!(x_of(&live, pl), Some(Value::Int64(value)));
            assert!(live.stage().composition_errors().is_empty());
            assert_matches_fresh(&live, &mut store, &[field_x]);
            assert_errors_match_fresh(&live, &mut store);
        }

        // A `defaultPrim` naming no prim is reported with the path it names.
        let absent_tok = store.tokens.intern("Absent");
        let absent = p(&mut store, "/Absent");
        store.layers.get_mut(&LayerId(3)).unwrap().default_prim = Some(absent_tok);
        live.notify_default_prim_edit(LayerId(3));
        live.recompose(&mut store);
        assert_eq!(x_of(&live, r), None);
        let dangling = live
            .stage()
            .composition_errors()
            .iter()
            .filter(|error| {
                matches!(error, CompositionError::UnresolvedDefaultPrim(error)
                    if error.layer == LayerId(3) && error.path == Some(absent))
            })
            .count();
        assert_eq!(
            dangling, 4,
            "the reference and the payload, for `/R` and `/P`"
        );
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_errors_match_fresh(&live, &mut store);
    }

    /// Regression: recomposing one sibling used to replace the root's child
    /// list with the masked composition's partial list, dropping `/B` from
    /// traversal while `has_prim(/B)` stayed true.
    #[test]
    fn prim_edit_preserves_sibling_hierarchy() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim_a = p(&mut store, "/A");
        let prim_b = p(&mut store, "/B");
        let prim_c = p(&mut store, "/C");
        let child = p(&mut store, "/A/Child");

        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(prim_a, PrimSpec::def().with_property(field_x, attr(1_i64)));
        layer.insert_prim(child, PrimSpec::def().with_property(field_x, attr(5_i64)));
        layer.insert_prim(prim_b, PrimSpec::def().with_property(field_x, attr(2_i64)));
        // `/C` references `/A`, so editing `/A` also recomposes `/C`.
        layer.insert_prim(
            prim_c,
            PrimSpec::def().with_reference(Reference::new(LayerId(1), prim_a)),
        );
        store.insert_layer(layer);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        assert_matches_fresh(&live, &mut store, &[field_x]);

        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .set_property(PropertyPath::new(prim_a, field_x), attr(10_i64));
        live.notify_prim_edit(prim_a);
        let updated = live.recompose(&mut store);
        assert!(updated.contains(&prim_a), "A recomposed");
        assert!(updated.contains(&prim_c), "C depends on A");
        assert!(!updated.contains(&prim_b), "B untouched");
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_eq!(
            live.stage()
                .resolve_field_path(PropertyPath::new(prim_c, field_x))
                .unwrap()
                .value,
            Value::Int64(10),
            "reference sees the edit"
        );

        // Later notifications still reach every dependent.
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .set_property(PropertyPath::new(prim_b, field_x), attr(20_i64));
        live.notify_layer_prim_edits(LayerId(1), &[prim_b]);
        let updated = live.recompose(&mut store);
        assert_eq!(updated, vec![prim_b], "only B recomposed");
        assert_matches_fresh(&live, &mut store, &[field_x]);

        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .set_property(PropertyPath::new(prim_a, field_x), attr(11_i64));
        live.notify_layer_prim_edits(LayerId(1), &[prim_a]);
        let updated = live.recompose(&mut store);
        assert!(updated.contains(&prim_c), "C still depends on A");
        assert_matches_fresh(&live, &mut store, &[field_x]);
    }

    /// An opinion edit that changes hierarchy (here, child order) cannot be
    /// patched from a masked composition; it falls back to a rebuild.
    #[test]
    fn prim_edit_that_reorders_children_matches_fresh() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let parent = p(&mut store, "/P");
        let a = p(&mut store, "/P/A");
        let b = p(&mut store, "/P/B");
        let a_tok = store.tokens.intern("A");
        let b_tok = store.tokens.intern("B");

        let mut layer = Layer::new(LayerId(1));
        let mut parent_spec = PrimSpec::def();
        parent_spec.authored_children = vec![a_tok, b_tok];
        layer.insert_prim(parent, parent_spec);
        layer.insert_prim(a, PrimSpec::def().with_field(field_x, 1_i64));
        layer.insert_prim(b, PrimSpec::def().with_field(field_x, 2_i64));
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_matches_fresh(&live, &mut store, &[field_x]);

        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&parent)
            .unwrap()
            .prim_order = Some(vec![b_tok, a_tok]);
        live.notify_prim_edit(parent);
        live.recompose(&mut store);
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_eq!(live.stage().children_of(parent), Some(&[b, a][..]));
    }

    /// The same reorder edit on a hierarchy authored only through
    /// `Layer::insert_prim`, which leaves `authored_children` empty.
    #[test]
    fn reorder_edit_without_authored_children_matches_fresh() {
        let mut store = InMemoryStore::default();
        let parent = p(&mut store, "/P");
        let a = p(&mut store, "/P/A");
        let b = p(&mut store, "/P/B");
        let a_tok = store.tokens.intern("A");
        let b_tok = store.tokens.intern("B");

        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(parent, PrimSpec::def());
        layer.insert_prim(a, PrimSpec::def());
        layer.insert_prim(b, PrimSpec::def());
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(live.stage().children_of(parent), Some(&[a, b][..]));

        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&parent)
            .unwrap()
            .prim_order = Some(vec![b_tok, a_tok]);
        live.notify_prim_edit(parent);
        live.recompose(&mut store);
        assert_matches_fresh(&live, &mut store, &[]);
        assert_eq!(live.stage().children_of(parent), Some(&[b, a][..]));
    }

    /// Creating and removing children of a parent whose layer does not list
    /// them composes what a fresh composition does. The stage keeps its
    /// child index across edits: creations patch it, removals index the
    /// layer anew, and value edits leave it alone.
    ///
    /// Spec: AOUSD Core §11 (stage population). OpenUSD:
    /// `PcpComposeSiteChildNames`.
    #[test]
    fn child_creations_and_removals_keep_the_child_index() {
        use crate::{EditTarget, Specifier, edit::Transaction};
        let mut store = InMemoryStore::default();
        let world = p(&mut store, "/World");
        let value = store.tokens.intern("x");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(world, PrimSpec::def());
        for i in 0..4 {
            let rock = p(&mut store, &alloc::format!("/World/Rock{i}"));
            layer.insert_prim(rock, PrimSpec::def());
        }
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let target = EditTarget::for_layer(LayerId(1));
        let index_work = |live: &LiveStage| {
            let work = live.recomposition_work();
            (work.child_index_rebuilds, work.child_index_patches)
        };

        let mut undo = Vec::new();
        for path in ["/World/Plant0", "/World/Plant1/Leaf", "/World/Rock1/Moss"] {
            let mut txn = Transaction::new();
            txn.create_prim(target.prim(p(&mut store, path)), Specifier::Def, None);
            undo.push(live.apply(&mut store, &txn).unwrap().inverse);
            assert_eq!(index_work(&live), (0, 1), "{path}");
            assert_fresh(&live, &mut store, value);
        }

        let mut txn = Transaction::new();
        txn.remove_spec(target.prim(p(&mut store, "/World/Rock2")));
        let restore = live.apply(&mut store, &txn).unwrap().inverse;
        assert_eq!(index_work(&live), (1, 0));
        assert_fresh(&live, &mut store, value);
        live.apply(&mut store, &restore).unwrap();
        assert_eq!(index_work(&live), (0, 1));
        assert_fresh(&live, &mut store, value);
        while let Some(inverse) = undo.pop() {
            live.apply(&mut store, &inverse).unwrap();
            assert_fresh(&live, &mut store, value);
        }

        let mut txn = Transaction::new();
        txn.create_property(
            target.property(PropertyPath::new(world, value)),
            PropertySpec::typed_attribute(crate::PropertyType::parse("double").unwrap())
                .with_default(Value::Double(1.0)),
        );
        live.apply(&mut store, &txn).unwrap();
        assert_eq!(index_work(&live), (0, 0), "a value edit re-indexes nothing");
        assert_fresh(&live, &mut store, value);

        // An edit made outside the stage patches the index too.
        let stone = p(&mut store, "/World/Stone");
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .insert_prim(stone, PrimSpec::def());
        live.notify_changed_layers(&store);
        live.recompose(&mut store);
        assert_eq!(index_work(&live), (0, 1));
        assert_fresh(&live, &mut store, value);
    }

    /// Asserts that `live` composes what a fresh composition of `store`
    /// does ([`assert_matches_fresh`]), inactive children included.
    fn assert_lists_match_fresh(live: &LiveStage, store: &mut InMemoryStore) {
        assert_matches_fresh(live, store, &[]);
        let fresh = Stage::compose(store, live.root, live.options.clone());
        for prim in fresh.prim_paths() {
            assert_eq!(
                live.stage().all_children_of(prim),
                fresh.all_children_of(prim),
                "children with inactive ones differ for {prim:?}"
            );
        }
    }

    /// Creating, removing and undoing children of a parent that lists and
    /// reorders them and holds an inactive child, and editing the parent's
    /// specs in the same transaction, compose what a fresh composition
    /// does. The partial composition composes the boundary parent's child
    /// names once, with its index, and the merge takes its whole child
    /// list: no boundary list is ordered by composing them again.
    ///
    /// Spec: AOUSD Core §11.3.1 (child order from `primChildren` and
    /// `primOrder`). OpenUSD: `PcpComposeSiteChildNames`,
    /// `PcpPrimIndex::ComputePrimChildNames`.
    #[test]
    fn local_child_edits_compose_boundary_child_names_once() {
        use crate::{EditTarget, Specifier, edit::Transaction};
        let mut store = InMemoryStore::default();
        let world = p(&mut store, "/World");
        let [a, b, c, d, e] = ["A", "B", "C", "D", "E"].map(|name| store.tokens.intern(name));
        let path =
            |store: &mut InMemoryStore, name: &str| p(store, &alloc::format!("/World/{name}"));
        let mut layer = Layer::new(LayerId(1));
        let mut parent = PrimSpec::def();
        parent.authored_children = vec![a, b, c];
        parent.prim_order = Some(vec![c, a]);
        layer.insert_prim(world, parent);
        layer.insert_prim(path(&mut store, "A"), PrimSpec::def());
        layer.insert_prim(path(&mut store, "B"), PrimSpec::def().with_active(false));
        layer.insert_prim(path(&mut store, "C"), PrimSpec::def());
        layer.insert_prim(p(&mut store, "/Other"), PrimSpec::def());
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_lists_match_fresh(&live, &mut store);
        let target = EditTarget::for_layer(LayerId(1));
        let composed_once = |live: &LiveStage| {
            let work = live.recomposition_work();
            assert!(!work.full_rebuild, "{work:?}");
            assert_eq!(
                (work.boundary_lists_composed, work.boundary_lists_reordered),
                (1, 0),
                "{work:?}"
            );
        };

        let mut undo = Vec::new();
        let mut txn = Transaction::new();
        txn.create_prim(target.prim(path(&mut store, "D")), Specifier::Def, None);
        undo.push(live.apply(&mut store, &txn).unwrap().inverse);
        composed_once(&live);
        assert_lists_match_fresh(&live, &mut store);

        // The parent's reorder changes with the child it is created for.
        let mut txn = Transaction::new();
        txn.create_prim(target.prim(path(&mut store, "E")), Specifier::Def, None);
        txn.set_prim_order(target.prim(world), vec![e, b, d]);
        undo.push(live.apply(&mut store, &txn).unwrap().inverse);
        composed_once(&live);
        assert_lists_match_fresh(&live, &mut store);

        // The inactive child goes last: its parent then lists no children,
        // inactive ones included.
        for name in ["A", "C", "D", "E", "B"] {
            let mut txn = Transaction::new();
            txn.remove_spec(target.prim(path(&mut store, name)));
            undo.push(live.apply(&mut store, &txn).unwrap().inverse);
            composed_once(&live);
            assert_lists_match_fresh(&live, &mut store);
        }
        assert_eq!(live.stage().all_children_of(world), None);
        while let Some(inverse) = undo.pop() {
            live.apply(&mut store, &inverse).unwrap();
            assert_lists_match_fresh(&live, &mut store);
        }
        assert_eq!(
            live.stage().children_of(world),
            Some(&[path(&mut store, "C"), path(&mut store, "A")][..])
        );
    }

    /// Creating and removing children of a prim whose selected variant
    /// branch authors children too recomposes the prim's namespace through
    /// a masked composition, which composes the boundary parent's child
    /// names once for its whole list, and matches a fresh composition.
    ///
    /// Spec: AOUSD Core §11.3.1 (child order), §10.5 (variant branches
    /// contribute children). OpenUSD: `PcpComposeSiteChildNames`.
    #[test]
    fn variant_branch_child_edits_compose_boundary_child_names_once() {
        use crate::{
            EditTarget, Specifier, VariantSetSpec, VariantSpec,
            edit::Transaction,
            spec_path::{SpecPath, VariantSelectionSite},
        };
        let mut store = InMemoryStore::default();
        let rock = p(&mut store, "/Rock");
        let [shape, smooth, moss, stone] =
            ["shape", "smooth", "Moss", "Stone"].map(|name| store.tokens.intern(name));
        let mut layer = Layer::new(LayerId(1));
        let mut spec = PrimSpec::def();
        let mut set = VariantSetSpec::default();
        set.variants.insert(
            smooth,
            VariantSpec {
                authored_children: vec![moss],
                ..VariantSpec::default()
            },
        );
        spec.variant_sets.insert(shape, set);
        spec.variant_set_names.prepend.push(shape);
        spec.variant_selections.insert(shape, smooth);
        spec.authored_children = vec![stone];
        spec.prim_order = Some(vec![moss, stone]);
        layer.insert_prim(rock, spec);
        layer.insert_prim(
            p(&mut store, "/Rock/Moss"),
            PrimSpec {
                outer_variant_sites: vec![VariantSelectionSite {
                    host_path: rock,
                    set: shape,
                    variant: smooth,
                }],
                ..PrimSpec::def()
            },
        );
        layer.insert_prim(p(&mut store, "/Rock/Stone"), PrimSpec::def());
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_lists_match_fresh(&live, &mut store);
        let target = EditTarget::for_layer(LayerId(1));
        let branch = |store: &mut InMemoryStore, text: &str| {
            crate::edit::Address::spec(
                LayerId(1),
                SpecPath::parse(text, &store.tokens, &store.paths).expect("spec path"),
            )
        };

        let mut edits = [Transaction::new(), Transaction::new(), Transaction::new()];
        edits[0].create_prim(
            target.prim(p(&mut store, "/Rock/Pebble")),
            Specifier::Def,
            None,
        );
        edits[1].create_prim(
            branch(&mut store, "/Rock{shape=smooth}Lichen"),
            Specifier::Def,
            None,
        );
        edits[2].remove_spec(target.prim(p(&mut store, "/Rock/Stone")));
        let mut undo = Vec::new();
        for txn in edits {
            undo.push(live.apply(&mut store, &txn).unwrap().inverse);
            let work = live.recomposition_work();
            assert!(!work.full_rebuild, "{work:?}");
            assert_eq!(work.boundary_lists_reordered, 0, "{work:?}");
            assert!(work.boundary_lists_composed >= 1, "{work:?}");
            assert_lists_match_fresh(&live, &mut store);
        }
        while let Some(inverse) = undo.pop() {
            live.apply(&mut store, &inverse).unwrap();
            assert_lists_match_fresh(&live, &mut store);
        }
    }

    /// Child order folds layer by layer: each layer appends its children,
    /// then reorders the names gathered so far. Editing the weaker layer's
    /// children or reorder recomposes to the same order as a fresh
    /// composition.
    ///
    /// Spec: AOUSD Core §11 (stage population). OpenUSD:
    /// `PcpComposeSiteChildNames` in `pxr/usd/pcp/composeSite.cpp`.
    #[test]
    fn child_order_edits_in_a_sublayer_match_fresh() {
        let mut store = InMemoryStore::default();
        let parent = p(&mut store, "/P");
        let [a, b, c, d, e] = ["a", "b", "c", "d", "e"].map(|name| {
            let path = p(&mut store, &alloc::format!("/P/{name}"));
            (store.tokens.intern(name), path)
        });

        // The weaker layer lists `a b c` and reorders them `c b a`.
        let mut weak = Layer::new(LayerId(2));
        let mut weak_parent = PrimSpec::def();
        weak_parent.authored_children = vec![a.0, b.0, c.0];
        weak_parent.prim_order = Some(vec![c.0, b.0, a.0]);
        weak.insert_prim(parent, weak_parent);
        for (_, child) in [a, b, c] {
            weak.insert_prim(child, PrimSpec::def());
        }
        store.insert_layer(weak);

        // The stronger layer adds `d` and reorders `a d`: `c b` stay in front.
        let mut strong = Layer::new(LayerId(1));
        strong.sublayers = vec![SublayerEntry::new(LayerId(2))];
        let mut strong_parent = PrimSpec::over();
        strong_parent.authored_children = vec![d.0];
        strong_parent.prim_order = Some(vec![a.0, d.0]);
        strong.insert_prim(parent, strong_parent);
        strong.insert_prim(d.1, PrimSpec::def());
        store.insert_layer(strong);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(
            live.stage().children_of(parent),
            Some(&[c.1, b.1, a.1, d.1][..])
        );
        assert_matches_fresh(&live, &mut store, &[]);

        // The weaker layer adds `e` and reorders `e a b`, and `b` carries
        // `c` along; the stronger reorder leaves `e` in front, and `a`
        // carries `b c` along.
        let weak = store.layers.get_mut(&LayerId(2)).unwrap();
        weak.insert_prim(e.1, PrimSpec::def());
        let weak_parent = weak.prims.get_mut(&parent).unwrap();
        weak_parent.authored_children.push(e.0);
        weak_parent.prim_order = Some(vec![e.0, a.0, b.0]);
        live.notify_layer_prim_edits(LayerId(2), &[parent, e.1]);
        live.recompose(&mut store);
        assert_matches_fresh(&live, &mut store, &[]);
        assert_eq!(
            live.stage().children_of(parent),
            Some(&[e.1, a.1, b.1, c.1, d.1][..])
        );
    }

    /// Path expressions authored across a reference are anchored and
    /// mapped into the referencing prim, and `%_` splices in the weaker
    /// one; editing the referenced expression recomposes to the same values
    /// as a fresh composition.
    ///
    /// Spec: AOUSD Core §10 (composition arcs map namespace), §12.3.
    /// OpenUSD: `PcpMapFunction::MapSourceToTarget(SdfPathExpression)` and
    /// `SdfPathExpression::ComposeOver`.
    #[test]
    fn path_expression_edits_across_a_reference_match_fresh() {
        let mut store = InMemoryStore::default();
        let field = store.tokens.intern("members");
        let part = p(&mut store, "/Part");
        let gear = p(&mut store, "/Gear");
        let expression =
            |text: &str| PropertySpec::attribute().with_default(Value::PathExpression(text.into()));

        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            part,
            PrimSpec::def()
                .with_reference(Reference::new(LayerId(2), gear))
                .with_property(field, expression("/Part/Axle %_")),
        );
        store.insert_layer(root);
        let mut asset = Layer::new(LayerId(2));
        asset.insert_prim(
            gear,
            PrimSpec::def().with_property(field, expression(".//")),
        );
        store.insert_layer(asset);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let members = |live: &LiveStage| {
            live.stage()
                .resolve_field_path(PropertyPath::new(part, field))
                .map(|resolved| resolved.value)
        };
        assert_eq!(
            members(&live),
            Some(Value::PathExpression("/Part/Axle /Part//".into()))
        );

        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .prims
            .get_mut(&gear)
            .unwrap()
            .set_property(field, expression("Tooth /Elsewhere"));
        live.notify_layer_prim_edits(LayerId(2), &[gear]);
        live.recompose(&mut store);
        assert_matches_fresh(&live, &mut store, &[field]);
        assert_eq!(
            members(&live),
            Some(Value::PathExpression("/Part/Axle /Part/Tooth".into()))
        );
    }

    /// Opinions and arcs authored beneath an instance never contribute:
    /// editing them, or toggling `instanceable`, recomposes to the same stage
    /// as a fresh composition. A prim that becomes or stops being an
    /// instance recomposes its descendants.
    ///
    /// Spec: AOUSD Core §11.3.3 (scene graph instancing). OpenUSD:
    /// `_ConvertNodeForChild` in `pxr/usd/pcp/primIndex.cpp`.
    #[test]
    fn instance_edits_match_fresh() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let leaf_tok = store.tokens.intern("Leaf");
        let seedling = p(&mut store, "/Seedling");
        let seedling_leaf = p(&mut store, "/Seedling/Leaf");
        let stray = p(&mut store, "/Stray");
        let stray_leaf = p(&mut store, "/Stray/Leaf");
        let grove = p(&mut store, "/Grove");
        let grove_leaf = p(&mut store, "/Grove/Leaf");

        let mut layer = Layer::new(LayerId(1));
        let mut parent = PrimSpec::def();
        parent.authored_children = vec![leaf_tok];
        layer.insert_prim(seedling, parent.clone());
        layer.insert_prim(
            seedling_leaf,
            PrimSpec::def().with_property(field_x, attr(1)),
        );
        layer.insert_prim(stray, parent);
        layer.insert_prim(stray_leaf, PrimSpec::def().with_property(field_x, attr(9)));
        let mut instance = PrimSpec::def().with_reference(Reference::new(LayerId(1), seedling));
        instance.instanceable = Some(true);
        instance.authored_children = vec![leaf_tok];
        layer.insert_prim(grove, instance);
        // The instance descendant authors a reference and a value.
        layer.insert_prim(
            grove_leaf,
            PrimSpec::over()
                .with_reference(Reference::new(LayerId(1), stray_leaf))
                .with_property(field_x, attr(5)),
        );
        store.insert_layer(layer);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let x_of = |live: &LiveStage| {
            live.stage()
                .resolve_field_path(PropertyPath::new(grove_leaf, field_x))
                .map(|resolved| resolved.value)
        };
        assert_eq!(x_of(&live), Some(Value::Int64(1)), "from the prototype");

        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&grove_leaf)
            .unwrap()
            .set_property(field_x, attr(6));
        live.notify_layer_prim_edits(LayerId(1), &[grove_leaf]);
        live.recompose(&mut store);
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_eq!(
            x_of(&live),
            Some(Value::Int64(1)),
            "the local value is inert"
        );

        let set_instanceable = |store: &mut InMemoryStore, value: bool| {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            layer.prims.get_mut(&grove).unwrap().instanceable = Some(value);
        };
        set_instanceable(&mut store, false);
        live.notify_prim_edit(grove);
        live.recompose(&mut store);
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_eq!(x_of(&live), Some(Value::Int64(6)), "the local value wins");

        set_instanceable(&mut store, true);
        live.notify_prim_edit(grove);
        live.recompose(&mut store);
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_eq!(x_of(&live), Some(Value::Int64(1)));
    }

    /// Asserts that `live` reports the same composition errors as a fresh
    /// composition, in the same order.
    fn assert_errors_match_fresh(live: &LiveStage, store: &mut InMemoryStore) {
        let fresh = Stage::compose(store, live.root, live.options.clone());
        assert_eq!(
            live.stage().composition_errors(),
            fresh.composition_errors(),
            "composition errors differ"
        );
    }

    /// Editing an asset's `defaultPrim` retargets every reference and payload
    /// with no authored prim path to that asset; each edit, once notified,
    /// recomposes to the same stage as a clean composition.
    ///
    /// Spec: AOUSD Core §10.3.2.1 (an omitted prim path assumes the target
    /// layer's `defaultPrim`).
    #[test]
    fn default_prim_edit_recomposes_dependent_placements() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let a = p(&mut store, "/A");
        let b = p(&mut store, "/B");
        let c = p(&mut store, "/C");
        let model = p(&mut store, "/Model");
        let model_child = p(&mut store, "/Model/ModelChild");
        let other = p(&mut store, "/Other");
        let other_child = p(&mut store, "/Other/OtherChild");
        let model_tok = store.tokens.intern("Model");
        let other_tok = store.tokens.intern("Other");
        let model_child_tok = store.tokens.intern("ModelChild");
        let other_child_tok = store.tokens.intern("OtherChild");

        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            a,
            PrimSpec::def().with_reference(Reference::to_default_prim(LayerId(2))),
        );
        root.insert_prim(
            b,
            PrimSpec::def()
                .with_payload(Reference::to_default_prim(LayerId(2)))
                .with_property(field_x, attr(5_i64)),
        );
        root.insert_prim(
            c,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), other)),
        );
        store.insert_layer(root);

        let mut asset = Layer::new(LayerId(2));
        asset.default_prim = Some(model_tok);
        asset.insert_prim(
            model,
            PrimSpec::def()
                .with_children(vec![model_child_tok])
                .with_property(field_x, attr(1_i64)),
        );
        asset.insert_prim(model_child, PrimSpec::def());
        asset.insert_prim(
            other,
            PrimSpec::def()
                .with_children(vec![other_child_tok])
                .with_property(field_x, attr(2_i64)),
        );
        asset.insert_prim(other_child, PrimSpec::def());
        store.insert_layer(asset);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        assert_eq!(live.prims_using_default_prim(LayerId(2), &store), [a, b]);
        assert!(live.prims_using_default_prim(LayerId(1), &store).is_empty());
        assert_matches_fresh(&live, &mut store, &[field_x]);

        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .set_property(PropertyPath::new(model, field_x), attr(3));
        live.notify_layer_prim_edits(LayerId(2), &[model]);
        live.recompose(&mut store);
        assert_eq!(live.prims_using_default_prim(LayerId(2), &store), [a, b]);
        assert_matches_fresh(&live, &mut store, &[field_x]);

        let a_child = |live: &LiveStage, store: &mut InMemoryStore, name: &str| {
            let child = p(store, &alloc::format!("/A/{name}"));
            live.stage().children_of(a) == Some(&[child][..])
        };
        assert!(a_child(&live, &mut store, "ModelChild"));

        // Nothing targets the root layer's `defaultPrim`, and an opinion
        // edit keeps the recorded dependencies.
        live.notify_default_prim_edit(LayerId(1));
        assert!(live.recompose(&mut store).is_empty());
        live.notify_prim_edit(a);
        assert_eq!(live.recompose(&mut store), [a]);
        assert_eq!(live.prims_using_default_prim(LayerId(2), &store), [a, b]);
        assert_matches_fresh(&live, &mut store, &[field_x]);

        // Retarget: `/A` and `/B` now compose `/Other`; `/C` is unchanged.
        store.layers.get_mut(&LayerId(2)).unwrap().default_prim = Some(other_tok);
        live.notify_default_prim_edit(LayerId(2));
        let updated = live.recompose(&mut store);
        assert!(updated.contains(&a) && updated.contains(&b));
        assert!(a_child(&live, &mut store, "OtherChild"));
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_errors_match_fresh(&live, &mut store);

        // Clear: both arcs are unresolved and contribute nothing.
        store.layers.get_mut(&LayerId(2)).unwrap().default_prim = None;
        live.notify_default_prim_edit(LayerId(2));
        live.recompose(&mut store);
        assert!(live.stage().children_of(a).unwrap_or(&[]).is_empty());
        assert_eq!(live.stage().composition_errors().len(), 2);
        assert_eq!(
            live.prims_using_default_prim(LayerId(2), &store),
            [a, b],
            "unresolved arcs still depend on the `defaultPrim`"
        );
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_errors_match_fresh(&live, &mut store);

        // Author it again: the placements resolve and the errors go away.
        store.layers.get_mut(&LayerId(2)).unwrap().default_prim = Some(model_tok);
        live.notify_default_prim_edit(LayerId(2));
        live.recompose(&mut store);
        assert!(a_child(&live, &mut store, "ModelChild"));
        assert_eq!(live.stage().composition_errors(), []);
        assert_matches_fresh(&live, &mut store, &[field_x]);
        assert_errors_match_fresh(&live, &mut store);
    }

    /// A scoped recomposition replaces the unresolved `defaultPrim` errors of
    /// the prims it recomposes, keeping the others.
    #[test]
    fn scoped_recompose_keeps_default_prim_errors() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let a = p(&mut store, "/A");
        let b = p(&mut store, "/B");

        let mut root = Layer::new(LayerId(1));
        for prim in [a, b] {
            root.insert_prim(
                prim,
                PrimSpec::def()
                    .with_reference(Reference::to_default_prim(LayerId(2)))
                    .with_property(field_x, attr(1_i64)),
            );
        }
        store.insert_layer(root);
        store.insert_layer(Layer::new(LayerId(2)));

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(live.stage().composition_errors().len(), 2);

        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .set_property(PropertyPath::new(a, field_x), attr(2_i64));
        live.notify_prim_edit(a);
        assert_eq!(live.recompose(&mut store), [a]);
        assert_eq!(live.stage().composition_errors().len(), 2);
        assert_errors_match_fresh(&live, &mut store);
        assert_matches_fresh(&live, &mut store, &[field_x]);
    }

    /// `/A` references a library prim from a sublayer with an offset, so
    /// its samples, and those of its namespace child, are read on that
    /// sublayer's timeline. Editing the sublayer's offset and scale and
    /// notifying the sublayer's layer recomposes both prims to match a
    /// fresh composition, at the default time and at every probed time.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (layer offsets on sublayers and
    /// references). OpenUSD: `_EvalRefOrPayloadArcs` in
    /// `pxr/usd/pcp/primIndex.cpp`.
    #[test]
    fn sublayer_offset_edit_retimes_arcs_it_authors() {
        use crate::{InterpolationType, LayerOffset};

        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let a = p(&mut store, "/A");
        let a_child = p(&mut store, "/A/Child");
        let source = p(&mut store, "/Source");
        let source_child = p(&mut store, "/Source/Child");
        let sampled = |samples: [(f64, f64); 2]| {
            PropertySpec::attribute()
                .with_time_samples(samples.map(|(t, v)| (t, Value::Double(v))).to_vec())
        };

        let mut root = Layer::new(LayerId(1));
        root.sublayers = vec![SublayerEntry::with_offset(
            LayerId(2),
            LayerOffset {
                offset: 10.0,
                scale: 1.0,
            },
        )];
        store.insert_layer(root);

        let mut sub = Layer::new(LayerId(2));
        let mut reference = Reference::with_asset(LayerId(3), source, "library.usda");
        reference.layer_offset = LayerOffset {
            offset: 5.0,
            scale: 1.0,
        };
        sub.insert_prim(a, PrimSpec::def().with_reference(reference));
        store.insert_layer(sub);

        let mut library = Layer::new(LayerId(3));
        library.insert_prim(
            source,
            PrimSpec::def().with_property(field_x, sampled([(0.0, 0.0), (10.0, 100.0)])),
        );
        library.insert_prim(
            source_child,
            PrimSpec::def().with_property(field_x, sampled([(0.0, 0.0), (4.0, 8.0)])),
        );
        store.insert_layer(library);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let x_at = |stage: &Stage, prim: PathId, time: f64| {
            stage
                .resolve_property_path_at_time(
                    PropertyPath::new(prim, field_x),
                    time,
                    InterpolationType::Linear,
                )
                .map(|resolved| resolved.value)
        };
        // 10 (the sublayer) + 5 (the reference).
        assert_eq!(x_at(live.stage(), a, 20.0), Some(Value::Double(50.0)));
        assert_eq!(x_at(live.stage(), a_child, 17.0), Some(Value::Double(4.0)));

        store.layers.get_mut(&LayerId(1)).unwrap().sublayers[0].offset = LayerOffset {
            offset: 4.0,
            scale: 2.0,
        };
        live.notify_layer_edit(LayerId(2));
        let updated = live.recompose(&mut store);
        assert!(updated.contains(&a), "`/A` authors its arc in the sublayer");
        assert!(updated.contains(&a_child), "`/A/Child` reads the arc");
        // 4 + 2 * (5 + t).
        assert_eq!(x_at(live.stage(), a, 19.0), Some(Value::Double(25.0)));
        assert_eq!(x_at(live.stage(), a_child, 18.0), Some(Value::Double(4.0)));
        assert_matches_fresh(&live, &mut store, &[field_x]);

        let fresh = Stage::compose(&mut store, LayerId(1), live.options.clone());
        for prim in [a, a_child] {
            for time in [0.0, 13.0, 14.0, 18.0, 22.0, 24.0, 34.0, 40.0] {
                assert_eq!(
                    x_at(live.stage(), prim, time),
                    x_at(&fresh, prim, time),
                    "{prim:?} at {time}"
                );
            }
        }
    }

    /// Regression: `/A` references `/Source` from a sublayer with an
    /// offset; `/Source` references `/Other`, which supplies `Child` and has
    /// a payload to `/Extra`, which supplies `Deep`. Neither child has a
    /// spec in the library `/A` references, only in the layers its nested
    /// arcs reach, and both are read on the sublayer's timeline. Editing
    /// the sublayer's offset recomposes them, as a fresh composition does.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (offsets compose along a chain of arcs).
    #[test]
    fn sublayer_offset_edit_retimes_prims_of_nested_arcs() {
        use crate::{InterpolationType, LayerOffset};

        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let a = p(&mut store, "/A");
        let a_child = p(&mut store, "/A/Child");
        let a_deep = p(&mut store, "/A/Deep");
        let source = p(&mut store, "/Source");
        let other = p(&mut store, "/Other");
        let other_child = p(&mut store, "/Other/Child");
        let extra = p(&mut store, "/Extra");
        let extra_deep = p(&mut store, "/Extra/Deep");
        let sampled = |samples: [(f64, f64); 2]| {
            PropertySpec::attribute()
                .with_time_samples(samples.map(|(t, v)| (t, Value::Double(v))).to_vec())
        };

        let mut root = Layer::new(LayerId(1));
        root.sublayers = vec![SublayerEntry::with_offset(
            LayerId(2),
            LayerOffset {
                offset: 10.0,
                scale: 1.0,
            },
        )];
        store.insert_layer(root);

        let mut sub = Layer::new(LayerId(2));
        let mut reference = Reference::with_asset(LayerId(3), source, "library.usda");
        reference.layer_offset = LayerOffset {
            offset: 5.0,
            scale: 1.0,
        };
        sub.insert_prim(a, PrimSpec::def().with_reference(reference));
        store.insert_layer(sub);

        let mut library = Layer::new(LayerId(3));
        library.insert_prim(
            source,
            PrimSpec::def().with_reference(Reference::with_asset(LayerId(4), other, "other.usda")),
        );
        store.insert_layer(library);

        let mut asset = Layer::new(LayerId(4));
        asset.insert_prim(
            other,
            PrimSpec::def().with_payload(Reference::with_asset(LayerId(5), extra, "extra.usda")),
        );
        asset.insert_prim(
            other_child,
            PrimSpec::def().with_property(field_x, sampled([(0.0, 0.0), (10.0, 100.0)])),
        );
        store.insert_layer(asset);

        let mut payload = Layer::new(LayerId(5));
        payload.insert_prim(extra, PrimSpec::def());
        payload.insert_prim(
            extra_deep,
            PrimSpec::def().with_property(field_x, sampled([(0.0, 0.0), (4.0, 8.0)])),
        );
        store.insert_layer(payload);

        let options = StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let x_at = |stage: &Stage, prim: PathId, time: f64| {
            stage
                .resolve_property_path_at_time(
                    PropertyPath::new(prim, field_x),
                    time,
                    InterpolationType::Linear,
                )
                .map(|resolved| resolved.value)
        };
        // 10 (the sublayer) + 5 (the reference).
        assert_eq!(x_at(live.stage(), a_child, 20.0), Some(Value::Double(50.0)));
        assert_eq!(x_at(live.stage(), a_deep, 17.0), Some(Value::Double(4.0)));

        store.layers.get_mut(&LayerId(1)).unwrap().sublayers[0].offset = LayerOffset {
            offset: 4.0,
            scale: 2.0,
        };
        live.notify_layer_edit(LayerId(2));
        let updated = live.recompose(&mut store);
        assert!(updated.contains(&a_child), "a nested reference supplies it");
        assert!(updated.contains(&a_deep), "a nested payload supplies it");
        // 4 + 2 * (5 + t).
        assert_eq!(x_at(live.stage(), a_child, 20.0), Some(Value::Double(30.0)));
        assert_eq!(x_at(live.stage(), a_deep, 20.0), Some(Value::Double(6.0)));
        assert_matches_fresh(&live, &mut store, &[field_x]);

        let fresh = Stage::compose(&mut store, LayerId(1), live.options.clone());
        for prim in [a_child, a_deep] {
            for time in [0.0, 13.0, 14.0, 15.0, 18.0, 20.0, 22.0, 34.0, 40.0] {
                assert_eq!(
                    x_at(live.stage(), prim, time),
                    x_at(&fresh, prim, time),
                    "{prim:?} at {time}"
                );
            }
        }
    }

    /// An edit that applies a multiple-apply instance recomposes the prim,
    /// which interns the names the instance forms, so the stage lists and
    /// resolves its properties without mutating the store.
    ///
    /// Spec: AOUSD Core §13.3.2 (instance names form property names).
    #[test]
    fn an_edit_applying_a_multiple_apply_instance_lists_its_properties() {
        use crate::{
            edit::{Address, Transaction},
            listop::ListOp,
            schema::{PropertyDefinition, SchemaDefinition, SchemaKind, SchemaRegistry},
            spec_path::SpecPath,
        };
        use alloc::sync::Arc;

        let mut store = InMemoryStore::default();
        let prim = p(&mut store, "/Floor");
        let slot = store.tokens.intern("SlotAPI");
        let index = store.tokens.intern("slot:__INSTANCE_NAME__:index");
        let api_schemas = store.tokens.intern("apiSchemas");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(prim, PrimSpec::def());
        store.insert_layer(layer);
        let mut builder = SchemaRegistry::builder();
        builder.register(
            SchemaDefinition::new(slot, SchemaKind::MultipleApplyApi)
                .with_property(PropertyDefinition::attribute(index).with_fallback(3)),
        );
        let options = StageOptions {
            schemas: Some(Arc::new(builder.build(&mut store.tokens))),
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        assert!(live.stage().property_names(prim, &store).is_empty());

        let instance = store.tokens.intern("SlotAPI:left:upper");
        let mut edit = Transaction::new();
        edit.set_metadata(
            Address::spec(LayerId(1), SpecPath::from_prim_path(prim)),
            api_schemas,
            FieldValue::TokenListOp(ListOp::prepended(vec![instance])),
        );
        live.apply(&mut store, &edit).expect("applies");

        let name = store
            .tokens
            .lookup("slot:left:upper:index")
            .expect("interned when the prim recomposed");
        let stage = live.stage();
        assert_eq!(stage.property_names(prim, &store), vec![name]);
        assert_eq!(
            stage
                .resolve_field_with_schema(prim, name, &store)
                .map(|resolved| resolved.value),
            Some(Value::Int(3))
        );
        let left_upper = store.tokens.lookup("left:upper").expect("instance name");
        let definition = stage.prim_definition(prim, &store).expect("on the stage");
        assert!(definition.has_api_instance(slot, left_upper));
    }

    /// Setting a property only the prim's schemas declare creates its spec
    /// with the definition's type and variability, as OpenUSD's
    /// `UsdPrim::CreateAttribute` does for a schema attribute; a value of
    /// another type is rejected; undo restores its absence exactly and redo
    /// restores it.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (the prim definition).
    #[test]
    fn setting_a_schema_property_creates_it_as_the_schema_declares() {
        use crate::{
            PropertyKind,
            edit::{EditError, EditTarget, Rejection, Transaction},
            property::{PropertyType, Variability},
            schema::{PropertyDefinition, SchemaDefinition, SchemaRegistry},
        };
        use alloc::sync::Arc;

        let mut store = InMemoryStore::default();
        let prim = p(&mut store, "/Rock");
        let rock = store.tokens.intern("Rock");
        let width = store.tokens.intern("width");
        let target = store.tokens.intern("target");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(prim, PrimSpec::def().with_type_name(rock));
        store.insert_layer(layer);
        let mut builder = SchemaRegistry::builder();
        builder.register(
            SchemaDefinition::typed(rock)
                .with_property(
                    PropertyDefinition::attribute(width)
                        .with_type(PropertyType::parse("float").unwrap())
                        .with_fallback(Value::Float(1.0))
                        .uniform(),
                )
                .with_property(PropertyDefinition::relationship(target)),
        );
        let options = StageOptions {
            schemas: Some(Arc::new(builder.build(&mut store.tokens))),
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let properties =
            |store: &InMemoryStore| store.layers[&LayerId(1)].prims[&prim].properties.clone();
        let absent = properties(&store);
        let at = |name| EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(prim, name));
        let resolved = |live: &LiveStage, store: &InMemoryStore| {
            live.stage()
                .resolve_field_with_schema(prim, width, store)
                .map(|r| r.value)
        };

        // A value of another type is rejected, and nothing is authored.
        let mut wrong = Transaction::new();
        wrong.set_default(at(width), Value::Int(3));
        assert!(matches!(
            live.apply(&mut store, &wrong),
            Err(EditError::Rejected {
                reason: Rejection::TypeMismatch { .. },
                ..
            })
        ));
        assert_eq!(properties(&store), absent);

        // A relationship is not an attribute.
        let mut relationship = Transaction::new();
        relationship.set_default(at(target), Value::Float(1.0));
        assert!(matches!(
            live.apply(&mut store, &relationship),
            Err(EditError::Rejected {
                reason: Rejection::NotAnAttribute(_),
                ..
            })
        ));

        let mut set = Transaction::new();
        set.set_default(at(width), Value::Float(2.0));
        let applied = live
            .apply(&mut store, &set)
            .expect("the schema declares it");
        let created = properties(&store);
        let spec = &created
            .iter()
            .find(|entry| entry.name == width)
            .expect("created")
            .spec;
        assert_eq!(spec.kind, PropertyKind::Attribute);
        assert_eq!(
            spec.type_name.as_ref().map(|t| t.type_name()),
            Some("float")
        );
        assert_eq!(spec.variability, Variability::Uniform);
        assert_eq!(spec.default, Some(Value::Float(2.0)));
        assert_eq!(resolved(&live, &store), Some(Value::Float(2.0)));

        let undone = live.apply(&mut store, &applied.inverse).expect("undo");
        assert_eq!(properties(&store), absent, "no spec is left behind");
        assert_eq!(resolved(&live, &store), Some(Value::Float(1.0)));

        live.apply(&mut store, &undone.inverse).expect("redo");
        assert_eq!(properties(&store), created);
        assert_eq!(resolved(&live, &store), Some(Value::Float(2.0)));
    }
}
