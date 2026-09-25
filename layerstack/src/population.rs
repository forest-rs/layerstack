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
    arc_cycle::ArcChain,
    arcs::{
        SelectionScope, collect_all_variant_branch_payloads, collect_all_variant_branch_references,
        collect_all_variant_child_references, resolve_inherits_for_prim, resolve_payloads_for_prim,
        resolve_references_for_prim, resolve_specializes_for_prim,
    },
    doc::LayerStore,
    doc::{LayerId, Reference},
    layer_stack::LayerStack,
    path::{Path, PathId, PathInterner},
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
    while idx < queue.len() {
        let path = queue[idx];
        idx += 1;
        let mut chain = ArcChain::new(stage_layer_stack, path);

        let inherits =
            resolve_inherits_for_prim(store, local_stack, path, SelectionScope::Discover);
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

        let refs = resolve_references_for_prim(store, local_stack, path, SelectionScope::Discover);
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
                &mut chain,
                &mut mapped_from,
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
                &mut chain,
                &mut mapped_from,
            );
        }

        // Payloads behave like references for population purposes.
        // Spec: AOUSD Core §10 (payloads arc, §5.1.22).
        let payloads =
            resolve_payloads_for_prim(store, local_stack, path, SelectionScope::Discover);
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
                &mut chain,
                &mut mapped_from,
            );
        }

        // Specializes behaves like inherits for population purposes.
        // Spec: AOUSD Core §10 (specializes arc, §5.1.33).
        let specializes =
            resolve_specializes_for_prim(store, local_stack, path, SelectionScope::Discover);
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
    }

    // Second pass: propagate reference-introduced paths through inherits.
    // After the main loop, some paths may have been introduced by references
    // under an inherit source but not yet mapped to the inherit destination.
    // The visited_inherits set contains all (dest, src) inherit/specializes
    // pairs discovered during population.
    let inherit_pairs: Vec<(PathId, PathId)> = visited_inherits.into_iter().collect();
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
        let mut chain = ArcChain::new(stage_layer_stack, path);

        let inherits =
            resolve_inherits_for_prim(store, local_stack, path, SelectionScope::Discover);
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
    chain: &mut ArcChain,
    mapped_from: &mut MappedFrom,
) {
    let layer_stack = layer_stack_root(stack);
    if chain.closes_cycle(store.paths(), dest_root, layer_stack, inherited_root) {
        return;
    }
    if !visited.insert((dest_root, inherited_root)) {
        return;
    }
    chain.push(layer_stack, inherited_root, dest_root);

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
            mapped_from.insert(dest_path_id, (remote_path_id, rel.len()));
        }

        let nested =
            resolve_inherits_for_prim(store, stack, remote_path_id, SelectionScope::Discover);
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
    }
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
    chain: &mut ArcChain,
    mapped_from: &mut MappedFrom,
) {
    let Some(reference_path) = reference.target_path(store) else {
        return;
    };
    if chain.closes_cycle(store.paths(), dest_root, reference.layer, reference_path) {
        return;
    }
    if !visited.insert((dest_root, reference.layer, reference_path)) {
        return;
    }
    chain.push(reference.layer, reference_path, dest_root);

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
        // Referenced content has specs of its own at this path.
        mapped_from.remove(&dest_path_id);

        let inherits = resolve_inherits_for_prim(
            store,
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
            &remote_stack,
            remote_path_id,
            SelectionScope::Discover,
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
                chain,
                mapped_from,
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
                chain,
                mapped_from,
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
                chain,
                mapped_from,
            );
        }

        // Expand direct payloads from the remote prim.
        let payloads = resolve_payloads_for_prim(
            store,
            &remote_stack,
            remote_path_id,
            SelectionScope::Discover,
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
        let (paths, _) = populate(store, &stack, None);
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
