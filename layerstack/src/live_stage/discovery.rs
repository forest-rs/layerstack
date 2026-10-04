// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained potential arc inventory, including unloaded/muted targets.
//! USD population includes possible variant branches (AOUSD Core §10–§11).
use super::*;

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
    fn at(layer: &crate::Layer, path: PathId) -> Self {
        let mut result = Self::default();
        for spec in layer.prim_specs(path) {
            result.add(&spec.references, &spec.payloads);
            for branch in spec.variant_branches() {
                result.add(&branch.spec.references, &branch.spec.payloads);
            }
        }
        result.targets.sort_unstable();
        result.targets.dedup();
        result
    }
    fn add(
        &mut self,
        references: &crate::ListOp<crate::Reference>,
        payloads: &crate::ListOp<crate::Reference>,
    ) {
        for arc in references.inserted_items().chain(payloads.inserted_items()) {
            self.expressions |= arc.is_expression();
            self.targets.push(arc.layer);
        }
    }
}
impl LayerDiscovery {
    pub(super) fn has_expressions(&self) -> bool {
        self.layers
            .values()
            .any(|reads| reads.expressions || reads.arcs.values().any(|a| a.expressions))
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
                    let arcs = ArcReads::at(layer, path);
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
        let arcs = layer
            .prims
            .keys()
            .copied()
            .map(|path| (path, ArcReads::at(layer, path)))
            .filter(|(_, arcs)| !arcs.targets.is_empty() || arcs.expressions)
            .collect();
        self.layers.insert(
            id,
            LayerReads {
                generation: layer.generation(),
                structure: layer.structural_generation(),
                sublayers: layer.sublayers.iter().map(|s| s.layer).collect(),
                expressions: layer.sublayers.iter().any(|s| s.is_expression()),
                arcs,
            },
        );
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
