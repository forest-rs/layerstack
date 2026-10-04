// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Subtree replacement for authored deltas and stage-local controls. AOUSD Core §10–§11;
//! OpenUSD `UsdStage::LoadAndUnload`, `PcpChanges` (significant changes).
use super::*;

/// Index and namespace work performed by the most recent recomposition.
/// Counts exclude dependency traversal, boundary child-list processing, and
/// prototype regrouping; these are work evidence, not a total CPU estimate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecompositionWork {
    /// Prim indexes constructed, including supporting ancestors and retries.
    pub composed_prim_indexes: usize,
    /// Existing or new prim slots replaced, including removals.
    pub replaced_prim_indexes: usize,
    /// Whether scope proof required reconstructing the complete stage.
    pub full_rebuild: bool,
    /// Authored namespace entries visited by discovery queries (duplicates count).
    pub inspected_source_paths: usize,
    /// Source slots indexed, including cache misses and authored journal deltas.
    pub indexed_source_paths: usize,
}

impl LiveStage {
    /// The most recent recomposition's index work. No-op recompositions reset
    /// this to zero; value refreshes construct no indexes.
    #[must_use]
    pub fn recomposition_work(&self) -> RecompositionWork {
        self.work
    }

    pub(super) fn queue_source_structure(
        &mut self,
        _store: &dyn LayerStore,
        layer: LayerId,
        paths: &[PathId],
    ) {
        self.structural_sources
            .extend(paths.iter().map(|p| (layer, *p)));
    }

    /// Map new/deleted source slots through the nearest retained source sites.
    /// Empty external layers use their recorded hosts as conservative roots.
    fn map_source_structure(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        paths: &[PathId],
    ) {
        if self.options.muted_layers.contains(&layer) {
            return;
        }
        let local = self.stage.layer_stack().contains(&layer);
        for &source in paths {
            if local {
                self.structural_roots.push(source);
            }
            let source_path = store.paths().resolve(source).clone();
            let mut ancestor = Some(source_path.clone());
            let mut mapped = false;
            while let Some(path) = ancestor {
                if let Some(id) = store.paths().lookup(&path) {
                    let dests = self.composed_prims_for_source(layer, id);
                    if !dests.is_empty() {
                        let suffix = source_path.strip_prefix(&path).expect("ancestor");
                        for dest in dests {
                            let mut mapped_path = store.paths().resolve(dest).clone();
                            for &name in suffix {
                                mapped_path = mapped_path.join(&[name]);
                            }
                            self.structural_roots
                                .push(store.paths_mut().intern(mapped_path));
                        }
                        mapped = true;
                        break;
                    }
                }
                ancestor = path.parent();
            }
            if !local && !mapped {
                let roots: Vec<_> = core::iter::once(self.root)
                    .chain(self.options.session_layer)
                    .collect();
                let Some(hosts) = self.layer_discovery.hosts_reaching(store, &roots, layer) else {
                    self.notify_structural_change();
                    return;
                };
                for (author, site) in hosts {
                    self.structural_roots
                        .extend(self.composed_prims_for_source(author, site));
                }
                if let Some(hosts) = self.layer_to_prims.get(&layer) {
                    self.structural_roots.extend(hosts.iter().copied());
                }
            }
        }
    }

    fn control_roots(&mut self, store: &mut dyn LayerStore) -> Option<Vec<PathId>> {
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
        for (layer, source) in core::mem::take(&mut self.structural_sources) {
            self.map_source_structure(store, layer, &[source]);
        }
        if self.needs_full_rebuild {
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
        roots.extend(self.structural_roots.iter().copied());
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
            let stacks: Vec<_> = core::iter::once(self.root)
                .chain(self.options.session_layer)
                .collect();
            for layer in changed {
                for (author, site) in self.layer_discovery.hosts_reaching(store, &stacks, layer)? {
                    roots.extend(self.composed_prims_for_source(author, site));
                }
            }
        }
        // A new deep spec can introduce previously absent ancestors. Replace
        // the highest missing ancestor too, rather than only its new leaf.
        for root in &mut roots {
            let mut path = store.paths().resolve(*root).clone();
            while let Some(parent) = path.parent() {
                if store
                    .paths()
                    .lookup(&parent)
                    .is_some_and(|p| self.stage.has_prim(p))
                {
                    break;
                }
                path = parent;
            }
            *root = store.paths_mut().intern(path);
        }
        Some(minimal_roots(store, &roots))
    }

    pub(super) fn recompose_controls(
        &mut self,
        store: &mut dyn LayerStore,
        changes: Option<&mut Changes>,
    ) -> Vec<PathId> {
        let local_structure_changed = self.structural_sources.iter().any(|(l, _)| *l == self.root);
        let Some(mut roots) = self.control_roots(store) else {
            return self.full_rebuild(store, changes);
        };
        self.controls_pending = false;
        self.structural_roots.clear();
        if roots.is_empty() {
            self.stage.capture_controls(self.options.clone());
            self.record_generations(store);
            return Vec::new();
        }
        let mut partial = loop {
            let old_paths: Vec<_> = roots
                .iter()
                .flat_map(|r| self.stage.traverse_all(*r))
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
            let partial = Stage::compose_retained(
                store,
                self.root,
                options,
                false,
                Some(&mut self.namespace_inventory),
                true,
            );
            self.work.composed_prim_indexes += partial.composition_work().composed_prim_indexes;
            self.work.inspected_source_paths += partial.composition_work().inspected_source_paths;
            self.work.indexed_source_paths += partial.composition_work().indexed_source_paths;
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
            // Deleting the last contributing descendant can remove an implicit
            // ancestor too (AOUSD Core §10, namespace composition). A masked
            // result cannot prove whether an omitted parent has other children:
            // replace its complete subtree before deciding to prune it.
            let missing_parents: Vec<_> = roots
                .iter()
                .filter_map(|root| store.paths().resolve(*root).parent())
                .filter(|parent| parent != &crate::Path::root())
                .filter_map(|parent| store.paths().lookup(&parent))
                .filter(|parent| self.stage.has_prim(*parent) && !partial.has_prim(*parent))
                .collect();
            if !missing_parents.is_empty() {
                roots.extend(missing_parents);
                roots = minimal_roots(store, &roots);
                continue;
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
        let old: HashSet<_> = roots
            .iter()
            .flat_map(|r| self.stage.traverse_all(*r))
            .collect();
        let new: HashSet<_> = partial
            .prim_paths()
            .filter(|p| is_beneath(store, *p, &roots))
            .collect();
        if new
            .iter()
            .all(|p| store.paths().resolve(*p) == &crate::Path::root())
            && self.stage.prim_paths().all(|p| {
                store.paths().resolve(p) == &crate::Path::root() || is_beneath(store, p, &roots)
            })
        {
            return self.full_rebuild(store, changes);
        }
        let mut affected: Vec<_> = old.union(&new).copied().collect();
        affected.sort_unstable();
        let hierarchy = affected.clone();
        // Structural authoring also changes source provenance/child-order at
        // boundary parents. Refresh their indexes while preserving full lists.
        for &root in &roots {
            if let Some(parent) = store
                .paths()
                .resolve(root)
                .parent()
                .and_then(|p| store.paths().lookup(&p))
                && partial.has_prim(parent)
                && !is_beneath(store, parent, &roots)
            {
                affected.push(parent);
            }
        }
        affected.sort_unstable();
        affected.dedup();
        self.work.replaced_prim_indexes = affected.len();
        let deps = partial.take_deps().unwrap_or_default();
        self.expression_variables
            .extend(deps.expression_variables.clone());
        self.stage.merge_control_subtrees(
            store,
            partial,
            &affected,
            &hierarchy,
            &roots,
            self.options.clone(),
        );
        self.update_prim_edges(&affected, &deps);
        for &prim in &affected {
            self.reindex_sources(prim);
        }
        self.record_generations(store);
        if local_structure_changed {
            self.local_namespace = local::LocalNamespace::build(store, self.root, &self.options);
        }
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
