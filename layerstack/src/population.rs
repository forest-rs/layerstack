//! Stage population.
//!
//! Population determines which prim paths exist in the composed stage and builds
//! a deterministic parent→children index for traversal.
//!
//! Spec: AOUSD Core §11 (stage population).

use alloc::{collections::BTreeSet, vec::Vec};

use hashbrown::{HashMap, HashSet};

use crate::{
    arcs::resolve_inherits_for_prim,
    arcs::resolve_references_for_prim,
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
    if !visited.insert((dest_root, reference.layer, reference.prim_path)) {
        return;
    }

    let remote_stack = LayerStack::gather(store, reference.layer);
    let target = store.paths().resolve(reference.prim_path).clone();
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
    }

    // Note: `paths_mut()` borrows the store mutably, so we materialize any
    // `strip_prefix` results before interning to avoid borrow conflicts.
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
