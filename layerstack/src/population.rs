// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage population.
//!
//! Population determines which prim paths exist in the composed stage and builds
//! a deterministic parent→children index for traversal.
//!
//! Spec: AOUSD Core §11 (stage population).

use alloc::{collections::BTreeSet, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    arcs::{
        collect_all_variant_branch_payloads, collect_all_variant_branch_references,
        collect_all_variant_child_references, resolve_inherits_for_prim, resolve_payloads_for_prim,
        resolve_reference_target_path, resolve_references_for_prim, resolve_specializes_for_prim,
    },
    doc::LayerStore,
    doc::{LayerId, Reference},
    layer_stack::LayerStack,
    path::{Path, PathId},
    stage::PopulationMask,
};

/// Produces the set of populated prim paths and a parent→children index.
pub(crate) fn populate(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    mask: Option<&PopulationMask>,
) -> (BTreeSet<PathId>, HashMap<PathId, Vec<PathId>>) {
    let mut paths = gather_populated_paths(store, local_stack);
    add_ancestor_paths(store, &mut paths);
    apply_population_mask(store, &mut paths, mask);
    let children = build_children_index(store, paths.iter().copied());
    (paths, children)
}

fn gather_populated_paths(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
) -> BTreeSet<PathId> {
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
    // Spec: AOUSD Core §10 (composition arcs) and §11 (stage population).
    let mut queue: Vec<PathId> = paths.iter().copied().collect();
    let mut idx = 0_usize;
    let mut visited_refs: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    while idx < queue.len() {
        let path = queue[idx];
        idx += 1;

        let inherits = resolve_inherits_for_prim(store, local_stack, path);
        for inherited_root in inherits {
            expand_inherit_paths(
                store,
                local_stack,
                path,
                inherited_root,
                &mut paths,
                &mut queue,
                &mut visited_inherits,
            );
        }

        let refs = resolve_references_for_prim(store, local_stack, path);
        for reference in refs {
            expand_reference_paths(
                store,
                path,
                reference,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
            );
        }

        // Also expand references from ALL variant branches of this prim's
        // parent, regardless of which variant is currently selected. This
        // ensures that paths introduced by variant-scoped child references
        // are discovered during population.
        let variant_refs = collect_all_variant_child_references(store, local_stack, path);
        for reference in variant_refs {
            expand_reference_paths(
                store,
                path,
                reference,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
            );
        }

        // Expand references from variant branch headers of this prim itself.
        // E.g. `"full" (add references = @...@) {}` on the prim's variant set.
        let branch_refs = collect_all_variant_branch_references(store, local_stack, path);
        for reference in branch_refs {
            expand_reference_paths(
                store,
                path,
                reference,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
            );
        }

        // Payloads behave like references for population purposes.
        // Spec: AOUSD Core §10 (payloads arc, §5.1.22).
        let payloads = resolve_payloads_for_prim(store, local_stack, path);
        for payload in payloads {
            expand_reference_paths(
                store,
                path,
                payload,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
            );
        }

        // Expand payloads from variant branch headers (all branches).
        let branch_payloads = collect_all_variant_branch_payloads(store, local_stack, path);
        for payload in branch_payloads {
            expand_reference_paths(
                store,
                path,
                payload,
                &mut paths,
                &mut queue,
                &mut visited_refs,
                &mut visited_inherits,
            );
        }

        // Specializes behaves like inherits for population purposes.
        // Spec: AOUSD Core §10 (specializes arc, §5.1.33).
        let specializes = resolve_specializes_for_prim(store, local_stack, path);
        for specialized_root in specializes {
            expand_inherit_paths(
                store,
                local_stack,
                path,
                specialized_root,
                &mut paths,
                &mut queue,
                &mut visited_inherits,
            );
        }
    }

    // Second pass: propagate reference-introduced paths through inherits.
    // After the main loop, some paths may have been introduced by references
    // under an inherit source but not yet mapped to the inherit destination.
    // The visited_inherits set contains all (dest, src) inherit/specializes
    // pairs discovered during population.
    let inherit_pairs: Vec<(PathId, PathId)> = visited_inherits.into_iter().collect();
    propagate_populated_through_inherits(store, &inherit_pairs, &mut paths, &mut queue);

    // Process any newly added paths from inherit propagation.
    while idx < queue.len() {
        let path = queue[idx];
        idx += 1;

        let inherits = resolve_inherits_for_prim(store, local_stack, path);
        for inherited_root in inherits {
            expand_inherit_paths(
                store,
                local_stack,
                path,
                inherited_root,
                &mut paths,
                &mut queue,
                &mut HashSet::new(),
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
) {
    if !visited.insert((dest_root, inherited_root)) {
        return;
    }

    let src_root = store.paths().resolve(inherited_root).clone();
    let dest_root_path = store.paths().resolve(dest_root).clone();

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

        let dest_path_id = store.paths_mut().intern(dest_root_path.join(&rel));
        if paths.insert(dest_path_id) {
            queue.push(dest_path_id);
        }

        let nested = resolve_inherits_for_prim(store, stack, remote_path_id);
        for nested_inherit in nested {
            expand_inherit_paths(
                store,
                stack,
                dest_path_id,
                nested_inherit,
                paths,
                queue,
                visited,
            );
        }
    }
}

fn expand_reference_paths(
    store: &mut dyn LayerStore,
    dest_root: PathId,
    reference: Reference,
    paths: &mut BTreeSet<PathId>,
    queue: &mut Vec<PathId>,
    visited: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut HashSet<(PathId, PathId)>,
) {
    let Some(reference_path) = resolve_reference_target_path(store, &reference) else {
        return;
    };
    if !visited.insert((dest_root, reference.layer, reference_path)) {
        return;
    }

    let remote_stack = LayerStack::gather(store, reference.layer);
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

        let dest_path_id = store.paths_mut().intern(base.join(&rel));
        if paths.insert(dest_path_id) {
            queue.push(dest_path_id);
        }

        let inherits = resolve_inherits_for_prim(store, &remote_stack, remote_path_id);
        for inherited_root in inherits {
            expand_inherit_paths(
                store,
                &remote_stack,
                dest_path_id,
                inherited_root,
                paths,
                queue,
                visited_inherits,
            );
        }

        let nested_refs = resolve_references_for_prim(store, &remote_stack, remote_path_id);
        for nested in nested_refs {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                visited,
                visited_inherits,
            );
        }

        // Expand variant-scoped child references from ALL variant branches.
        let variant_refs =
            collect_all_variant_child_references(store, &remote_stack, remote_path_id);
        for nested in variant_refs {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                visited,
                visited_inherits,
            );
        }

        // Expand variant branch-level references from ALL variant branches.
        let branch_refs =
            collect_all_variant_branch_references(store, &remote_stack, remote_path_id);
        for nested in branch_refs {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                visited,
                visited_inherits,
            );
        }

        // Expand variant branch-level payloads from ALL variant branches.
        let branch_payloads =
            collect_all_variant_branch_payloads(store, &remote_stack, remote_path_id);
        for nested in branch_payloads {
            expand_reference_paths(
                store,
                dest_path_id,
                nested,
                paths,
                queue,
                visited,
                visited_inherits,
            );
        }

        // Expand direct payloads from the remote prim.
        let payloads = resolve_payloads_for_prim(store, &remote_stack, remote_path_id);
        for payload in payloads {
            expand_reference_paths(
                store,
                dest_path_id,
                payload,
                paths,
                queue,
                visited,
                visited_inherits,
            );
        }
    }

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
        );
    }

    // Note: `paths_mut()` borrows the store mutably, so we materialize any
    // `strip_prefix` results before interning to avoid borrow conflicts.
}

/// After the main queue loop, some paths introduced by references may exist
/// under an inherit source (e.g. `/Model/Class/RefFromHighClassStuff`) but
/// not yet be mapped to the inherit destination (e.g. `/Model/Scope/RefFromHighClassStuff`).
///
/// This function takes a set of (destination, source) inherit/specializes
/// pairs collected during population and propagates populated paths through them.
fn propagate_populated_through_inherits(
    store: &mut dyn LayerStore,
    inherit_pairs: &[(PathId, PathId)],
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
                let dest_id = store.paths_mut().intern(dest_path);
                if !paths.contains(&dest_id) {
                    to_add.push(dest_id);
                }
            }
            for id in to_add {
                paths.insert(id);
                queue.push(id);
                changed = true;
            }
        }
    }
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
