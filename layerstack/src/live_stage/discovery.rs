// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained potential arc inventory, including unloaded/muted targets.
//! USD population includes possible variant branches (AOUSD Core §10–§11).
use super::*;
use crate::sort;

#[derive(Debug, Default)]
pub(super) struct LayerDiscovery {
    layers: HashMap<LayerId, LayerReads>,
}
#[derive(Debug)]
struct LayerReads {
    generation: u64,
    structure: u64,
    sublayers: Vec<LayerId>,
    expressions: bool,
    arcs: HashMap<PathId, ArcReads>,
}
#[derive(Debug, Default, Clone)]
struct ArcReads {
    targets: Vec<LayerId>,
    expressions: bool,
}
impl ArcReads {
    fn at(store: &dyn LayerStore, layer: &crate::Layer, path: PathId) -> Self {
        let mut result = Self::default();
        for spec in layer.prim_specs(path) {
            result.add(store, &spec.references, &spec.payloads);
            for branch in spec.variant_branches() {
                result.add(store, &branch.spec.references, &branch.spec.payloads);
            }
        }
        sort::sort_unstable(&mut result.targets);
        result.targets.dedup();
        result
    }
    fn add(
        &mut self,
        store: &dyn LayerStore,
        references: &crate::ListOp<crate::Reference>,
        payloads: &crate::ListOp<crate::Reference>,
    ) {
        for arc in references.inserted_items().chain(payloads.inserted_items()) {
            self.expressions |= arc.is_expression();
            self.targets.extend(arc.stored_target_layer(store));
        }
    }
}
/// The layers `layer`'s sublayers name as the store resolves them
/// ([`crate::SublayerEntry::stored_layer`]): a sublayer authored by asset
/// path reads the layer the store binds it to, so the layer is watched,
/// and its own sublayers and arcs reached, as the layer stack gathers it.
///
/// Spec: AOUSD Core §10.3.1 (sublayers). OpenUSD opens each sublayer as it
/// computes the layer stack (`PcpLayerStack::_BuildLayerStack`).
fn sublayer_layers(store: &dyn LayerStore, layer: &crate::Layer) -> Vec<LayerId> {
    layer
        .sublayers
        .iter()
        .filter_map(|sublayer| sublayer.stored_layer(store, layer.id))
        .collect()
}

impl LayerReads {
    /// What `layer` authors that discovery reads, found by visiting its
    /// specs, unless a reader noted that none authors a reference or
    /// payload ([`crate::Layer::known_without_reference_arcs`]).
    fn scan(store: &dyn LayerStore, layer: &crate::Layer) -> Self {
        // Most specs author no arc and hold no variant that could: they
        // are passed over without looking their path up again.
        let arcs = if layer.known_without_reference_arcs() {
            HashMap::new()
        } else {
            layer
                .prims
                .iter()
                .filter(|(path, spec)| {
                    !spec.references.is_empty()
                        || !spec.payloads.is_empty()
                        || !spec.variant_sets().is_empty()
                        || layer.variant_prims.contains_key(*path)
                })
                .map(|(&path, _)| (path, ArcReads::at(store, layer, path)))
                .filter(|(_, arcs)| !arcs.targets.is_empty() || arcs.expressions)
                .collect()
        };
        Self {
            generation: layer.generation(),
            structure: layer.structural_generation(),
            sublayers: sublayer_layers(store, layer),
            expressions: layer.sublayers.iter().any(|s| s.is_expression()),
            arcs,
        }
    }

    /// Whether a sublayer or an arc is authored as an expression.
    fn has_expressions(&self) -> bool {
        self.expressions || self.arcs.values().any(|a| a.expressions)
    }

    /// The layers the layer's sublayers and arcs name.
    fn reached(&self) -> impl Iterator<Item = LayerId> + '_ {
        self.sublayers
            .iter()
            .copied()
            .chain(self.arcs.values().flat_map(|a| a.targets.iter().copied()))
    }
}

/// The separately locked parts that the layers
/// [`LayerDiscovery::participating_on`] has reached are kept in, so that
/// the threads scanning layers seldom wait on one another to claim one; a
/// power of two.
const SEEN_SHARDS: usize = 64;

/// The part of the layers reached that holds `id` ([`SEEN_SHARDS`]):
/// layer ids are numbered in turn or hashed from identifiers, so their low
/// bits spread them.
#[expect(
    clippy::cast_possible_truncation,
    reason = "only the low bits pick a part"
)]
fn seen_shard(id: LayerId) -> usize {
    (id.0 as usize) & (SEEN_SHARDS - 1)
}

impl LayerDiscovery {
    pub(super) fn has_expressions(&self) -> bool {
        self.layers.values().any(LayerReads::has_expressions)
    }

    fn refresh(&mut self, store: &dyn LayerStore, id: LayerId) {
        let Some(layer) = store.layer(id) else {
            self.layers.remove(&id);
            return;
        };
        if let Some(reads) = self.layers.get_mut(&id) {
            if reads.generation == layer.generation() {
                return;
            }
            if reads.structure == layer.structural_generation() {
                reads.generation = layer.generation();
                return;
            }
            if let Some(changed) = layer.changed_paths_since(reads.generation) {
                for path in changed {
                    let arcs = ArcReads::at(store, layer, path);
                    if arcs.targets.is_empty() && !arcs.expressions {
                        reads.arcs.remove(&path);
                    } else {
                        reads.arcs.insert(path, arcs);
                    }
                }
                reads.generation = layer.generation();
                reads.structure = layer.structural_generation();
                return;
            }
        }
        self.layers.insert(id, LayerReads::scan(store, layer));
    }

    /// Every layer reached from `root`, as [`Self::participating`] finds
    /// them, scanning the layers on the threads of `threads` when nothing is
    /// retained yet, as when a stage composes: each layer's reads are its
    /// own, and the layers reached are the same whichever thread reaches
    /// each first.
    pub(super) fn participating_on(
        &mut self,
        store: &dyn LayerStore,
        root: LayerId,
        muted: &BTreeSet<LayerId>,
        threads: &crate::parallel::Threads,
    ) -> HashSet<LayerId> {
        if !self.layers.is_empty() || threads.limit() <= 1 {
            return self.participating(store, root, muted);
        }
        let seen: [crate::sync::ReadLock<HashSet<LayerId>>; SEEN_SHARDS] =
            core::array::from_fn(|_| crate::sync::ReadLock::new(HashSet::new()));
        let scan = |id: LayerId, added: &mut Vec<LayerId>| {
            if id == LayerId::UNRESOLVED
                || muted.contains(&id)
                || !seen[seen_shard(id)].write(|seen| seen.insert(id))
            {
                return None;
            }
            let reads = LayerReads::scan(store, store.layer(id)?);
            added.extend(reads.reached());
            Some((id, reads))
        };
        let scanned =
            crate::parallel::with_pool_in(threads, &scan, |pool| pool.run(alloc::vec![root]));
        let mut expressions = false;
        for (id, reads) in scanned.into_iter().flatten() {
            expressions |= reads.has_expressions();
            self.layers.insert(id, reads);
        }
        if expressions {
            return participating_layers(store, root, muted);
        }
        let mut found = HashSet::with_capacity(self.layers.len());
        for mut shard in seen {
            found.extend(core::mem::take(shard.get_mut()));
        }
        found
    }

    pub(super) fn participating(
        &mut self,
        store: &dyn LayerStore,
        root: LayerId,
        muted: &BTreeSet<LayerId>,
    ) -> HashSet<LayerId> {
        let mut seen = HashSet::new();
        let mut pending = alloc::vec![root];
        while let Some(id) = pending.pop() {
            if id == LayerId::UNRESOLVED || muted.contains(&id) || !seen.insert(id) {
                continue;
            }
            self.refresh(store, id);
            if let Some(reads) = self.layers.get(&id) {
                if reads.expressions || reads.arcs.values().any(|a| a.expressions) {
                    return participating_layers(store, root, muted);
                }
                pending.extend(reads.sublayers.iter().copied());
                pending.extend(reads.arcs.values().flat_map(|a| a.targets.iter().copied()));
            }
        }
        seen
    }

    fn reaches(&mut self, store: &dyn LayerStore, root: LayerId, target: LayerId) -> Option<bool> {
        let mut seen = HashSet::new();
        let mut pending = alloc::vec![root];
        while let Some(id) = pending.pop() {
            if id == target {
                return Some(true);
            }
            if !seen.insert(id) {
                continue;
            }
            self.refresh(store, id);
            if let Some(reads) = self.layers.get(&id) {
                if reads.expressions || reads.arcs.values().any(|a| a.expressions) {
                    return None;
                }
                pending.extend(reads.sublayers.iter().copied());
                pending.extend(reads.arcs.values().flat_map(|a| a.targets.iter().copied()));
            }
        }
        Some(false)
    }

    /// Authored arc hosts whose potential target graph reaches the layer.
    pub(super) fn hosts_reaching(
        &mut self,
        store: &dyn LayerStore,
        roots: &[LayerId],
        target: LayerId,
    ) -> Option<Vec<(LayerId, PathId)>> {
        let mut layers = HashSet::new();
        for &root in roots {
            layers.extend(self.participating(store, root, &BTreeSet::new()));
        }
        let mut arcs = Vec::new();
        for layer in layers {
            self.refresh(store, layer);
            if let Some(reads) = self.layers.get(&layer) {
                if reads.expressions || reads.arcs.values().any(|a| a.expressions) {
                    return None;
                }
                arcs.extend(
                    reads
                        .arcs
                        .iter()
                        .map(|(path, a)| (layer, *path, a.targets.clone())),
                );
            }
        }
        let mut hosts = Vec::new();
        for (layer, path, targets) in arcs {
            for id in targets {
                if self.reaches(store, id, target)? {
                    hosts.push((layer, path));
                    break;
                }
            }
        }
        Some(hosts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryStore, Layer, PrimSpec, Reference, SublayerEntry, parallel::Threads};

    /// A discovery that scans on threads reaches the layers, and retains
    /// the reads, a discovery on one thread does, muted layers and layers
    /// the store does not hold included as on one thread.
    #[test]
    fn layers_scanned_on_threads_are_those_found_on_one() {
        let mut store = InMemoryStore::default();
        // Layer `n` sublayers `2n` and references `2n + 1` from a prim and
        // from a variant of it, up to layer 63; 64 and beyond are missing.
        for n in 1..64_u64 {
            let mut layer = Layer::new(LayerId(n));
            layer.sublayers.push(SublayerEntry::new(LayerId(2 * n)));
            let prim = store.path(&alloc::format!("/P{n}"));
            layer.insert_prim(
                prim,
                PrimSpec::def().with_reference(Reference::to_default_prim(LayerId(2 * n + 1))),
            );
            store.insert_layer(layer);
        }
        let muted = BTreeSet::from([LayerId(5)]);
        let mut one = LayerDiscovery::default();
        let found = one.participating(&store, LayerId(1), &muted);
        assert!(found.contains(&LayerId(64)) && !found.contains(&LayerId(5)));
        assert!(
            !found.contains(&LayerId(10)),
            "only muted layer 5 reaches it"
        );
        for threads in [1, 4] {
            let mut many = LayerDiscovery::default();
            assert_eq!(
                many.participating_on(&store, LayerId(1), &muted, &Threads::new(threads)),
                found
            );
            let mut retained: Vec<_> = many.layers.keys().copied().collect();
            retained.sort_unstable();
            let mut expected: Vec<_> = one.layers.keys().copied().collect();
            expected.sort_unstable();
            assert_eq!(retained, expected);
        }
        assert_eq!(found, participating_layers(&store, LayerId(1), &muted));
    }
}
