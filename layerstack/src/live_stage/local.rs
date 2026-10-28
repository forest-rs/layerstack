// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A source namespace index for the deliberately bounded local edit path.
//! This records actual stored prim paths, not optional authored child names.
//! It owns source enumeration and eligibility; shared stage composition still
//! owns child order, activation and pruning (AOUSD Core §11.3.1).

use crate::{LayerId, LayerStore, Path, PathId, PrimSpec, StageOptions};
use alloc::vec::Vec;
use hashbrown::HashMap;

#[derive(Debug)]
pub(crate) struct LocalNamespace {
    pub(crate) layer: LayerId,
    pub(crate) generation: u64,
    children: HashMap<PathId, Vec<PathId>>,
}

/// A [`LocalNamespace`] built when an edit first asks for it, rather than
/// with each composition: the index serves only the local edit path, and
/// building it walks and sorts every prim spec of the root layer.
///
/// A composition records the root layer's generation and whether the
/// stage's options allow the index; the first edit builds the index from
/// the layer as it was composed. When the layer has changed since, no index
/// is current, as an index built with the composition would not be.
#[derive(Debug)]
pub(crate) enum LocalSlot {
    /// Not built yet: the root layer's generation when composed, and
    /// whether the composition's options allow an index.
    Pending {
        generation: Option<u64>,
        allowed: bool,
    },
    /// Built, or found not to apply (`None`).
    Built(Option<LocalNamespace>),
}

impl LocalSlot {
    /// The slot a composition of `root` with `options` leaves, to be
    /// built on first use.
    pub(crate) fn pending(store: &dyn LayerStore, root: LayerId, options: &StageOptions) -> Self {
        Self::Pending {
            generation: store.layer(root).map(crate::Layer::generation),
            allowed: options.session_layer.is_none() && options.mask.is_none(),
        }
    }

    /// The index, built now if it is pending and the root layer is
    /// unchanged since the composition.
    pub(crate) fn get(
        &mut self,
        store: &dyn LayerStore,
        root: LayerId,
    ) -> Option<&mut LocalNamespace> {
        if let Self::Pending {
            generation,
            allowed,
        } = *self
        {
            let unchanged = generation.is_some()
                && store.layer(root).map(crate::Layer::generation) == generation;
            *self = Self::Built(
                (allowed && unchanged)
                    .then(|| LocalNamespace::build_allowed(store, root))
                    .flatten(),
            );
        }
        match self {
            Self::Built(index) => index.as_mut(),
            Self::Pending { .. } => None,
        }
    }

    /// The index if built, without building it.
    pub(crate) fn built(&self) -> Option<&LocalNamespace> {
        match self {
            Self::Built(index) => index.as_ref(),
            Self::Pending { .. } => None,
        }
    }
}

impl LocalNamespace {
    /// The index of `root`, for a stage whose options allow one.
    fn build_allowed(store: &dyn LayerStore, root: LayerId) -> Option<Self> {
        let layer = store.layer(root)?;
        if !layer.sublayers.is_empty()
            || !layer.relocates().is_empty()
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
        let root_path = store.paths().lookup(&Path::root());
        for &path in layer.prims.keys() {
            // The interner keeps each path's parent: no path is built.
            if let Some(id) = store.paths().parent(path) {
                // Implicit ancestors require additional pruning context. Leave
                // that case to ordinary composition until it is indexed too.
                if Some(id) != root_path && !layer.prims.contains_key(&id) {
                    return None;
                }
                index.children.entry(id).or_default().push(path);
            }
        }
        // Keep the population order once, so a changed boundary does not
        // sort every sibling again. Authored ordering is applied separately.
        for children in index.children.values_mut() {
            store
                .paths()
                .sort_in_namespace_order(children, store.tokens());
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
            || !layer.relocates().is_empty()
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
            } else if parent.is_some() {
                // A removed spec's children went with it. The pseudo-root's
                // are root prims, whose specs remain when its own goes.
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
    store.paths().sort_paths(edits, store.tokens());
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
    crate::arcs::is_local_spec(spec)
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
