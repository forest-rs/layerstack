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
//! layer, and the transaction lists the slots whose content changed. A
//! layer outside every stage's layer stack has only its arcs into the
//! edited layers repaired ([`arcs_into`]), so its copy holds only the
//! slots authoring those, and a layer authoring none is not copied.
//!
//! Spec: AOUSD Core §8 (namespace), §10 (composition arcs).

use crate::sort;
use alloc::borrow::Cow;

use super::*;
use crate::{
    HashSet, Relocate,
    prim_indexing::ChildIndex,
    stage::preparation::{LayerArcs, PreparationIndex, layer_arcs, layer_links},
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

/// The references and payloads of `layer` ([`LayerArcs`]), as
/// [`links_of`] finds the slots that author paths.
pub(super) fn arcs_of<'a>(indexes: &[&'a PreparationIndex], layer: &Layer) -> Cow<'a, LayerArcs> {
    let index = indexes
        .iter()
        .copied()
        .find(|index| index.arcs(layer).is_some());
    layer_arcs(index.or(indexes.first().copied()), layer)
}

/// The layers [`reachable_layers`] finds, from a stage's retained opinion
/// layers and the layers each layer's arcs name ([`arcs_of`]): the specs
/// of arcs the store resolves are read again.
pub(super) fn reachable_layers(
    store: &dyn LayerStore,
    indexes: &[&PreparationIndex],
    index: &PreparationIndex,
    root: LayerId,
) -> Vec<LayerId> {
    let mut found = Vec::new();
    let mut pending = alloc::vec![root];
    pending.extend(index.stack_layers());
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            continue;
        };
        found.push(id);
        pending.extend(layer.sublayers.iter().map(|s| s.layer));
        let arcs = arcs_of(indexes, layer);
        pending.extend(arcs.layers());
        for path in arcs.unresolved() {
            for spec in layer
                .prims
                .get(path)
                .into_iter()
                .chain(layer.variant_prims.get(path).into_iter().flatten())
            {
                for refs in [&spec.references, &spec.payloads] {
                    collect_ref_layers(store, refs, &mut pending);
                }
                collect_variant_layers(store, spec.variant_sets(), &mut pending);
            }
        }
    }
    sort::sort_unstable(&mut found);
    found
}

/// Whether a spec of `layer` at the prim path `path`, outside variant
/// branches or in one, has a reference or payload targeting one of
/// `edited`, or an asset expression.
///
/// In a layer outside every stage's layer stack these are the only specs
/// a namespace edit repairs ([`repair_prim`] with no local move retargets
/// the arcs into the edited layers and rejects asset expressions), so the
/// edit copies and visits only them. OpenUSD likewise edits a dependent
/// layer only at the arcs `PcpGatherDependentNamespaceEdits` finds
/// targeting the edited site. AOUSD Core §10.3.2.2 (references), §10.3.2.3
/// (payloads).
pub(super) fn arcs_into(
    store: &dyn LayerStore,
    layer: &Layer,
    path: PathId,
    edited: &[LayerId],
) -> bool {
    fn repairable(store: &dyn LayerStore, refs: &ListOp<Reference>, edited: &[LayerId]) -> bool {
        refs.items().any(|r| {
            r.is_expression()
                || r.stored_target_layer(store)
                    .is_some_and(|target| edited.contains(&target))
        })
    }
    fn in_variants(store: &dyn LayerStore, sets: &VariantSets, edited: &[LayerId]) -> bool {
        sets.values()
            .flat_map(|set| set.variants.values())
            .any(|v| {
                repairable(store, &v.references, edited)
                    || repairable(store, &v.payloads, edited)
                    || in_variants(store, &v.variant_sets, edited)
            })
    }
    layer
        .prims
        .get(&path)
        .into_iter()
        .chain(layer.variant_prims.get(&path).into_iter().flatten())
        .any(|spec| {
            repairable(store, &spec.references, edited)
                || repairable(store, &spec.payloads, edited)
                || in_variants(store, spec.variant_sets(), edited)
        })
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
    sort::sort_unstable(&mut paths);
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

/// What a prepared edit changes in one layer, against its working copy.
pub(super) struct LayerEdit {
    /// The prim slots whose content changes, sorted.
    pub(super) slots: Vec<PathId>,
    /// Those of `slots` that change other than in their child lists and
    /// child order, a missing spec counting as a bare `over`.
    pub(super) structural: Vec<PathId>,
    /// Whether the layer's metadata changes.
    pub(super) metadata: bool,
    /// The paths whose relocation the layer's `layerRelocates` change
    /// ([`relocation_changes`]).
    pub(super) relocated: Vec<PathId>,
}

/// The paths whose relocation changes from the relocates `old` to `new`:
/// the source and target of each relocate only one of them authors. A
/// source relocated elsewhere is one relocate removed and one added.
///
/// Spec: AOUSD Core §10.3.2.6 (relocates). OpenUSD:
/// `_DeterminePathsAffectedByRelocationChanges` (`pxr/usd/pcp/changes.cpp`),
/// which finds the same paths, less the source of a relocate whose target
/// alone changes.
pub(super) fn relocation_changes(old: &[Relocate], new: &[Relocate]) -> Vec<PathId> {
    let mut paths = Vec::new();
    for (ours, theirs) in [(old, new), (new, old)] {
        for relocate in ours.iter().filter(|r| !theirs.contains(r)) {
            paths.push(relocate.source);
            paths.extend(relocate.target);
        }
    }
    sort::sort_unstable(&mut paths);
    paths.dedup();
    paths
}

/// Whether `before` and `after` differ in their child lists or child order
/// only.
fn children_only(before: &PrimSpec, after: &PrimSpec) -> bool {
    let without_children = |spec: &PrimSpec| {
        let mut spec = PrimSpec {
            authored_children: Vec::new(),
            ..spec.clone()
        };
        if let Some(rare) = spec.authored_rare_mut() {
            rare.prim_order = None;
        }
        spec
    };
    without_children(before).same(&without_children(after))
}

/// What the working copy `edited` of `original` changes at the slots
/// `paths` it may differ at, and in the layer's fields; `None` for
/// nothing.
pub(super) fn layer_edit(original: &Layer, edited: &Layer, paths: &[PathId]) -> Option<LayerEdit> {
    let mut slots = Vec::new();
    let mut structural = Vec::new();
    for &path in paths {
        let (before, after) = (original.prims.get(&path), edited.prims.get(&path));
        let branches_same = original
            .variant_prims
            .get(&path)
            .same_ref(edited.variant_prims.get(&path));
        if before.same_ref(after) && branches_same {
            continue;
        }
        slots.push(path);
        // A bare `over` holds no opinion but its children: creating one
        // to hold a reparented child, or removing it, changes the child
        // list alone.
        let bare = PrimSpec::over();
        let (b, a) = (before.unwrap_or(&bare), after.unwrap_or(&bare));
        if !(branches_same && children_only(b, a)) {
            structural.push(path);
        }
    }
    let metadata = !original.metadata.same(&edited.metadata);
    let relocated = relocation_changes(original.relocates(), edited.relocates());
    if slots.is_empty()
        && !metadata
        && relocated.is_empty()
        && original.default_prim == edited.default_prim
    {
        return None;
    }
    Some(LayerEdit {
        slots,
        structural,
        metadata,
        relocated,
    })
}

/// `original` as `edit` leaves it, from its working copy `edited`.
pub(super) fn materialize(original: &Layer, edited: &Layer, edit: &LayerEdit) -> Layer {
    let mut layer = original.clone();
    for &path in &edit.slots {
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
    layer
}

/// A layer's content an [`InPlace`] projection replaced.
struct Displaced {
    layer: LayerId,
    slots: Vec<(PathId, Option<PrimSpec>, Option<Vec<PrimSpec>>)>,
    default_prim: Option<TokenId>,
    metadata: Vec<FieldEntry>,
    relocates: Option<Vec<Relocate>>,
}

/// `store`, with the slots and fields each [`LayerEdit`] changes holding
/// its working copy's content, until dropped: the projected store, made
/// without copying any layer whole. The layers keep their generations and
/// change histories, and get their own content back exactly when this is
/// dropped, so no edit is recorded or visible after preparation returns.
pub(super) struct InPlace<'a> {
    store: &'a mut dyn LayerStore,
    displaced: Vec<Displaced>,
    complete: bool,
}

impl<'a> InPlace<'a> {
    /// Projects the `edits` of the working copies `edited` into `store`.
    /// [`Self::complete`] tells whether the store lent every layer.
    pub(super) fn project(
        store: &'a mut dyn LayerStore,
        edited: &crate::HashMap<LayerId, Layer>,
        edits: &crate::HashMap<LayerId, LayerEdit>,
    ) -> Self {
        let mut projection = Self {
            store,
            displaced: Vec::new(),
            complete: true,
        };
        let mut ids: Vec<LayerId> = edits.keys().copied().collect();
        sort::sort_unstable(&mut ids);
        for id in ids {
            let (edit, copy) = (&edits[&id], &edited[&id]);
            let Some(layer) = projection.store.layer_mut(id) else {
                projection.complete = false;
                break;
            };
            let mut displaced = Displaced {
                layer: id,
                slots: Vec::with_capacity(edit.slots.len()),
                default_prim: core::mem::replace(&mut layer.default_prim, copy.default_prim),
                metadata: core::mem::replace(&mut layer.metadata, copy.metadata.clone()),
                relocates: core::mem::replace(&mut layer.relocates, copy.relocates.clone()),
            };
            for &path in &edit.slots {
                let main = match copy.prims.get(&path) {
                    Some(spec) => layer.prims.insert(path, spec.clone()),
                    None => layer.prims.remove(&path),
                };
                let branches = match copy.variant_prims.get(&path) {
                    Some(specs) => layer.variant_prims.insert(path, specs.clone()),
                    None => layer.variant_prims.remove(&path),
                };
                displaced.slots.push((path, main, branches));
            }
            projection.displaced.push(displaced);
        }
        projection
    }

    /// Whether every edited layer is projected.
    pub(super) fn complete(&self) -> bool {
        self.complete
    }

    /// The projected store.
    pub(super) fn store(&mut self) -> &mut dyn LayerStore {
        &mut *self.store
    }
}

impl Drop for InPlace<'_> {
    fn drop(&mut self) {
        for displaced in self.displaced.drain(..).rev() {
            let layer = self
                .store
                .layer_mut(displaced.layer)
                .expect("a projected layer stays in its store");
            layer.default_prim = displaced.default_prim;
            layer.metadata = displaced.metadata;
            layer.relocates = displaced.relocates;
            for (path, main, branches) in displaced.slots {
                match main {
                    Some(spec) => layer.prims.insert(path, spec),
                    None => layer.prims.remove(&path),
                };
                match branches {
                    Some(specs) => layer.variant_prims.insert(path, specs),
                    None => layer.variant_prims.remove(&path),
                };
            }
        }
    }
}

/// The paths of `paths` beneath none of the others, sorted.
pub(super) fn minimal_roots(store: &dyn LayerStore, paths: &[PathId]) -> Vec<PathId> {
    let set: HashSet<PathId> = paths.iter().copied().collect();
    let mut roots: Vec<PathId> = set
        .iter()
        .copied()
        .filter(|path| {
            let mut current = store.paths().parent(*path);
            while let Some(parent) = current {
                if set.contains(&parent) {
                    return false;
                }
                current = store.paths().parent(parent);
            }
            true
        })
        .collect();
    sort::sort_unstable(&mut roots);
    roots
}

/// Whether `path` is one of `roots` or beneath one.
pub(super) fn is_beneath(store: &dyn LayerStore, path: PathId, roots: &HashSet<PathId>) -> bool {
    let mut current = Some(path);
    while let Some(at) = current {
        if roots.contains(&at) {
            return true;
        }
        current = store.paths().parent(at);
    }
    false
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
