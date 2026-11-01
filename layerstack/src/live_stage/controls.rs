// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Subtree replacement for authored deltas and stage-local controls. AOUSD Core §10–§11;
//! OpenUSD `UsdStage::LoadAndUnload`, `PcpChanges` (significant changes).
use super::*;
use crate::sort;

/// Index and namespace work performed by the most recent recomposition.
/// Counts exclude dependency traversal and prototype regrouping; these are
/// work evidence, not a total CPU estimate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecompositionWork {
    /// Prim indexes constructed, including supporting ancestors and retries.
    pub composed_prim_indexes: usize,
    /// Existing or new prim slots replaced, including removals.
    pub replaced_prim_indexes: usize,
    /// Whether scope proof required reconstructing the complete stage.
    pub full_rebuild: bool,
    /// Layers whose child prim specs were indexed from every prim spec they
    /// hold, as composition reads them for prims whose specs do not list
    /// their children. The stage keeps the index across recompositions, so
    /// this counts layers first read, replaced, or that removed specs or
    /// outlived their change history.
    pub child_index_rebuilds: usize,
    /// Layers whose kept child index was patched with the prim specs they
    /// added since it was last brought up to date.
    pub child_index_patches: usize,
    /// Boundary parents, just outside the recomposed subtrees, whose child
    /// lists the partial composition composed whole: their child names were
    /// composed once, with the parent's index.
    pub boundary_lists_composed: usize,
    /// Boundary parents the partial composition did not index, whose child
    /// lists were ordered by composing their child names again against
    /// their retained index.
    pub boundary_lists_reordered: usize,
}

impl LiveStage {
    /// The most recent recomposition's index work, boundary child lists
    /// included in its child index counts. No-op recompositions reset this
    /// to zero; value refreshes construct no indexes.
    #[must_use]
    pub fn recomposition_work(&self) -> RecompositionWork {
        let index = self.child_index.work();
        RecompositionWork {
            child_index_rebuilds: index.rebuilds - self.child_index_mark.rebuilds,
            child_index_patches: index.patches - self.child_index_mark.patches,
            ..self.work
        }
    }

    /// Counts how a merge ordered its boundary parents' child lists.
    pub(super) fn record_boundary_orders(&mut self, orders: crate::stage::BoundaryOrders) {
        self.work.boundary_lists_composed += orders.composed;
        self.work.boundary_lists_reordered += orders.reordered;
    }

    /// Starts counting the work of a new recomposition.
    pub(super) fn reset_work(&mut self) {
        self.work = RecompositionWork::default();
        self.child_index_mark = self.child_index.work();
    }

    /// Queues the structural changes of `layer` at `paths`, and at
    /// `supports` the prims whose specs only support existing children.
    pub(super) fn queue_source_structure(
        &mut self,
        _store: &dyn LayerStore,
        layer: LayerId,
        paths: &[PathId],
        supports: &[PathId],
    ) {
        self.structural_sources
            .extend(paths.iter().map(|p| (layer, *p)));
        self.support_sources
            .extend(supports.iter().map(|p| (layer, *p)));
    }

    /// The composed prims the source prim `source` of `layer` composes
    /// into, or would if it held a spec: `source` itself in the stage's
    /// own layer stack, and the prims of the nearest site at or above it
    /// that a prim composes from, with the rest of its path.
    pub(super) fn source_prims(
        &mut self,
        store: &dyn LayerStore,
        layer: LayerId,
        source: PathId,
    ) -> Vec<PathId> {
        let mut prims = Vec::new();
        if self.stage.layer_stack().contains(&layer) {
            prims.push(source);
        }
        let source_path = store.paths().resolve(source).clone();
        let mut ancestor = Some(source_path.clone());
        while let Some(path) = ancestor {
            if let Some(id) = store.paths().lookup(&path) {
                let dests: Vec<PathId> = self.source_share(layer).prims(id).copied().collect();
                if !dests.is_empty() {
                    let suffix = source_path.strip_prefix(&path).expect("ancestor");
                    let paths = store.paths();
                    prims.extend(
                        dests
                            .into_iter()
                            .map(|dest| paths.intern_joined(dest, suffix)),
                    );
                    break;
                }
            }
            ancestor = path.parent();
        }
        prims
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
            // The prims whose dynamic payload arguments read the spec may
            // target other layers now.
            if let Some(dests) = self.dynamic_to_prims.get(&(layer, source)) {
                self.structural_roots.extend(dests.iter().copied());
            }
            let source_path = store.paths().resolve(source).clone();
            let mut ancestor = Some(source_path.clone());
            let mut mapped = false;
            while let Some(path) = ancestor {
                if let Some(id) = store.paths().lookup(&path) {
                    let dests: Vec<PathId> = self.source_share(layer).prims(id).copied().collect();
                    if !dests.is_empty() {
                        let suffix = source_path.strip_prefix(&path).expect("ancestor");
                        // Each destination's path is read, not copied: an
                        // edit to a prim as many prims reference maps to
                        // as many roots.
                        let paths = store.paths();
                        self.structural_roots.extend(
                            dests
                                .into_iter()
                                .map(|dest| paths.intern_joined(dest, suffix)),
                        );
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
                    let prims: Vec<PathId> =
                        self.source_share(author).prims(site).copied().collect();
                    self.structural_roots.extend(prims);
                }
                if let Some(hosts) = self.layer_to_prims.get(&layer) {
                    self.structural_roots.extend(hosts.iter().copied());
                }
            }
        }
    }

    /// The roots of the namespace to recompose, the prims beyond them
    /// whose prim indexes alone need refreshing, and every root requested
    /// before the roots beneath others were dropped; None for a full
    /// rebuild.
    fn control_roots(
        &mut self,
        store: &mut dyn LayerStore,
    ) -> Option<(Vec<PathId>, Vec<PathId>, Vec<PathId>)> {
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
                .any(|id| store.layer(*id).is_some_and(|l| !l.relocates().is_empty()))
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
            .loadable_paths(store.paths().lookup(&crate::Path::root())?, store)
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
                // The stage is composed again whole; with nothing else to
                // recompose, its report compares the stages.
                self.layer_stack_diff =
                    (roots.is_empty() && self.support_sources.is_empty()).then_some(false);
                return None;
            }
            let stacks: Vec<_> = core::iter::once(self.root)
                .chain(self.options.session_layer)
                .collect();
            for layer in changed {
                for (author, site) in self.layer_discovery.hosts_reaching(store, &stacks, layer)? {
                    roots.extend(self.source_share(author).prims(site).copied());
                }
            }
        }
        // A prim whose spec only supports existing children keeps its
        // namespace; when it is an ancestor of what is recomposed, the
        // masked composition composes its index too, and that is all it
        // needs. Any other is recomposed with its namespace.
        let pseudo_root = store.paths().lookup(&crate::Path::root());
        let mut refreshed = Vec::new();
        // The strict ancestors of `roots`, which each supporting prim is
        // looked up in: a bulk edit makes as many roots as prims it touches.
        let mut above = HashSet::new();
        if !self.support_sources.is_empty() {
            for &root in &roots {
                insert_ancestors(store.paths(), &mut above, root);
            }
        }
        for (layer, prim) in core::mem::take(&mut self.support_sources) {
            let local = self.stage.layer_stack().contains(&layer);
            // The pseudo-root's spec stack follows the root layer stack's
            // pseudo-root specs; with nothing beneath it recomposed, a
            // change of only those leaves its composed opinions as they are.
            if Some(prim) == pseudo_root {
                if local && !roots.is_empty() {
                    refreshed.push(prim);
                }
                continue;
            }
            if local && self.stage.has_prim(prim) && above.contains(&prim) {
                refreshed.push(prim);
            } else if local {
                insert_ancestors(store.paths(), &mut above, prim);
                roots.push(prim);
            } else {
                self.map_source_structure(store, layer, &[prim]);
                for &root in &self.structural_roots {
                    insert_ancestors(store.paths(), &mut above, root);
                }
                roots.append(&mut self.structural_roots);
                if self.needs_full_rebuild {
                    return None;
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
            *root = store.paths().intern(path);
        }
        let requested = roots.clone();
        let roots = minimal_roots(store, &roots);
        let within: HashSet<PathId> = roots.iter().copied().collect();
        refreshed.retain(|prim| !is_beneath_set(store, *prim, &within));
        sort::sort_unstable(&mut refreshed);
        refreshed.dedup();
        Some((roots, refreshed, requested))
    }

    /// Checks that each boundary parent of `retained`, whose index an edit
    /// beneath it kept, has the index a fresh composition gives it.
    #[cfg(test)]
    fn check_retained_boundaries(&self, store: &mut dyn LayerStore, retained: &[PathId]) {
        if retained.is_empty() {
            return;
        }
        let fresh = Stage::compose(store, self.root, self.options.clone());
        for parent in retained {
            let kept = self.stage.index_of(*parent).expect("retained index");
            let composed = fresh.index_of(*parent).expect("composed index");
            assert!(
                kept.data.same_records(&composed.data),
                "{parent:?} keeps {kept:?}, composes {composed:?}"
            );
        }
    }

    pub(super) fn recompose_controls(
        &mut self,
        store: &mut dyn LayerStore,
        changes: Option<&mut Changes>,
    ) -> Vec<PathId> {
        let local_structure_changed = self
            .structural_sources
            .iter()
            .chain(&self.support_sources)
            .any(|(l, _)| *l == self.root);
        // An edit beneath a boundary parent leaves the parent's own index
        // as it is, unless the edit also authored a spec only supporting
        // the new ones (an `over` above them), which the parent may draw
        // on through an arc: then the parents' indexes are composed again.
        let retain_boundaries = self.support_sources.is_empty();
        let Some((mut roots, mut refreshed, mut requested)) = self.control_roots(store) else {
            return self.full_rebuild(store, changes);
        };
        self.controls_pending = false;
        self.structural_roots.clear();
        // The prims recomposed for depending on what the roots hold.
        let mut dependent_roots = Vec::new();
        if roots.is_empty() {
            self.stage.capture_controls(self.options.clone());
            self.record_generations(store);
            return Vec::new();
        }
        let (mut partial, within) = loop {
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
            dependent_roots.extend(dependents.iter().copied());
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
            // The parents just outside the roots are the boundaries the
            // merge orders children of. Membership below the roots is an
            // ancestor walk in this set: an edit can resync as many sibling
            // roots as prims reference what it changed.
            let mut within: HashSet<PathId> = roots.iter().copied().collect();
            let boundaries = Boundaries {
                parents: roots
                    .iter()
                    .filter_map(|root| store.paths().parent(*root))
                    .filter(|parent| !is_beneath_set(store, *parent, &within))
                    .collect(),
                retained: retain_boundaries,
            };
            let partial = Stage::compose_merged(
                store,
                self.root,
                options,
                false,
                &boundaries,
                &self.child_index,
            );
            self.work.composed_prim_indexes += partial.composition_work().composed_prim_indexes;
            if partial
                .composition_errors()
                .iter()
                .any(|e| e.prim().is_none())
                || partial
                    .used_layers(false)
                    .iter()
                    .any(|id| store.layer(*id).is_some_and(|l| !l.relocates().is_empty()))
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
                .filter(|parent| self.stage.has_prim(*parent) && !indexed(&partial, *parent))
                .collect();
            if !missing_parents.is_empty() {
                requested.extend(missing_parents.iter().copied());
                roots.extend(missing_parents);
                roots = minimal_roots(store, &roots);
                continue;
            }
            // A supporting ancestor the masked composition left out is
            // recomposed with its namespace instead.
            let unrefreshed: Vec<_> = refreshed
                .iter()
                .copied()
                .filter(|prim| !partial.has_prim(*prim))
                .collect();
            if !unrefreshed.is_empty() {
                refreshed.retain(|prim| partial.has_prim(*prim));
                requested.extend(unrefreshed.iter().copied());
                roots.extend(unrefreshed);
                roots = minimal_roots(store, &roots);
                within = roots.iter().copied().collect();
                refreshed.retain(|prim| !is_beneath_set(store, *prim, &within));
                continue;
            }
            // Newly discovered prims may already be sources of retained arcs.
            for prim in partial
                .prim_paths()
                .filter(|p| is_beneath_set(store, *p, &within))
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
            if dependents
                .iter()
                .all(|p| is_beneath_set(store, *p, &within))
            {
                break (partial, within);
            }
            dependent_roots.extend(dependents.iter().copied());
            roots.extend(dependents);
            roots = minimal_roots(store, &roots);
        };
        let old: HashSet<_> = roots
            .iter()
            .flat_map(|r| self.stage.traverse_all(*r))
            .collect();
        let new: HashSet<_> = partial
            .prim_paths()
            .filter(|p| is_beneath_set(store, *p, &within))
            .collect();
        if new
            .iter()
            .all(|p| store.paths().resolve(*p) == &crate::Path::root())
            && self.stage.prim_paths().all(|p| {
                store.paths().resolve(p) == &crate::Path::root()
                    || is_beneath_set(store, p, &within)
            })
        {
            return self.full_rebuild(store, changes);
        }
        let mut affected: Vec<_> = old.union(&new).copied().collect();
        let hierarchy = affected.clone();
        // Structural authoring changes the child lists of the boundary
        // parents, whose full lists the partial composition composed. Their
        // indexes are replaced only when it composed them again.
        let mut retained = Vec::new();
        for &root in &roots {
            if let Some(parent) = store.paths().parent(root)
                && indexed(&partial, parent)
                && !is_beneath_set(store, parent, &within)
            {
                if partial.has_prim(parent) {
                    affected.push(parent);
                } else {
                    retained.push(parent);
                }
            }
        }
        // A refreshed prim changed when its spec stack did: an inert spec
        // came or went (`didAddInertPrim`, which `PcpChanges::DidChange`
        // records as a spec stack change of the prim alone, and OpenUSD
        // reports info-only). One whose specs changed only the child names
        // they list did not, and the pseudo-root, whose spec layerstack
        // makes for its first listed child (OpenUSD's always exists), never
        // does.
        let pseudo_root = store.paths().lookup(&crate::Path::root());
        let mut restacked: HashSet<PathId> = refreshed
            .iter()
            .copied()
            .filter(|&prim| {
                Some(prim) != pseudo_root && self.stage.prim_stack(prim) != partial.prim_stack(prim)
            })
            .collect();
        // A root the transaction changed only by inert specs, at it and at
        // every root requested beneath it, whose subtree composes the same
        // prims in the same order, changed only the spec stacks of the
        // prims an inert spec came to or left: OpenUSD's `DidChangeSpecs`
        // (`pxr/usd/pcp/changes.cpp`), which `UsdStage` reports info-only.
        // The prims depending on such prims alone changed only their spec
        // stacks too (`DidChangeSpecs` of each dependency); those depending
        // on another root requested, found by marking those roots again,
        // may have changed in any way.
        let mut inert = core::mem::take(&mut self.inert_prims);
        let opaque: Vec<PathId> = requested
            .iter()
            .copied()
            .filter(|root| !inert.contains(root))
            .collect();
        let mut tainted = HashSet::new();
        if !opaque.is_empty() && !dependent_roots.is_empty() {
            for &root in &opaque {
                self.tracker.mark(root, OPINION_EDIT);
                for prim in self.stage.traverse_all(root) {
                    self.tracker.mark(prim, OPINION_EDIT);
                }
            }
            tainted.extend(
                self.tracker
                    .drain(OPINION_EDIT)
                    .affected()
                    .scratch(&mut self.traversal_scratch)
                    .run(),
            );
        }
        for prim in &tainted {
            inert.remove(prim);
        }
        requested.extend(tainted.iter().copied());
        inert.extend(
            dependent_roots
                .iter()
                .copied()
                .filter(|prim| !tainted.contains(prim)),
        );
        // Beneath such a root, the roots requested for other changes are
        // resynced on their own, as OpenUSD resyncs `/Sky/Cloud` and
        // changes `/Sky` info-only when `DefinePrim` of `/Sky/Cloud`
        // authors an `over` of `/Sky` too, provided the rest of the root's
        // subtree composes the same prims in the same order.
        let mut restacked_roots: HashSet<PathId> = HashSet::new();
        let mut inner_resyncs = Vec::new();
        for &root in &roots {
            if Some(root) == pseudo_root
                || !inert.contains(&root)
                || !self.stage.has_prim(root)
                || !partial.has_prim(root)
            {
                continue;
            }
            let at_root = HashSet::from([root]);
            let inner: Vec<PathId> = requested
                .iter()
                .copied()
                .filter(|&at| !inert.contains(&at) && is_beneath_set(store, at, &at_root))
                .collect();
            if inner.contains(&root) {
                continue;
            }
            let inner = minimal_roots(store, &inner);
            let changed: HashSet<PathId> = inner.iter().copied().collect();
            let kept = |prim: &PathId| !is_beneath_set(store, *prim, &changed);
            if !self
                .stage
                .traverse_all(root)
                .filter(kept)
                .eq(partial.traverse_all(root).filter(kept))
            {
                continue;
            }
            restacked.extend(
                self.stage
                    .traverse_all(root)
                    .filter(kept)
                    .filter(|&prim| self.stage.prim_stack(prim) != partial.prim_stack(prim)),
            );
            restacked_roots.insert(root);
            inner_resyncs.extend(inner.into_iter().map(|at| (root, at)));
        }
        // The boundary parents' child lists before the merge: an edit that
        // keeps every child can still change their order.
        let boundary_lists: Vec<(PathId, Vec<PathId>)> = roots
            .iter()
            .filter_map(|&root| store.paths().parent(root))
            .filter(|&parent| !is_beneath_set(store, parent, &within))
            .map(|parent| {
                let children = self.stage.all_children_of(parent).unwrap_or_default();
                (parent, children.to_vec())
            })
            .collect();
        affected.extend(refreshed.iter().copied());
        store.paths().sort_paths(&mut affected, store.tokens());
        self.work.replaced_prim_indexes = affected.len();
        let deps = partial.take_deps().unwrap_or_default();
        self.expression_variables
            .extend(deps.expression_variables.clone());
        let orders = self.stage.merge_control_subtrees(
            store,
            partial,
            &affected,
            &hierarchy,
            &roots,
            &within,
            self.options.clone(),
            &self.child_index,
        );
        self.record_boundary_orders(orders);
        self.update_prim_edges(&affected, &deps);
        for &prim in &affected {
            self.reindex_sources(store.paths(), prim);
        }
        self.record_generations(store);
        if local_structure_changed {
            self.local_namespace = local::LocalSlot::pending(store, self.root, &self.options);
        }
        #[cfg(test)]
        self.check_retained_boundaries(store, &retained);
        // The boundary parents whose child lists changed are reported with
        // the prims recomposed, in namespace order: the change lists below
        // are filtered from them, so each is sorted already.
        let affected = merge_sorted(store, affected, retained);
        if let Some(changes) = changes {
            let filtered = |keep: &dyn Fn(PathId) -> bool| -> Vec<PathId> {
                affected.iter().copied().filter(|p| keep(*p)).collect()
            };
            changes.created = filtered(&|p| new.contains(&p) && !old.contains(&p));
            changes.removed = filtered(&|p| old.contains(&p) && !new.contains(&p));
            // A boundary parent whose child list gained or lost a prim is
            // not reported: OpenUSD resyncs the child's path alone (see
            // `LiveStage::refresh_local_structure`). A refreshed prim whose
            // spec stack changed is.
            // A child list that keeps its children can change order: a spec
            // added to or removed from a layer weaker than the others of a
            // child moves it, as child names compose weakest layer first.
            // The child is resynced, so that a client reads its parent's
            // children again, whether or not its spec is inert (OpenUSD
            // reports the inert one info-only, and keeps the old order:
            // `override_reorders_children` in `prim_resync_oracle.py`).
            let reordered: HashSet<PathId> = boundary_lists
                .iter()
                .filter(|(parent, before)| {
                    let after = self.stage.all_children_of(*parent).unwrap_or_default();
                    kept_order(before, after) != kept_order(after, before)
                })
                .map(|(parent, _)| *parent)
                .collect();
            restacked_roots.retain(|root| {
                store
                    .paths()
                    .parent(*root)
                    .is_none_or(|parent| !reordered.contains(&parent))
            });
            // A root no prim was or is at is not among `affected`.
            roots.retain(|root| !restacked_roots.contains(root));
            roots.extend(
                inner_resyncs
                    .iter()
                    .filter(|(root, _)| restacked_roots.contains(root))
                    .map(|(_, at)| *at),
            );
            let resynced: HashSet<PathId> = roots.iter().copied().collect();
            changes.changed_info_only =
                filtered(&|p| restacked.contains(&p) && !is_beneath_set(store, p, &resynced));
            changes.resynced = filtered(&|p| resynced.contains(&p));
            if changes.resynced.len() < roots.len() {
                changes.resynced = roots;
                store
                    .paths()
                    .sort_paths(&mut changes.resynced, store.tokens());
            }
        }
        affected
    }
}

/// The paths of `list` that `other` lists too, in `list`'s order.
pub(super) fn kept_order(list: &[PathId], other: &[PathId]) -> Vec<PathId> {
    let other: HashSet<PathId> = other.iter().copied().collect();
    list.iter().copied().filter(|p| other.contains(p)).collect()
}

/// Whether the partial composition `partial` indexed `prim`: it holds the
/// prim's index, or the whole child list of a boundary parent it indexed
/// for its children only ([`Boundaries::retained`]).
fn indexed(partial: &Stage, prim: PathId) -> bool {
    partial.has_prim(prim) || partial.has_boundary_children(prim)
}

/// The paths of `sorted`, in namespace order, and those of `more`, sorted
/// here, in one list in namespace order, each once.
fn merge_sorted(store: &dyn LayerStore, sorted: Vec<PathId>, mut more: Vec<PathId>) -> Vec<PathId> {
    if more.is_empty() {
        return sorted;
    }
    let (paths, tokens) = (store.paths(), store.tokens());
    paths.sort_paths(&mut more, tokens);
    let mut merged = Vec::with_capacity(sorted.len() + more.len());
    let (mut a, mut b) = (sorted.into_iter().peekable(), more.into_iter().peekable());
    while let (Some(&x), Some(&y)) = (a.peek(), b.peek()) {
        match paths.cmp_paths(x, y, tokens) {
            core::cmp::Ordering::Less => merged.extend(a.next()),
            core::cmp::Ordering::Greater => merged.extend(b.next()),
            core::cmp::Ordering::Equal => {
                merged.extend(a.next());
                b.next();
            }
        }
    }
    merged.extend(a);
    merged.extend(b);
    merged
}

/// Inserts the strict ancestors of `path` into `set`, stopping at the first
/// one already there: its ancestors are too.
pub(super) fn insert_ancestors(
    paths: &crate::PathInterner,
    set: &mut HashSet<PathId>,
    path: PathId,
) {
    let mut current = paths.parent(path);
    while let Some(at) = current {
        if !set.insert(at) {
            break;
        }
        current = paths.parent(at);
    }
}

/// The mask that composes `roots` within `mask`: each root beneath a
/// selected path, and each selected path beneath a root. Both are ancestor
/// walks, so the cost is the paths' depth, not roots times selections.
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
    let root_set: HashSet<PathId> = roots.iter().copied().collect();
    let selected: HashSet<PathId> = mask.include.iter().copied().collect();
    let include: Vec<PathId> = mask
        .include
        .iter()
        .copied()
        .filter(|s| is_beneath_set(store, *s, &root_set))
        .chain(
            roots
                .iter()
                .copied()
                .filter(|r| is_beneath_set(store, *r, &selected)),
        )
        .collect();
    PopulationMask {
        include: minimal_roots(store, &include),
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use crate::{
        InMemoryStore, Layer, LayerId, LayerStore, LiveStage, PathInterner, PrimSpec, Reference,
        Specifier, StageOptions, TokenInterner, Transaction, edit::EditTarget,
    };

    /// A store counting the reads of its path interner: the work of
    /// telling whether a path is beneath the roots of a recomposition.
    struct CountingStore {
        inner: InMemoryStore,
        path_reads: AtomicUsize,
    }

    impl LayerStore for CountingStore {
        fn layer(&self, id: LayerId) -> Option<&Layer> {
            self.inner.layer(id)
        }
        fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
            self.inner.layer_mut(id)
        }
        fn tokens(&self) -> &TokenInterner {
            self.inner.tokens()
        }
        fn paths(&self) -> &PathInterner {
            self.path_reads.fetch_add(1, Ordering::Relaxed);
            self.inner.paths()
        }
    }

    /// The path reads of each of three edits to `/Proto` while `count`
    /// siblings under `/World` reference it: deactivating it, activating
    /// it again, and authoring a new child spec under it. Each resyncs
    /// every sibling.
    fn edit_reads(count: usize) -> [usize; 3] {
        let mut store = InMemoryStore::default();
        let proto = store.path("/Proto");
        let geo = store.path("/Proto/Geo");
        let world = store.path("/World");
        let added = store.path("/Proto/Added");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(proto, PrimSpec::def());
        layer.insert_prim(geo, PrimSpec::def());
        layer.insert_prim(world, PrimSpec::def());
        for i in 0..count {
            let sibling = store.path(&alloc::format!("/World/I{i}"));
            layer.insert_prim(
                sibling,
                PrimSpec::def().with_reference(Reference::new(LayerId(1), proto)),
            );
        }
        store.insert_layer(layer);
        let mut store = CountingStore {
            inner: store,
            path_reads: AtomicUsize::new(0),
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let target = EditTarget::for_layer(LayerId(1));
        let mut deactivate = Transaction::new();
        deactivate.set_active(target.prim(proto), false);
        let mut activate = Transaction::new();
        activate.set_active(target.prim(proto), true);
        let mut create = Transaction::new();
        create.create_prim(target.prim(added), Specifier::Def, None);
        let mut reads = [0; 3];
        for (read, edit) in reads.iter_mut().zip([deactivate, activate, create]) {
            store.path_reads.store(0, Ordering::Relaxed);
            live.apply(&mut store, &edit).expect("edit applies");
            assert!(!live.recomposition_work().full_rebuild);
            *read = store.path_reads.load(Ordering::Relaxed);
        }
        let added_under = store
            .inner
            .path(&alloc::format!("/World/I{}/Added", count - 1));
        assert!(live.stage().has_prim(added_under));
        reads
    }

    /// A child spec removed from and authored again under a prim many
    /// siblings reference leaves and returns under each of them: removing
    /// the prims' keys from the dependency graph keeps the edges the
    /// recomposed siblings need.
    #[test]
    fn removed_child_returns_under_every_sibling() {
        let mut store = InMemoryStore::default();
        let proto = store.path("/Proto");
        let added = store.path("/Proto/Added");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(proto, PrimSpec::def());
        let siblings: alloc::vec::Vec<_> = (0..20)
            .map(|i| store.path(&alloc::format!("/World/I{i}")))
            .collect();
        for &sibling in &siblings {
            layer.insert_prim(
                sibling,
                PrimSpec::def().with_reference(Reference::new(LayerId(1), proto)),
            );
        }
        store.insert_layer(layer);
        let under: alloc::vec::Vec<_> = (0..20)
            .map(|i| store.path(&alloc::format!("/World/I{i}/Added")))
            .collect();
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let target = EditTarget::for_layer(LayerId(1));
        let mut create = Transaction::new();
        create.create_prim(target.prim(added), Specifier::Def, None);
        let mut remove = Transaction::new();
        remove.remove_spec(target.prim(added));
        // The created or removed prims, `/Proto/Added` first, in namespace
        // order (`/World/I10/Added` before `/World/I2/Added`).
        let mut changed = under.clone();
        changed.push(added);
        store.paths.sort_paths(&mut changed, &store.tokens);
        for (edit, present) in [(&create, true), (&remove, false), (&create, true)] {
            let applied = live.apply(&mut store, edit).expect("edit applies");
            assert!(!live.recomposition_work().full_rebuild);
            let listed = if present {
                &applied.changes.created
            } else {
                &applied.changes.removed
            };
            assert_eq!(*listed, changed);
            assert_eq!(applied.changes.resynced, changed);
            for &prim in &under {
                assert_eq!(live.stage().has_prim(prim), present, "{prim:?}");
            }
        }
        assert_fresh_edges(&live, &mut store);
    }

    /// A child spec authored beneath a prim that siblings reference leaves
    /// the siblings' own indexes as they are: only the new children are
    /// composed into the stage.
    ///
    /// Spec: AOUSD Core §10.3.2 (references), §11 (stage population).
    #[test]
    fn child_specs_keep_their_parents_indexes() {
        let mut store = InMemoryStore::default();
        let proto = store.path("/Proto");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(proto, PrimSpec::def());
        let siblings: alloc::vec::Vec<_> = (0..4)
            .map(|i| store.path(&alloc::format!("/World/I{i}")))
            .collect();
        for &sibling in &siblings {
            layer.insert_prim(
                sibling,
                PrimSpec::def().with_reference(Reference::new(LayerId(1), proto)),
            );
        }
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let kept = live
            .stage()
            .index_of(siblings[0])
            .expect("index")
            .data
            .clone();
        let mut create = Transaction::new();
        let added = store.path("/Proto/Added");
        create.create_prim(
            EditTarget::for_layer(LayerId(1)).prim(added),
            Specifier::Def,
            None,
        );
        live.apply(&mut store, &create).expect("edit applies");
        let work = live.recomposition_work();
        assert!(!work.full_rebuild);
        // `/Proto/Added` and the four new children; no sibling.
        assert_eq!(work.replaced_prim_indexes, 5, "{work:?}");
        let index = live.stage().index_of(siblings[0]).expect("index");
        assert!(alloc::sync::Arc::ptr_eq(&kept, &index.data));
        assert_prim_stacks_match_fresh(&live, &mut store);
    }

    /// A child spec authored in a layer that had no spec of its parent also
    /// authors an `over` of the parent there, which only supports the
    /// child. A prim referencing the parent draws on that `over` too: its
    /// index is composed again, though the edit adds a child beneath it,
    /// and its prim stack gains the layer.
    ///
    /// Spec: AOUSD Core §10.3.2 (references), §11 (stage population).
    #[test]
    fn an_over_supporting_a_child_spec_recomposes_the_parents_drawing_on_it() {
        use crate::SublayerEntry;
        let mut store = InMemoryStore::default();
        let asset = store.path("/Asset");
        let copy = store.path("/Copy");
        let mut root = Layer::new(LayerId(1));
        root.insert_prim(asset, PrimSpec::def());
        root.insert_prim(
            copy,
            PrimSpec::def().with_reference(Reference::new(LayerId(1), asset)),
        );
        store.insert_layer(root);
        store.insert_layer(Layer::new(LayerId(2)));
        let mut session = Layer::new(LayerId(3));
        session.sublayers.push(SublayerEntry::new(LayerId(2)));
        store.insert_layer(session);
        let options = StageOptions {
            session_layer: Some(LayerId(3)),
            ..StageOptions::default()
        };
        // One stage authors the edit; another reads it from the layer's
        // history, which records the `over` as only supporting the child.
        let mut author = LiveStage::compose(&mut store, LayerId(1), options.clone());
        let mut reader = LiveStage::compose(&mut store, LayerId(1), options);
        let leaf = store.path("/Asset/New/Leaf");
        let mut create = Transaction::new();
        create.create_prim(
            EditTarget::for_layer(LayerId(2)).prim(leaf),
            Specifier::Def,
            None,
        );
        author.apply(&mut store, &create).expect("edit applies");
        reader.synchronize(&mut store);
        for live in [&author, &reader] {
            assert!(!live.recomposition_work().full_rebuild);
            assert!(live.stage().has_prim(store.path("/Copy/New/Leaf")));
            let stack = live.stage().prim_stack(copy).expect("prim stack");
            assert!(
                stack.iter().any(|(layer, _)| *layer == LayerId(2)),
                "{stack:?}"
            );
            assert_prim_stacks_match_fresh(live, &mut store);
        }
    }

    /// An inert `over` added to a sublayer for a prim that composes
    /// already changes that prim's spec stack, and that of the prim
    /// inheriting it, info-only, as its removal does. With another edit of
    /// the same prim in the same transaction, the prim is resynced: the
    /// inert spec does not hide the other edit. One that moves its prim
    /// among its siblings (child names compose weakest layer first)
    /// resyncs it.
    ///
    /// Spec: AOUSD Core §10 (namespace composition), §11.3.1 (child
    /// order). OpenUSD: `PcpChanges::DidChangeSpecs` for an inert prim spec
    /// (`didAddInertPrim`), which `UsdStage` reports info-only.
    #[test]
    fn an_inert_over_changes_its_prim_info_only_unless_edited_otherwise() {
        use crate::SublayerEntry;
        let mut store = InMemoryStore::default();
        let lamp = store.path("/Lamp");
        let rock = store.path("/Rock");
        let mut root = Layer::new(LayerId(1));
        root.insert_prim(lamp, PrimSpec::def());
        root.insert_prim(rock, PrimSpec::def().with_inherit(lamp));
        root.sublayers.push(SublayerEntry::new(LayerId(2)));
        store.insert_layer(root);
        store.insert_layer(Layer::new(LayerId(2)));
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let over = |path| {
            let mut create = Transaction::new();
            create.create_prim(
                EditTarget::for_layer(LayerId(2)).prim(path),
                Specifier::Over,
                None,
            );
            create
        };
        let create = over(lamp);
        let added = live.apply(&mut store, &create).expect("edit applies");
        assert!(added.changes.resynced.is_empty(), "{:?}", added.changes);
        assert_eq!(added.changes.changed_info_only, [lamp, rock]);
        assert_prim_stacks_match_fresh(&live, &mut store);
        let removed = live.apply(&mut store, &added.inverse).expect("undo");
        assert!(removed.changes.resynced.is_empty(), "{:?}", removed.changes);
        assert_eq!(removed.changes.changed_info_only, [lamp, rock]);
        assert_prim_stacks_match_fresh(&live, &mut store);

        let scope = store.tokens().intern("Scope");
        let mut both = create.clone();
        both.set_type_name(EditTarget::for_layer(LayerId(1)).prim(lamp), scope);
        let typed = live.apply(&mut store, &both).expect("edit applies");
        assert!(
            typed.changes.resynced.contains(&lamp),
            "{:?}",
            typed.changes
        );
        assert!(!typed.changes.changed_info_only.contains(&lamp));
        assert_prim_stacks_match_fresh(&live, &mut store);
        live.apply(&mut store, &typed.inverse).expect("undo");

        // `Rock` comes first in the sublayer, which lists it alone.
        let moved = live.apply(&mut store, &over(rock)).expect("edit applies");
        assert_eq!(moved.changes.resynced, [rock], "{:?}", moved.changes);
        assert_prim_stacks_match_fresh(&live, &mut store);
    }

    /// The prim stack of every prim of `live` matches that of a fresh
    /// composition with its options.
    fn assert_prim_stacks_match_fresh(live: &LiveStage, store: &mut InMemoryStore) {
        let fresh = crate::Stage::compose(store, LayerId(1), live.options().clone());
        let prims: alloc::vec::Vec<_> = fresh.prim_paths().collect();
        assert_eq!(prims.len(), live.stage().prim_paths().count());
        for prim in prims {
            assert_eq!(
                live.stage().prim_stack(prim),
                fresh.prim_stack(prim),
                "{prim:?}"
            );
        }
    }

    /// The edges of every prim of `live` match those of a fresh composition
    /// with its options.
    fn assert_fresh_edges(live: &LiveStage, store: &mut InMemoryStore) {
        let fresh = LiveStage::compose(store, LayerId(1), live.options().clone());
        let prims: alloc::vec::Vec<_> = fresh.stage().prim_paths().collect();
        assert_eq!(prims.len(), live.stage().prim_paths().count());
        for prim in prims {
            let edges = |stage: &LiveStage| {
                let mut edges: alloc::vec::Vec<_> = stage
                    .tracker
                    .graph()
                    .dependencies(prim, super::OPINION_EDIT)
                    .collect();
                edges.sort_unstable();
                edges
            };
            assert_eq!(edges(live), edges(&fresh), "{prim:?}");
        }
    }

    /// Unloading and loading back half of 20 prims with one payload each
    /// drops and restores their edges to the payload's prims, many to one
    /// key, as a fresh composition has them.
    #[test]
    fn bulk_unload_keeps_the_edges_a_fresh_composition_has() {
        let mut store = InMemoryStore::default();
        let asset = store.path("/Asset");
        let mut layer = Layer::new(LayerId(2));
        layer.insert_prim(asset, PrimSpec::def());
        layer.insert_prim(store.path("/Asset/Geo"), PrimSpec::def());
        store.insert_layer(layer);
        let mut layer = Layer::new(LayerId(1));
        let holders: alloc::vec::Vec<_> = (0..20)
            .map(|i| store.path(&alloc::format!("/World/P{i}")))
            .collect();
        for &holder in &holders {
            layer.insert_prim(
                holder,
                PrimSpec::def().with_payload(Reference::new(LayerId(2), asset)),
            );
        }
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let half: alloc::vec::Vec<_> = holders.iter().copied().step_by(2).collect();
        live.load_and_unload(&store, &[], &half, crate::LoadPolicy::WithDescendants);
        live.recompose_changes(&mut store);
        assert!(!live.recomposition_work().full_rebuild);
        assert_fresh_edges(&live, &mut store);
        live.load_and_unload(&store, &half, &[], crate::LoadPolicy::WithDescendants);
        live.recompose_changes(&mut store);
        assert_fresh_edges(&live, &mut store);
    }

    /// An edit that resyncs many sibling roots does work in proportion to
    /// them: membership beneath the roots is an ancestor walk, not a scan
    /// of every root for each prim.
    #[test]
    fn resyncing_many_siblings_is_linear() {
        let small = edit_reads(100);
        let large = edit_reads(800);
        for (small, large) in small.into_iter().zip(large) {
            // Eight times the siblings; a scan of the roots per prim
            // would read the paths about 64 times as often.
            assert!(
                large <= small * 10,
                "{large} path reads for 800 siblings, {small} for 100"
            );
        }
    }
}
