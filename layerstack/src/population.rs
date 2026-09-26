// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage population.
//!
//! Population determines which prim paths exist in the composed stage and builds
//! a deterministic parent→children index for traversal.
//!
//! Spec: AOUSD Core §11 (stage population).

use alloc::{collections::BTreeSet, rc::Rc, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::variant_fallbacks::VariantFallbacks;
use crate::{
    arc_cycle::ArcChain,
    arcs::{
        SelectionScope, collect_all_variant_branch_payloads, collect_all_variant_branch_references,
        collect_all_variant_child_references, resolve_inherits_for_prim, resolve_payloads_for_prim,
        resolve_references_for_prim, resolve_specializes_for_prim,
    },
    doc::LayerStore,
    doc::{LayerId, Reference, ReferenceTarget},
    expression_variables::{ArcAnchor, ExpressionScope},
    layer_stack::LayerStack,
    path::{Path, PathId, PathInterner},
    relocates::{LiftedSet, Relocations, Walk},
    stage::PopulationMask,
};

/// Produces the set of populated prim paths and a parent→children index.
///
/// Arcs place their targets' prims through the relocations of the layer
/// stacks they reach; a prim at or beneath a relocation source of the
/// stage's layer stack is not populated (AOUSD Core §10.3.2.6). Population
/// follows the arcs of unselected variant branches too, so the sources of
/// the relocations it lifts through them stay: composition removes those
/// of the arcs it follows.
pub(crate) fn populate(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    mask: Option<&PopulationMask>,
    relocations: &mut Relocations,
) -> (BTreeSet<PathId>, HashMap<PathId, Vec<PathId>>) {
    let mut paths = gather_populated_paths(store, local_stack, relocations);
    let moved = relocations.take_moved();
    let placed = split_moved_paths(store, &mut paths, &moved);
    add_ancestor_paths(store, &mut paths);
    add_moved_paths(store, &mut paths, placed);
    add_relocation_targets(store, relocations, &mut paths);
    paths.retain(|path| !relocations.is_prohibited(store.paths(), *path));
    apply_population_mask(store, &mut paths, mask);
    let children = build_children_index(store, paths.iter().copied());
    (paths, children)
}

/// Removes from `paths` those an arc placed only through a relocation,
/// `moved`, and those beneath them, and returns them.
fn split_moved_paths(
    store: &dyn LayerStore,
    paths: &mut BTreeSet<PathId>,
    moved: &HashSet<PathId>,
) -> Vec<PathId> {
    if moved.is_empty() {
        return Vec::new();
    }
    let interner = store.paths();
    let beneath_moved = |path: PathId| {
        let mut current = interner.resolve(path).parent();
        while let Some(parent) = current {
            if interner
                .lookup(&parent)
                .is_some_and(|id| moved.contains(&id))
            {
                return true;
            }
            current = parent.parent();
        }
        false
    };
    let split: Vec<PathId> = paths
        .iter()
        .copied()
        .filter(|path| moved.contains(path) || beneath_moved(*path))
        .collect();
    for path in &split {
        paths.remove(path);
    }
    split
}

/// Adds each of `placed`, paths an arc placed through a relocation and
/// those beneath them, whose parent is populated, parents first.
///
/// A relocated prim composes at its target only beneath a parent that
/// exists without it: moving a prim never creates the target's ancestors.
///
/// Spec: AOUSD Core §10.3.2.6, §11.3.1 (a prim's children are composed
/// from the prim index of the prim, which exists first). OpenUSD:
/// `_ComposePrimChildNamesAtNode` in `pxr/usd/pcp/primIndex.cpp`.
fn add_moved_paths(store: &dyn LayerStore, paths: &mut BTreeSet<PathId>, mut placed: Vec<PathId>) {
    let interner = store.paths();
    placed.sort_by_key(|path| interner.resolve(*path).depth());
    for path in placed {
        let parent = interner
            .resolve(path)
            .parent()
            .and_then(|parent| interner.lookup(&parent));
        if parent.is_some_and(|parent| paths.contains(&parent)) {
            paths.insert(path);
        }
    }
}

/// Adds each relocation target whose parent is populated: a relocation
/// that moves a prim to a new parent adds a child to that parent whatever
/// the source's opinions, and one that renames a child within its parent
/// replaces a child that the arcs above the source bring. The relocating
/// layer stack's own specs at the source do not count: they are ignored.
///
/// Spec: AOUSD Core §11.3.1 (relocates that "extend" add children; those
/// that "rename" replace them). OpenUSD: `_ComposePrimChildNamesAtNode` in
/// `pxr/usd/pcp/primIndex.cpp`.
fn add_relocation_targets(
    store: &mut dyn LayerStore,
    relocations: &Relocations,
    paths: &mut BTreeSet<PathId>,
) {
    // A target may lie beneath another target: repeat until none is added.
    loop {
        let mut added = Vec::new();
        for (target, source, _) in relocations.proposed_targets() {
            if paths.contains(&target) {
                continue;
            }
            let (target_parent, source_parent) = {
                let paths = store.paths();
                (
                    paths.resolve(target).parent(),
                    paths.resolve(source).parent(),
                )
            };
            let Some(parent) = target_parent
                .as_ref()
                .and_then(|parent| store.paths().lookup(parent))
            else {
                continue;
            };
            let renames = target_parent == source_parent;
            if paths.contains(&parent) && (!renames || relocations.is_reached(target)) {
                added.push(target);
            }
        }
        if added.is_empty() {
            return;
        }
        paths.extend(added);
    }
}

fn gather_populated_paths(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    relocations: &mut Relocations,
) -> BTreeSet<PathId> {
    // Population reads no variant fallbacks: a branch whose set has no
    // authored selection counts as selected, so population
    // over-approximates, and composition prunes what it does not select.
    let fallbacks = &VariantFallbacks::default();
    // Keep this ordered set: deterministic iteration here helps keep derived
    // path interning stable across runs.
    let mut paths = BTreeSet::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        paths.extend(layer.prims.keys().copied());
    }

    // Expand using references and inherits (including descendants and nested arcs).
    //
    // Every expansion follows arcs through an `ArcChain` rooted at the prim
    // being expanded, so an arc that would close a cycle is not followed and
    // the recursion terminates.
    //
    // Spec: AOUSD Core §10 (composition arcs) and §11 (stage population).
    let stage_layer_stack = layer_stack_root(local_stack);
    let mut queue: Vec<PathId> = paths.iter().copied().collect();
    let mut idx = 0_usize;
    let mut visited_refs: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    let mut mapped_from = MappedFrom::new();
    let mut implied_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    while idx < queue.len() {
        let path = queue[idx];
        idx += 1;
        let mut chain = Chain::new(stage_layer_stack, path, relocations);
        // Discovery evaluates asset path expressions as composition does;
        // composition reports what they find.
        let expressions = chain.expression_scope();
        let anchor = ArcAnchor::new(stage_layer_stack, Some(&expressions));

        let inherits = resolve_inherits_for_prim(
            store,
            fallbacks,
            local_stack,
            path,
            SelectionScope::Discover,
        );
        for inherited_root in inherits {
            expand_inherit_paths(
                store,
                local_stack,
                path,
                inherited_root,
                &mut paths,
                &mut queue,
                &mut visited_inherits,
                &mut chain,
                &mut mapped_from,
            );
        }

        let refs = resolve_references_for_prim(
            store,
            fallbacks,
            local_stack,
            path,
            SelectionScope::Discover,
            anchor,
        );
        for reference in refs {
            expand_reference_paths(
                store,
                path,
                reference,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
                &mut chain,
                &mut mapped_from,
            );
        }

        // Also expand references from ALL variant branches of this prim's
        // parent, regardless of which variant is currently selected. This
        // ensures that paths introduced by variant-scoped child references
        // are discovered during population.
        let variant_refs = collect_all_variant_child_references(store, local_stack, path, anchor);
        for reference in variant_refs {
            expand_reference_paths(
                store,
                path,
                reference,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
                &mut chain,
                &mut mapped_from,
            );
        }

        // Expand references from variant branch headers of this prim itself.
        // E.g. `"full" (add references = @...@) {}` on the prim's variant set.
        let branch_refs =
            collect_all_variant_branch_references(store, fallbacks, local_stack, path, anchor);
        for reference in branch_refs {
            expand_reference_paths(
                store,
                path,
                reference,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
                &mut chain,
                &mut mapped_from,
            );
        }

        // Payloads behave like references for population purposes.
        // Spec: AOUSD Core §10 (payloads arc, §5.1.22).
        let payloads = resolve_payloads_for_prim(
            store,
            fallbacks,
            local_stack,
            path,
            SelectionScope::Discover,
            anchor,
        );
        for payload in payloads {
            expand_reference_paths(
                store,
                path,
                payload,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
                &mut chain,
                &mut mapped_from,
            );
        }

        // Expand payloads from variant branch headers (all branches).
        let branch_payloads =
            collect_all_variant_branch_payloads(store, fallbacks, local_stack, path, anchor);
        for payload in branch_payloads {
            expand_reference_paths(
                store,
                path,
                payload,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
                &mut chain,
                &mut mapped_from,
            );
        }

        // Specializes behaves like inherits for population purposes.
        // Spec: AOUSD Core §10 (specializes arc, §5.1.33).
        let specializes = resolve_specializes_for_prim(
            store,
            fallbacks,
            local_stack,
            path,
            SelectionScope::Discover,
        );
        for specialized_root in specializes {
            expand_inherit_paths(
                store,
                local_stack,
                path,
                specialized_root,
                &mut paths,
                &mut queue,
                &mut visited_inherits,
                &mut chain,
                &mut mapped_from,
            );
        }
        implied_inherits.extend(chain.implied.drain(..));
    }

    // Second pass: propagate reference-introduced paths through inherits.
    // After the main loop, some paths may have been introduced by references
    // under an inherit source but not yet mapped to the inherit destination.
    // The visited_inherits set contains all (dest, src) inherit/specializes
    // pairs discovered during population, and the classes they imply into
    // the stronger layer stacks, in the stage namespace.
    let inherit_pairs: Vec<(PathId, PathId)> = visited_inherits
        .into_iter()
        .chain(implied_inherits)
        .collect();
    propagate_populated_through_inherits(
        store,
        &inherit_pairs,
        &mut mapped_from,
        &mut paths,
        &mut queue,
    );

    // Process any newly added paths from inherit propagation.
    while idx < queue.len() {
        let path = queue[idx];
        idx += 1;
        let mut chain = Chain::new(stage_layer_stack, path, relocations);

        let inherits = resolve_inherits_for_prim(
            store,
            fallbacks,
            local_stack,
            path,
            SelectionScope::Discover,
        );
        for inherited_root in inherits {
            expand_inherit_paths(
                store,
                local_stack,
                path,
                inherited_root,
                &mut paths,
                &mut queue,
                &mut HashSet::new(),
                &mut chain,
                &mut mapped_from,
            );
        }
    }

    paths
}

fn expand_inherit_paths(
    store: &mut dyn LayerStore,
    stack: &LayerStack,
    dest_root: PathId,
    inherited_root: PathId,
    paths: &mut BTreeSet<PathId>,
    queue: &mut Vec<PathId>,
    visited: &mut HashSet<(PathId, PathId)>,
    chain: &mut Chain<'_>,
    mapped_from: &mut MappedFrom,
) {
    // No variant fallbacks, as in `gather_populated_paths`.
    let fallbacks = &VariantFallbacks::default();
    let layer_stack = layer_stack_root(stack);
    if chain.closes_cycle(store.paths(), dest_root, layer_stack, inherited_root) {
        return;
    }
    if !visited.insert((dest_root, inherited_root)) {
        return;
    }
    // The class is implied into each stronger layer stack on the chain, and
    // lastly into the stage's, where it is the stage prim its path maps to:
    // that prim's children are the implied classes' children.
    let stage_class = chain.arcs.stage_path(store.paths_mut(), inherited_root);
    if stage_class != inherited_root {
        chain.implied.push((dest_root, stage_class));
    }
    chain.push(store, stack, inherited_root, dest_root);
    let expressions = chain.expression_scope();
    let anchor = ArcAnchor::new(layer_stack, Some(&expressions));

    let src_root = store.paths().resolve(inherited_root).clone();

    let mut remote_paths: Vec<PathId> = stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| layer.prims.keys().copied())
        .collect();
    remote_paths.sort_by(|a, b| {
        store
            .paths()
            .resolve(*a)
            .cmp_with_tokens(store.paths().resolve(*b), store.tokens())
    });
    remote_paths.dedup();

    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&src_root) else {
                continue;
            };
            rel.to_vec()
        };

        let Some((dest_path_id, moved)) = chain.walk().place(store, dest_root, &rel) else {
            continue;
        };
        let new = paths.insert(dest_path_id);
        chain.relocations.place(dest_path_id, moved, new);
        if new {
            queue.push(dest_path_id);
            mapped_from.insert(dest_path_id, (remote_path_id, rel.len()));
        }

        let nested = resolve_inherits_for_prim(
            store,
            fallbacks,
            stack,
            remote_path_id,
            SelectionScope::Discover,
        );
        for nested_inherit in nested {
            expand_inherit_paths(
                store,
                stack,
                dest_path_id,
                nested_inherit,
                paths,
                queue,
                visited,
                chain,
                mapped_from,
            );
        }

        // The class's references and payloads, and through them the
        // ancestral arcs of their subroot targets, populate the class's
        // namespace in the destination as well.
        let mut nested_refs = resolve_references_for_prim(
            store,
            fallbacks,
            stack,
            remote_path_id,
            SelectionScope::Discover,
            anchor,
        );
        nested_refs.extend(resolve_payloads_for_prim(
            store,
            fallbacks,
            stack,
            remote_path_id,
            SelectionScope::Discover,
            anchor,
        ));
        for nested in nested_refs {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                &mut HashSet::new(),
                visited,
                chain,
                mapped_from,
            );
        }
    }
    expand_ancestral_paths(
        store,
        stack,
        dest_root,
        inherited_root,
        paths,
        queue,
        &mut HashSet::new(),
        visited,
        chain,
        mapped_from,
    );
    chain.pop();
}

fn expand_reference_paths(
    store: &mut dyn LayerStore,
    dest_root: PathId,
    reference: Reference,
    paths: &mut BTreeSet<PathId>,
    queue: &mut Vec<PathId>,
    visited: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut HashSet<(PathId, PathId)>,
    chain: &mut Chain<'_>,
    mapped_from: &mut MappedFrom,
) {
    // No variant fallbacks, as in `gather_populated_paths`.
    let fallbacks = &VariantFallbacks::default();
    let Some(reference_path) = reference.target_path(store) else {
        return;
    };
    if chain.closes_cycle(store.paths(), dest_root, reference.layer, reference_path) {
        return;
    }
    // Each occurrence of a site is a node of its own (AOUSD Core §10.4;
    // OpenUSD `_AddArc` in `pxr/usd/pcp/primIndex.cpp` adds expressed
    // arcs without skipping duplicate sites), but one discovers the same
    // paths as another unless relocations lifted along the chain move
    // them.
    if !chain.lifts_relocations() && !visited.insert((dest_root, reference.layer, reference_path)) {
        return;
    }
    let remote_stack = LayerStack::gather(store, reference.layer);
    chain.push(store, &remote_stack, reference_path, dest_root);
    let expressions = chain.expression_scope();
    let anchor = ArcAnchor::new(reference.layer, Some(&expressions));

    let target = store.paths().resolve(reference_path).clone();
    let base = store.paths().resolve(dest_root).clone();

    let mut remote_paths: Vec<PathId> = remote_stack
        .layers
        .iter()
        .filter_map(|id| store.layer(*id))
        .flat_map(|layer| layer.prims.keys().copied())
        .collect();
    remote_paths.sort_by(|a, b| {
        store
            .paths()
            .resolve(*a)
            .cmp_with_tokens(store.paths().resolve(*b), store.tokens())
    });
    remote_paths.dedup();

    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&target) else {
                continue;
            };
            rel.to_vec()
        };

        let Some((dest_path_id, moved)) = chain.walk().place(store, dest_root, &rel) else {
            continue;
        };
        let new = paths.insert(dest_path_id);
        chain.relocations.place(dest_path_id, moved, new);
        if new {
            queue.push(dest_path_id);
        }
        // Referenced content has specs of its own at this path.
        mapped_from.remove(&dest_path_id);

        let inherits = resolve_inherits_for_prim(
            store,
            fallbacks,
            &remote_stack,
            remote_path_id,
            SelectionScope::Discover,
        );
        for inherited_root in inherits {
            expand_inherit_paths(
                store,
                &remote_stack,
                dest_path_id,
                inherited_root,
                paths,
                queue,
                visited_inherits,
                chain,
                mapped_from,
            );
        }

        // Specializes authored inside referenced content populate like
        // inherits, as they do at the stage's own layer stack.
        //
        // Spec: AOUSD Core §10 (specializes arc), §11 (population).
        let specializes = resolve_specializes_for_prim(
            store,
            fallbacks,
            &remote_stack,
            remote_path_id,
            SelectionScope::Discover,
        );
        for specialized_root in specializes {
            expand_inherit_paths(
                store,
                &remote_stack,
                dest_path_id,
                specialized_root,
                paths,
                queue,
                visited_inherits,
                chain,
                mapped_from,
            );
        }

        let nested_refs = resolve_references_for_prim(
            store,
            fallbacks,
            &remote_stack,
            remote_path_id,
            SelectionScope::Discover,
            anchor,
        );
        for nested in nested_refs {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                visited,
                visited_inherits,
                chain,
                mapped_from,
            );
        }

        // Expand variant-scoped child references from ALL variant branches.
        let variant_refs =
            collect_all_variant_child_references(store, &remote_stack, remote_path_id, anchor);
        for nested in variant_refs {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                visited,
                visited_inherits,
                chain,
                mapped_from,
            );
        }

        // Expand variant branch-level references from ALL variant branches.
        let branch_refs = collect_all_variant_branch_references(
            store,
            fallbacks,
            &remote_stack,
            remote_path_id,
            anchor,
        );
        for nested in branch_refs {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                visited,
                visited_inherits,
                chain,
                mapped_from,
            );
        }

        // Expand variant branch-level payloads from ALL variant branches.
        let branch_payloads = collect_all_variant_branch_payloads(
            store,
            fallbacks,
            &remote_stack,
            remote_path_id,
            anchor,
        );
        for nested in branch_payloads {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                visited,
                visited_inherits,
                chain,
                mapped_from,
            );
        }

        // Expand direct payloads from the remote prim.
        let payloads = resolve_payloads_for_prim(
            store,
            fallbacks,
            &remote_stack,
            remote_path_id,
            SelectionScope::Discover,
            anchor,
        );
        for payload in payloads {
            expand_reference_paths(
                store,
                dest_path_id,
                payload,
                paths,
                queue,
                visited,
                visited_inherits,
                chain,
                mapped_from,
            );
        }
    }

    expand_ancestral_paths(
        store,
        &remote_stack,
        dest_root,
        reference_path,
        paths,
        queue,
        visited,
        visited_inherits,
        chain,
        mapped_from,
    );

    // Propagate inherits discovered by nested references through this
    // reference's layer stack. When a nested reference (e.g. prop.usd) discovers
    // an inherit (e.g. /_class_Prop), the inherit target may also have children
    // in the current reference's layers (e.g. set.usd's /_class_Prop). We need
    // to expand those here so the mapped paths get populated.
    //
    // Spec: AOUSD Core §10 (inherit propagation through references).
    let new_inherits: Vec<(PathId, PathId)> = visited_inherits
        .iter()
        .copied()
        .filter(|(dest, _src)| {
            // Only process inherits whose destination is under (or equal to)
            // our reference destination root.
            *dest == dest_root || {
                let dest_path = store.paths().resolve(*dest);
                base.is_prefix_of(dest_path)
            }
        })
        .collect();

    for (inherit_dest, inherit_src) in new_inherits {
        // Use a fresh visited set: the same (dest, src) pair may have been
        // expanded in a deeper reference stack but not yet in this one.
        expand_inherit_paths(
            store,
            &remote_stack,
            inherit_dest,
            inherit_src,
            paths,
            queue,
            &mut HashSet::new(),
            chain,
            mapped_from,
        );
    }
    chain.pop();

    // Note: `paths_mut()` borrows the store mutably, so we materialize any
    // `strip_prefix` results before interning to avoid borrow conflicts.
}

/// Expands the arcs authored on the namespace ancestors of `target`, a
/// subroot arc target in `stack`, into `dest_root`: an arc from the
/// ancestor `/T` to `/C` reaches the target `/T/B` as an arc to `/C/B`.
///
/// Spec: AOUSD Core §10.2 and §11; OpenUSD builds a subroot target's index
/// from its parent's (`_BuildInitialPrimIndexFromAncestor` in
/// `pxr/usd/pcp/primIndex.cpp`). Composition follows the same arcs (see
/// `AncestralArcs` in `compose.rs`).
fn expand_ancestral_paths(
    store: &mut dyn LayerStore,
    stack: &LayerStack,
    dest_root: PathId,
    target: PathId,
    paths: &mut BTreeSet<PathId>,
    queue: &mut Vec<PathId>,
    visited_refs: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut HashSet<(PathId, PathId)>,
    chain: &mut Chain<'_>,
    mapped_from: &mut MappedFrom,
) {
    expand_ancestral_paths_from(
        store,
        stack,
        dest_root,
        target,
        paths,
        queue,
        visited_refs,
        visited_inherits,
        chain,
        mapped_from,
        0,
    );
}

/// Expands the arcs of the ancestors of `target` past the `skip` nearest
/// ones, as [`expand_ancestral_paths`] does.
///
/// At the deepest relocation target of `stack` at or above the target, the
/// arcs of the ancestors above it give way to those of its relocation
/// source, extended towards the target, as composition expands them
/// beneath a relocate node (AOUSD Core §10.3.2.6; see `AncestralArcs` in
/// `compose.rs`).
fn expand_ancestral_paths_from(
    store: &mut dyn LayerStore,
    stack: &LayerStack,
    dest_root: PathId,
    target: PathId,
    paths: &mut BTreeSet<PathId>,
    queue: &mut Vec<PathId>,
    visited_refs: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut HashSet<(PathId, PathId)>,
    chain: &mut Chain<'_>,
    mapped_from: &mut MappedFrom,
    skip: usize,
) {
    // No variant fallbacks, as in `gather_populated_paths`.
    let fallbacks = &VariantFallbacks::default();
    let expressions = chain.expression_scope();
    let anchor = ArcAnchor::new(layer_stack_root(stack), Some(&expressions));
    let target_path = store.paths().resolve(target).clone();
    let mut ancestors = Vec::new();
    let mut cursor = target_path.parent();
    while let Some(path) = cursor {
        if path.depth() == 0 {
            break;
        }
        cursor = path.parent();
        ancestors.push(path);
    }
    ancestors.drain(..skip.min(ancestors.len()));
    let table = chain.relocations.table(store, stack);
    let relocated = if table.is_empty() {
        None
    } else {
        let found = |path: &Path| {
            let id = store.paths().lookup(path)?;
            Some((id, table.source_of(id)?))
        };
        let own = (skip == 0).then(|| found(&target_path)).flatten();
        own.map(|(at, source)| (0, at, source)).or_else(|| {
            ancestors
                .iter()
                .enumerate()
                .find_map(|(index, path)| found(path).map(|(at, source)| (index + 1, at, source)))
        })
    };
    if let Some((kept, _, _)) = relocated {
        ancestors.truncate(kept);
    }
    for ancestor_path in ancestors {
        let Some(ancestor) = store.paths().lookup(&ancestor_path) else {
            continue;
        };
        let rel = target_path
            .strip_prefix(&ancestor_path)
            .expect("an ancestor prefixes its descendant")
            .to_vec();
        let mapped = |store: &mut dyn LayerStore, path: PathId| {
            let joined = store.paths().resolve(path).join(&rel);
            store.paths_mut().intern(joined)
        };
        let scope = SelectionScope::Discover;
        let mut references =
            resolve_references_for_prim(store, fallbacks, stack, ancestor, scope, anchor);
        references.extend(collect_all_variant_child_references(
            store, stack, ancestor, anchor,
        ));
        references.extend(collect_all_variant_branch_references(
            store, fallbacks, stack, ancestor, anchor,
        ));
        references.extend(resolve_payloads_for_prim(
            store, fallbacks, stack, ancestor, scope, anchor,
        ));
        references.extend(collect_all_variant_branch_payloads(
            store, fallbacks, stack, ancestor, anchor,
        ));
        for reference in references {
            let Some(path) = reference.target_path(store) else {
                continue;
            };
            let reference = Reference {
                target: ReferenceTarget::Prim(mapped(store, path)),
                ..reference
            };
            expand_reference_paths(
                store,
                dest_root,
                reference,
                paths,
                queue,
                visited_refs,
                visited_inherits,
                chain,
                mapped_from,
            );
        }
        let mut classes = resolve_inherits_for_prim(store, fallbacks, stack, ancestor, scope);
        classes.extend(resolve_specializes_for_prim(
            store, fallbacks, stack, ancestor, scope,
        ));
        for class in classes {
            let class = mapped(store, class);
            expand_inherit_paths(
                store,
                stack,
                dest_root,
                class,
                paths,
                queue,
                visited_inherits,
                chain,
                mapped_from,
            );
        }
    }
    let Some((_, relocated_at, source)) = relocated else {
        return;
    };
    let rel = target_path
        .strip_prefix(store.paths().resolve(relocated_at))
        .expect("the relocation target is at or above the target")
        .to_vec();
    let joined = store.paths().resolve(source).join(&rel);
    let source_view = store.paths_mut().intern(joined);
    expand_ancestral_paths_from(
        store,
        stack,
        dest_root,
        source_view,
        paths,
        queue,
        visited_refs,
        visited_inherits,
        chain,
        mapped_from,
        rel.len(),
    );
}

/// The chain of arcs population follows from one prim, with the
/// relocations of each layer stack on it lifted into the stage namespace
/// (see [`Walk`]).
struct Chain<'r> {
    arcs: ArcChain,
    relocations: &'r mut Relocations,
    stage: Rc<LiftedSet>,
    /// The relocations lifted by each arc on the chain, outermost first.
    lifted: Vec<Option<Rc<LiftedSet>>>,
    /// The class arcs found on the chain whose class is implied into the
    /// stage's layer stack at another path: `(destination, stage path of
    /// the class)` (AOUSD Core §10.4.2.4).
    implied: Vec<(PathId, PathId)>,
}

impl<'r> Chain<'r> {
    fn new(layer_stack: LayerId, prim: PathId, relocations: &'r mut Relocations) -> Self {
        let stage = relocations.stage();
        Self {
            arcs: ArcChain::new(layer_stack, prim),
            relocations,
            stage,
            lifted: Vec::new(),
            implied: Vec::new(),
        }
    }

    fn closes_cycle(
        &self,
        paths: &PathInterner,
        dest: PathId,
        layer_stack: LayerId,
        target: PathId,
    ) -> bool {
        self.arcs.closes_cycle(paths, dest, layer_stack, target)
    }

    /// Follows an arc mapping `target` in `stack` onto the stage path
    /// `dest`, lifting the relocations of `stack` it reaches.
    fn push(
        &mut self,
        store: &mut dyn LayerStore,
        stack: &LayerStack,
        target: PathId,
        dest: PathId,
    ) {
        let layer_stack = layer_stack_root(stack);
        self.arcs.push(layer_stack, target, dest);
        let table = self.relocations.table(store, stack);
        let lifted = if table.is_empty() {
            None
        } else {
            let outer = self.walk().nested();
            let lifted = LiftedSet::lift(store, &table, layer_stack, target, dest, &outer);
            (!lifted.is_empty()).then(|| Rc::new(lifted))
        };
        if let Some(lifted) = &lifted {
            self.relocations.propose(lifted);
        }
        self.lifted.push(lifted);
    }

    /// Whether an arc on the chain lifts relocations, which move the paths
    /// the arcs beneath it map.
    fn lifts_relocations(&self) -> bool {
        self.lifted.iter().any(Option::is_some)
    }

    /// The scope the arcs of the layer stack last pushed evaluate their
    /// asset path expressions in (see [`ArcChain::expression_stacks`]).
    fn expression_scope(&self) -> ExpressionScope {
        ExpressionScope::new(self.arcs.expression_stacks())
    }

    fn pop(&mut self) {
        self.arcs.pop();
        self.lifted.pop();
    }

    /// The walk of the arc last pushed: the relocations lifted before it
    /// are outer, its own are `own`.
    fn walk(&self) -> Walk<'_> {
        let (own, outer) = match self.lifted.split_last() {
            Some((own, outer)) => (own.as_deref(), outer),
            None => (None, &[][..]),
        };
        Walk::new(
            core::iter::once(&*self.stage).chain(outer.iter().filter_map(|set| set.as_deref())),
            own,
        )
    }
}

/// After the main queue loop, some paths introduced by references may exist
/// under an inherit source (e.g. `/Model/Class/RefFromHighClassStuff`) but
/// not yet be mapped to the inherit destination (e.g. `/Model/Scope/RefFromHighClassStuff`).
///
/// This function takes a set of (destination, source) inherit/specializes
/// pairs collected during population and propagates populated paths through them.
///
/// Mapping stops where it would close an arc cycle (see
/// [`propagation_closes_cycle`]); otherwise inherits that feed each other
/// (`/A/B` inherits `/C` while `/C/D` inherits `/A`) would map paths into
/// each other forever. Every path this adds is recorded in `mapped_from`.
///
/// Spec: AOUSD Core §10.3.2.3 (inherits), §10.6 (composition errors).
fn propagate_populated_through_inherits(
    store: &mut dyn LayerStore,
    inherit_pairs: &[(PathId, PathId)],
    mapped_from: &mut MappedFrom,
    paths: &mut BTreeSet<PathId>,
    queue: &mut Vec<PathId>,
) {
    let mut changed = true;
    while changed {
        changed = false;
        let snapshot: Vec<PathId> = paths.iter().copied().collect();
        for (dest, src) in inherit_pairs {
            let dest_root = store.paths().resolve(*dest).clone();
            let src_root = store.paths().resolve(*src).clone();
            let mut to_add = Vec::new();
            for populated in &snapshot {
                let rel: Vec<_> = {
                    let pop_path = store.paths().resolve(*populated);
                    let Some(rel) = pop_path.strip_prefix(&src_root) else {
                        continue;
                    };
                    if rel.is_empty() {
                        continue;
                    }
                    rel.to_vec()
                };
                let dest_path = dest_root.join(&rel);
                if propagation_closes_cycle(
                    store.paths(),
                    mapped_from,
                    &dest_path,
                    *populated,
                    rel.len(),
                ) {
                    continue;
                }
                let dest_id = store.paths_mut().intern(dest_path);
                if !paths.contains(&dest_id) {
                    to_add.push((dest_id, *populated, rel.len()));
                }
            }
            for (id, from, rel_len) in to_add {
                if paths.insert(id) {
                    queue.push(id);
                    mapped_from.insert(id, (from, rel_len));
                    changed = true;
                }
            }
        }
    }
}

/// Maps a path populated through an inherit or specializes arc to the path
/// it was mapped from and the length of the relative path the mapping
/// carried: `(source/rel, rel.len())` for a path `dest/rel`.
type MappedFrom = HashMap<PathId, (PathId, usize)>;

/// Returns `true` when mapping `from` to `candidate` (both extended by the
/// same `rel_len` trailing segments) would close an arc cycle.
///
/// `from` may itself have been mapped from another path, and so on. Each
/// step of that chain is one inherit arc from `dest` to `source`, recovered
/// by trimming the step's relative path. The arc closes a cycle when
/// `source` and `candidate`, trimmed to the same depth, are prefix-related:
/// this is the rule of [`ArcChain`], applied to the chain of mappings, with
/// `candidate` as the prim being composed. An arc repeated on the chain is a
/// cycle as well, which also bounds the walk.
fn propagation_closes_cycle(
    paths: &PathInterner,
    mapped_from: &MappedFrom,
    candidate: &Path,
    from: PathId,
    rel_len: usize,
) -> bool {
    let trim = |path: &Path, rel_len: usize| -> Option<Path> {
        let segments = path.segments();
        let keep = segments.len().checked_sub(rel_len)?;
        Some(Path::root().join(&segments[..keep]))
    };
    let mut arcs: Vec<(Path, Path)> = Vec::new();
    let mut site = candidate.clone();
    let (mut next, mut rel_len) = (from, rel_len);
    loop {
        let (Some(dest), Some(source), Some(at_depth)) = (
            trim(&site, rel_len),
            trim(paths.resolve(next), rel_len),
            trim(candidate, rel_len),
        ) else {
            return false;
        };
        if source.is_prefix_of(&at_depth) || at_depth.is_prefix_of(&source) {
            return true;
        }
        let arc = (dest, source);
        if arcs.contains(&arc) {
            return true;
        }
        arcs.push(arc);
        let Some(&(previous, previous_rel_len)) = mapped_from.get(&next) else {
            return false;
        };
        site = paths.resolve(next).clone();
        (next, rel_len) = (previous, previous_rel_len);
    }
}

/// Returns the root layer that identifies `stack` for arc cycle detection.
fn layer_stack_root(stack: &LayerStack) -> LayerId {
    stack
        .layers
        .first()
        .copied()
        .expect("a gathered layer stack contains its root layer")
}

fn add_ancestor_paths(store: &mut dyn LayerStore, paths: &mut BTreeSet<PathId>) {
    let mut extra = Vec::new();
    for path_id in paths.iter().copied() {
        let mut current = store.paths().resolve(path_id).clone();
        while let Some(parent) = current.parent() {
            let parent_id = store.paths_mut().intern(parent.clone());
            extra.push(parent_id);
            current = parent;
        }
    }
    paths.extend(extra);
}

fn apply_population_mask(
    store: &mut dyn LayerStore,
    paths: &mut BTreeSet<PathId>,
    mask: Option<&PopulationMask>,
) {
    let Some(mask) = mask else {
        return;
    };

    let mut allowed = HashSet::new();
    for include in &mask.include {
        let mut current = store.paths().resolve(*include).clone();
        let include_id = store.paths_mut().intern(current.clone());
        allowed.insert(include_id);
        while let Some(parent) = current.parent() {
            let parent_id = store.paths_mut().intern(parent.clone());
            allowed.insert(parent_id);
            current = parent;
        }
    }

    allowed.insert(store.paths_mut().intern(Path::root()));
    paths.retain(|p| allowed.contains(p));
}

fn build_children_index(
    store: &mut dyn LayerStore,
    prim_paths: impl IntoIterator<Item = PathId>,
) -> HashMap<PathId, Vec<PathId>> {
    let mut children: HashMap<PathId, Vec<PathId>> = HashMap::new();

    let prims: Vec<PathId> = prim_paths.into_iter().collect();
    let prim_set: HashSet<PathId> = prims.iter().copied().collect();

    for path_id in prims {
        let Some(parent) = store.paths().resolve(path_id).parent() else {
            continue;
        };
        let parent_id = store.paths_mut().intern(parent);
        if prim_set.contains(&parent_id) {
            children.entry(parent_id).or_default().push(path_id);
        }
    }

    for list in children.values_mut() {
        // Use token-string ordering (not `TokenId` ordering) for AOUSD-aligned
        // namespace ordering.
        //
        // Spec: AOUSD Core §8 (paths and namespace ordering).
        list.sort_by(|a, b| {
            store
                .paths()
                .resolve(*a)
                .cmp_with_tokens(store.paths().resolve(*b), store.tokens())
        });
    }

    children
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{InMemoryStore, Layer, PrimSpec};
    use alloc::{string::String, vec};

    /// Populates the stage rooted at `root` and returns its prim paths,
    /// sorted.
    fn populated(store: &mut InMemoryStore, root: LayerId) -> Vec<String> {
        let stack = LayerStack::gather(store, root);
        let mut relocations = Relocations::new(store, &stack);
        let (paths, _) = populate(store, &stack, None, &mut relocations);
        let mut names: Vec<String> = paths
            .into_iter()
            .map(|id| store.paths.display(id, &store.tokens))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn inheriting_an_ancestor_terminates() {
        // `/A/B` inherits `/A`: mapping `/A`'s namespace onto `/A/B` would
        // yield `/A/B/B`, `/A/B/B/B`, ... The arc is a cycle and is not
        // followed.
        let mut store = InMemoryStore::default();
        let a = store.path("/A");
        let b = store.path("/A/B");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(a, PrimSpec::def());
        layer.insert_prim(b, PrimSpec::over().with_inherit(a));
        store.insert_layer(layer);

        assert_eq!(populated(&mut store, LayerId(1)), vec!["/", "/A", "/A/B"]);
    }

    #[test]
    fn co_recursive_inherits_terminate() {
        // `/P1/C1` inherits `/P2` and `/P2/C2` inherits `/P1`, so each maps
        // the other's child under its own. OpenUSD composes `/P1/C1/C2` and
        // `/P2/C2/C1` and rejects the arcs that would go further
        // (`ErrorArcCycle_root`, `CoRecursiveParent*`).
        let mut store = InMemoryStore::default();
        let p1 = store.path("/P1");
        let c1 = store.path("/P1/C1");
        let p2 = store.path("/P2");
        let c2 = store.path("/P2/C2");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(p1, PrimSpec::def());
        layer.insert_prim(c1, PrimSpec::over().with_inherit(p2));
        layer.insert_prim(p2, PrimSpec::def());
        layer.insert_prim(c2, PrimSpec::over().with_inherit(p1));
        store.insert_layer(layer);

        assert_eq!(
            populated(&mut store, LayerId(1)),
            vec![
                "/",
                "/P1",
                "/P1/C1",
                "/P1/C1/C2",
                "/P2",
                "/P2/C2",
                "/P2/C2/C1"
            ]
        );
    }

    #[test]
    fn referencing_an_ancestor_terminates() {
        // `/P/C` references `/M` in another layer, which references `/P`
        // back: mapped onto `/P/C`, that would nest `/P/C/C/...` without end.
        let mut store = InMemoryStore::default();
        let p = store.path("/P");
        let c = store.path("/P/C");
        let m = store.path("/M");
        let mut root = Layer::new(LayerId(1));
        root.insert_prim(p, PrimSpec::def());
        root.insert_prim(
            c,
            PrimSpec::over().with_reference(Reference::new(LayerId(2), m)),
        );
        store.insert_layer(root);
        let mut model = Layer::new(LayerId(2));
        model.insert_prim(
            m,
            PrimSpec::def().with_reference(Reference::new(LayerId(1), p)),
        );
        store.insert_layer(model);

        assert_eq!(populated(&mut store, LayerId(1)), vec!["/", "/P", "/P/C"]);
    }

    #[test]
    fn mutual_references_terminate() {
        // `/R` references `/A`, which references `/B`, which references
        // `/A` again (`ErrorArcCycle_root`, `GroupRoot`).
        let mut store = InMemoryStore::default();
        let r = store.path("/R");
        let a = store.path("/A");
        let a_child = store.path("/A/Child");
        let b = store.path("/B");
        let mut root = Layer::new(LayerId(1));
        root.insert_prim(
            r,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), a)),
        );
        store.insert_layer(root);
        let mut layer_a = Layer::new(LayerId(2));
        layer_a.insert_prim(
            a,
            PrimSpec::def().with_reference(Reference::new(LayerId(3), b)),
        );
        layer_a.insert_prim(a_child, PrimSpec::def());
        store.insert_layer(layer_a);
        let mut layer_b = Layer::new(LayerId(3));
        layer_b.insert_prim(
            b,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), a)),
        );
        store.insert_layer(layer_b);

        assert_eq!(
            populated(&mut store, LayerId(1)),
            vec!["/", "/R", "/R/Child"]
        );
    }
}
