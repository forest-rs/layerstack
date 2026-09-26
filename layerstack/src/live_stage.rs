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

use alloc::vec::Vec;

use hashbrown::{HashMap, HashSet};
use invalidation::{Channel, CycleHandling, InvalidationTracker};

use crate::{
    dependency_map::{ArcDependency, CompositionDeps},
    doc::{LayerId, LayerStore},
    edit::{Applied, EditError, Transaction},
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
    ) {
        for list in [references, payloads] {
            let items = list
                .explicit
                .iter()
                .flatten()
                .chain(&list.prepend)
                .chain(&list.append);
            pending.extend(items.map(|arc| arc.layer));
        }
    }
    let mut seen = HashSet::new();
    let mut pending = alloc::vec![root];
    while let Some(id) = pending.pop() {
        if id == LayerId::UNRESOLVED || !seen.insert(id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            continue;
        };
        pending.extend(layer.sublayers.iter().map(|entry| entry.layer));
        for spec in layer
            .prims
            .values()
            .chain(layer.variant_prims.values().flatten())
        {
            arc_layers(&spec.references, &spec.payloads, &mut pending);
            for set in spec.variant_sets.values() {
                for variant in set.variants.values() {
                    arc_layers(&variant.references, &variant.payloads, &mut pending);
                }
            }
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
    /// Tracks dependency topology and dirty prim state.
    tracker: InvalidationTracker<PathId>,
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
    /// The generation and structural generation of every layer the stage
    /// reads, as this stage last saw them; `None` for a layer the store
    /// did not hold (see [`LiveStage::notify_changed_layers`]).
    generations: HashMap<LayerId, Option<LayerGenerations>>,
    root: LayerId,
    options: StageOptions,
    needs_full_rebuild: bool,
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
            tracker,
            arc_metadata: deps.arcs,
            layer_to_prims: deps.layer_to_prims,
            prim_to_layers: deps.prim_to_layers,
            source_to_prims: HashMap::new(),
            prim_to_sources: HashMap::new(),
            default_prim_dependents: deps.default_prim_dependents,
            relocation_layers: deps.relocation_layers,
            generations: HashMap::new(),
            root,
            options,
            needs_full_rebuild: false,
        };
        live.reindex_all_sources();
        live.record_generations(store);
        live
    }

    /// Applies `txn` to the layers of `store` (see [`Transaction::apply`])
    /// and recomposes the prims it affects: the entry point for authoring a
    /// live stage.
    ///
    /// Edits of opinions on existing specs are notified as edits of their
    /// source sites ([`LiveStage::notify_layer_prim_edits`]), so only the
    /// prims drawing on those specs are recomposed. Edits that create or
    /// remove specs, or change variant selections, may change namespace and
    /// are notified as structural changes, which rebuild the stage.
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
        let outcome = crate::edit::apply(store, txn, Some(&self.stage))?;
        if outcome.structural {
            self.notify_structural_change();
        } else {
            for &(layer, prim) in &outcome.touched {
                self.notify_layer_prim_edits(layer, &[prim]);
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
        let recomposed = self.recompose(store);
        Ok(Applied {
            inverse: outcome.inverse,
            recomposed,
        })
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
        self.generations = participating_layers(store, self.root)
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
        if self.needs_full_rebuild {
            return self.full_rebuild(store);
        }

        if !self.tracker.has_invalidated(OPINION_EDIT) {
            return Vec::new();
        }

        // Drain with lazy expansion: roots → all transitive dependents.
        let affected: Vec<PathId> = self.tracker.drain_affected_sorted(OPINION_EDIT).collect();

        // Expand the affected set to include arc sources so composition can
        // read inherit/reference targets.
        // Also include each affected prim's current children, so the masked
        // composition's child lists for affected prims are complete and
        // hierarchy changes can be detected below.
        let mut mask_set: HashSet<PathId> = HashSet::from_iter(affected.iter().copied());
        for &prim in &affected {
            for dep in self.tracker.graph().dependencies(prim, OPINION_EDIT) {
                mask_set.insert(dep);
            }
            mask_set.extend(self.stage.children_of(prim).unwrap_or(&[]).iter().copied());
        }
        let mask_vec: Vec<PathId> = mask_set.into_iter().collect();

        // Run scoped composition with a population mask.
        let scoped_opts = StageOptions {
            mask: Some(PopulationMask { include: mask_vec }),
            with_provenance: self.options.with_provenance,
            with_dependencies: true,
            variant_fallbacks: self.options.variant_fallbacks.clone(),
        };
        let mut partial = Stage::compose(store, self.root, scoped_opts);

        // An opinion edit that turns out to change hierarchy (activation,
        // child ordering, ...) cannot be patched from a masked composition,
        // whose child lists are partial by construction.
        if self.stage.hierarchy_diverges(&partial, &affected) {
            return self.full_rebuild(store);
        }

        // Extract partial dependency data before merging the stage.
        let partial_deps = partial.take_deps().unwrap_or_default();

        // Replace only the recomposed prim indexes; hierarchy is unchanged.
        self.stage.merge_prims_from(partial, &affected);

        // Incrementally update dependency edges for each affected prim.
        for &prim in &affected {
            self.update_prim_edges(prim, &partial_deps);
            self.reindex_sources(prim);
        }

        affected
    }

    /// Returns a reference to the underlying composed stage.
    #[must_use]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// Removes all edges involving `prim` (as target) and re-adds them from
    /// the partial composition's dependency data.
    fn update_prim_edges(&mut self, prim: PathId, partial: &CompositionDeps) {
        // Remove old arc metadata for this prim (as target).
        self.arc_metadata.retain(|a| a.target != prim);

        // Remove old graph edges where prim is the dependent.
        let old_deps: Vec<PathId> = self
            .tracker
            .graph()
            .dependencies(prim, OPINION_EDIT)
            .collect();
        for dep in old_deps {
            self.tracker.remove_dependency(prim, dep, OPINION_EDIT);
        }

        // Remove old layer-opinion edges for this prim.
        if let Some(old_layers) = self.prim_to_layers.remove(&prim) {
            for layer in &old_layers {
                if let Some(prim_set) = self.layer_to_prims.get_mut(layer) {
                    prim_set.remove(&prim);
                }
            }
        }

        // Add new arcs from the partial composition.
        let new_arcs: Vec<ArcDependency> = partial
            .arcs
            .iter()
            .filter(|a| a.target == prim)
            .copied()
            .collect();
        for arc in &new_arcs {
            self.arc_metadata.insert(*arc);
            let _ = self
                .tracker
                .add_dependency(arc.target, arc.source, OPINION_EDIT);
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

        // Replace the prim's `defaultPrim` dependencies.
        for dependents in self.default_prim_dependents.values_mut() {
            dependents.remove(&prim);
        }
        for (layer, dependents) in &partial.default_prim_dependents {
            if dependents.contains(&prim) {
                self.default_prim_dependents
                    .entry(*layer)
                    .or_default()
                    .insert(prim);
            }
        }
        self.default_prim_dependents
            .retain(|_, dependents| !dependents.is_empty());
    }

    /// Recomposes the whole stage and returns every path in the new stage
    /// plus every path removed relative to the old one (see
    /// [`recompose`](Self::recompose)), sorted by [`PathId`].
    fn full_rebuild(&mut self, store: &mut dyn LayerStore) -> Vec<PathId> {
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
        self.reindex_all_sources();
        self.record_generations(store);

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
        let sites = self.stage.source_sites(prim);
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
        layer.prims.get_mut(&a).unwrap().properties[0].spec.default = Some(Value::Int64(3));
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
}
