// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Preparation from the evidence a live stage retains
//! ([`crate::stage::preparation`]) rather than from the whole stage.
//!
//! A namespace edit changes the specs at and beneath the paths it moves or
//! deletes, their parents' child lists, and the specs that author a path
//! or an arc. Every other spec is left as it is, so preparation edits a
//! working copy of each layer holding only those slots: the move and
//! repair steps visit every spec of the copy, as they would of the whole
//! layer, and the transaction lists the slots whose content changed.
//!
//! Spec: AOUSD Core §8 (namespace), §10 (composition arcs).

use alloc::borrow::Cow;

use super::*;
use crate::{
    HashSet,
    prim_indexing::ChildIndex,
    stage::preparation::{PreparationIndex, layer_links},
};

/// The prim slots of `layer` that author paths, from the first of
/// `indexes` that holds the layer at its generation, otherwise found by
/// visiting every spec.
pub(super) fn links_of<'a>(
    indexes: &[&'a PreparationIndex],
    layer: &Layer,
) -> Cow<'a, HashSet<PathId>> {
    let index = indexes
        .iter()
        .copied()
        .find(|index| index.links(layer).is_some());
    layer_links(index.or(indexes.first().copied()), layer)
}

/// The layers [`reachable_layers`] finds, from a stage's retained opinion
/// layers and the specs of each layer that author arcs.
pub(super) fn reachable_layers(
    store: &dyn LayerStore,
    indexes: &[&PreparationIndex],
    index: &PreparationIndex,
    root: LayerId,
) -> Vec<LayerId> {
    let mut found = Vec::new();
    let mut pending = alloc::vec![root];
    pending.extend(index.stack_layers());
    while let Some(id) = pending.pop() {
        if found.contains(&id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            continue;
        };
        found.push(id);
        pending.extend(layer.sublayers.iter().map(|s| s.layer));
        for path in links_of(indexes, layer).iter() {
            for spec in layer
                .prims
                .get(path)
                .into_iter()
                .chain(layer.variant_prims.get(path).into_iter().flatten())
            {
                for refs in [&spec.references, &spec.payloads] {
                    collect_ref_layers(store, refs, &mut pending);
                }
                collect_variant_layers(store, &spec.variant_sets, &mut pending);
            }
        }
    }
    found.sort_unstable();
    found
}

/// The prim slots `layer` holds at or beneath `root`, in any variant
/// branches, found through the child names `children` indexes.
pub(super) fn slots_beneath(
    store: &dyn LayerStore,
    children: &ChildIndex,
    layer: &Layer,
    root: PathId,
) -> Vec<PathId> {
    let mut slots = Vec::new();
    let mut pending = alloc::vec![root];
    while let Some(path) = pending.pop() {
        if layer.prims.contains_key(&path) || layer.variant_prims.contains_key(&path) {
            slots.push(path);
        }
        for name in children.child_names(store, layer, path) {
            pending.push(store.paths().intern_joined(path, &[name]));
        }
    }
    slots
}

/// `path` and each of its ancestors, the pseudo-root included.
pub(super) fn at_or_above(store: &dyn LayerStore, path: PathId) -> impl Iterator<Item = PathId> {
    let paths = store.paths();
    let mut chain = Vec::new();
    let mut current = Some(path);
    while let Some(at) = current {
        chain.push(at);
        current = paths.parent(at);
    }
    chain.into_iter()
}

/// A working copy of `layer` holding only its slots at `slots`, with its
/// layer fields and relocates.
pub(super) fn working_copy(layer: &Layer, slots: &[PathId]) -> Layer {
    let mut copy = Layer::new(layer.id);
    copy.default_prim = layer.default_prim;
    copy.metadata.clone_from(&layer.metadata);
    copy.relocates.clone_from(&layer.relocates);
    for path in slots {
        if let Some(spec) = layer.prims.get(path) {
            copy.prims.insert(*path, spec.clone());
        }
        if let Some(specs) = layer.variant_prims.get(path) {
            copy.variant_prims.insert(*path, specs.clone());
        }
    }
    copy
}

/// The slots a working copy of `original` holding `slots` may differ from
/// it at, sorted: those it was made with and those it holds now.
pub(super) fn copy_slots(slots: &[PathId], copy: &Layer) -> Vec<PathId> {
    let mut paths: Vec<PathId> = slots
        .iter()
        .chain(copy.prims.keys())
        .chain(copy.variant_prims.keys())
        .copied()
        .collect();
    paths.sort_unstable();
    paths.dedup();
    paths
}

/// [`append_delta`] for a working copy `edited` of `original`, at the
/// sorted slots `paths` it may differ at: the same steps, in the same
/// order, as for the whole layer.
pub(super) fn append_delta_at(
    original: &Layer,
    edited: &Layer,
    paths: &[PathId],
    transaction: &mut Transaction,
    specs: &mut Vec<(LayerId, SpecPath)>,
) {
    for &path in paths {
        let main = edited.prims.get(&path);
        let branches = edited.variant_prims.get(&path);
        if original.prims.get(&path).same_ref(main)
            && original.variant_prims.get(&path).same_ref(branches)
        {
            continue;
        }
        let before = Raw::PrimSlots {
            layer: original.id,
            path,
            main: original.prims.get(&path).cloned(),
            branches: original.variant_prims.get(&path).cloned(),
        };
        let after = Raw::PrimSlots {
            layer: edited.id,
            path,
            main: main.cloned(),
            branches: branches.cloned(),
        };
        push(transaction, after, before);
        specs.push((original.id, SpecPath::from_prim_path(path)));
    }
}

/// `original` with the slots of its working copy `edited` at `paths`, and
/// its layer fields and relocates: the whole edited layer, for composing
/// the projected stage. `None` when nothing differs.
pub(super) fn project(original: &Layer, edited: &Layer, paths: &[PathId]) -> Option<Layer> {
    let differs = |path: &PathId| {
        !original.prims.get(path).same_ref(edited.prims.get(path))
            || !original
                .variant_prims
                .get(path)
                .same_ref(edited.variant_prims.get(path))
    };
    if !paths.iter().any(differs)
        && original.default_prim == edited.default_prim
        && original.metadata.same(&edited.metadata)
        && original.relocates == edited.relocates
    {
        return None;
    }
    let mut layer = original.clone();
    for &path in paths.iter().filter(|path| differs(path)) {
        match edited.prims.get(&path) {
            Some(spec) => layer.prims.insert(path, spec.clone()),
            None => layer.prims.remove(&path),
        };
        match edited.variant_prims.get(&path) {
            Some(specs) => layer.variant_prims.insert(path, specs.clone()),
            None => layer.variant_prims.remove(&path),
        };
    }
    layer.default_prim = edited.default_prim;
    layer.metadata.clone_from(&edited.metadata);
    layer.relocates.clone_from(&edited.relocates);
    Some(layer)
}

/// [`Same`] between optional borrowed slots.
trait SameRef<T> {
    fn same_ref(self, other: Option<&T>) -> bool;
}
impl<T: Same> SameRef<T> for Option<&T> {
    fn same_ref(self, other: Option<&T>) -> bool {
        match (self, other) {
            (Some(a), Some(b)) => a.same(b),
            (None, None) => true,
            _ => false,
        }
    }
}
