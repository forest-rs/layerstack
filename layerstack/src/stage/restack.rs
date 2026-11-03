// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Carrying a composed stage across a change of the layer stacks it reads.
//!
//! Every node of a prim index records the resident layers of its layer
//! stack, and every opinion source the strength of its layer within it
//! ([`crate::OpinionKey::layer_strength`]). A sublayer inserted, removed,
//! moved, muted or unmuted changes those layers, and so the strengths of
//! the layers after it, in every prim index reading the stack, though only
//! the prims with specs in the layers that came, went or moved compose
//! anything differently.
//!
//! [`Stage::plan_restack`] finds how each layer stack the stage reads
//! changed, [`Stage::restack_sites`] the prims whose specs a changed layer
//! holds, and [`Stage::restack`] renumbers every other prim index in place
//! (old strength to new, the new layers shared by every node of a stack),
//! so that only the prims with specs in those layers are composed again.
//! OpenUSD's nodes refer to their layer stack, which it computes again in
//! place (`PcpLayerStack::Apply`) for a sublayer added or removed, and
//! recomposes only the prims whose specs the sublayer adds or removes
//! (`_DidAddOrRemoveSublayer` in `pxr/usd/pcp/changes.cpp`). Here a node
//! holds its stack's layers itself, as a prim's opinions hold their
//! strengths ([`crate::OpinionKey::layer_strength`], public and read by
//! value resolution, edit targets and instancing): renumbering them, in
//! one pass with no composition, stands for the in-place update.
//!
//! Spec: AOUSD Core §10.3.1 (sublayers form a layer stack), §10.4
//! (strength ordering within a layer stack).

use super::*;
use crate::layer_stack::LayerStackIdentifier;
use crate::prim_index::PrimIndexData;
use crate::prim_index_graph::{GraphShape, StackOffsets};
use crate::prim_indexing::layer_stacks::LayerStacks;

/// The strength an opinion source of a layer that left its stack, or
/// moved within it, is given until its prim is composed again.
const GONE: u16 = u16::MAX;

/// How the layer stacks a stage reads changed, each layer kept keeping
/// its offset and its order among the others kept.
#[derive(Debug, Default)]
pub(crate) struct Restack {
    /// Each layer stack whose layers changed, by its identifier.
    pub(crate) stacks: HashMap<LayerStackIdentifier, Restacked>,
    /// The sublayer asset requests of the stage's own layer stack, at the
    /// pseudo-root, as composing it now makes them.
    root_requests: Vec<crate::AssetRequest>,
}

/// How one layer stack's layers changed.
#[derive(Debug)]
pub(crate) struct Restacked {
    /// The resident layers before, strongest first, with their offsets.
    pub(crate) old: StackOffsets,
    /// The resident layers now.
    pub(crate) new: StackOffsets,
    /// The strength now of the layer at each strength before; [`GONE`]
    /// for a layer that left the stack or moved within it.
    strengths: Vec<u16>,
    /// The layers that joined the stack, left it, or moved within it: the
    /// prims with specs in them compose differently.
    pub(crate) changed: Vec<LayerId>,
}

/// The prims of a stage whose specs the layers a [`Restack`] changed hold.
#[derive(Debug, Default)]
pub(crate) struct RestackSites {
    /// Composed prims drawing on a site at which a changed layer has prim
    /// specs, each with whether all those specs are inert (an `over`
    /// holding properties and children alone), and so change the prim's
    /// spec stack alone.
    pub(crate) touched: HashMap<PathId, bool>,
    /// Prims a changed layer's specs place where no prim is composed.
    pub(crate) created: Vec<PathId>,
    /// Whether a changed layer of the stage's own layer stack has a
    /// pseudo-root spec, which the pseudo-root's index reads.
    pub(crate) pseudo_root: bool,
}

/// Whether `spec` composes nothing but its spec stack: an `over` (or no
/// specifier) holding properties and child specs, and nothing else.
pub(crate) fn inert_spec(spec: &PrimSpec) -> bool {
    let mut bare = spec.clone();
    bare.authored_children.clear();
    bare.properties.clear();
    if bare.specifier == Some(Specifier::Over) {
        bare.specifier = None;
    }
    bare == PrimSpec::default()
}

/// The positions of `positions` in a longest increasing run of them.
fn longest_run(positions: &[usize]) -> Vec<bool> {
    let mut tails: Vec<usize> = Vec::new();
    let mut previous = alloc::vec![usize::MAX; positions.len()];
    for at in 0..positions.len() {
        let length = tails.partition_point(|&tail| positions[tail] < positions[at]);
        if length > 0 {
            previous[at] = tails[length - 1];
        }
        if length == tails.len() {
            tails.push(at);
        } else {
            tails[length] = at;
        }
    }
    let mut kept = alloc::vec![false; positions.len()];
    let mut at = tails.last().copied().unwrap_or(usize::MAX);
    while at != usize::MAX {
        kept[at] = true;
        at = previous[at];
    }
    kept
}

impl Stage {
    /// How the layer stacks this stage's prim indexes read changed in
    /// `store`, with `muted` muted; `None` when carrying the stage across
    /// the change is not known to give what composing it again would:
    /// a stack's layers are pending, it reports errors or relocates, a
    /// variable expression composes it, it reads a layer twice, a layer
    /// kept changed its offset (a significant change, which OpenUSD
    /// resyncs the stage for), or a stack other than the stage's own asks
    /// for a sublayer asset.
    pub(crate) fn plan_restack(
        &self,
        store: &mut dyn LayerStore,
        root: LayerStackIdentifier,
        muted: &alloc::collections::BTreeSet<LayerId>,
    ) -> Option<Restack> {
        // Composition reads the store with the muted layers hidden.
        let controlled = ControlledStore {
            inner: store,
            root: root.root,
            muted,
        };
        let store: &dyn LayerStore = &controlled;
        // The stacks the prim indexes read, as each gathered them: every
        // node of one stack holds its layers.
        let mut old: HashMap<LayerStackIdentifier, StackOffsets> = HashMap::new();
        let mut shapes: HashSet<usize> = HashSet::new();
        for index in self.prims.values() {
            if !shapes.insert(Arc::as_ptr(index.graph.shape()) as usize) {
                continue;
            }
            for (id, offsets) in index.graph.stacks() {
                match old.entry(id) {
                    hashbrown::hash_map::Entry::Vacant(entry) => {
                        entry.insert(Arc::clone(offsets));
                    }
                    hashbrown::hash_map::Entry::Occupied(entry) => {
                        if !Arc::ptr_eq(entry.get(), offsets) && entry.get()[..] != offsets[..] {
                            return None;
                        }
                    }
                }
            }
        }
        let stacks = LayerStacks::muting(muted);
        let mut plan = Restack::default();
        for (id, old) in old {
            let computed = stacks.get(stacks.find_or_compute(store, id, None).ok()?);
            if !computed.chain.is_empty() || !computed.errors.is_empty() || computed.has_relocates()
            {
                return None;
            }
            if id == root {
                let pseudo_root = store.paths().intern(Path::root());
                plan.root_requests = computed
                    .requests
                    .iter()
                    .map(|request| crate::AssetRequest {
                        anchor: request.anchor,
                        asset_path: request.asset_path.clone(),
                        prim: pseudo_root,
                        used_by: crate::AssetUse::Sublayer,
                    })
                    .collect();
            } else if !computed.requests.is_empty() {
                return None;
            }
            if computed.layer_offsets[..] == old[..] {
                continue;
            }
            let new = Arc::clone(&computed.layer_offsets);
            let position: HashMap<LayerId, usize> = new
                .iter()
                .enumerate()
                .map(|(at, (layer, _))| (*layer, at))
                .collect();
            // A layer a stack reads twice (two sublayers holding it) has
            // two strengths: not renumbered here.
            let distinct: HashSet<LayerId> = old.iter().map(|(layer, _)| *layer).collect();
            if position.len() != new.len() || distinct.len() != old.len() {
                return None;
            }
            // The layers kept, in their old order, at their new positions.
            let mut kept = Vec::new();
            for (at, (layer, offset)) in old.iter().enumerate() {
                if let Some(&now) = position.get(layer) {
                    if new[now].1 != *offset {
                        return None;
                    }
                    kept.push((at, now));
                }
            }
            let positions: Vec<usize> = kept.iter().map(|(_, now)| *now).collect();
            let in_order = longest_run(&positions);
            let mut strengths = alloc::vec![GONE; old.len()];
            let mut stays: HashSet<LayerId> = HashSet::new();
            for (&(at, now), in_order) in kept.iter().zip(in_order) {
                if in_order {
                    strengths[at] = u16::try_from(now).unwrap_or(GONE);
                    stays.insert(old[at].0);
                }
            }
            let mut changed: Vec<LayerId> = old
                .iter()
                .chain(new.iter())
                .map(|(layer, _)| *layer)
                .filter(|layer| !stays.contains(layer))
                .collect();
            sort::sort_unstable(&mut changed);
            changed.dedup();
            plan.stacks.insert(
                id,
                Restacked {
                    old,
                    new,
                    strengths,
                    changed,
                },
            );
        }
        Some(plan)
    }

    /// The prims whose specs the layers `plan` changed hold, in `store`;
    /// `None` when a changed layer is not in the store or has a
    /// pseudo-root spec that composes more than its children.
    ///
    /// A prim draws on the specs of a layer of a stack at each of its
    /// nodes' sites, and at every site beneath an ancestor's site with the
    /// rest of its path, whether or not a node is kept there (composition
    /// culls nodes with no specs): each spec of a changed layer is mapped
    /// through every node of its stack at the spec's path or above it.
    pub(crate) fn restack_sites(
        &self,
        store: &dyn LayerStore,
        plan: &Restack,
        root: LayerStackIdentifier,
    ) -> Option<RestackSites> {
        let paths = store.paths();
        let pseudo_root = paths.intern(Path::root());
        let mut sites = RestackSites::default();
        // Each site at or above a spec of a changed layer, with the specs
        // at or beneath it and whether they are inert.
        let mut beneath: HashMap<(LayerStackIdentifier, PathId), Vec<(PathId, bool)>> =
            HashMap::new();
        for (&id, restacked) in &plan.stacks {
            for &layer in &restacked.changed {
                let layer = store.layer(layer)?;
                // A layer holding a spec has a site at each of its
                // ancestors too, which composition reads for their child
                // names (a spec with no parent spec is listed by none).
                let mut sited: HashMap<PathId, bool> = HashMap::new();
                for &path in layer.prims.keys() {
                    let inert = layer.prim_specs(path).all(inert_spec);
                    if path == pseudo_root {
                        if !inert {
                            return None;
                        }
                        continue;
                    }
                    *sited.entry(path).or_insert(true) &= inert;
                    let mut at = paths.parent(path);
                    while let Some(site) = at.filter(|site| *site != pseudo_root) {
                        sited.entry(site).or_insert(true);
                        at = paths.parent(site);
                    }
                }
                sites.pseudo_root |= id == root && !layer.prims.is_empty();
                for (path, inert) in sited {
                    let mut at = Some(path);
                    while let Some(site) = at {
                        beneath.entry((id, site)).or_default().push((path, inert));
                        at = paths.parent(site);
                    }
                }
            }
        }
        let mut found: HashMap<PathId, bool> = HashMap::new();
        for (&prim, index) in &self.prims {
            for (_, node) in index.graph.nodes() {
                let id = node.layer_stack_identifier();
                let site = node.site().prim_path();
                let Some(specs) = beneath.get(&(id, site)) else {
                    continue;
                };
                let site_path = paths.resolve(site).clone();
                for &(spec, inert) in specs {
                    let dest = if spec == site {
                        prim
                    } else {
                        let spec_path = paths.resolve(spec);
                        let suffix = spec_path.strip_prefix(&site_path).expect("beneath");
                        paths.intern_joined(prim, suffix)
                    };
                    let entry = found.entry(dest).or_insert(true);
                    *entry &= inert;
                }
            }
            // A source of a layer that left or moved is read again.
            let gone = index.sources.iter().any(|source| {
                index
                    .graph
                    .stack_of(source.key.node)
                    .and_then(|(id, _)| plan.stacks.get(&id))
                    .is_some_and(|restacked| {
                        restacked
                            .strengths
                            .get(usize::from(source.key.layer_strength))
                            == Some(&GONE)
                    })
            });
            if gone {
                found.entry(prim).or_insert(false);
            }
        }
        for (prim, inert) in found {
            if self.prims.contains_key(&prim) {
                sites.touched.insert(prim, inert);
            } else {
                sites.created.push(prim);
            }
        }
        sort::sort_unstable(&mut sites.created);
        Some(sites)
    }

    /// Carries every prim index across the layer stack changes of `plan`:
    /// each node of a changed stack reads its new layers, and each opinion
    /// source the new strength of its layer. Sources of layers that left
    /// or moved keep no meaningful strength: their prims must be composed
    /// again ([`Self::restack_sites`] lists them). Graphs and records shared
    /// before stay shared.
    ///
    /// Also records the stage's own layer stack anew, the layers each arc
    /// reads, and the stage's own sublayer asset requests.
    pub(crate) fn restack(
        &mut self,
        store: &dyn LayerStore,
        plan: &Restack,
        root: LayerStackIdentifier,
    ) {
        if plan.stacks.is_empty() {
            self.restack_requests(store, plan);
            return;
        }
        let mut shapes: HashMap<usize, (Arc<GraphShape>, Option<Arc<GraphShape>>)> = HashMap::new();
        // Shared records by their address and their graph's shape's.
        let mut records: HashMap<(usize, usize), (Arc<PrimIndexData>, Arc<PrimIndexData>)> =
            HashMap::new();
        let strength = |graph: &PrimIndexGraph, key: &OpinionKey| {
            graph
                .stack_of(key.node)
                .and_then(|(id, _)| plan.stacks.get(&id))
                .map(|restacked| {
                    restacked
                        .strengths
                        .get(usize::from(key.layer_strength))
                        .copied()
                        .unwrap_or(GONE)
                })
        };
        for index in self.prims.values_mut() {
            // Node ids and stacks are kept: the strengths are read by the
            // node's stack, before or after.
            if !index.graph.restack(&mut shapes, |id| {
                plan.stacks.get(&id).map(|r| Arc::clone(&r.new))
            }) {
                continue;
            }
            let renumbers = index
                .data
                .sources
                .iter()
                .any(|source| strength(&index.graph, &source.key).is_some());
            if renumbers {
                if let Some(data) = Arc::get_mut(&mut index.data) {
                    for source in &mut data.sources {
                        if let Some(now) = strength(&index.graph, &source.key) {
                            source.key.layer_strength = now;
                        }
                    }
                } else {
                    let key = (
                        Arc::as_ptr(&index.data) as usize,
                        Arc::as_ptr(index.graph.shape()) as usize,
                    );
                    let (_, data) = records.entry(key).or_insert_with(|| {
                        let mut data = (*index.data).clone();
                        for source in &mut data.sources {
                            if let Some(now) = strength(&index.graph, &source.key) {
                                source.key.layer_strength = now;
                            }
                        }
                        (Arc::clone(&index.data), Arc::new(data))
                    });
                    index.data = Arc::clone(data);
                }
            }
        }
        if let Some(restacked) = plan.stacks.get(&root) {
            self.local_layers = restacked.new.iter().map(|(layer, _)| *layer).collect();
        }
        // Each prim that introduces arcs records the layers of their stacks:
        // the layers of each changed stack it read are replaced by the new.
        for (path, layers) in &mut self.used_layer_sites {
            let Some(index) = self.prims.get(path) else {
                continue;
            };
            let mut read: Vec<(LayerStackIdentifier, &StackOffsets)> = Vec::new();
            for (id, offsets) in index.graph.stacks() {
                if !read.iter().any(|(seen, _)| *seen == id) {
                    read.push((id, offsets));
                }
            }
            let before = |id: &LayerStackIdentifier, now: &StackOffsets| {
                plan.stacks
                    .get(id)
                    .map_or_else(|| Arc::clone(now), |restacked| Arc::clone(&restacked.old))
            };
            let introduced: Vec<_> = read
                .iter()
                .filter(|(id, now)| {
                    before(id, now)
                        .iter()
                        .all(|(layer, _)| layers.contains(layer))
                })
                .collect();
            if !introduced
                .iter()
                .any(|(id, _)| plan.stacks.contains_key(id))
            {
                continue;
            }
            let mut kept = layers.clone();
            for (id, now) in &introduced {
                for (layer, _) in before(id, now).iter() {
                    kept.remove(layer);
                }
            }
            for (_, now) in &introduced {
                kept.extend(now.iter().map(|(layer, _)| *layer));
            }
            for layer in layers.difference(&kept) {
                let count = self.used_layer_counts.get_mut(layer).expect("counted use");
                *count -= 1;
                if *count == 0 {
                    self.used_layer_counts.remove(layer);
                }
            }
            for layer in kept.difference(layers) {
                *self.used_layer_counts.entry(*layer).or_default() += 1;
            }
            *layers = kept;
        }
        self.used_layers = sort::collect_set(
            self.local_layers
                .iter()
                .copied()
                .chain(self.used_layer_counts.keys().copied()),
        );
        // Instances keep their prototypes: two instances' indexes are
        // renumbered alike exactly when they were alike, and the records
        // shared stay shared.
        self.restack_requests(store, plan);
    }

    /// Replaces the sublayer asset requests of the stage's own layer stack
    /// with those `plan` found.
    fn restack_requests(&mut self, store: &dyn LayerStore, plan: &Restack) {
        let pseudo_root = store.paths().intern(Path::root());
        self.asset_requests.retain(|request| {
            !(request.used_by == crate::AssetUse::Sublayer && request.prim == pseudo_root)
        });
        self.asset_requests
            .extend(plan.root_requests.iter().cloned());
        sort_asset_requests(&mut self.asset_requests);
    }

    /// Whether this stage holds what [`Self::plan_restack`] cannot carry
    /// across a layer stack change: errors of layer stacks, or of arcs to
    /// muted layers when `muting` (the muted layers change), value clips,
    /// prims pending layers, a spent node budget, or sublayer requests of
    /// a stack other than its own.
    pub(crate) fn restack_unsupported(&self, store: &dyn LayerStore, muting: bool) -> bool {
        let pseudo_root = store.paths().lookup(&Path::root());
        self.errors.iter().any(|error| {
            error.prim().is_none()
                || (muting && matches!(error, CompositionError::MutedAsset(_)))
                || matches!(
                    error,
                    CompositionError::SublayerCycle(_)
                        | CompositionError::UnresolvedSublayer(_)
                        | CompositionError::InvalidAuthoredRelocation(_)
                        | CompositionError::InvalidConflictingRelocation(_)
                        | CompositionError::InvalidSameTargetRelocations(_)
                        | CompositionError::OpinionAtRelocationSource(_)
                        | CompositionError::VariableExpressionError(_)
                        | CompositionError::InvalidSublayerAndOffsetCount(_)
                        | CompositionError::MissingLayer(_)
                        | CompositionError::LimitExceeded(_)
                )
        }) || !self.clip_layers().is_empty()
            || !self.pending_prims.is_empty()
            || self.reached_node_limit()
            || self.asset_requests.iter().any(|request| {
                request.used_by == crate::AssetUse::Sublayer && Some(request.prim) != pseudo_root
            })
    }
}
