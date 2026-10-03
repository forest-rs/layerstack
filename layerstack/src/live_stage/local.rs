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
        // Keep the population order once, so a changed boundary does not
        // sort every sibling again. Authored ordering is applied separately.
        for children in index.children.values_mut() {
            children.sort_by(|a, b| compare_paths(store, *a, *b));
        }
        Some(index)
    }

    pub(crate) fn current(&self, store: &dyn LayerStore) -> bool {
        store
            .layer(self.layer)
            .is_some_and(|l| l.generation() == self.generation)
    }

    /// Direct source children in token-string namespace order.
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
        let mut pending: HashMap<PathId, Vec<PathId>> = HashMap::new();
        for &path in changed {
            let parent = match store.paths().resolve(path).parent() {
                Some(parent) => {
                    let Some(id) = store.paths().lookup(&parent) else {
                        return false;
                    };
                    Some((id, parent == Path::root()))
                }
                None => None,
            };
            if let Some(spec) = layer.prims.get(&path) {
                if !supported(store, spec)
                    || parent.is_some_and(|(id, root)| !root && !layer.prims.contains_key(&id))
                {
                    return false;
                }
            } else {
                self.children.remove(&path);
            }
            let Some((parent, _)) = parent else {
                continue;
            };
            if changed.len() == 1 {
                let children = self.children.entry(parent).or_default();
                update_child(store, layer, children, path);
                if children.is_empty() {
                    self.children.remove(&parent);
                }
            } else {
                pending.entry(parent).or_default().push(path);
            }
        }
        for (parent, mut edits) in pending {
            let children = self.children.entry(parent).or_default();
            if edits.len() == 1 {
                update_child(store, layer, children, edits[0]);
            } else {
                merge_child_edits(store, layer, children, &mut edits);
            }
            if children.is_empty() {
                self.children.remove(&parent);
            }
        }
        self.generation = layer.generation();
        true
    }
}

fn update_child(
    store: &dyn LayerStore,
    layer: &crate::Layer,
    children: &mut Vec<PathId>,
    path: PathId,
) {
    match (
        children.binary_search_by(|child| compare_paths(store, *child, path)),
        layer.prims.contains_key(&path),
    ) {
        (Err(at), true) => children.insert(at, path),
        (Ok(at), false) => {
            children.remove(at);
        }
        _ => {}
    }
}

/// The committed source marks missing paths as tombstones. Merge each parent's
/// edits once, copying surviving runs instead of shifting the tail for every
/// deletion or restoration. Nothing dead is retained between transactions.
fn merge_child_edits(
    store: &dyn LayerStore,
    layer: &crate::Layer,
    children: &mut Vec<PathId>,
    edits: &mut Vec<PathId>,
) {
    edits.sort_by(|a, b| compare_paths(store, *a, *b));
    edits.dedup();
    let mut merged = Vec::with_capacity(children.len() + edits.len());
    let mut at = 0;
    for &path in edits.iter() {
        let start = at;
        // Consecutive edits often already meet the next child. Search only
        // when an untouched run needs to be copied ahead of this edit.
        if children
            .get(at)
            .is_some_and(|child| *child != path && compare_paths(store, *child, path).is_lt())
        {
            at +=
                children[at..].partition_point(|child| compare_paths(store, *child, path).is_lt());
        }
        merged.extend_from_slice(&children[start..at]);
        if children.get(at) == Some(&path) {
            at += 1;
        }
        if layer.prims.contains_key(&path) {
            merged.push(path);
        }
    }
    merged.extend_from_slice(&children[at..]);
    *children = merged;
}

fn compare_paths(store: &dyn LayerStore, a: PathId, b: PathId) -> core::cmp::Ordering {
    store
        .paths()
        .resolve(a)
        .cmp_with_tokens(store.paths().resolve(b), store.tokens())
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
        && !spec.fields.iter().any(|f| {
            matches!(
                store.tokens().resolve(f.name),
                "instanceable" | "clips" | "clipSets"
            )
        })
}

impl crate::edit::SourceNamespace for LocalNamespace {
    fn subtree(&self, layer: LayerId, root: PathId) -> Option<Vec<PathId>> {
        (layer == self.layer).then(|| self.subtree(root))
    }
}

#[cfg(test)]
mod tests;
