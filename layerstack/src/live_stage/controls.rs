// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Subtree replacement for authored deltas and stage-local controls. AOUSD Core §10–§11;
//! OpenUSD `UsdStage::LoadAndUnload`, `PcpChanges` (significant changes).
use super::*;

/// Index and namespace work performed by the most recent recomposition.
/// Counts exclude dependency traversal and prototype regrouping; these are
/// work evidence, not a total CPU estimate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

    /// The roots of the namespace to recompose, and the prims beyond them
    /// whose prim indexes alone need refreshing; None for a full rebuild.
    fn control_roots(&mut self, store: &mut dyn LayerStore) -> Option<(Vec<PathId>, Vec<PathId>)> {
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
            *root = store.paths_mut().intern(path);
        }
        let roots = minimal_roots(store, &roots);
        let within: HashSet<PathId> = roots.iter().copied().collect();
        refreshed.retain(|prim| !is_beneath_set(store, *prim, &within));
        refreshed.sort_unstable();
        refreshed.dedup();
        Some((roots, refreshed))
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
        let Some((mut roots, mut refreshed)) = self.control_roots(store) else {
            return self.full_rebuild(store, changes);
        };
        self.controls_pending = false;
        self.structural_roots.clear();
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
            let boundaries: HashSet<PathId> = roots
                .iter()
                .filter_map(|root| store.paths().parent(*root))
                .filter(|parent| !is_beneath_set(store, *parent, &within))
                .collect();
            let partial = Stage::compose_retained(
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
            // A supporting ancestor the masked composition left out is
            // recomposed with its namespace instead.
            let unrefreshed: Vec<_> = refreshed
                .iter()
                .copied()
                .filter(|prim| !partial.has_prim(*prim))
                .collect();
            if !unrefreshed.is_empty() {
                refreshed.retain(|prim| partial.has_prim(*prim));
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
        affected.sort_unstable();
        let hierarchy = affected.clone();
        // Structural authoring also changes source provenance/child-order at
        // boundary parents. Refresh their indexes while preserving full lists.
        let mut lists_changed = Vec::new();
        for &root in &roots {
            if let Some(parent) = store.paths().parent(root)
                && partial.has_prim(parent)
                && !is_beneath_set(store, parent, &within)
            {
                affected.push(parent);
                // A child that came or went changed the parent's child list.
                if self.stage.has_prim(root) != partial.has_prim(root) {
                    lists_changed.push(parent);
                }
            }
        }
        affected.extend(refreshed.iter().copied());
        affected.sort_unstable();
        affected.dedup();
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
        if let Some(changes) = changes {
            changes.created = new.difference(&old).copied().collect();
            changes.removed = old.difference(&new).copied().collect();
            lists_changed.extend(refreshed);
            lists_changed.sort_unstable();
            lists_changed.dedup();
            changes.changed_info_only = lists_changed;
            changes.resynced = roots;
        }
        affected
    }
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
        fn tokens_mut(&mut self) -> &mut TokenInterner {
            self.inner.tokens_mut()
        }
        fn paths(&self) -> &PathInterner {
            self.path_reads.fetch_add(1, Ordering::Relaxed);
            self.inner.paths()
        }
        fn paths_mut(&mut self) -> &mut PathInterner {
            self.inner.paths_mut()
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
        for (edit, present) in [(&create, true), (&remove, false), (&create, true)] {
            live.apply(&mut store, edit).expect("edit applies");
            assert!(!live.recomposition_work().full_rebuild);
            for &prim in &under {
                assert_eq!(live.stage().has_prim(prim), present, "{prim:?}");
            }
        }
        assert_fresh_edges(&live, &mut store);
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
