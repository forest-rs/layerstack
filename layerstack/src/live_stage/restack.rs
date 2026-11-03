// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Recomposition after changes of the layer stacks alone: a sublayer
//! inserted, removed, reordered or bound, a layer muted or unmuted.
//!
//! Every prim index reading a changed layer stack is carried across the
//! change in place ([`Stage::restack`]): its nodes read the stack's new
//! layers, its sources the new strengths of theirs. Only the prims with
//! specs in a layer that joined, left or moved within a stack are composed
//! again: the prims at an inert spec (an `over` holding properties and
//! children alone) have their indexes recomposed, those at any other
//! spec, or new, their subtrees, as an authored structural edit would.
//! What the change announces is then found by comparing those prims
//! before and after ([`layer_stack_changes_of`]), as a full composition
//! compares every prim.
//!
//! OpenUSD: `PcpChanges::DidChange` handles a sublayer added or removed
//! (`_DidAddOrRemoveSublayer`, `pxr/usd/pcp/changes.cpp`, with
//! `PCP_ENABLE_MINIMAL_CHANGES_FOR_LAYER_OPERATIONS`) as a change of the
//! layers of the layer stacks holding it (`LayerStackLayersChange`), which
//! `PcpLayerStack::Apply` computes again in place, under the nodes that
//! refer to it, and the sublayer's difference from an empty layer as its
//! specs added or removed, whose prims are recomposed or rescanned for
//! specs (`PcpCache::Apply`, `Pcp_RescanForSpecs`). An offset changed is
//! significant there, and here: the stage is composed whole.
//!
//! Spec: AOUSD Core §10.3.1 (sublayers), §10.4 (strength ordering).

use super::*;
use crate::layer_stack::LayerStackIdentifier;
use crate::stage::PrototypeId;
use crate::stage::restack::RestackSites;

/// What a stage held of one prim before a layer stack change.
struct PrimBefore {
    exists: bool,
    prototype: Option<PrototypeId>,
    /// The prim's spec stack and child list, or `None` for an instance
    /// remembered for its prototype alone, which its prim index, carried
    /// across, keeps as they were.
    held: Option<Held>,
}

/// A prim's spec stack and child list.
struct Held {
    stack: Option<Vec<(LayerId, crate::SpecPath)>>,
    children: Option<Vec<PathId>>,
}

/// The stage before a layer stack change, as remembered of the prims it
/// may compose differently; it composed every other prim as it does now.
struct Remembered<'a> {
    prims: HashMap<PathId, PrimBefore>,
    paths: HashSet<PathId>,
    now: &'a Stage,
}

impl StageBefore for Remembered<'_> {
    fn has_prim(&self, prim: PathId) -> bool {
        self.prims
            .get(&prim)
            .map_or_else(|| self.paths.contains(&prim), |before| before.exists)
    }
    fn instance_prototype(&self, prim: PathId) -> Option<PrototypeId> {
        self.prims.get(&prim).map_or_else(
            || self.now.instance_prototype(prim),
            |before| before.prototype,
        )
    }
    fn prim_stack(&self, prim: PathId) -> Option<Vec<(LayerId, crate::SpecPath)>> {
        match self
            .prims
            .get(&prim)
            .and_then(|before| before.held.as_ref())
        {
            Some(held) => held.stack.clone(),
            None => self.now.prim_stack(prim),
        }
    }
    fn all_children_of(&self, prim: PathId) -> Option<&[PathId]> {
        match self
            .prims
            .get(&prim)
            .and_then(|before| before.held.as_ref())
        {
            Some(held) => held.children.as_deref(),
            None => self.now.all_children_of(prim),
        }
    }
}

impl LiveStage {
    /// Recomposes after changes of the layer stacks alone, pending with
    /// nothing else ([`Self::layer_stack_diff`]), carrying the prims they
    /// do not change across them, and returns the prims recomposed; `None`
    /// when the stage must be composed whole instead, with nothing changed.
    pub(super) fn recompose_restacked(
        &mut self,
        store: &mut dyn LayerStore,
        changes: Option<&mut Changes>,
    ) -> Option<Vec<PathId>> {
        let diff = self.layer_stack_diff.clone()?;
        #[cfg(test)]
        if self.restack_disabled {
            return None;
        }
        let previous = self.stage.options();
        if self.tracker.has_invalidated(OPINION_EDIT)
            || !self.structural_roots.is_empty()
            || !self.structural_sources.is_empty()
            || !self.support_sources.is_empty()
            || self.stage.store_identity() != Some(&store.identity())
            || previous.load_rules != self.options.load_rules
            || previous.mask != self.options.mask
            || previous.session_layer != self.options.session_layer
            || self.layer_discovery.has_expressions()
            || self
                .relocation_layers
                .iter()
                .any(|id| store.layer(*id).is_some_and(|l| !l.relocates().is_empty()))
            || self
                .stage
                .restack_unsupported(store, previous.muted_layers != self.options.muted_layers)
        {
            return None;
        }
        // Each layer that changed since the stage last saw it changed only
        // the layers its sublayers are bound to, or joined.
        for (&layer, seen) in &self.generations {
            if *seen == generations_of(store, layer) {
                continue;
            }
            let stacked_only = seen.is_none_or(|seen| {
                store
                    .layer(layer)
                    .is_some_and(|found| found.changed_layer_stack_only_since(seen.0))
            });
            if !stacked_only {
                return None;
            }
        }
        let root = LayerStackIdentifier {
            root: self.root,
            session: self.options.session_layer,
        };
        let plan = self
            .stage
            .plan_restack(store, root, &self.options.muted_layers)?;
        let sites = self.stage.restack_sites(store, &plan, root)?;
        let pseudo_root = store.paths().intern(crate::Path::root());
        let (mut roots, mut inert, refresh_root) = classify(store, &sites, pseudo_root);
        if roots.contains(&pseudo_root) {
            return None;
        }
        self.layer_stack_diff = None;

        // What the stage holds of the prims it may compose differently.
        let paths: HashSet<PathId> = self.stage.prim_paths().collect();
        let mut before = HashMap::new();
        self.remember_subtrees(store, &roots, &mut before);
        for &prim in &inert {
            self.remember(prim, &mut before);
            if let Some(parent) = store.paths().parent(prim) {
                self.remember(parent, &mut before);
            }
        }
        // An instance the change does not compose again may still take
        // another prototype, as prototypes are numbered in the order of
        // their first instances.
        for prim in self.stage.instance_roots() {
            before.entry(prim).or_insert_with(|| PrimBefore {
                exists: true,
                prototype: self.stage.instance_prototype(prim),
                held: None,
            });
        }
        self.remember(pseudo_root, &mut before);

        self.stage.restack(store, &plan, root);
        self.stage.capture_controls(self.options.clone());
        self.needs_full_rebuild = false;
        self.controls_pending = false;
        self.layer_discovery = discovery::LayerDiscovery::default();
        self.record_generations(store);
        self.local_namespace = local::LocalSlot::pending(store, self.root, &self.options);

        let mut recomposed = Vec::new();
        // The pseudo-root's index reads the specs of its layer stack, and
        // its child list orders the root prims: it is composed again with
        // the root prims whose specs alone changed.
        let root_prims: Vec<PathId> = inert
            .iter()
            .copied()
            .filter(|prim| store.paths().parent(*prim) == Some(pseudo_root))
            .collect();
        if !root_prims.is_empty() || (refresh_root && roots.is_empty()) {
            inert.push(pseudo_root);
        } else if refresh_root {
            self.support_sources.push((self.root, pseudo_root));
        }
        if !inert.is_empty() {
            let more = self.recompose_inert(store, &inert, &mut recomposed);
            if more.contains(&pseudo_root) {
                // The root prims moved among themselves: those whose specs
                // changed are placed again with their subtrees, and the
                // pseudo-root's index refreshed with them. A root prim is
                // placed only where a spec of a changed layer is.
                debug_assert!(
                    !root_prims.is_empty() || !roots.is_empty(),
                    "the root prims moved with no root prim's specs changed"
                );
                roots.extend(root_prims);
                if refresh_root {
                    self.support_sources.push((self.root, pseudo_root));
                }
            }
            let more: Vec<PathId> = more.into_iter().filter(|p| *p != pseudo_root).collect();
            self.remember_subtrees(store, &more, &mut before);
            roots.extend(more);
            roots = minimal_roots(store, &roots);
        }
        if !roots.is_empty() {
            self.structural_roots = roots;
            recomposed.extend(self.recompose_controls(store, None));
        }
        sort::sort_unstable(&mut recomposed);
        recomposed.dedup();
        if let Some(changes) = changes {
            let remembered = Remembered {
                prims: before,
                paths,
                now: &self.stage,
            };
            let mut prims: Vec<PathId> = remembered.prims.keys().copied().collect();
            prims.extend(recomposed.iter().copied());
            // The prims drawing on a sublayer reordered, whose specs count
            // as lost and gained again, though they compose the same.
            for layer in diff.moved_layers(store) {
                if let Some(found) = self.layer_to_prims.get(&layer) {
                    prims.extend(found.iter().copied());
                }
            }
            sort::sort_unstable(&mut prims);
            prims.dedup();
            // Only the prims a stage composes, before or after, are compared.
            prims.retain(|&prim| self.stage.has_prim(prim) || remembered.has_prim(prim));
            changes.created = prims
                .iter()
                .copied()
                .filter(|&prim| self.stage.has_prim(prim) && !remembered.has_prim(prim))
                .collect();
            changes.removed = prims
                .iter()
                .copied()
                .filter(|&prim| !self.stage.has_prim(prim) && remembered.has_prim(prim))
                .collect();
            let found =
                layer_stack_changes_of(store, &remembered, &self.stage, prims.into_iter(), &diff);
            changes.resynced = found.resynced;
            changes.changed_info_only = found.changed_info_only;
            changes.resynced_properties = found.resynced_properties;
            self.exact_report = true;
        }
        Some(recomposed)
    }

    /// Recomposes the indexes of `inert`, prims whose spec stacks alone a
    /// layer stack change changed, as an opinion edit does, and returns
    /// those whose children or instancing changed after all, with every
    /// other of `inert` beneath or above one of them: their subtrees are
    /// to be recomposed instead.
    fn recompose_inert(
        &mut self,
        store: &mut dyn LayerStore,
        inert: &[PathId],
        recomposed: &mut Vec<PathId>,
    ) -> Vec<PathId> {
        let mut inert = inert.to_vec();
        sort::sort_unstable(&mut inert);
        inert.dedup();
        // Their children are listed whole, not composed: those whose specs
        // the change holds are composed in their own right.
        let mut partial = self.compose_exact(store, &inert, false);
        self.work.composed_prim_indexes += partial.composition_work().composed_prim_indexes;
        // Inactive children counted: the masked composition, which does
        // not compose them, lists them among the others.
        let diverging: HashSet<PathId> = inert
            .iter()
            .copied()
            .filter(|&prim| {
                self.stage.has_prim(prim) != partial.has_prim(prim)
                    || self.stage.all_children_of(prim) != partial.all_children_of(prim)
                    || self.stage.is_instance(prim) != partial.is_instance(prim)
            })
            .collect();
        let mut above = HashSet::new();
        for &prim in &diverging {
            controls::insert_ancestors(store.paths(), &mut above, prim);
        }
        // The pseudo-root, above every prim, is placed again only for its
        // own children.
        let pseudo_root = store.paths().lookup(&crate::Path::root());
        let (rest, more): (Vec<PathId>, Vec<PathId>) = inert.iter().copied().partition(|prim| {
            if Some(*prim) == pseudo_root {
                return !diverging.contains(prim);
            }
            !is_beneath_set(store, *prim, &diverging) && !above.contains(prim)
        });
        let mut partial_deps = partial.take_deps().unwrap_or_default();
        self.expression_variables
            .extend(core::mem::take(&mut partial_deps.expression_variables));
        self.stage.merge_prims_from(store, partial, &rest, &rest);
        self.update_prim_edges(&rest, &partial_deps);
        for &prim in &rest {
            self.reindex_sources(store.paths(), prim);
        }
        self.work.replaced_prim_indexes += rest.len();
        recomposed.extend(rest);
        more
    }

    /// Remembers each prim of the subtrees at `roots`, the parents of the
    /// roots, and the same of every prim depending on them, transitively,
    /// as [`Self::recompose_controls`] finds them.
    fn remember_subtrees(
        &mut self,
        store: &dyn LayerStore,
        roots: &[PathId],
        before: &mut HashMap<PathId, PrimBefore>,
    ) {
        let mut pending: Vec<PathId> = roots.to_vec();
        let mut seen: HashSet<PathId> = HashSet::new();
        while !pending.is_empty() {
            for root in core::mem::take(&mut pending) {
                if !seen.insert(root) {
                    continue;
                }
                if let Some(parent) = store.paths().parent(root) {
                    self.remember(parent, before);
                }
                self.remember(root, before);
                self.tracker.mark(root, OPINION_EDIT);
                let subtree: Vec<PathId> = self.stage.traverse_all(root).collect();
                for prim in subtree {
                    self.remember(prim, before);
                    self.tracker.mark(prim, OPINION_EDIT);
                }
            }
            pending.extend(
                self.tracker
                    .drain(OPINION_EDIT)
                    .affected()
                    .scratch(&mut self.traversal_scratch)
                    .run()
                    .filter(|prim| !seen.contains(prim)),
            );
        }
    }

    /// Remembers what the stage holds of `prim`, unless it already does.
    fn remember(&self, prim: PathId, before: &mut HashMap<PathId, PrimBefore>) {
        let entry = before.entry(prim).or_insert_with(|| PrimBefore {
            exists: self.stage.has_prim(prim),
            prototype: self.stage.instance_prototype(prim),
            held: None,
        });
        entry.held.get_or_insert_with(|| Held {
            stack: self.stage.prim_stack(prim),
            children: self.stage.all_children_of(prim).map(<[PathId]>::to_vec),
        });
    }
}

/// The roots of the subtrees to recompose (the prims created and those at
/// specs composing more than their spec stacks), the prims whose indexes
/// alone are to be recomposed, outside those subtrees, and whether the
/// pseudo-root's index is.
fn classify(
    store: &dyn LayerStore,
    sites: &RestackSites,
    pseudo_root: PathId,
) -> (Vec<PathId>, Vec<PathId>, bool) {
    let mut roots = sites.created.clone();
    let mut inert = Vec::new();
    for (&prim, &is_inert) in &sites.touched {
        if prim == pseudo_root {
            continue;
        }
        if is_inert {
            inert.push(prim);
        } else {
            roots.push(prim);
        }
    }
    let roots = minimal_roots(store, &roots);
    let within: HashSet<PathId> = roots.iter().copied().collect();
    inert.retain(|prim| !is_beneath_set(store, *prim, &within));
    sort::sort_unstable(&mut inert);
    let refresh_root = sites.pseudo_root || sites.touched.contains_key(&pseudo_root);
    (roots, inert, refresh_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        InMemoryStore, Layer, LayerOffset, Path, PrimSpec, PropertySpec, Reference, SublayerEntry,
        VariantSetSpec, VariantSpec, spec_path::VariantSelectionSite,
    };
    use alloc::{format, string::String, vec};

    /// A deterministic sequence of numbers (xorshift).
    struct Dice(u64);

    impl Dice {
        fn roll(&mut self, sides: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            usize::try_from(self.0 % sides as u64).expect("small")
        }
    }

    const ROOT: LayerId = LayerId(1);
    const ASSET: LayerId = LayerId(10);
    /// The layers a scene starts with in a layer stack.
    const STACKED: [LayerId; 3] = [LayerId(2), LayerId(3), LayerId(11)];
    /// The layers no stack holds at first; layer 8 has layer 9 beneath it.
    const POOL: [LayerId; 8] = [
        LayerId(4),
        LayerId(5),
        LayerId(6),
        LayerId(7),
        LayerId(8),
        LayerId(9),
        LayerId(12),
        LayerId(13),
    ];

    fn path(store: &InMemoryStore, text: &str) -> PathId {
        store
            .paths()
            .intern(Path::parse_absolute(text, store.tokens()).expect("path"))
    }

    /// Specs a sublayer of the stage (or, for an asset layer, of the
    /// asset) may hold: defs, inert overs, overs with an inherit or a
    /// reference, deep or new prims.
    fn random_layer(store: &mut InMemoryStore, id: LayerId, asset: bool, dice: &mut Dice) -> Layer {
        let x = store.tokens.intern("x");
        let y = store.tokens.intern("y");
        let mut layer = Layer::new(id);
        let names: &[&str] = if asset {
            &[
                "/Asset",
                "/Asset/Part",
                "/Asset/Part/Bit",
                "/Asset/Extra",
                "/Asset/Hi",
            ]
        } else {
            &[
                "/A",
                "/A/K1",
                "/A/K2",
                "/B",
                "/B/Part",
                "/C",
                "/Class",
                "/Class/Sub",
                "/D",
                "/Inst1",
                "/New",
                "/New/Leaf",
                "/V",
                "/V/Ball",
                "/V/Box",
                "/S",
            ]
        };
        let (set, variant, host) = if asset {
            ("lod", "hi", "/Asset")
        } else {
            ("shape", "square", "/V")
        };
        let count = 1 + dice.roll(4);
        for _ in 0..count {
            let at = path(store, names[dice.roll(names.len())]);
            let value = i64::try_from(dice.roll(100)).expect("small");
            let property = if dice.roll(2) == 0 { x } else { y };
            let spec = match dice.roll(11) {
                0..=2 => PrimSpec::over()
                    .with_property(property, PropertySpec::attribute().with_default(value)),
                3 => PrimSpec::def()
                    .with_property(property, PropertySpec::attribute().with_default(value)),
                4 if !asset => PrimSpec::over().with_inherit(path(store, "/Class")),
                5 if !asset => {
                    PrimSpec::over().with_reference(Reference::new(ASSET, path(store, "/Asset")))
                }
                6 => {
                    let mut spec = PrimSpec::over();
                    let (set, variant) = (store.tokens.intern(set), store.tokens.intern(variant));
                    spec.rare_mut().variant_selections.insert(set, variant);
                    layer.insert_prim(path(store, host), spec);
                    continue;
                }
                7 if !asset => PrimSpec::over().with_specialize(path(store, "/Class")),
                8 if !asset => {
                    PrimSpec::over().with_payload(Reference::new(ASSET, path(store, "/Asset")))
                }
                9 => PrimSpec::over().with_active(dice.roll(2) == 0),
                _ => PrimSpec::over(),
            };
            layer.insert_prim(at, spec);
        }
        layer
    }

    /// Layer 1 sublayers 2 and 3 and authors `/A`, `/B` referencing
    /// `/Asset` of layer 10, two instances of it, `/C` inheriting
    /// `/Class`; layer 10 sublayers 11. Every other layer is random.
    fn scene(seed: u64) -> InMemoryStore {
        let mut dice = Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut store = InMemoryStore::default();
        let x = store.tokens.intern("x");
        let mut root = Layer::new(ROOT);
        root.sublayers = vec![
            SublayerEntry::new(LayerId(2)),
            SublayerEntry::new(LayerId(3)),
        ];
        let asset = path(&store, "/Asset");
        let class = path(&store, "/Class");
        root.insert_prim(path(&store, "/A"), PrimSpec::def());
        root.insert_prim(
            path(&store, "/B"),
            PrimSpec::def().with_reference(Reference::new(ASSET, asset)),
        );
        for name in ["/Inst1", "/Inst2"] {
            let mut spec = PrimSpec::def().with_reference(Reference::new(ASSET, asset));
            spec.instanceable = Some(true);
            root.insert_prim(path(&store, name), spec);
        }
        root.insert_prim(path(&store, "/C"), PrimSpec::def().with_inherit(class));
        root.insert_prim(path(&store, "/S"), PrimSpec::def().with_specialize(class));
        // A target only some sublayers of the asset define.
        root.insert_prim(
            path(&store, "/D"),
            PrimSpec::def().with_reference(Reference::new(ASSET, path(&store, "/Asset/Extra"))),
        );
        let v = path(&store, "/V");
        root.insert_prim(
            v,
            variant_host(
                &mut store,
                v,
                "shape",
                &[("round", "Ball"), ("square", "Box")],
            ),
        );
        for (name, variant) in [("/V/Ball", "round"), ("/V/Box", "square")] {
            let site = VariantSelectionSite {
                host_path: v,
                set: store.tokens.intern("shape"),
                variant: store.tokens.intern(variant),
            };
            root.insert_prim(
                path(&store, name),
                PrimSpec::def()
                    .with_property(x, PropertySpec::attribute().with_default(3))
                    .with_outer_variant_sites(vec![site]),
            );
        }
        root.insert_prim(
            class,
            PrimSpec::class().with_property(x, PropertySpec::attribute().with_default(1)),
        );
        store.insert_layer(root);
        let mut asset_root = Layer::new(ASSET);
        asset_root.sublayers = vec![SublayerEntry::new(LayerId(11))];
        asset_root.insert_prim(
            asset,
            variant_host(&mut store, asset, "lod", &[("lo", "Lo"), ("hi", "Hi")]),
        );
        for (name, variant) in [("/Asset/Lo", "lo"), ("/Asset/Hi", "hi")] {
            let site = VariantSelectionSite {
                host_path: asset,
                set: store.tokens.intern("lod"),
                variant: store.tokens.intern(variant),
            };
            asset_root.insert_prim(
                path(&store, name),
                PrimSpec::def().with_outer_variant_sites(vec![site]),
            );
        }
        asset_root.insert_prim(
            path(&store, "/Asset/Part"),
            PrimSpec::def().with_property(x, PropertySpec::attribute().with_default(2)),
        );
        store.insert_layer(asset_root);
        for id in STACKED.into_iter().chain(POOL) {
            let asset = id.0 >= 11;
            let mut layer = random_layer(&mut store, id, asset, &mut dice);
            if id == LayerId(8) {
                layer.sublayers = vec![SublayerEntry::new(LayerId(9))];
            }
            store.insert_layer(layer);
        }
        store
    }

    /// A `def` with a variant set `set` whose variants each author one
    /// child, the first selected.
    fn variant_host(
        store: &mut InMemoryStore,
        _at: PathId,
        set: &str,
        variants: &[(&str, &str)],
    ) -> PrimSpec {
        let set_name = store.tokens.intern(set);
        let mut spec = PrimSpec::def();
        let mut variant_set = VariantSetSpec::default();
        for (variant, child) in variants {
            variant_set.variants.insert(
                store.tokens.intern(variant),
                VariantSpec {
                    authored_children: vec![store.tokens.intern(child)],
                    ..VariantSpec::default()
                },
            );
        }
        spec.rare_mut().variant_sets.insert(set_name, variant_set);
        spec.rare_mut()
            .variant_set_names
            .make_mut()
            .prepend
            .push(set_name);
        let first = store.tokens.intern(variants[0].0);
        spec.rare_mut().variant_selections.insert(set_name, first);
        spec
    }

    /// The layers the stage's stacks hold now, from layers 1 and 10.
    fn stacked(store: &InMemoryStore) -> HashSet<LayerId> {
        let mut found = HashSet::new();
        let mut pending = vec![ROOT, ASSET];
        while let Some(id) = pending.pop() {
            if found.insert(id) {
                pending.extend(store.layers[&id].sublayers.iter().map(|entry| entry.layer));
            }
        }
        found
    }

    /// `layer` and the layers beneath it.
    fn beneath(store: &InMemoryStore, layer: LayerId) -> HashSet<LayerId> {
        let mut found = HashSet::new();
        let mut pending = vec![layer];
        while let Some(id) = pending.pop() {
            if found.insert(id) {
                pending.extend(store.layers[&id].sublayers.iter().map(|entry| entry.layer));
            }
        }
        found
    }

    /// One random layer stack edit.
    enum Step {
        Edit(Transaction),
        Mute(LayerId),
        Unmute(LayerId),
    }

    fn random_step(store: &InMemoryStore, live: &LiveStage, dice: &mut Dice) -> Step {
        let held = stacked(store);
        // The layers whose sublayers may change: those of the stage's own
        // stack and of the asset's.
        let mut hosts: Vec<LayerId> = held.iter().copied().collect();
        hosts.sort_unstable();
        let host = hosts[dice.roll(hosts.len())];
        let list = store.layers[&host].sublayers.clone();
        let mut txn = Transaction::new();
        match dice.roll(10) {
            0..=2 => {
                let free: Vec<LayerId> = POOL
                    .iter()
                    .copied()
                    .filter(|id| !held.contains(id))
                    .filter(|id| (id.0 >= 12) == (host.0 >= 10))
                    .filter(|id| !beneath(store, *id).contains(&host))
                    .collect();
                if free.is_empty() {
                    return random_step(store, live, dice);
                }
                let layer = free[dice.roll(free.len())];
                txn.insert_sublayer(host, dice.roll(list.len() + 1), SublayerEntry::new(layer));
            }
            3 | 4 if !list.is_empty() => {
                txn.remove_sublayer(host, dice.roll(list.len()));
            }
            5 if list.len() > 1 => {
                let mut order = list.clone();
                let from = dice.roll(order.len());
                let entry = order.remove(from);
                order.insert(dice.roll(order.len() + 1), entry);
                txn.set_sublayers(host, order);
            }
            6 | 7 => {
                let muted: Vec<LayerId> = live.options.muted_layers.iter().copied().collect();
                if !muted.is_empty() && dice.roll(2) == 0 {
                    return Step::Unmute(muted[dice.roll(muted.len())]);
                }
                let candidates: Vec<LayerId> =
                    hosts.iter().copied().filter(|id| *id != ROOT).collect();
                return Step::Mute(candidates[dice.roll(candidates.len())]);
            }
            8 if !list.is_empty() && dice.roll(3) == 0 => {
                let offset = LayerOffset {
                    offset: f64::from(u8::try_from(dice.roll(5)).expect("small")),
                    scale: 1.0,
                };
                txn.set_sublayer_offset(host, dice.roll(list.len()), offset);
            }
            _ => return random_step(store, live, dice),
        }
        Step::Edit(txn)
    }

    fn describe(step: &Step) -> String {
        match step {
            Step::Edit(txn) => format!("{txn:?}"),
            Step::Mute(layer) => format!("mute {layer:?}"),
            Step::Unmute(layer) => format!("unmute {layer:?}"),
        }
    }

    /// `changes` spelled out, in the order they are listed: the two
    /// stores compared intern paths in different orders.
    fn spelled(changes: &Changes, store: &InMemoryStore) -> Vec<Vec<String>> {
        let paths = |list: &[PathId]| {
            list.iter()
                .map(|path| store.paths().display(*path, store.tokens()))
                .collect::<Vec<_>>()
        };
        vec![
            paths(&changes.created),
            paths(&changes.removed),
            paths(&changes.resynced),
            paths(&changes.changed_info_only),
            changes
                .resynced_properties
                .iter()
                .map(|property| {
                    format!(
                        "{}.{}",
                        store.paths().display(property.prim_path(), store.tokens()),
                        store.tokens().resolve(property.property())
                    )
                })
                .collect(),
            changes
                .prim_fields
                .iter()
                .map(|change| format!("{:?}", change.field))
                .collect(),
            changes
                .changed_properties
                .iter()
                .map(|change| {
                    format!(
                        "{}.{} {:?}",
                        store
                            .paths()
                            .display(change.property.prim_path(), store.tokens()),
                        store.tokens().resolve(change.property.property()),
                        change.field
                    )
                })
                .collect(),
        ]
    }

    fn take(live: &mut LiveStage, store: &mut InMemoryStore, step: &Step) -> Changes {
        match step {
            Step::Edit(txn) => live.apply(store, txn).expect("applies").changes,
            Step::Mute(layer) => {
                live.mute_layer(*layer).expect("not the root");
                live.recompose_changes(store)
            }
            Step::Unmute(layer) => {
                live.unmute_layer(*layer);
                live.recompose_changes(store)
            }
        }
    }

    /// The live stage holds what a fresh live composition of its layers
    /// holds: every prim index node for node and record for record (layer
    /// strengths and offsets included), child lists, instancing, errors,
    /// asset requests and layers used, and the dependency and source
    /// indexes it recomposes by.
    fn assert_as_fresh(live: &LiveStage, store: &mut InMemoryStore, context: &str) {
        let fresh = LiveStage::compose(store, live.root, live.options.clone());
        let (a, b) = (live.stage(), fresh.stage());
        let root = store.paths().intern(Path::root());
        let order = |stage: &Stage| {
            stage
                .traverse_all(root)
                .map(|prim| store.paths().display(prim, store.tokens()))
                .collect::<Vec<_>>()
        };
        assert_eq!(order(a), order(b), "{context}: prims");
        let mut prims: Vec<PathId> = b.prim_paths().collect();
        let mut held: Vec<PathId> = a.prim_paths().collect();
        prims.sort_unstable();
        held.sort_unstable();
        assert_eq!(held, prims, "{context}: prim indexes");
        for prim in prims {
            let (x, y) = (
                a.index_of(prim).expect("held"),
                b.index_of(prim).expect("held"),
            );
            let at = format!("{context}: {}", store.paths().display(prim, store.tokens()));
            assert!(
                x.data.same_records(&y.data),
                "{at}: records {:?} vs {:?}",
                x.sources,
                y.sources
            );
            assert_eq!(x.graph.len(), y.graph.len(), "{at}: nodes");
            for ((_, m), (_, n)) in x.graph.nodes().zip(y.graph.nodes()) {
                assert_eq!(m.arc_kind(), n.arc_kind(), "{at}");
                assert_eq!(m.parent(), n.parent(), "{at}");
                assert_eq!(m.origin(), n.origin(), "{at}");
                assert_eq!(
                    m.layer_stack_identifier(),
                    n.layer_stack_identifier(),
                    "{at}"
                );
                assert_eq!(m.site(), n.site(), "{at}");
                assert_eq!(m.namespace_depth(), n.namespace_depth(), "{at}");
                assert_eq!(m.sibling_index(), n.sibling_index(), "{at}");
                assert_eq!(m.is_implied(), n.is_implied(), "{at}");
                let layers = |node: crate::PrimNode<'_>| {
                    node.layers()
                        .map(|layer| (layer, node.layer_offset_for_layer(layer)))
                        .collect::<Vec<_>>()
                };
                assert_eq!(layers(m), layers(n), "{at}: layers");
            }
            assert_eq!(x.type_name, y.type_name, "{at}");
            assert_eq!(x.type_info.is_some(), y.type_info.is_some(), "{at}");
            assert_eq!(x.dynamic.is_some(), y.dynamic.is_some(), "{at}");
            assert_eq!(a.all_children_of(prim), b.all_children_of(prim), "{at}");
            assert_eq!(a.is_instance(prim), b.is_instance(prim), "{at}");
            assert_eq!(
                a.instance_prototype(prim),
                b.instance_prototype(prim),
                "{at}"
            );
            assert_eq!(a.prim_stack(prim), b.prim_stack(prim), "{at}");
        }
        assert_eq!(a.composition_errors(), b.composition_errors(), "{context}");
        assert_eq!(a.asset_requests(), b.asset_requests(), "{context}");
        assert_eq!(a.used_layers(true), b.used_layers(true), "{context}");
        assert_eq!(a.layer_stack(), b.layer_stack(), "{context}");
        assert_eq!(
            a.prototypes().count(),
            b.prototypes().count(),
            "{context}: prototypes"
        );
        let edges = |live: &LiveStage| {
            let graph = live.tracker.graph();
            let mut edges: Vec<(PathId, PathId)> = graph
                .keys()
                .flat_map(|key| {
                    graph
                        .dependencies(key, OPINION_EDIT)
                        .map(move |source| (key, source))
                })
                .collect();
            edges.sort_unstable();
            edges.dedup();
            edges
        };
        assert_eq!(edges(live), edges(&fresh), "{context}: arc edges");
        let non_empty = |map: &HashMap<LayerId, HashSet<PathId>>| {
            map.iter()
                .filter(|(_, prims)| !prims.is_empty())
                .map(|(layer, prims)| (*layer, prims.clone()))
                .collect::<HashMap<_, _>>()
        };
        assert_eq!(
            non_empty(&live.layer_to_prims),
            non_empty(&fresh.layer_to_prims),
            "{context}: layer → prims"
        );
        assert_eq!(
            live.prim_to_layers, fresh.prim_to_layers,
            "{context}: prim → layers"
        );
        assert_eq!(
            non_empty(&live.default_prim_dependents),
            non_empty(&fresh.default_prim_dependents),
            "{context}"
        );
        assert_eq!(
            live.generations, fresh.generations,
            "{context}: generations"
        );
        let sources = |live: &LiveStage| {
            let mut found: Vec<(PathId, Vec<(LayerId, PathId)>)> = live
                .stage
                .prim_paths()
                .map(|prim| (prim, live.stage.source_sites(prim)))
                .collect();
            found.sort_unstable();
            found
        };
        assert_eq!(sources(live), sources(&fresh), "{context}: source sites");
    }

    /// Over many random sequences of sublayer edits (inserts, removals and
    /// reorders in the stage's own layer stack, in nested sublayers and in
    /// a referenced asset's stack, offsets, mutes and unmutes), the live
    /// stage carried across each layer stack change equals a fresh
    /// composition of its layers, and announces exactly what composing
    /// the stage whole announces.
    ///
    /// Spec: AOUSD Core §10.3.1 (sublayers), §10.4 (strength ordering).
    #[test]
    fn layer_stack_edits_compose_as_a_fresh_stage_does() {
        let mut restacked_steps = 0;
        for seed in 1..=200_u64 {
            let mut store = scene(seed);
            let mut reference_store = scene(seed);
            let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
            let mut reference =
                LiveStage::compose(&mut reference_store, ROOT, StageOptions::default());
            reference.restack_disabled = true;
            let mut dice = Dice(seed | 1);
            for step_at in 0..20 {
                let step = random_step(&store, &live, &mut dice);
                let context: String = format!("seed {seed} step {step_at} {}", describe(&step));
                let changes = take(&mut live, &mut store, &step);
                let expected = take(&mut reference, &mut reference_store, &step);
                if !live.recomposition_work().full_rebuild {
                    restacked_steps += 1;
                }
                assert_as_fresh(&live, &mut store, &context);
                assert_as_fresh(
                    &reference,
                    &mut reference_store,
                    &format!("{context} (composed whole)"),
                );
                assert_eq!(
                    spelled(&changes, &store),
                    spelled(&expected, &reference_store),
                    "{context}: changes"
                );
            }
        }
        // Most steps are carried across, not composed whole.
        assert!(restacked_steps > 400, "{restacked_steps}");
    }

    /// A sublayer inserted by an asset path no layer is bound to yet
    /// composes nothing, as an empty sublayer does, but asks for the
    /// asset; once a layer is bound to it, the prims its specs reach are
    /// composed, and nothing else: the stage is composed once for the
    /// insert, and only where it changes. Each step announces what
    /// composing the stage whole announces.
    ///
    /// Spec: AOUSD Core §10.3.1 (sublayers). OpenUSD opens the sublayer
    /// as it computes the layer stack (`PcpLayerStack::_BuildLayerStack`),
    /// and `_DidAddOrRemoveSublayer` adds its specs.
    #[test]
    fn a_sublayer_by_asset_path_composes_once_where_it_binds() {
        let build = || {
            let mut store = InMemoryStore::default();
            let x = store.tokens.intern("x");
            let mut root = Layer::new(ROOT);
            root.sublayers = vec![SublayerEntry::new(LayerId(2))];
            for at in 0..20 {
                root.insert_prim(path(&store, &format!("/R{at}")), PrimSpec::def());
                root.insert_prim(
                    path(&store, &format!("/R{at}/Child")),
                    PrimSpec::def().with_property(x, PropertySpec::attribute().with_default(1)),
                );
            }
            store.insert_layer(root);
            let mut weaker = Layer::new(LayerId(2));
            weaker.insert_prim(
                path(&store, "/R0"),
                PrimSpec::over().with_property(x, PropertySpec::attribute().with_default(2)),
            );
            store.insert_layer(weaker);
            store
        };
        let (mut store, mut reference_store) = (build(), build());
        let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
        let mut reference = LiveStage::compose(&mut reference_store, ROOT, StageOptions::default());
        reference.restack_disabled = true;
        let mut insert = Transaction::new();
        insert.insert_sublayer(
            ROOT,
            0,
            SublayerEntry::unresolved("./added.usda", LayerOffset::IDENTITY),
        );
        let step = Step::Edit(insert);
        let changes = take(&mut live, &mut store, &step);
        let expected = take(&mut reference, &mut reference_store, &step);
        assert_eq!(
            spelled(&changes, &store),
            spelled(&expected, &reference_store)
        );
        let work = live.recomposition_work();
        assert_eq!((work.composed_prim_indexes, work.full_rebuild), (0, false));
        let asked: Vec<&str> = live
            .stage()
            .asset_requests()
            .iter()
            .map(|request| request.asset_path.as_str())
            .collect();
        assert_eq!(asked, ["./added.usda"]);
        assert_as_fresh(&live, &mut store, "asked");

        for (store, live) in [
            (&mut store, &mut live),
            (&mut reference_store, &mut reference),
        ] {
            let x = store.tokens.intern("x");
            let mut added = Layer::new(LayerId(3));
            added.insert_prim(path(store, "/New"), PrimSpec::def());
            added.insert_prim(
                path(store, "/R3/Child"),
                PrimSpec::over().with_property(x, PropertySpec::attribute().with_default(3)),
            );
            store.insert_layer(added);
            store.insert_asset_layer(ROOT, "./added.usda", LayerId(3));
            assert_eq!(live.notify_changed_layers(store), [ROOT]);
        }
        let changes = live.recompose_changes(&mut store);
        let expected = reference.recompose_changes(&mut reference_store);
        assert_eq!(
            spelled(&changes, &store),
            spelled(&expected, &reference_store)
        );
        // `/R3` holds no spec of the layer, only its child does: it is
        // resynced, as composing the stage whole finds.
        assert_eq!(
            spelled(&changes, &store)[2..4],
            [vec![String::from("/New"), String::from("/R3")], vec![]]
        );
        let work = live.recomposition_work();
        assert!(!work.full_rebuild, "{work:?}");
        // `/New`, `/R3` and `/R3/Child`, the pseudo-root and the root prims
        // its child list is checked with: not the other prims beneath them.
        assert!(work.composed_prim_indexes < 30, "{work:?}");
        assert!(live.stage().asset_requests().is_empty());
        assert_as_fresh(&live, &mut store, "bound");
    }

    /// A stage composing no prim yet, but the pseudo-root, takes the prims
    /// of a sublayer inserted, and gives them back when it is removed.
    #[test]
    fn an_empty_stage_takes_the_prims_of_a_sublayer() {
        let build = || {
            let mut store = InMemoryStore::default();
            store.insert_layer(Layer::new(ROOT));
            let mut added = Layer::new(LayerId(2));
            added.insert_prim(path(&store, "/Only"), PrimSpec::def());
            store.insert_layer(added);
            store
        };
        let (mut store, mut reference_store) = (build(), build());
        let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
        let mut reference = LiveStage::compose(&mut reference_store, ROOT, StageOptions::default());
        reference.restack_disabled = true;
        let mut insert = Transaction::new();
        insert.insert_sublayer(ROOT, 0, SublayerEntry::new(LayerId(2)));
        let mut remove = Transaction::new();
        remove.remove_sublayer(ROOT, 0);
        for (at, step) in [Step::Edit(insert), Step::Edit(remove)].iter().enumerate() {
            let changes = take(&mut live, &mut store, step);
            let expected = take(&mut reference, &mut reference_store, step);
            assert_eq!(
                spelled(&changes, &store),
                spelled(&expected, &reference_store)
            );
            assert_as_fresh(&live, &mut store, &format!("step {at}"));
        }
    }

    /// A reference whose target prim no layer of its asset defines finds
    /// it once a sublayer defining it joins the asset's layer stack, and
    /// loses it again with the sublayer: the node its arc introduces is
    /// kept, specs or none, and found at the target's site.
    #[test]
    fn an_unresolved_reference_finds_a_target_a_sublayer_brings() {
        let build = || {
            let mut store = InMemoryStore::default();
            let mut root = Layer::new(ROOT);
            root.insert_prim(
                path(&store, "/Holder"),
                PrimSpec::def().with_reference(Reference::new(ASSET, path(&store, "/Missing"))),
            );
            store.insert_layer(root);
            let mut asset = Layer::new(ASSET);
            asset.insert_prim(path(&store, "/Present"), PrimSpec::def());
            store.insert_layer(asset);
            let mut brings = Layer::new(LayerId(12));
            brings.insert_prim(path(&store, "/Missing"), PrimSpec::def());
            brings.insert_prim(path(&store, "/Missing/Inside"), PrimSpec::def());
            store.insert_layer(brings);
            store
        };
        let (mut store, mut reference_store) = (build(), build());
        let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
        let mut reference = LiveStage::compose(&mut reference_store, ROOT, StageOptions::default());
        reference.restack_disabled = true;
        let mut insert = Transaction::new();
        insert.insert_sublayer(ASSET, 0, SublayerEntry::new(LayerId(12)));
        let mut remove = Transaction::new();
        remove.remove_sublayer(ASSET, 0);
        for (at, step) in [Step::Edit(insert), Step::Edit(remove)].iter().enumerate() {
            let changes = take(&mut live, &mut store, step);
            let expected = take(&mut reference, &mut reference_store, step);
            assert_eq!(
                spelled(&changes, &store),
                spelled(&expected, &reference_store)
            );
            assert_as_fresh(&live, &mut store, &format!("step {at}"));
        }
        let inside = path(&store, "/Holder/Inside");
        assert!(!live.stage().has_prim(inside));
    }
}
