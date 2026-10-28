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
pub(crate) use layer_sets::LayerSets;

use hashbrown::{HashMap, HashSet};
use invalidation::{CycleHandling, InvalidationGraph};

use crate::{
    doc::LayerId,
    expression_variables::VariableReads,
    parallel::{Threads, join},
    path::PathId,
    prim_index::ArcKind,
};

use crate::live_stage::OPINION_EDIT;

/// A composition arc edge discovered during composition.
///
/// Direction: source was composed into target. "If source changes,
/// target may need recomposition."
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
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

/// What one composed prim depends on, as composition records it on the
/// thread that indexes the prim.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrimDependencies {
    /// The layers that contribute opinions to the prim, or author a
    /// reference or payload that reaches it, sorted, each once.
    pub(crate) layers: Vec<LayerId>,
    /// The layers whose `defaultPrim` composing the prim consulted.
    pub(crate) default_prim_layers: Vec<LayerId>,
    /// The arcs the prim receives opinions through: the site prim each
    /// reads (or the arc target, for inherits), its kind, and the root
    /// layer of the stack it reads; each once.
    pub(crate) arcs: Vec<(PathId, ArcKind, LayerId)>,
}

impl PrimDependencies {
    /// Lists each arc once, in no particular order.
    pub(crate) fn dedup_arcs(&mut self) {
        self.arcs
            .sort_unstable_by_key(|&(source, kind, layer)| (source, kind as u8, layer));
        self.arcs.dedup();
    }
}

/// The fewest prims whose dependency maps are built on threads
/// ([`DependencyRecords::builds_on_threads`]): about a millisecond of work.
const THREADED_BUILD_PRIMS: usize = 1024;

/// What composition recorded of a stage's dependencies, which
/// [`Self::build`] makes [`CompositionDeps`] of.
///
/// The arc dependencies of every composed prim become an
/// [`InvalidationGraph`], with their metadata and the layer-opinion edges
/// kept for queries.
pub(crate) struct DependencyRecords {
    /// What each composed prim the composition keeps depends on.
    prims: HashMap<PathId, PrimDependencies>,
    /// Whether [`Self::build`] builds [`CompositionDeps::arcs`].
    arc_metadata: bool,
    relocation_layers: HashSet<LayerId>,
    expression_variables: VariableReads,
}

impl DependencyRecords {
    /// The records of `prims`, each composed prim's under its path.
    pub(crate) fn new(prims: HashMap<PathId, PrimDependencies>) -> Self {
        Self {
            prims,
            arc_metadata: true,
            relocation_layers: HashSet::new(),
            expression_variables: VariableReads::default(),
        }
    }

    /// These records, building [`CompositionDeps::arcs`] only when
    /// `arc_metadata`: a live stage keeps no arc metadata of its full
    /// compositions, and an empty set leaves the rest of the maps as they
    /// are.
    pub(crate) fn with_arc_metadata(self, arc_metadata: bool) -> Self {
        Self {
            arc_metadata,
            ..self
        }
    }

    /// Whether the records are many enough for [`Self::build`] to build the
    /// maps on threads: for a few prims, as a live edit recomposes,
    /// starting a thread costs more than the maps.
    pub(crate) fn builds_on_threads(&self) -> bool {
        self.prims.len() >= THREADED_BUILD_PRIMS
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

    /// The dependency maps the records make.
    ///
    /// The graph, the arc metadata and the two layer maps are built from
    /// the prims' records apart, each by one pass over them, at once on as
    /// many of `threads` as the budget spares: each is the same whichever
    /// thread builds it, and the graph is built in the order the records
    /// iterate in, as adding the arcs one by one would.
    pub(crate) fn build(self, threads: &Threads) -> CompositionDeps {
        let one = Threads::new(1);
        let threads = if self.builds_on_threads() {
            threads
        } else {
            &one
        };
        let ((graph, arc_set), ((layer_to_prims, default_prim_dependents), prim_to_layers)) = join(
            threads,
            || {
                join(
                    threads,
                    || arc_graph(&self.prims),
                    || {
                        if self.arc_metadata {
                            arc_set(&self.prims)
                        } else {
                            HashSet::new()
                        }
                    },
                )
            },
            || {
                join(
                    threads,
                    || layer_prims(&self.prims),
                    || {
                        let mut sets = LayerSets::with_capacity(self.prims.len());
                        for (&prim, record) in &self.prims {
                            sets.insert(prim, &record.layers);
                        }
                        sets
                    },
                )
            },
        );
        CompositionDeps {
            graph,
            arcs: arc_set,
            layer_to_prims,
            prim_to_layers,
            default_prim_dependents,
            relocation_layers: self.relocation_layers,
            expression_variables: self.expression_variables,
        }
    }
}

/// The arcs of `prims` as dependency edges: each prim depends on the
/// sites it reads, in `OPINION_EDIT`.
fn arc_graph(prims: &HashMap<PathId, PrimDependencies>) -> InvalidationGraph<PathId> {
    let mut graph = InvalidationGraph::new();
    for (&target, record) in prims {
        for &(source, _, _) in &record.arcs {
            add_edge(&mut graph, target, source);
        }
    }
    graph
}

/// Adds the edge `target` depends on `source`, unless it closes a cycle:
/// if `source` changes, `target` needs recomposition.
fn add_edge(graph: &mut InvalidationGraph<PathId>, target: PathId, source: PathId) {
    // The edge closes a cycle only along a path from `source` to `target`:
    // when `source` depends on nothing yet, or nothing depends on `target`,
    // there is none, and the search for one, a walk of everything `source`
    // depends on, is skipped. A self-dependency is ignored either way.
    let handling = if graph.has_dependencies(source, OPINION_EDIT)
        && graph.has_dependents(target, OPINION_EDIT)
    {
        CycleHandling::Ignore
    } else {
        CycleHandling::Allow
    };
    let _ = graph.add_dependency(target, source, OPINION_EDIT, handling);
}

/// The arcs of `prims`, for diagnostic queries.
fn arc_set(prims: &HashMap<PathId, PrimDependencies>) -> HashSet<ArcDependency> {
    let mut arcs = HashSet::with_capacity(prims.values().map(|record| record.arcs.len()).sum());
    for (&target, record) in prims {
        arcs.extend(
            record
                .arcs
                .iter()
                .map(|&(source, arc_kind, layer)| ArcDependency {
                    source,
                    target,
                    arc_kind,
                    layer,
                }),
        );
    }
    arcs
}

/// The prims of `prims` each layer contributes to, and those whose
/// composition consulted each layer's `defaultPrim`.
fn layer_prims(prims: &HashMap<PathId, PrimDependencies>) -> (PrimsByLayer, PrimsByLayer) {
    // Each layer's set is sized once, rather than rehashed as it grows.
    let mut counts: HashMap<LayerId, usize> = HashMap::new();
    for record in prims.values() {
        for &layer in &record.layers {
            *counts.entry(layer).or_default() += 1;
        }
    }
    let mut layer_to_prims: PrimsByLayer = counts
        .into_iter()
        .map(|(layer, count)| (layer, HashSet::with_capacity(count)))
        .collect();
    let mut default_prim_dependents: PrimsByLayer = HashMap::new();
    for (&prim, record) in prims {
        for layer in &record.layers {
            if let Some(set) = layer_to_prims.get_mut(layer) {
                set.insert(prim);
            }
        }
        for &layer in &record.default_prim_layers {
            default_prim_dependents
                .entry(layer)
                .or_default()
                .insert(prim);
        }
    }
    (layer_to_prims, default_prim_dependents)
}

/// Prims by a layer they depend on.
type PrimsByLayer = HashMap<LayerId, HashSet<PathId>>;

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};

    use super::*;

    #[test]
    fn a_prims_arcs_are_listed_once() {
        let arc = (PathId::from_raw(1), ArcKind::References, LayerId(10));
        let mut record = PrimDependencies {
            arcs: alloc::vec![
                arc,
                (PathId::from_raw(3), ArcKind::Inherits, LayerId(10)),
                arc
            ],
            ..PrimDependencies::default()
        };
        record.dedup_arcs();
        let prims = HashMap::from([(PathId::from_raw(2), record)]);
        let deps = DependencyRecords::new(prims).build(&Threads::new(1));
        assert_eq!(deps.arcs.len(), 2, "each arc once");
    }

    #[test]
    fn layer_maps_list_each_prims_layers() {
        let record = |layers: &[u64], default_prims: &[u64]| PrimDependencies {
            layers: layers.iter().map(|&id| LayerId(id)).collect(),
            default_prim_layers: default_prims.iter().map(|&id| LayerId(id)).collect(),
            arcs: Vec::new(),
        };
        let prims = HashMap::from([
            (PathId::from_raw(5), record(&[1, 2], &[])),
            (PathId::from_raw(6), record(&[1, 2], &[3])),
            (PathId::from_raw(7), record(&[2], &[])),
        ]);
        let deps = DependencyRecords::new(prims).build(&Threads::new(1));
        let prims_of = |layer| {
            let mut prims: Vec<u32> = deps.layer_to_prims[&LayerId(layer)]
                .iter()
                .map(|prim| prim.get())
                .collect();
            prims.sort_unstable();
            prims
        };
        assert_eq!(prims_of(1), [5, 6]);
        assert_eq!(prims_of(2), [5, 6, 7]);
        assert_eq!(
            **deps.prim_to_layers.get(&PathId::from_raw(6)).unwrap(),
            [LayerId(1), LayerId(2)]
        );
        // Equal sets share storage.
        assert!(Arc::ptr_eq(
            &deps.prim_to_layers[&PathId::from_raw(5)],
            &deps.prim_to_layers[&PathId::from_raw(6)]
        ));
        assert_eq!(
            deps.default_prim_dependents[&LayerId(3)],
            HashSet::from([PathId::from_raw(6)])
        );
    }

    /// The dependencies built on any number of threads are the ones built
    /// on one.
    #[test]
    fn dependencies_are_the_same_on_any_number_of_threads() {
        let prims: HashMap<PathId, PrimDependencies> = (0..200_u32)
            .map(|at| {
                let layer = u64::from(at);
                let record = PrimDependencies {
                    layers: alloc::vec![LayerId(layer % 7), LayerId(10 + layer % 3)],
                    default_prim_layers: alloc::vec![LayerId(layer % 5)],
                    arcs: alloc::vec![
                        (PathId::from_raw(at / 2), ArcKind::References, LayerId(1)),
                        (PathId::from_raw(at + 1), ArcKind::Inherits, LayerId(2)),
                    ],
                };
                (PathId::from_raw(at), record)
            })
            .collect();
        let edges = |deps: &CompositionDeps| {
            let mut edges: Vec<(u32, u32)> = deps
                .graph
                .edges(OPINION_EDIT)
                .map(|(from, to)| (from.get(), to.get()))
                .collect();
            edges.sort_unstable();
            edges
        };
        let one = DependencyRecords::new(prims.clone()).build(&Threads::new(1));
        for threads in [2, 4] {
            let many = DependencyRecords::new(prims.clone()).build(&Threads::new(threads));
            assert_eq!(edges(&many), edges(&one));
            assert_eq!(many.arcs, one.arcs);
            assert_eq!(many.layer_to_prims, one.layer_to_prims);
            assert_eq!(many.prim_to_layers, one.prim_to_layers);
            assert_eq!(many.default_prim_dependents, one.default_prim_dependents);
        }
    }

    #[test]
    fn an_arc_closing_a_cycle_is_ignored_wherever_it_is_added() {
        let mut graph = InvalidationGraph::new();
        let mut add = |source, target| {
            add_edge(
                &mut graph,
                PathId::from_raw(target),
                PathId::from_raw(source),
            );
        };
        // 2 depends on 1, 3 on 2: nothing to search for yet.
        add(1, 2);
        add(2, 3);
        // 1 depending on 3 would close 1 -> 3 -> 2 -> 1.
        add(3, 1);
        // 4 depending on 3 closes nothing, though 3 depends on 2.
        add(3, 4);
        let depends = |key| {
            graph
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
        let record = PrimDependencies {
            arcs: alloc::vec![(PathId::from_raw(1), ArcKind::Inherits, LayerId(1))],
            ..PrimDependencies::default()
        };
        let prims = HashMap::from([(PathId::from_raw(2), record)]);
        let deps = DependencyRecords::new(prims).build(&Threads::new(1));

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
