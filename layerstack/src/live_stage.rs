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
    changes
        .changed_info_only
        .retain(|path| !is_beneath(store, *path, &changes.resynced));
}

/// Every layer a stage rooted at `root` reads or would read: the root's
/// layer stack, and, transitively, the layer stacks every reference and
/// payload authored in them targets, whether or not they contribute
/// opinions yet. Layers the store does not hold are included too.
///
/// Spec: AOUSD Core §9 (layer stacks), §10.3.2.1 and §10.3.2.2
/// (references and payloads).
fn participating_layers(store: &dyn LayerStore, root: LayerId) -> HashSet<LayerId> {
    fn arc_layers(
        references: &crate::ListOp<crate::Reference>,
        payloads: &crate::ListOp<crate::Reference>,
        pending: &mut Vec<LayerId>,
        expressions: &mut bool,
    ) {
        for list in [references, payloads] {
            for arc in list.inserted_items() {
                *expressions |= arc.is_expression();
                pending.push(arc.layer);
            }
        }
    }
    let mut seen = HashSet::new();
    let mut expressions = false;
    let mut pending = alloc::vec![root];
    while let Some(id) = pending.pop() {
        if id == LayerId::UNRESOLVED || !seen.insert(id) {
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
                &spec.references,
                &spec.payloads,
                &mut pending,
                &mut expressions,
            );
            for branch in spec.variant_branches() {
                arc_layers(
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
            seen.extend(walked.stack.layers);
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
    prim_to_layers: HashMap<PathId, HashSet<LayerId>>,
    /// Source site `(layer, prim path in that layer)` → composed prims whose
    /// prim index draws specs or opinions from it.
    source_to_prims: HashMap<(LayerId, PathId), HashSet<PathId>>,
    /// Composed prim → source sites recorded in `source_to_prims`.
    prim_to_sources: HashMap<PathId, Vec<(LayerId, PathId)>>,
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
    notices: notices::Journal,
}

impl LiveStage {
    /// Performs an initial full composition and builds the dependency graph.
    pub fn compose(store: &mut dyn LayerStore, root: LayerId, options: StageOptions) -> Self {
        let opts = StageOptions {
            with_dependencies: true,
            ..options.clone()
        };
        let mut stage = Stage::compose(store, root, opts);
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
            default_prim_dependents: deps.default_prim_dependents,
            relocation_layers: deps.relocation_layers,
            expression_variables: deps.expression_variables,
            generations: HashMap::new(),
            root,
            options,
            needs_full_rebuild: false,
            notices: notices::Journal::default(),
        };
        live.reindex_all_sources();
        live.record_generations(store);
        live
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
    /// Other structural edits rebuild the stage; other opinion edits use
    /// scoped composition.
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
        // Precision is valid only against the source generations we composed.
        // Unknown external edits must not be hidden behind this transaction.
        let sources_current = self
            .generations
            .iter()
            .all(|(&layer, seen)| generations_of(store, layer) == *seen);
        let local_current = !self.needs_full_rebuild
            && !pending_opinions
            && self
                .local_namespace
                .as_ref()
                .is_some_and(|index| index.current(store));
        let namespace = local_current
            .then_some(self.local_namespace.as_ref())
            .flatten()
            .map(|index| index as &dyn crate::edit::SourceNamespace);
        let outcome = crate::edit::apply(store, txn, Some(&self.stage), namespace)?;
        let mut property_changes = alloc::collections::BTreeMap::new();
        if sources_current
            && !pending_opinions
            && !self.needs_full_rebuild
            && let Some(properties) = &outcome.properties
        {
            for &(layer, source, field) in properties {
                for prim in self.composed_prims_for_source(layer, source) {
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
            .flat_map(|&(layer, path)| self.composed_prims_for_source(layer, path))
            .collect();
        if outcome.structural
            && local_current
            && outcome.layers.iter().all(|layer| *layer == self.root)
            && let Some(paths) = &outcome.local_structure
            && !paths.is_empty()
            && let Some((recomposed, mut changes)) =
                self.refresh_local_structure(store, paths, &outcome.touched)
        {
            include_resyncs(store, &mut changes, &declaration_resyncs);
            self.notices.publish(&changes, &self.stage, store);
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
            if self.needs_full_rebuild || self.tracker.has_invalidated(OPINION_EDIT) {
                return None;
            }
            let dependents: Vec<Vec<PathId>> = edits
                .iter()
                .map(|edit| self.composed_prims_for_source(edit.layer, edit.path.prim_path()))
                .collect();
            let mut affected: Vec<PathId> = dependents.iter().flatten().copied().collect();
            affected.sort_unstable();
            affected.dedup();
            // An unnotified source edit may have changed more than this
            // transaction's slots. Leave it to ordinary recomposition.
            for prim in &affected {
                for layer in self.prim_to_layers.get(prim).into_iter().flatten() {
                    let Some(Some(seen)) = self.generations.get(layer) else {
                        return None;
                    };
                    let expected = (seen.0 + u64::from(outcome.layers.contains(layer)), seen.1);
                    if generations_of(store, *layer) != Some(expected) {
                        return None;
                    }
                }
            }
            self.stage
                .refresh_values(store, edits, &dependents)
                .then_some(affected)
        });
        if refreshed.is_none() {
            if outcome.structural {
                self.notify_structural_change();
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
        let recomposed = match refreshed {
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
            .map(|(prim, mut fields)| {
                fields.sort_unstable();
                fields.dedup();
                crate::PrimPropertyChanges { prim, fields }
            })
            .collect();
        self.notices.publish(&changes, &self.stage, store);
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
            .filter(|parent| !hierarchy.contains(parent))
            .collect();
        parents.sort_unstable();
        parents.dedup();
        // Supporting ancestors supply activation context. Boundary children
        // are ordered separately against the retained stage; their indexes
        // do not change merely because a sibling was added or removed.
        let boundary_children: Vec<_> = parents
            .iter()
            .map(|parent| (*parent, index.children(*parent).to_vec()))
            .collect();
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
        let mut partial = Stage::compose_local_paths(store, self.root, options, seed);
        let deps = partial.take_deps().unwrap_or_default();
        let mut changes = Changes::default();
        for path in &affected {
            match (self.stage.has_prim(*path), partial.has_prim(*path)) {
                (false, true) => changes.created.push(*path),
                (true, false) => changes.removed.push(*path),
                _ => {}
            }
        }
        changes.created.sort_unstable();
        changes.removed.sort_unstable();
        changes.resynced = roots
            .into_iter()
            .filter(|root| self.stage.has_prim(*root) || partial.has_prim(*root))
            .collect();
        changes.changed_info_only = parents
            .iter()
            .copied()
            .chain(touched.iter().map(|(_, path)| *path))
            .filter(|path| partial.has_prim(*path) && !is_beneath(store, *path, &changes.resynced))
            .collect();
        changes.changed_info_only.sort_unstable();
        changes.changed_info_only.dedup();
        let mut affected: Vec<_> = affected.into_iter().collect();
        affected.sort_unstable();
        self.stage
            .merge_local_subtrees(store, partial, &affected, &hierarchy, boundary_children);
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
    /// sets, so it is notified as a structural change
    /// ([`notify_structural_change`](Self::notify_structural_change)), as
    /// is a layer that joined or left the store. A layer whose other edits
    /// only changed opinion values is notified with
    /// [`notify_layer_edit`](Self::notify_layer_edit), which recomposes the
    /// prims drawing on it.
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
    /// Generations cannot tell which prims changed, so this is coarser than
    /// the notifications that name them, and a layer edited by a host that
    /// did notify precisely is reported again. Writes into the public
    /// fields of a layer do not move its generations and are not found.
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
                (Some(seen), Some(found)) if seen.1 == found.1 => self.notify_layer_edit(layer),
                _ => self.notify_structural_change(),
            }
        }
        changed.into_iter().map(|(layer, _)| layer).collect()
    }

    /// Records the generations of every layer the stage reads.
    fn record_generations(&mut self, store: &dyn LayerStore) {
        let mut layers = participating_layers(store, self.root);
        layers.extend(self.stage.clip_layers());
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
    pub fn notify_layer_prim_edits(&mut self, layer: LayerId, prims: &[PathId]) {
        for &source in prims {
            if let Some(dests) = self.source_to_prims.get(&(layer, source)) {
                for &prim in dests {
                    self.tracker.mark(prim, OPINION_EDIT);
                }
            }
        }
    }

    /// Returns the composed prims whose prim index draws specs or opinions
    /// from the prim spec at `source` within `layer`, sorted by [`PathId`].
    ///
    /// The mapping is taken from the composed prim indexes (local opinions,
    /// variants, and every arc, including nested ones), so a library prim
    /// referenced from several places maps to each referencing prim. It
    /// reflects the last composition; it is updated by
    /// [`recompose`](Self::recompose).
    #[must_use]
    pub fn composed_prims_for_source(&self, layer: LayerId, source: PathId) -> Vec<PathId> {
        let mut prims: Vec<PathId> = self
            .source_to_prims
            .get(&(layer, source))
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        prims.sort_unstable();
        prims
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
    pub fn prims_using_default_prim(&self, layer: LayerId) -> Vec<PathId> {
        let mut prims: Vec<PathId> = self
            .default_prim_dependents
            .get(&layer)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        prims.sort_unstable();
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
    /// This forces a full rebuild on the next [`recompose`](Self::recompose) call.
    pub fn notify_structural_change(&mut self) {
        self.needs_full_rebuild = true;
    }

    /// Recomposes affected prims and returns the set of prims that were updated.
    ///
    /// - If a structural change was notified, performs a full rebuild and
    ///   returns every prim path of the new stage plus every path that the
    ///   previous stage had and the new one lacks (removed prims), sorted by
    ///   [`PathId`]. Removal is computed as a before/after difference; callers
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
    ///   reordering edits), falls back to a full rebuild, with the same return
    ///   value as a structural change.
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
        let paths = self.recompose_report(store, Some(&mut changes));
        let info_only = core::mem::take(&mut changes.changed_info_only);
        include_resyncs(store, &mut changes, &info_only);
        self.notices.publish(&changes, &self.stage, store);
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
        self.notices.publish(&changes, &self.stage, store);
        changes
    }

    /// Detects source edits and recomposes before exposing a current scene.
    ///
    /// Scans participating layer generations once per call. Layer methods and
    /// transactions need no observer-specific authoring session. Direct writes
    /// to importer fields must call [`crate::Layer::touch`]. Unknown source
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
    /// Reports are retained only after the first observer subscribes. At most
    /// 64 batches are retained; a slower reader receives an explicit history
    /// error and must refresh its derived state. No callbacks are registered.
    pub fn change_cursor(&mut self) -> ChangeCursor {
        self.notices.cursor()
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
        if self.needs_full_rebuild {
            return self.full_rebuild(store, changes);
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

            // An opinion edit that turns out to change hierarchy (activation,
            // child ordering, ...) cannot be patched from a masked
            // composition, whose child lists are partial by construction.
            if self.stage.hierarchy_diverges(&partial, &affected) {
                return self.full_rebuild(store, changes);
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

        // Replace only the recomposed prim indexes; hierarchy is unchanged.
        self.stage.merge_prims_from(store, partial, &affected);
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
            changes.changed_info_only = affected
                .iter()
                .copied()
                .filter(|path| !is_beneath(store, *path, &changes.resynced))
                .collect();
        }
        affected
    }

    /// Returns a reference to the underlying composed stage.
    #[must_use]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// Replaces dependency data for the affected batch, leaving other
    /// dependents intact. Scan global metadata once, not once per prim.
    fn update_prim_edges(&mut self, affected: &[PathId], partial: &CompositionDeps) {
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
            for layer in &old_layers {
                if let Some(prim_set) = self.layer_to_prims.get_mut(layer) {
                    prim_set.remove(&prim);
                }
            }
        }

        // Add new layer-opinion edges from the partial composition.
        let mut layers_for_prim = HashSet::new();
        if let Some(partial_layers) = partial.prim_to_layers.get(&prim) {
            for &layer in partial_layers {
                self.layer_to_prims.entry(layer).or_default().insert(prim);
                layers_for_prim.insert(layer);
            }
        }
        if !layers_for_prim.is_empty() {
            self.prim_to_layers.insert(prim, layers_for_prim);
        }
    }

    /// Composes `affected` with a population mask that also holds the arc
    /// sources they draw on and their current children.
    ///
    /// The arc sources let composition read inherit and reference targets;
    /// the children make the masked composition's child lists for
    /// `affected` complete, so [`Stage::hierarchy_diverges`] can detect
    /// hierarchy changes.
    fn compose_scoped(&self, store: &mut dyn LayerStore, affected: &[PathId]) -> Stage {
        let mut mask_set: HashSet<PathId> = HashSet::from_iter(affected.iter().copied());
        for &prim in affected {
            for dep in self.tracker.graph().dependencies(prim, OPINION_EDIT) {
                mask_set.insert(dep);
            }
            mask_set.extend(self.stage.children_of(prim).unwrap_or(&[]).iter().copied());
        }
        let mask_vec: Vec<PathId> = mask_set.into_iter().collect();
        let scoped_opts = StageOptions {
            mask: Some(PopulationMask { include: mask_vec }),
            with_provenance: self.options.with_provenance,
            with_dependencies: true,
            variant_fallbacks: self.options.variant_fallbacks.clone(),
            schemas: self.options.schemas.clone(),
        };
        Stage::compose(store, self.root, scoped_opts)
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
        self.tracker.clear(OPINION_EDIT);
        let old_prims: HashSet<PathId> = self.stage.prim_paths().collect();

        let opts = StageOptions {
            with_dependencies: true,
            ..self.options.clone()
        };
        let mut stage = Stage::compose(store, self.root, opts);
        let deps = stage.take_deps().unwrap_or_default();

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
            changes.created.sort_unstable();
            changes.removed.sort_unstable();
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
        let prims: Vec<PathId> = self.stage.prim_paths().collect();
        for prim in prims {
            self.reindex_sources(prim);
        }
    }

    /// Replaces the source-site index entries for `prim` with those of its
    /// current prim index.
    fn reindex_sources(&mut self, prim: PathId) {
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
        let path = Path::parse_absolute(s, &mut store.tokens).expect("valid path");
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
    fn relocated_opinion_edits_recompose_the_relocated_prim() {
        // Spec: AOUSD Core §10.3.2.6. `/A/C` reads `/R/B` through the
        // relocation; an edit there recomposes it in scope.
        let mut store = InMemoryStore::default();
        let (_, c, field_x) = relocated_scene(&mut store);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let r_b = p(&mut store, "/R/B");
        assert_eq!(live.composed_prims_for_source(LayerId(2), r_b), [c]);

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

    /// Relocates cleared through the public fields and `touch` are found by
    /// polling, which rebuilds the namespace they moved.
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
    /// the public fields are not seen.
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

    /// A prim spec removed, through the public fields and `touch`, removes
    /// its composed prim.
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
        expected.sort_unstable();
        assert_eq!(updated, expected, "new-stage paths plus removed paths");
        let removed: Vec<PathId> = updated
            .iter()
            .copied()
            .filter(|path| !live.stage().has_prim(*path))
            .collect();
        let mut expected_removed = vec![b, b_child];
        expected_removed.sort_unstable();
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
        assert_eq!(live.composed_prims_for_source(LayerId(2), source), [a, b]);
        assert_eq!(live.composed_prims_for_source(LayerId(1), c), [c]);
        assert!(
            live.composed_prims_for_source(LayerId(2), a).is_empty(),
            "`/A` is not a path in the library layer"
        );

        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .set_property(PropertyPath::new(source, field_x), attr(2_i64));
        live.notify_layer_prim_edits(LayerId(2), &[source]);
        let mut updated = live.recompose(&mut store);
        updated.sort_unstable();
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
            let mut updated = live.recompose(&mut store);
            updated.sort_unstable();
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
        model_spec.variant_set_order.push(lod);
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
        assert_eq!(live.composed_prims_for_source(LayerId(1), geom), [geom]);

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
        model_spec.variant_set_order.push(lod);
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
            live.composed_prims_for_source(LayerId(2), source_child),
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
        assert_eq!(live.prims_using_default_prim(LayerId(3)), [r, pl]);
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
    /// composition, in any order.
    fn assert_errors_match_fresh(live: &LiveStage, store: &mut InMemoryStore) {
        let fresh = Stage::compose(store, live.root, live.options.clone());
        let as_set =
            |errors: &[crate::CompositionError]| errors.iter().cloned().collect::<HashSet<_>>();
        assert_eq!(
            as_set(live.stage().composition_errors()),
            as_set(fresh.composition_errors()),
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
        assert_eq!(live.prims_using_default_prim(LayerId(2)), [a, b]);
        assert!(live.prims_using_default_prim(LayerId(1)).is_empty());
        assert_matches_fresh(&live, &mut store, &[field_x]);

        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .set_property(PropertyPath::new(model, field_x), attr(3));
        live.notify_layer_prim_edits(LayerId(2), &[model]);
        live.recompose(&mut store);
        assert_eq!(live.prims_using_default_prim(LayerId(2)), [a, b]);
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
        assert_eq!(live.prims_using_default_prim(LayerId(2)), [a, b]);
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
            live.prims_using_default_prim(LayerId(2)),
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
            Address::spec(LayerId(1), SpecPath::from_prim_path(prim, &store.paths)),
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
                        .with_type(PropertyType::new("float", false, Value::Float(0.0)))
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
            spec.type_name.as_ref().map(|t| &*t.type_name),
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
