// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Subtree replacement for stage-local controls. AOUSD Core §10–§11;
//! OpenUSD `UsdStage::LoadAndUnload`, `PcpChanges` (significant changes).
use super::*;

/// Index work performed by the most recent explicit recomposition. Source
/// namespace discovery and dependency inspection can still visit other prims;
/// these counts describe index construction and replacement, not total work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecompositionWork {
    /// Prim indexes constructed, including supporting ancestors and retries.
    pub composed_prim_indexes: usize,
    /// Existing or new prim slots replaced, including removals.
    pub replaced_prim_indexes: usize,
    /// Whether scope proof required reconstructing the complete stage.
    pub full_rebuild: bool,
}

impl LiveStage {
    /// The most recent recomposition's index work. No-op recompositions reset
    /// this to zero; value refreshes construct no indexes.
    #[must_use]
    pub fn recomposition_work(&self) -> RecompositionWork {
        self.work
    }

    fn control_roots(&self, store: &dyn LayerStore) -> Option<Vec<PathId>> {
        // Unnotified authoring and relocation changes need a unified rebuild.
        if self
            .generations
            .iter()
            .any(|(id, seen)| *seen != generations_of(store, *id))
            || self
                .stage
                .composition_errors()
                .iter()
                .any(|e| e.prim().is_none())
            || self.tracker.has_invalidated(OPINION_EDIT)
            || self
                .relocation_layers
                .iter()
                .any(|id| store.layer(*id).is_some_and(|l| !l.relocates.is_empty()))
        {
            return None;
        }
        let previous = self.stage.options();
        let mut roots: Vec<_> = self
            .stage
            .loadable_paths(store.paths(), store.paths().lookup(&crate::Path::root())?)
            .into_iter()
            .filter(|p| {
                previous.load_rules.is_loaded(store.paths().resolve(*p))
                    != self.options.load_rules.is_loaded(store.paths().resolve(*p))
            })
            .collect();
        let changed: Vec<_> = previous
            .muted_layers
            .symmetric_difference(&self.options.muted_layers)
            .copied()
            .collect();
        if !changed.is_empty() {
            let identifier = crate::LayerStackIdentifier {
                root: self.root,
                session: self.options.session_layer,
            };
            let local = crate::LayerStack::gather_identifier(store, identifier);
            if changed.iter().any(|id| local.layers.contains(id)) {
                return None;
            }
            for &prim in self.stage.prim_paths().collect::<Vec<_>>().iter() {
                for (layer, site) in self.stage.source_sites(prim) {
                    let Some(layer) = store.layer(layer) else {
                        continue;
                    };
                    for spec in layer.prim_specs(site) {
                        for list in [&spec.references, &spec.payloads] {
                            for arc in list.inserted_items() {
                                if arc.is_expression() {
                                    return None;
                                }
                                if arc.asset.is_none() {
                                    continue;
                                }
                                if reaches_layer(store, arc.layer, &changed)? {
                                    roots.push(prim);
                                }
                            }
                        }
                        for branch in spec.variant_branches() {
                            for list in [&branch.spec.references, &branch.spec.payloads] {
                                for arc in list.inserted_items() {
                                    if arc.is_expression() {
                                        return None;
                                    }
                                    if arc.asset.is_some()
                                        && reaches_layer(store, arc.layer, &changed)?
                                    {
                                        roots.push(prim);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Some(minimal_roots(store, &roots))
    }

    pub(super) fn recompose_controls(
        &mut self,
        store: &mut dyn LayerStore,
        changes: Option<&mut Changes>,
    ) -> Vec<PathId> {
        let Some(mut roots) = self.control_roots(store) else {
            return self.full_rebuild(store, changes);
        };
        self.controls_pending = false;
        if roots.is_empty() {
            self.stage.capture_controls(self.options.clone());
            self.record_generations(store);
            return Vec::new();
        }
        let mut partial = loop {
            let old_paths: Vec<_> = self
                .stage
                .prim_paths()
                .filter(|p| is_beneath(store, *p, &roots))
                .collect();
            for prim in roots.iter().chain(&old_paths) {
                self.tracker.mark(*prim, OPINION_EDIT);
            }
            let dependents: Vec<_> = self
                .tracker
                .drain(OPINION_EDIT)
                .affected()
                .scratch(&mut self.traversal_scratch)
                .run()
                .collect();
            roots.extend(dependents);
            roots = minimal_roots(store, &roots);
            if roots
                .iter()
                .any(|p| store.paths().resolve(*p) == &crate::Path::root())
            {
                return self.full_rebuild(store, changes);
            }
            let mask = intersect_mask(store, &roots, self.options.mask.as_ref());
            let options = StageOptions {
                mask: Some(mask),
                with_dependencies: true,
                ..self.options.clone()
            };
            let partial = Stage::compose(store, self.root, options);
            self.work.composed_prim_indexes += partial.composition_work().composed_prim_indexes;
            if partial
                .composition_errors()
                .iter()
                .any(|e| e.prim().is_none())
                || partial
                    .used_layers(false)
                    .iter()
                    .any(|id| store.layer(*id).is_some_and(|l| !l.relocates.is_empty()))
            {
                return self.full_rebuild(store, changes);
            }
            // Newly discovered prims may already be sources of retained arcs.
            for prim in partial
                .prim_paths()
                .filter(|p| is_beneath(store, *p, &roots))
            {
                self.tracker.mark(prim, OPINION_EDIT);
            }
            let dependents: Vec<_> = self
                .tracker
                .drain(OPINION_EDIT)
                .affected()
                .scratch(&mut self.traversal_scratch)
                .run()
                .collect();
            if dependents.iter().all(|p| is_beneath(store, *p, &roots)) {
                break partial;
            }
            roots.extend(dependents);
            roots = minimal_roots(store, &roots);
        };
        let old: HashSet<_> = self
            .stage
            .prim_paths()
            .filter(|p| is_beneath(store, *p, &roots))
            .collect();
        let new: HashSet<_> = partial
            .prim_paths()
            .filter(|p| is_beneath(store, *p, &roots))
            .collect();
        let mut affected: Vec<_> = old.union(&new).copied().collect();
        affected.sort_unstable();
        self.work.replaced_prim_indexes = affected.len();
        let deps = partial.take_deps().unwrap_or_default();
        self.expression_variables
            .extend(deps.expression_variables.clone());
        self.stage
            .merge_control_subtrees(store, partial, &affected, &roots, self.options.clone());
        self.update_prim_edges(&affected, &deps);
        for &prim in &affected {
            self.reindex_sources(prim);
        }
        self.record_generations(store);
        self.local_namespace = local::LocalNamespace::build(store, self.root, &self.options);
        if let Some(changes) = changes {
            changes.created = new.difference(&old).copied().collect();
            changes.removed = old.difference(&new).copied().collect();
            changes.created.sort_unstable();
            changes.removed.sort_unstable();
            changes.resynced = roots;
        }
        affected
    }
}

/// Inspect potential targets even while muted. Returning `None` requires a
/// fallback when expression-dependent assets cannot be proved from this walk.
fn reaches_layer(store: &dyn LayerStore, root: LayerId, targets: &[LayerId]) -> Option<bool> {
    let mut pending = alloc::vec![root];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if targets.contains(&id) {
            return Some(true);
        }
        if !seen.insert(id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            continue;
        };
        for sub in &layer.sublayers {
            if sub.is_expression() {
                return None;
            }
            pending.push(sub.layer);
        }
        for spec in layer
            .prims
            .values()
            .chain(layer.variant_prims.values().flatten())
        {
            for list in [&spec.references, &spec.payloads] {
                for arc in list.inserted_items() {
                    if arc.is_expression() {
                        return None;
                    }
                    if arc.asset.is_some() {
                        pending.push(arc.layer);
                    }
                }
            }
            for branch in spec.variant_branches() {
                for list in [&branch.spec.references, &branch.spec.payloads] {
                    for arc in list.inserted_items() {
                        if arc.is_expression() {
                            return None;
                        }
                        if arc.asset.is_some() {
                            pending.push(arc.layer);
                        }
                    }
                }
            }
        }
    }
    Some(false)
}

fn intersect_mask(
    store: &dyn LayerStore,
    roots: &[PathId],
    mask: Option<&PopulationMask>,
) -> PopulationMask {
    let Some(mask) = mask else {
        return PopulationMask {
            include: roots.to_vec(),
        };
    };
    let mut include = Vec::new();
    for &root in roots {
        for &selected in &mask.include {
            let r = store.paths().resolve(root);
            let s = store.paths().resolve(selected);
            if r.is_prefix_of(s) {
                include.push(selected);
            } else if s.is_prefix_of(r) {
                include.push(root);
            }
        }
    }
    PopulationMask {
        include: minimal_roots(store, &include),
    }
}
