// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A source namespace index for the deliberately bounded local edit path.
//! This records actual stored prim paths, not optional authored child names.
//! It owns source enumeration and eligibility; shared stage composition still
//! owns child order, activation and pruning (AOUSD Core §11.3.1).

use crate::{LayerId, LayerStore, ListOp, Path, PathId, PrimSpec, StageOptions};
use alloc::vec::Vec;
use hashbrown::HashMap;

#[derive(Debug)]
pub(crate) struct LocalNamespace {
    pub(crate) layer: LayerId,
    pub(crate) generation: u64,
    children: HashMap<PathId, Vec<PathId>>,
}

impl LocalNamespace {
    pub(crate) fn build(
        store: &dyn LayerStore,
        root: LayerId,
        options: &StageOptions,
    ) -> Option<Self> {
        let layer = store.layer(root)?;
        if options.mask.is_some()
            || !layer.sublayers.is_empty()
            || !layer.relocates.is_empty()
            || !layer.variant_prims.is_empty()
            || !layer.prims.values().all(|s| supported(store, s))
        {
            return None;
        }
        let mut index = Self {
            layer: root,
            generation: layer.generation(),
            children: HashMap::new(),
        };
        for &path in layer.prims.keys() {
            if let Some(parent) = store.paths().resolve(path).parent() {
                let id = store.paths().lookup(&parent)?;
                // Implicit ancestors require additional pruning context. Leave
                // that case to ordinary composition until it is indexed too.
                if parent != Path::root() && !layer.prims.contains_key(&id) {
                    return None;
                }
                index.children.entry(id).or_default().push(path);
            }
        }
        Some(index)
    }

    pub(crate) fn current(&self, store: &dyn LayerStore) -> bool {
        store
            .layer(self.layer)
            .is_some_and(|l| l.generation() == self.generation)
    }

    pub(crate) fn children(&self, path: PathId) -> &[PathId] {
        self.children.get(&path).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn subtree(&self, root: PathId) -> Vec<PathId> {
        let mut result = alloc::vec![root];
        let mut at = 0;
        while at < result.len() {
            let path = result[at];
            result.extend_from_slice(self.children(path));
            at += 1;
        }
        result
    }

    /// Reconcile after atomic success only; failed transactions never mutate
    /// this index. Every changed slot must remain within the supported scope.
    pub(crate) fn reconcile(&mut self, store: &dyn LayerStore, changed: &[PathId]) -> bool {
        let Some(layer) = store.layer(self.layer) else {
            return false;
        };
        // An inverse may restore branch slots even when the current stage
        // was purely local. Eligibility follows the resulting authored layer.
        if !layer.sublayers.is_empty()
            || !layer.relocates.is_empty()
            || !layer.variant_prims.is_empty()
        {
            return false;
        }
        for &path in changed {
            if let Some(spec) = layer.prims.get(&path) {
                if !supported(store, spec) {
                    return false;
                }
                let Some(parent) = store.paths().resolve(path).parent() else {
                    continue;
                };
                let Some(id) = store.paths().lookup(&parent) else {
                    return false;
                };
                if parent != Path::root() && !layer.prims.contains_key(&id) {
                    return false;
                }
                let children = self.children.entry(id).or_default();
                if !children.contains(&path) {
                    children.push(path);
                }
            } else {
                self.children.remove(&path);
                if let Some(parent) = store
                    .paths()
                    .resolve(path)
                    .parent()
                    .and_then(|p| store.paths().lookup(&p))
                    && let Some(children) = self.children.get_mut(&parent)
                {
                    children.retain(|p| *p != path);
                    if children.is_empty() {
                        self.children.remove(&parent);
                    }
                }
            }
        }
        self.generation = layer.generation();
        true
    }
}

fn supported(store: &dyn LayerStore, spec: &PrimSpec) -> bool {
    spec.references == ListOp::default()
        && spec.payloads == ListOp::default()
        && spec.inherits == ListOp::default()
        && spec.specializes == ListOp::default()
        && spec.outer_variant_sites.is_empty()
        && spec.variant_sets.is_empty()
        && spec.variant_selections.is_empty()
        && spec.variant_set_order.is_empty()
        && spec.deleted_variant_sets.is_empty()
        && spec.instanceable.is_none()
        && !spec
            .fields
            .iter()
            .any(|f| store.tokens().resolve(f.name) == "instanceable")
}

impl crate::edit::SourceNamespace for LocalNamespace {
    fn subtree(&self, layer: LayerId, root: PathId) -> Option<Vec<PathId>> {
        (layer == self.layer).then(|| self.subtree(root))
    }
}

#[cfg(test)]
mod tests;
