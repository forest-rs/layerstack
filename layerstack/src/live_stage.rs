//! Incremental recomposition via `invalidation`.
//!
//! [`LiveStage`] wraps a composed [`Stage`] and owns an
//! [`InvalidationGraph`] to support scoped recomposition: when a layer's
//! opinions change, only the transitively affected prims are recomposed.
//!
//! Callers mark roots, and propagation to transitive dependents happens at
//! drain time during [`LiveStage::recompose`].

use alloc::vec::Vec;

use hashbrown::{HashMap, HashSet};
use invalidation::{Channel, CycleHandling, InvalidationGraph, InvalidationSet};

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
/// `LiveStage` owns a fully composed [`Stage`] and an [`InvalidationGraph`]
/// (the single source of truth for dependency topology). An
/// [`InvalidationSet`] tracks which prims are dirty.
///
/// Notifications use lazy propagation: callers mark roots via
/// [`notify_layer_edit`](Self::notify_layer_edit) or
/// [`notify_prim_edit`](Self::notify_prim_edit), and transitive dependents
/// are expanded at drain time during [`recompose`](Self::recompose).
#[derive(Debug)]
pub struct LiveStage {
    stage: Stage,
    /// The dependency graph: target depends on source in `OPINION_EDIT`.
    graph: InvalidationGraph<PathId>,
    /// Arc metadata for incremental edge updates and diagnostics.
    arc_metadata: HashSet<ArcDependency>,
    /// Layer → prims that receive opinions from that layer.
    layer_to_prims: HashMap<LayerId, HashSet<PathId>>,
    /// Prim → layers that contribute opinions to it.
    prim_to_layers: HashMap<PathId, HashSet<LayerId>>,
    /// Tracks which prims are dirty.
    invalidated: InvalidationSet<PathId>,
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

        Self {
            stage,
            graph: deps.graph,
            arc_metadata: deps.arcs,
            layer_to_prims: deps.layer_to_prims,
            prim_to_layers: deps.prim_to_layers,
            invalidated: InvalidationSet::new(),
            root,
            options,
            needs_full_rebuild: false,
        }
    }

    /// Notifies that opinions in `layer` have been edited.
    ///
    /// Marks all prims that receive opinions from this layer as invalidation
    /// roots. Propagation to transitive dependents is deferred to
    /// [`recompose`](Self::recompose).
    pub fn notify_layer_edit(&mut self, layer: LayerId) {
        if let Some(prims) = self.layer_to_prims.get(&layer) {
            for &prim in prims {
                self.invalidated.mark(prim, OPINION_EDIT);
            }
        }
    }

    /// Notifies that opinions for specific prims within `layer` have changed.
    ///
    /// This is more precise than [`notify_layer_edit`](Self::notify_layer_edit):
    /// only the named prims are marked dirty (plus their transitive dependents
    /// at drain time), rather than every prim that receives opinions from the
    /// layer. Prims not actually connected to `layer` are silently ignored.
    pub fn notify_layer_prim_edits(&mut self, layer: LayerId, prims: &[PathId]) {
        if let Some(layer_prims) = self.layer_to_prims.get(&layer) {
            for &prim in prims {
                if layer_prims.contains(&prim) {
                    self.invalidated.mark(prim, OPINION_EDIT);
                }
            }
        }
    }

    /// Notifies that a specific prim's opinions have changed (any layer).
    ///
    /// This is more precise than [`notify_layer_edit`](Self::notify_layer_edit):
    /// only the named prim is marked dirty (plus its transitive dependents at
    /// drain time), rather than every prim that receives opinions from the layer.
    pub fn notify_prim_edit(&mut self, prim: PathId) {
        self.invalidated.mark(prim, OPINION_EDIT);
    }

    /// Batch-marks multiple prims as dirty (any layer).
    ///
    /// Equivalent to calling [`notify_prim_edit`](Self::notify_prim_edit) for
    /// each prim, but more convenient for bulk edits.
    pub fn notify_prim_edits(&mut self, prims: &[PathId]) {
        for &prim in prims {
            self.invalidated.mark(prim, OPINION_EDIT);
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
    /// - If a structural change was notified, performs a full rebuild and returns all prims.
    /// - If no prims are invalidated, returns an empty vec.
    /// - Otherwise, drains the invalidation set (expanding lazy roots to all
    ///   transitive dependents), performs a scoped recomposition, and updates
    ///   the dependency graph incrementally for the affected prims.
    pub fn recompose(&mut self, store: &mut dyn LayerStore) -> Vec<PathId> {
        if self.needs_full_rebuild {
            return self.full_rebuild(store);
        }

        if !self.invalidated.has_invalidated(OPINION_EDIT) {
            return Vec::new();
        }

        // Drain with lazy expansion: roots → all transitive dependents.
        let affected: Vec<PathId> =
            invalidation::drain_affected_sorted(&mut self.invalidated, &self.graph, OPINION_EDIT)
                .collect();

        // Expand the affected set to include arc sources so composition can
        // read inherit/reference targets.
        let mut mask_set: HashSet<PathId> = HashSet::from_iter(affected.iter().copied());
        for &prim in &affected {
            for dep in self.graph.dependencies(prim, OPINION_EDIT) {
                mask_set.insert(dep);
            }
        }
        let mask_vec: Vec<PathId> = mask_set.into_iter().collect();

        // Run scoped composition with a population mask.
        let scoped_opts = StageOptions {
            mask: Some(PopulationMask { include: mask_vec }),
            with_provenance: self.options.with_provenance,
            with_dependencies: true,
        };
        let mut partial = Stage::compose(store, self.root, scoped_opts);

        // Extract partial dependency data before merging the stage.
        let partial_deps = partial.take_deps().unwrap_or_default();

        // Merge partial prim/children results into the existing stage.
        self.stage.merge_from(partial);

        // Incrementally update dependency edges for each affected prim.
        for &prim in &affected {
            self.update_prim_edges(prim, &partial_deps);
        }

        affected
    }

    /// Returns a reference to the underlying composed stage.
    #[must_use]
    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// Returns a reference to the dependency graph.
    #[must_use]
    pub fn graph(&self) -> &InvalidationGraph<PathId> {
        &self.graph
    }

    /// Removes all edges involving `prim` (as target) and re-adds them from
    /// the partial composition's dependency data.
    fn update_prim_edges(&mut self, prim: PathId, partial: &CompositionDeps) {
        // Remove old arc metadata for this prim (as target).
        self.arc_metadata.retain(|a| a.target != prim);

        // Remove old graph edges where prim is the dependent.
        let old_deps: Vec<PathId> = self.graph.dependencies(prim, OPINION_EDIT).collect();
        for dep in old_deps {
            self.graph.remove_dependency(prim, dep, OPINION_EDIT);
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
            let _ = self.graph.add_dependency(
                arc.target,
                arc.source,
                OPINION_EDIT,
                CycleHandling::Ignore,
            );
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

    fn full_rebuild(&mut self, store: &mut dyn LayerStore) -> Vec<PathId> {
        self.needs_full_rebuild = false;
        self.invalidated.clear(OPINION_EDIT);

        let opts = StageOptions {
            with_dependencies: true,
            ..self.options.clone()
        };
        let mut stage = Stage::compose(store, self.root, opts);
        let deps = stage.take_deps().unwrap_or_default();

        self.stage = stage;
        self.graph = deps.graph;
        self.arc_metadata = deps.arcs;
        self.layer_to_prims = deps.layer_to_prims;
        self.prim_to_layers = deps.prim_to_layers;

        self.stage.prim_paths().collect()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{
        FieldValue, HashMap, Layer, PrimSpec, Reference, Value, doc::InMemoryStore, path::Path,
    };

    fn p(store: &mut InMemoryStore, s: &str) -> PathId {
        let path = Path::parse_absolute(s, &mut store.tokens).expect("valid path");
        store.paths.intern(path)
    }

    #[test]
    fn live_stage_matches_full_compose() {
        let mut store = InMemoryStore::default();
        let field_x = store.tokens.intern("x");
        let prim = p(&mut store, "/P");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            prims: HashMap::new(),
        };
        let mut spec = PrimSpec::default();
        spec.fields
            .insert(field_x, FieldValue::Value(Value::Int64(42)));
        layer.prims.insert(prim, spec);
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
            prims: HashMap::new(),
        };
        layer.prims.insert(prim, PrimSpec::default());
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
            prims: HashMap::new(),
        };
        let mut spec = PrimSpec::default();
        spec.fields
            .insert(field_x, FieldValue::Value(Value::Int64(1)));
        layer.prims.insert(prim, spec);
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
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(99)));
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
    fn structural_change_triggers_full_rebuild() {
        let mut store = InMemoryStore::default();
        let prim = p(&mut store, "/P");

        let mut layer = Layer {
            id: LayerId(1),
            sublayers: vec![],
            prims: HashMap::new(),
        };
        layer.prims.insert(prim, PrimSpec::default());
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Add a new prim to the store.
        let prim_q = p(&mut store, "/Q");
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            layer.prims.insert(prim_q, PrimSpec::default());
        }

        live.notify_structural_change();
        let updated = live.recompose(&mut store);

        assert!(!updated.is_empty(), "full rebuild should return prims");
        assert!(
            live.stage().has_prim(prim_q),
            "new prim should be in the stage"
        );
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
            prims: HashMap::new(),
        };
        let mut p_spec = PrimSpec::default();
        p_spec.references.append.push(Reference {
            layer: LayerId(2),
            prim_path: prim_q,
            asset: None,
        });
        root.prims.insert(prim_p, p_spec);
        store.insert_layer(root);

        let mut ref_layer = Layer {
            id: LayerId(2),
            sublayers: vec![],
            prims: HashMap::new(),
        };
        let mut q_spec = PrimSpec::default();
        q_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(10)));
        ref_layer.prims.insert(prim_q, q_spec);
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
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(77)));
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
            prims: HashMap::new(),
        };
        let mut spec2 = PrimSpec::default();
        spec2
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(10)));
        layer2.prims.insert(prim, spec2);
        store.insert_layer(layer2);

        let mut layer1 = Layer {
            id: LayerId(1),
            sublayers: vec![LayerId(2)],
            prims: HashMap::new(),
        };
        let mut spec1 = PrimSpec::default();
        spec1
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(20)));
        layer1.prims.insert(prim, spec1);
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
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(99)));
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
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(55)));
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
            prims: HashMap::new(),
        };
        // /Class_C defines x = 42.
        let mut class_spec = PrimSpec::default();
        class_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(42)));
        layer.prims.insert(class_c, class_spec);
        // /P inherits from /Class_C.
        let mut p_spec = PrimSpec::default();
        p_spec.inherits.append.push(class_c);
        layer.prims.insert(prim_p, p_spec);
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
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(100)));
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
            sublayers: vec![LayerId(2)],
            prims: HashMap::new(),
        };
        let mut a_spec = PrimSpec::default();
        a_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(1)));
        layer1.prims.insert(prim_a, a_spec);
        store.insert_layer(layer1);

        let mut layer2 = Layer {
            id: LayerId(2),
            sublayers: vec![],
            prims: HashMap::new(),
        };
        let mut b_spec = PrimSpec::default();
        b_spec
            .fields
            .insert(field_y, FieldValue::Value(Value::Int64(2)));
        layer2.prims.insert(prim_b, b_spec);
        store.insert_layer(layer2);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit both layers before recomposing.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim_a).unwrap();
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(11)));
        }
        {
            let layer = store.layers.get_mut(&LayerId(2)).unwrap();
            let spec = layer.prims.get_mut(&prim_b).unwrap();
            spec.fields
                .insert(field_y, FieldValue::Value(Value::Int64(22)));
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
            prims: HashMap::new(),
        };
        let mut spec = PrimSpec::default();
        spec.fields
            .insert(field_x, FieldValue::Value(Value::Int64(1)));
        layer.prims.insert(prim, spec);
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit and recompose.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim).unwrap();
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(99)));
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
            prims: HashMap::new(),
        };
        let mut p_spec = PrimSpec::default();
        p_spec.references.append.push(Reference {
            layer: LayerId(2),
            prim_path: prim_q,
            asset: None,
        });
        p_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(1)));
        root.prims.insert(prim_p, p_spec);
        store.insert_layer(root);

        let mut ref_layer = Layer {
            id: LayerId(2),
            sublayers: vec![],
            prims: HashMap::new(),
        };
        let mut q_spec = PrimSpec::default();
        q_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(100)));
        ref_layer.prims.insert(prim_q, q_spec);
        store.insert_layer(ref_layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit the referenced layer.
        {
            let layer = store.layers.get_mut(&LayerId(2)).unwrap();
            let spec = layer.prims.get_mut(&prim_q).unwrap();
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(200)));
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
            prims: HashMap::new(),
        };
        let mut a_spec = PrimSpec::default();
        a_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(1)));
        layer.prims.insert(prim_a, a_spec);

        let mut b_spec = PrimSpec::default();
        b_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(2)));
        layer.prims.insert(prim_b, b_spec);
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Only edit prim A directly.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim_a).unwrap();
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(99)));
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
            prims: HashMap::new(),
        };
        let mut a_spec = PrimSpec::default();
        a_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(1)));
        layer.prims.insert(prim_a, a_spec);

        let mut b_spec = PrimSpec::default();
        b_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(2)));
        layer.prims.insert(prim_b, b_spec);
        store.insert_layer(layer);

        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());

        // Edit only prim A in layer 1.
        {
            let layer = store.layers.get_mut(&LayerId(1)).unwrap();
            let spec = layer.prims.get_mut(&prim_a).unwrap();
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(99)));
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
            sublayers: vec![LayerId(2)],
            prims: HashMap::new(),
        };
        let mut a_spec = PrimSpec::default();
        a_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(1)));
        layer1.prims.insert(prim_a, a_spec);
        store.insert_layer(layer1);

        let mut layer2 = Layer {
            id: LayerId(2),
            sublayers: vec![],
            prims: HashMap::new(),
        };
        let mut b_spec = PrimSpec::default();
        b_spec
            .fields
            .insert(field_x, FieldValue::Value(Value::Int64(2)));
        layer2.prims.insert(prim_b, b_spec);
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
            prims: HashMap::new(),
        };
        for &(prim, val) in &[(prim_a, 1), (prim_b, 2), (prim_c, 3)] {
            let mut spec = PrimSpec::default();
            spec.fields
                .insert(field_x, FieldValue::Value(Value::Int64(val)));
            layer.prims.insert(prim, spec);
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
                .fields
                .insert(field_x, FieldValue::Value(Value::Int64(10)));
            layer
                .prims
                .get_mut(&prim_b)
                .unwrap()
                .fields
                .insert(field_x, FieldValue::Value(Value::Int64(20)));
        }

        live.notify_prim_edits(&[prim_a, prim_b]);
        let updated = live.recompose(&mut store);

        assert!(updated.contains(&prim_a), "A should be updated");
        assert!(updated.contains(&prim_b), "B should be updated");
        assert!(
            !updated.contains(&prim_c),
            "C should not be updated"
        );
    }
}
