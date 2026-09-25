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
    path::PathId,
    stage::{PopulationMask, Stage, StageOptions},
};

/// Invalidation channel for opinion (field value) edits.
pub const OPINION_EDIT: Channel = Channel::new(0);

/// Invalidation channel for structural changes (prims added/removed, arcs changed).
///
/// Structural changes fall back to a full rebuild.
pub const STRUCTURAL: Channel = Channel::new(1);

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
            root,
            options,
            needs_full_rebuild: false,
        };
        live.reindex_all_sources();
        live
    }

    /// Notifies that opinions in `layer` have been edited.
    ///
    /// Marks all prims that receive opinions from this layer as invalidation
    /// roots. Propagation to transitive dependents is deferred to
    /// [`recompose`](Self::recompose).
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
        self.reindex_all_sources();

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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
            prims: HashMap::new(),
            variant_prims: HashMap::new(),
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
}
