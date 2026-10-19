// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dependency edges discovered during stage composition.
//!
//! When [`crate::stage::StageOptions::with_dependencies`] is enabled, the
//! composition algorithm records every arc and layer-opinion edge it discovers.
//! The resulting data is queryable on the composed [`crate::stage::Stage`].
//!
//! Arc dependencies are stored in an [`InvalidationGraph`] which serves as the
//! single source of truth for the dependency topology. Arc metadata
//! ([`ArcKind`], [`LayerId`]) and layer-opinion maps are stored separately for
//! diagnostic and notification queries.

use alloc::vec::Vec;
mod layer_sets;
pub use layer_sets::LayerDependencyStorage;
pub(crate) use layer_sets::{LayerSets, MutableLayerSet};

use hashbrown::{HashMap, HashSet};
use invalidation::{CycleHandling, InvalidationGraph};

use crate::{doc::LayerId, expression_variables::VariableReads, path::PathId, prim_index::ArcKind};

use crate::live_stage::OPINION_EDIT;

/// A composition arc edge discovered during composition.
///
/// Direction: source was composed into target. "If source changes,
/// target may need recomposition."
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArcDependency {
    /// The prim that authored the arc (or the arc target, for inherits).
    pub source: PathId,
    /// The composed prim that received opinions through this arc.
    pub target: PathId,
    /// Which arc type introduced this dependency.
    pub arc_kind: ArcKind,
    /// The layer providing the referenced opinions.
    pub layer: LayerId,
}

/// Composition dependency data built when `with_dependencies` is enabled.
///
/// This is the internal representation consumed by [`crate::stage::Stage`]
/// (which exposes public query methods) and [`crate::live_stage::LiveStage`]
/// (which takes ownership for incremental recomposition).
#[derive(Clone, Debug, Default)]
pub(crate) struct CompositionDeps {
    /// The invalidation graph: `target` depends on `source` in `OPINION_EDIT`.
    pub graph: InvalidationGraph<PathId>,
    /// Arc metadata for diagnostic queries.
    pub arcs: HashSet<ArcDependency>,
    /// Layer → prims that receive opinions from that layer, or that a
    /// reference or payload authored in it reaches.
    pub layer_to_prims: HashMap<LayerId, HashSet<PathId>>,
    /// Prim → layers that contribute opinions to it, or author a reference
    /// or payload that reaches it.
    pub prim_to_layers: LayerSets,
    /// Layer → prims whose composition follows (or fails to resolve) a
    /// reference or payload targeting that layer's `defaultPrim`.
    pub default_prim_dependents: HashMap<LayerId, HashSet<PathId>>,
    /// Layers whose `layerRelocates` composition consulted.
    pub relocation_layers: HashSet<LayerId>,
    /// The expression variables composition read evaluating variable
    /// expressions.
    pub expression_variables: VariableReads,
}

impl CompositionDeps {
    pub(crate) fn add_layer_opinion(&mut self, layer: LayerId, prim: PathId) {
        self.layer_to_prims.entry(layer).or_default().insert(prim);
        self.prim_to_layers.add(prim, layer);
    }
}

/// Builder for composition dependency data, used during composition.
///
/// Writes arc dependencies directly into an [`InvalidationGraph`] and
/// records metadata and layer-opinion edges for queries.
pub(crate) struct DependencyBuilder {
    graph: InvalidationGraph<PathId>,
    arc_set: HashSet<ArcDependency>,
    layer_to_prims: HashMap<LayerId, HashSet<PathId>>,
    prim_to_layers: HashMap<PathId, MutableLayerSet>,
    default_prim_dependents: HashMap<LayerId, HashSet<PathId>>,
    relocation_layers: HashSet<LayerId>,
    expression_variables: VariableReads,
}

impl DependencyBuilder {
    pub(crate) fn new() -> Self {
        Self {
            graph: InvalidationGraph::new(),
            arc_set: HashSet::new(),
            layer_to_prims: HashMap::new(),
            prim_to_layers: HashMap::new(),
            default_prim_dependents: HashMap::new(),
            relocation_layers: HashSet::new(),
            expression_variables: VariableReads::default(),
        }
    }

    /// Records the expression variables composition read.
    ///
    /// Changing one of them may change an asset path or variant selection,
    /// so the stage must be recomposed (OpenUSD resyncs the prims that
    /// depend on them, `PcpChanges::DidChange` in `pxr/usd/pcp/changes.cpp`).
    pub(crate) fn add_expression_variable_reads(&mut self, reads: VariableReads) {
        self.expression_variables.extend(reads);
    }

    /// Records that composition consulted the `layerRelocates` of `layers`.
    ///
    /// Editing them moves prims of every layer stack holding them
    /// (AOUSD Core §10.3.2.6).
    pub(crate) fn add_relocation_layers(&mut self, layers: impl IntoIterator<Item = LayerId>) {
        self.relocation_layers.extend(layers);
    }

    /// Records an arc dependency edge.
    ///
    /// Writes both the graph edge (target depends on source) and the
    /// diagnostic metadata.
    pub(crate) fn add_arc(&mut self, dep: ArcDependency) {
        if self.arc_set.insert(dep) {
            // The edge closes a cycle only along a path from `source` to
            // `target`: when `source` depends on nothing yet, or nothing
            // depends on `target`, there is none, and the search for one,
            // a walk of everything `source` depends on, is skipped. A
            // self-dependency is ignored either way.
            let handling = if self.graph.has_dependencies(dep.source, OPINION_EDIT)
                && self.graph.has_dependents(dep.target, OPINION_EDIT)
            {
                CycleHandling::Ignore
            } else {
                CycleHandling::Allow
            };
            // target depends on source: if source changes, target needs recomposition.
            let _ = self
                .graph
                .add_dependency(dep.target, dep.source, OPINION_EDIT, handling);
        }
    }

    /// Records layer-opinion dependency edges: `prim` receives opinions
    /// from each of `layers`, or is reached by a reference or payload one
    /// authors. `layers` is sorted, without repeats.
    pub(crate) fn add_layer_opinions(&mut self, prim: PathId, layers: &[LayerId]) {
        debug_assert!(layers.is_sorted_by(|a, b| a < b), "sorted and unique");
        for &layer in layers {
            self.layer_to_prims.entry(layer).or_default().insert(prim);
        }
        match self.prim_to_layers.entry(prim) {
            hashbrown::hash_map::Entry::Occupied(mut set) => {
                for &layer in layers {
                    set.get_mut().add(layer);
                }
            }
            hashbrown::hash_map::Entry::Vacant(slot) => match layers {
                [] => {}
                [layer] => {
                    slot.insert(MutableLayerSet::One(*layer));
                }
                _ => {
                    slot.insert(MutableLayerSet::Many(layers.to_vec()));
                }
            },
        }
    }

    /// Makes room for `additional` more arc edges.
    pub(crate) fn reserve_arcs(&mut self, additional: usize) {
        self.arc_set.reserve(additional);
    }

    /// Records that composing `prim` consulted the `defaultPrim` of `layer`,
    /// whether or not it resolved.
    ///
    /// Changing that `defaultPrim` retargets the arc, so `prim` must be
    /// recomposed (OpenUSD treats the change as a resync, see
    /// `PcpChanges::DidChange` in `pxr/usd/pcp/changes.cpp`).
    pub(crate) fn add_default_prim_dependency(&mut self, layer: LayerId, prim: PathId) {
        self.default_prim_dependents
            .entry(layer)
            .or_default()
            .insert(prim);
    }

    /// Drops every recorded dependency whose dependent prim is not in `prims`.
    ///
    /// Composition records dependencies while expanding arcs, before variant
    /// pruning and spec-less prim removal decide the final prim set. A
    /// dependency on a prim that is not composed would make invalidation
    /// target a path that does not exist.
    pub(crate) fn retain_prims<V>(&mut self, prims: &HashMap<PathId, V>) {
        let dropped: Vec<ArcDependency> = self
            .arc_set
            .iter()
            .filter(|dep| !prims.contains_key(&dep.target))
            .copied()
            .collect();
        for dep in dropped {
            self.arc_set.remove(&dep);
            let _ = self
                .graph
                .remove_dependency(dep.target, dep.source, OPINION_EDIT);
        }
        for layer_prims in self.layer_to_prims.values_mut() {
            layer_prims.retain(|prim| prims.contains_key(prim));
        }
        self.layer_to_prims
            .retain(|_, layer_prims| !layer_prims.is_empty());
        self.prim_to_layers
            .retain(|prim, _| prims.contains_key(prim));
        for dependents in self.default_prim_dependents.values_mut() {
            dependents.retain(|prim| prims.contains_key(prim));
        }
        self.default_prim_dependents
            .retain(|_, dependents| !dependents.is_empty());
    }

    /// Consumes the builder and produces [`CompositionDeps`].
    pub(crate) fn finish(self) -> CompositionDeps {
        CompositionDeps {
            graph: self.graph,
            arcs: self.arc_set,
            layer_to_prims: self.layer_to_prims,
            prim_to_layers: LayerSets::from_mutable(self.prim_to_layers),
            default_prim_dependents: self.default_prim_dependents,
            relocation_layers: self.relocation_layers,
            expression_variables: self.expression_variables,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn builder_deduplicates_arcs() {
        let mut builder = DependencyBuilder::new();
        let dep = ArcDependency {
            source: PathId::from_raw(1),
            target: PathId::from_raw(2),
            arc_kind: ArcKind::References,
            layer: LayerId(10),
        };
        builder.add_arc(dep);
        builder.add_arc(dep);
        let deps = builder.finish();
        assert_eq!(deps.arcs.len(), 1, "duplicate arcs should be deduplicated");
    }

    #[test]
    fn builder_deduplicates_layer_opinions() {
        let mut builder = DependencyBuilder::new();
        builder.add_layer_opinions(PathId::from_raw(5), &[LayerId(1)]);
        builder.add_layer_opinions(PathId::from_raw(5), &[LayerId(1), LayerId(2)]);
        let deps = builder.finish();
        assert_eq!(
            deps.layer_to_prims.get(&LayerId(1)).unwrap().len(),
            1,
            "duplicate layer opinions should be deduplicated"
        );
        assert_eq!(
            **deps.prim_to_layers.get(&PathId::from_raw(5)).unwrap(),
            [LayerId(1), LayerId(2)]
        );
    }

    #[test]
    fn an_arc_closing_a_cycle_is_ignored_wherever_it_is_added() {
        let arc = |source, target| ArcDependency {
            source: PathId::from_raw(source),
            target: PathId::from_raw(target),
            arc_kind: ArcKind::References,
            layer: LayerId(1),
        };
        let mut builder = DependencyBuilder::new();
        // 2 depends on 1, 3 on 2: nothing to search for yet.
        builder.add_arc(arc(1, 2));
        builder.add_arc(arc(2, 3));
        // 1 depending on 3 would close 1 -> 3 -> 2 -> 1.
        builder.add_arc(arc(3, 1));
        // 4 depending on 3 closes nothing, though 3 depends on 2.
        builder.add_arc(arc(3, 4));
        let deps = builder.finish();
        let depends = |key| {
            deps.graph
                .dependencies(PathId::from_raw(key), OPINION_EDIT)
                .collect::<Vec<_>>()
        };
        assert_eq!(depends(1), []);
        assert_eq!(depends(2), [PathId::from_raw(1)]);
        assert_eq!(depends(3), [PathId::from_raw(2)]);
        assert_eq!(depends(4), [PathId::from_raw(3)]);
    }

    #[test]
    fn graph_has_correct_topology() {
        let mut builder = DependencyBuilder::new();
        builder.add_arc(ArcDependency {
            source: PathId::from_raw(1),
            target: PathId::from_raw(2),
            arc_kind: ArcKind::Inherits,
            layer: LayerId(1),
        });
        let deps = builder.finish();

        // target (2) depends on source (1).
        let graph_deps: Vec<_> = deps
            .graph
            .dependencies(PathId::from_raw(2), OPINION_EDIT)
            .collect();
        assert!(
            graph_deps.contains(&PathId::from_raw(1)),
            "target should depend on source in the graph"
        );

        let dependents: Vec<_> = deps
            .graph
            .dependents(PathId::from_raw(1), OPINION_EDIT)
            .collect();
        assert!(
            dependents.contains(&PathId::from_raw(2)),
            "source should have target as a dependent"
        );
    }
}
