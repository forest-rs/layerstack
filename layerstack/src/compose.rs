// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition entry points.
//!
//! This module is responsible for producing composition results (`PrimIndex`es)
//! which are then wrapped by [`crate::stage::Stage`].
//!
//! Spec: AOUSD Core §9–§12 (layer stacks, arcs/strength ordering, population, and resolution).

use alloc::{collections::BTreeSet, vec::Vec};

use core::cmp::Ordering;

use hashbrown::{HashMap, HashSet};

use crate::{
    arcs::{
        resolve_direct_references_for_prim, resolve_inherits_for_prim, resolve_payloads_for_prim,
        resolve_reference_target_path, resolve_references_for_prim, resolve_specializes_for_prim,
        resolve_variant_branch_payloads, resolve_variant_branch_references,
        resolve_variant_child_references, resolve_variant_selections_for_prim,
    },
    dependency_map::{ArcDependency, DependencyBuilder},
    doc::{FieldValue, LayerId, LayerOffset, LayerStore, Reference},
    interner::TokenId,
    layer_stack::LayerStack,
    path::{PathId, PropertyPath, TargetPath},
    population::populate,
    prim_index::{ArcKind, Opinion, OpinionKey, PrimIndex},
    property::PropertyType,
    spec_path::{SpecPath, VariantSelectionSite},
    stage::{Stage, StageOptions},
};

fn prim_spec_path(
    store: &dyn LayerStore,
    prim_path: PathId,
    outer_variant_sites: &[VariantSelectionSite],
) -> SpecPath {
    if outer_variant_sites.is_empty() {
        SpecPath::from_prim_path(prim_path, store.paths())
    } else {
        SpecPath::from_variant_selection_sites(prim_path, outer_variant_sites, store.paths())
    }
}

fn property_spec_path(
    store: &dyn LayerStore,
    prim_path: PathId,
    outer_variant_sites: &[VariantSelectionSite],
    property: TokenId,
) -> SpecPath {
    prim_spec_path(store, prim_path, outer_variant_sites).with_property(property)
}

fn variant_spec_path(
    store: &dyn LayerStore,
    prim_path: PathId,
    selection_sites: &[VariantSelectionSite],
) -> SpecPath {
    SpecPath::from_variant_selection_sites(prim_path, selection_sites, store.paths())
}

fn variant_property_spec_path(
    store: &dyn LayerStore,
    prim_path: PathId,
    selection_sites: &[VariantSelectionSite],
    property: TokenId,
) -> SpecPath {
    variant_spec_path(store, prim_path, selection_sites).with_property(property)
}

fn combined_variant_sites(
    outer: &[VariantSelectionSite],
    current: VariantSelectionSite,
) -> Vec<VariantSelectionSite> {
    let mut out = outer.to_vec();
    out.push(current);
    out
}

fn normalize_forwarded_spec_path(
    store: &mut dyn LayerStore,
    spec_path: &SpecPath,
    provenance_remap: Option<(PathId, PathId)>,
    stronger_selections: &HashMap<TokenId, TokenId>,
) -> SpecPath {
    let mut out = if let Some((dest_root, src_root)) = provenance_remap {
        let dest_root = store.paths().resolve(dest_root).clone();
        let src_root = store.paths().resolve(src_root).clone();
        remap_spec_path(store, spec_path, &dest_root, &src_root)
    } else {
        spec_path.clone()
    };

    let mut selection_sites = spec_path_selection_sites(store, &out);
    if selection_sites.is_empty() {
        return out;
    }

    let mut changed = false;
    for site in &mut selection_sites {
        if let Some(selected) = stronger_selections.get(&site.set)
            && *selected != site.variant
        {
            site.variant = *selected;
            changed = true;
        }
    }

    if !changed {
        return out;
    }

    out = SpecPath::from_variant_selection_sites(out.prim_path(), &selection_sites, store.paths());
    if let Some(property) = spec_path.property() {
        out = out.with_property(property);
    }
    out
}

fn normalized_prim_spec_path(
    store: &mut dyn LayerStore,
    prim_path: PathId,
    outer_variant_sites: &[VariantSelectionSite],
    provenance_remap: Option<(PathId, PathId)>,
    stronger_selections: &HashMap<TokenId, TokenId>,
) -> SpecPath {
    let raw = prim_spec_path(store, prim_path, outer_variant_sites);
    normalize_forwarded_spec_path(store, &raw, provenance_remap, stronger_selections)
}

fn normalized_property_spec_path(
    store: &mut dyn LayerStore,
    prim_path: PathId,
    outer_variant_sites: &[VariantSelectionSite],
    property: TokenId,
    provenance_remap: Option<(PathId, PathId)>,
    stronger_selections: &HashMap<TokenId, TokenId>,
) -> SpecPath {
    let raw = property_spec_path(store, prim_path, outer_variant_sites, property);
    normalize_forwarded_spec_path(store, &raw, provenance_remap, stronger_selections)
}

fn normalized_variant_spec_path(
    store: &mut dyn LayerStore,
    prim_path: PathId,
    selection_sites: &[VariantSelectionSite],
    provenance_remap: Option<(PathId, PathId)>,
    stronger_selections: &HashMap<TokenId, TokenId>,
) -> SpecPath {
    let raw = variant_spec_path(store, prim_path, selection_sites);
    normalize_forwarded_spec_path(store, &raw, provenance_remap, stronger_selections)
}

fn normalized_variant_property_spec_path(
    store: &mut dyn LayerStore,
    prim_path: PathId,
    selection_sites: &[VariantSelectionSite],
    property: TokenId,
    provenance_remap: Option<(PathId, PathId)>,
    stronger_selections: &HashMap<TokenId, TokenId>,
) -> SpecPath {
    let raw = variant_property_spec_path(store, prim_path, selection_sites, property);
    normalize_forwarded_spec_path(store, &raw, provenance_remap, stronger_selections)
}

/// Composes a stage from a root layer.
///
/// This implements:
/// - Layer stack gathering (layer is stronger than its sublayers)
/// - Stage population (including prims introduced via references)
/// - Value resolution (scalar + `ListOp`)
pub(crate) fn compose_stage(
    store: &mut dyn LayerStore,
    root: LayerId,
    options: StageOptions,
) -> Stage {
    let layer_stack = LayerStack::gather(store, root);
    let (paths, mut children) = populate(store, &layer_stack, options.mask.as_ref());

    let mut prims: HashMap<PathId, PrimIndex> = paths
        .iter()
        .copied()
        .map(|path| (path, PrimIndex::default()))
        .collect();

    let mut prim_order_opinions: HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>> = HashMap::new();
    let mut authored_children_opinions: HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>> =
        HashMap::new();

    let mut dep_builder = if options.with_dependencies {
        Some(DependencyBuilder::new())
    } else {
        None
    };

    add_local_and_variant_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        dep_builder.as_mut(),
    );
    add_inherit_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        dep_builder.as_mut(),
    );
    add_reference_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        dep_builder.as_mut(),
    );
    add_payload_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        dep_builder.as_mut(),
    );
    add_specializes_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
        dep_builder.as_mut(),
    );

    for prim in prims.values_mut() {
        prim.finalize();
    }

    apply_child_order(
        store,
        &authored_children_opinions,
        &prim_order_opinions,
        &mut children,
    );

    filter_variant_children(store, &prims, &mut children);

    strip_instance_descendants(
        store,
        &mut prims,
        &mut children,
        &authored_children_opinions,
    );

    prune_deactivated(store, &mut prims, &mut children);

    let dependencies = dep_builder.map(DependencyBuilder::finish);
    Stage::from_parts(prims, children, options.with_provenance, dependencies)
}

/// Filters children maps by removing variant-only children that don't belong
/// to the selected variant.
///
/// For each prim that has variant sets (found via its composed opinion sources),
/// the selected variants determine which variant children remain. Children that
/// exist only in non-selected variant branches are removed.
///
/// Spec: AOUSD Core §10.5 (variant selection), §11 (population).
fn filter_variant_children(
    store: &dyn LayerStore,
    prims: &HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    use hashbrown::HashSet;

    let parent_paths: Vec<PathId> = children.keys().copied().collect();
    for parent_path in parent_paths {
        let Some(prim_index) = prims.get(&parent_path) else {
            continue;
        };

        // Collect all variant set specs and variant selections across opinion sources.
        let mut all_variant_children: HashSet<TokenId> = HashSet::new();
        let mut selected_children: HashSet<TokenId> = HashSet::new();
        let mut has_variant_sets = false;
        let mut variant_set_order: Vec<TokenId> = Vec::new();

        // First, resolve variant selections and variant set order from all opinion sources.
        let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
        for source in &prim_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            for (set, variant) in &spec.variant_selections {
                selections.entry(*set).or_insert(*variant);
            }
            // Use the first non-empty variant_set_order we find.
            if variant_set_order.is_empty() && !spec.variant_set_order.is_empty() {
                variant_set_order = spec.variant_set_order.clone();
            }
        }

        // Expand selections from within selected variant branches (chaining).
        loop {
            let mut new_sels = HashMap::new();
            for source in &prim_index.sources {
                let Some(layer) = store.layer(source.layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(&source.lookup_path) else {
                    continue;
                };
                for (set, selected_variant) in &selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            if !selections.contains_key(inner_set) {
                                new_sels.entry(*inner_set).or_insert(*inner_variant);
                            }
                        }
                    }
                }
            }
            if new_sels.is_empty() {
                break;
            }
            selections.extend(new_sels);
        }

        // Then check each source for variant sets.
        for source in &prim_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };

            for (set_name, set_spec) in &spec.variant_sets {
                for (variant_name, variant_spec) in &set_spec.variants {
                    for child in &variant_spec.authored_children {
                        has_variant_sets = true;
                        all_variant_children.insert(*child);
                        if selections.get(set_name) == Some(variant_name) {
                            // Check outer variant requirements for nested children.
                            let outer_ok = variant_spec
                                .required_outer_variant_sites
                                .get(child)
                                .is_none_or(|reqs| {
                                    reqs.iter().all(|site| {
                                        selections.get(&site.set) == Some(&site.variant)
                                    })
                                });
                            if outer_ok {
                                selected_children.insert(*child);
                            }
                        }
                    }
                }
            }
        }

        if !has_variant_sets || all_variant_children.is_empty() {
            continue;
        }

        let unselected: HashSet<TokenId> = all_variant_children
            .difference(&selected_children)
            .copied()
            .collect();

        if unselected.is_empty() && variant_set_order.is_empty() {
            continue;
        }

        // Filter children list.
        if let Some(child_list) = children.get_mut(&parent_path) {
            child_list.retain(|child_path| {
                let child = store.paths().resolve(*child_path);
                if let Some(leaf) = child.leaf() {
                    !unselected.contains(&leaf)
                } else {
                    true
                }
            });

            // Re-order variant children: group by source arc (weakest first),
            // then by variant set order within each group (later sets first).
            // Within a variant set, children from deeper nesting levels
            // (more required_outer_variant_sites) come before shallower ones
            // because deeper variant opinions are stronger.
            use alloc::collections::BTreeMap;
            let mut arc_groups: BTreeMap<u16, Vec<TokenId>> = BTreeMap::new();

            // Collect nesting depth for each child across all sources.
            let mut child_nesting_depth: HashMap<TokenId, usize> = HashMap::new();
            for source in &prim_index.sources {
                let Some(layer) = store.layer(source.layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(&source.lookup_path) else {
                    continue;
                };
                for set_spec in spec.variant_sets.values() {
                    for variant_spec in set_spec.variants.values() {
                        for child in &variant_spec.authored_children {
                            let depth = variant_spec
                                .required_outer_variant_sites
                                .get(child)
                                .map_or(0, |r| r.len());
                            let entry = child_nesting_depth.entry(*child).or_insert(0);
                            *entry = (*entry).max(depth);
                        }
                    }
                }
            }

            for set_tok in variant_set_order.iter().rev() {
                let Some(&selected_variant) = selections.get(set_tok) else {
                    continue;
                };
                for source in &prim_index.sources {
                    let Some(layer) = store.layer(source.layer_id) else {
                        continue;
                    };
                    let Some(spec) = layer.prims.get(&source.lookup_path) else {
                        continue;
                    };
                    if let Some(set_spec) = spec.variant_sets.get(set_tok)
                        && let Some(variant_spec) = set_spec.variants.get(&selected_variant)
                    {
                        let group = arc_groups.entry(source.arc_list_index).or_default();
                        for child in &variant_spec.authored_children {
                            if !group.contains(child) {
                                group.push(*child);
                            }
                        }
                    }
                }
            }
            // Weakest arc first (highest arc_list_index first).
            let mut ordered_variant_children: Vec<TokenId> = Vec::new();
            for (_arc_idx, children) in arc_groups.iter().rev() {
                for child in children {
                    if !ordered_variant_children.contains(child) {
                        ordered_variant_children.push(*child);
                    }
                }
            }

            // Sort by nesting depth (deepest first = strongest opinions).
            // Children from deeper variant nesting have more required
            // outer selections and represent stronger opinions.
            ordered_variant_children.sort_by(|a, b| {
                let a_depth = child_nesting_depth.get(a).copied().unwrap_or(0);
                let b_depth = child_nesting_depth.get(b).copied().unwrap_or(0);
                b_depth.cmp(&a_depth)
            });

            if !ordered_variant_children.is_empty() {
                // Build a position map for stable sorting.
                let child_pos: HashMap<TokenId, usize> = ordered_variant_children
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (*c, i))
                    .collect();

                // Sort: variant children first (in authored order),
                // then non-variant children (preserving existing order).
                child_list.sort_by(|a, b| {
                    let a_leaf = store.paths().resolve(*a).leaf();
                    let b_leaf = store.paths().resolve(*b).leaf();
                    let a_pos = a_leaf.and_then(|l| child_pos.get(&l).copied());
                    let b_pos = b_leaf.and_then(|l| child_pos.get(&l).copied());
                    match (a_pos, b_pos) {
                        (None, None) => Ordering::Equal,
                        (None, Some(_)) => Ordering::Greater,
                        (Some(_), None) => Ordering::Less,
                        (Some(ai), Some(bi)) => ai.cmp(&bi),
                    }
                });
            }
        }
    }

    // Second pass: filter children based on parent's variant-scoped
    // child_authored_children. When a prim's parent has variant sets with
    // child_authored_children entries for this prim, grandchild prims that
    // are not in the selected variant branch should be removed.
    let parent_paths2: Vec<PathId> = children.keys().copied().collect();
    for parent_path in parent_paths2 {
        let parent_leaf = store.paths().resolve(parent_path).leaf();
        let grandparent = store.paths().resolve(parent_path).parent();
        let (Some(leaf), Some(gp)) = (parent_leaf, grandparent) else {
            continue;
        };
        let Some(gp_id) = store.paths().lookup(&gp) else {
            continue;
        };
        let Some(gp_index) = prims.get(&gp_id) else {
            continue;
        };

        // Resolve grandparent's variant selections (with chaining).
        let mut gp_selections: HashMap<TokenId, TokenId> = HashMap::new();
        for source in &gp_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            for (set, variant) in &spec.variant_selections {
                gp_selections.entry(*set).or_insert(*variant);
            }
        }
        // Chain through variant branches to discover transitive selections.
        loop {
            let mut new_sels = HashMap::new();
            for source in &gp_index.sources {
                let Some(layer) = store.layer(source.layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(&source.lookup_path) else {
                    continue;
                };
                for (set, selected_variant) in &gp_selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            if !gp_selections.contains_key(inner_set) {
                                new_sels.entry(*inner_set).or_insert(*inner_variant);
                            }
                        }
                    }
                }
            }
            if new_sels.is_empty() {
                break;
            }
            gp_selections.extend(new_sels);
        }

        // Collect all and selected grandchild authored children.
        let mut all_gc: HashSet<TokenId> = HashSet::new();
        let mut selected_gc: HashSet<TokenId> = HashSet::new();
        let mut has_gc = false;

        for source in &gp_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            for (set_name, set_spec) in &spec.variant_sets {
                for (variant_name, variant_spec) in &set_spec.variants {
                    if let Some(gc_list) = variant_spec.child_authored_children.get(&leaf) {
                        has_gc = true;
                        for gc in gc_list {
                            all_gc.insert(*gc);
                            if gp_selections.get(set_name) == Some(variant_name) {
                                selected_gc.insert(*gc);
                            }
                        }
                    }
                }
            }
        }

        if !has_gc {
            continue;
        }

        let unselected_gc: HashSet<TokenId> = all_gc.difference(&selected_gc).copied().collect();
        if unselected_gc.is_empty() {
            continue;
        }

        if let Some(child_list) = children.get_mut(&parent_path) {
            child_list.retain(|child_path| {
                let child = store.paths().resolve(*child_path);
                if let Some(child_leaf) = child.leaf() {
                    !unselected_gc.contains(&child_leaf)
                } else {
                    true
                }
            });

            // Re-order: variant-scoped grandchildren should come after
            // non-variant children (e.g. children from references).
            let (gc_children, other_children): (Vec<_>, Vec<_>) =
                child_list.iter().copied().partition(|child_path| {
                    let child = store.paths().resolve(*child_path);
                    child
                        .leaf()
                        .map(|l| selected_gc.contains(&l))
                        .unwrap_or(false)
                });
            *child_list = other_children;
            child_list.extend(gc_children);
        }
    }

    // Third pass: for prims that inherit or specialize from another prim,
    // remove any children that exist under the destination but were filtered
    // out from the source's children. This handles the case where variant
    // children of an inherited class are filtered at the class level but
    // still appear under the inheriting prim.
    let parent_paths3: Vec<PathId> = children.keys().copied().collect();
    for parent_path in parent_paths3 {
        let Some(prim_index) = prims.get(&parent_path) else {
            continue;
        };

        // Check all sources for inherit/specialize arcs by looking at the
        // prim's opinion sources for inherit-kind arcs.
        let mut inherited_sources: Vec<PathId> = Vec::new();
        for source in &prim_index.sources {
            if source.arc_kind == ArcKind::Inherits
                || source.arc_kind == ArcKind::Specializes
                || source.nested_arc_kind == Some(ArcKind::Inherits)
                || source.nested_arc_kind == Some(ArcKind::Specializes)
            {
                // The spec_path points to the source prim in its original namespace.
                // We need the mapped path in the same namespace as parent_path.
                // The source might be in a different namespace (e.g. /Model/Class
                // for /Model/Scope). We need the direct inherit source path.
                if source.lookup_path != parent_path {
                    inherited_sources.push(source.lookup_path);
                }
            }
        }

        if inherited_sources.is_empty() {
            continue;
        }

        // For each inherited source, check which of its children survived filtering.
        let mut to_remove: HashSet<TokenId> = HashSet::new();
        for src_path in &inherited_sources {
            let src_children = children.get(src_path);
            if let Some(src_child_list) = src_children {
                let src_leaves: HashSet<TokenId> = src_child_list
                    .iter()
                    .filter_map(|c| store.paths().resolve(*c).leaf())
                    .collect();

                // Any child of parent_path whose leaf matches a child that was
                // present at the source but got filtered out should be removed.
                if let Some(dest_child_list) = children.get(&parent_path) {
                    for child in dest_child_list {
                        let child_leaf = store.paths().resolve(*child).leaf();
                        if let Some(leaf) = child_leaf {
                            // Check if this child comes from inheritance by checking
                            // if the source prim originally had a path with this leaf
                            // as a child. If the source no longer has it (filtered),
                            // but the source's parent's variants had it, remove it.
                            let src_child_path = {
                                let sp = store.paths().resolve(*src_path).clone();
                                sp.join(&[leaf])
                            };
                            let src_child_id = store.paths().lookup(&src_child_path);
                            if let Some(sc_id) = src_child_id {
                                // Source namespace has this path, but it's not in
                                // source's filtered children → it was filtered out.
                                if !src_leaves.contains(&leaf) && prims.contains_key(&sc_id) {
                                    to_remove.insert(leaf);
                                }
                            }
                        }
                    }
                }
            }
        }

        if !to_remove.is_empty()
            && let Some(child_list) = children.get_mut(&parent_path)
        {
            child_list.retain(|child| {
                let leaf = store.paths().resolve(*child).leaf();
                leaf.map(|l| !to_remove.contains(&l)).unwrap_or(true)
            });
        }

        // Reorder: children from the prim's own arcs come before children
        // inherited from the source prim. Use the source's filtered child
        // list to identify which children are inherited. Only reorder when
        // variant filtering actually removed children — otherwise
        // `apply_authored_children_base_order` already established the
        // correct ordering.
        if to_remove.is_empty() {
            continue;
        }

        let inherited_leaves: HashSet<TokenId> = inherited_sources
            .iter()
            .filter_map(|src| children.get(src))
            .flat_map(|list| list.iter())
            .filter_map(|c| store.paths().resolve(*c).leaf())
            .collect();

        // Collect the source's child order for inherited children.
        let src_order: Vec<TokenId> = inherited_sources
            .iter()
            .filter_map(|src| children.get(src))
            .flat_map(|list| list.iter())
            .filter_map(|c| store.paths().resolve(*c).leaf())
            .collect();

        if !inherited_leaves.is_empty()
            && let Some(child_list) = children.get_mut(&parent_path)
        {
            // Only apply partition+reorder when there are children that
            // exist ONLY as direct children (not from inheritance). When
            // all children also exist in the inherited source, the normal
            // `apply_child_order` with `prim_order` opinions handles
            // ordering correctly.
            let has_direct_only = child_list.iter().any(|child| {
                let leaf = store.paths().resolve(*child).leaf();
                leaf.map(|l| !inherited_leaves.contains(&l))
                    .unwrap_or(false)
            });

            if has_direct_only {
                let (direct, mut inherited): (Vec<_>, Vec<_>) =
                    child_list.iter().copied().partition(|child| {
                        let leaf = store.paths().resolve(*child).leaf();
                        leaf.map(|l| !inherited_leaves.contains(&l)).unwrap_or(true)
                    });

                // Sort inherited children to match the source's child order.
                inherited.sort_by(|a, b| {
                    let a_leaf = store.paths().resolve(*a).leaf();
                    let b_leaf = store.paths().resolve(*b).leaf();
                    let a_pos = a_leaf.and_then(|l| src_order.iter().position(|s| *s == l));
                    let b_pos = b_leaf.and_then(|l| src_order.iter().position(|s| *s == l));
                    a_pos.cmp(&b_pos)
                });

                *child_list = direct;
                child_list.extend(inherited);
            }
        }
    }
}

/// Strips descendant opinions and children for effective instances.
///
/// A prim is an *effective instance* when it has `instanceable = true`
/// (strongest opinion) AND at least one composition arc (references,
/// payloads, inherits). For each effective instance:
///
/// 1. Children introduced only by identity (local/instanceable) sources are
///    removed along with their entire subtrees.
/// 2. On surviving descendants, local (`is_local == true`) sources whose
///    `spec_path` is a namespace descendant of an identity path are stripped.
///    Variant, inherit, and reference sources survive even when their
///    `spec_path` falls under an identity path.
///
/// Spec: AOUSD Core §11 (instancing), §5.1.14 (instanceable).
fn strip_instance_descendants(
    store: &dyn LayerStore,
    prims: &mut HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
    authored_children_opinions: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
) {
    use hashbrown::HashSet;

    // Step 1: Identify effective instances and their identity paths.
    //
    // An "identity path" is a (LayerId, PathId) pair representing the instance
    // prim's own definition. Sources that are local or whose PrimSpec has
    // `instanceable == Some(true)` are considered identity.
    let mut instance_identity: Vec<(PathId, Vec<(LayerId, PathId)>)> = Vec::new();

    let all_prim_paths: Vec<PathId> = prims.keys().copied().collect();
    for &prim_path in &all_prim_paths {
        let Some(index) = prims.get(&prim_path) else {
            continue;
        };

        // Resolve instanceable: strongest opinion wins.
        let mut is_instanceable = false;
        for source in &index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            if let Some(val) = spec.instanceable {
                is_instanceable = val;
                break; // strongest wins
            }
        }
        if !is_instanceable {
            continue;
        }

        // Check for composition arcs (references, payloads, inherits).
        let has_arcs = index.sources.iter().any(|s| {
            matches!(
                s.arc_kind,
                ArcKind::References | ArcKind::Payloads | ArcKind::Inherits
            ) && !s.is_local
        });
        if !has_arcs {
            continue;
        }

        // Collect identity paths: local sources and sources with instanceable=true.
        let mut identity_paths: Vec<(LayerId, PathId)> = Vec::new();
        for source in &index.sources {
            if source.is_local {
                identity_paths.push((source.layer_id, source.lookup_path));
                continue;
            }
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            if spec.instanceable == Some(true) {
                identity_paths.push((source.layer_id, source.lookup_path));
            }
        }

        if !identity_paths.is_empty() {
            instance_identity.push((prim_path, identity_paths));
        }
    }

    // Step 2: For each effective instance, determine surviving children and
    // strip descendant local opinions.
    for (instance_path, identity_paths) in &instance_identity {
        // 2a. Determine which children survive: a child survives if it appears
        // in any non-identity source's authored_children or variant children.
        let non_identity_children = collect_non_identity_children(
            store,
            *instance_path,
            identity_paths,
            prims,
            authored_children_opinions,
        );

        // Find all descendant prim paths.
        let descendants: Vec<PathId> = all_prim_paths
            .iter()
            .copied()
            .filter(|p| {
                *p != *instance_path
                    && store
                        .paths()
                        .resolve(*instance_path)
                        .is_prefix_of(store.paths().resolve(*p))
            })
            .collect();

        // Collect variant-introduced children from identity sources.
        // Descendants under variant-introduced children keep their local
        // sources because variants are their own arc (V in LIVERPS) and
        // should survive instancing stripping.
        let variant_children: HashSet<TokenId> = {
            let identity_set: HashSet<(LayerId, PathId)> = identity_paths.iter().copied().collect();
            let mut vc = HashSet::new();
            if let Some(index) = prims.get(instance_path) {
                let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
                for source in &index.sources {
                    let Some(layer) = store.layer(source.layer_id) else {
                        continue;
                    };
                    let Some(spec) = layer.prims.get(&source.lookup_path) else {
                        continue;
                    };
                    for (set, variant) in &spec.variant_selections {
                        selections.entry(*set).or_insert(*variant);
                    }
                }
                for source in &index.sources {
                    if !identity_set.contains(&(source.layer_id, source.lookup_path)) {
                        continue;
                    }
                    let Some(layer) = store.layer(source.layer_id) else {
                        continue;
                    };
                    let Some(spec) = layer.prims.get(&source.lookup_path) else {
                        continue;
                    };
                    for (set, set_spec) in &spec.variant_sets {
                        let Some(selected) = selections.get(set) else {
                            continue;
                        };
                        let Some(variant_spec) = set_spec.variants.get(selected) else {
                            continue;
                        };
                        for child in &variant_spec.authored_children {
                            vc.insert(*child);
                        }
                    }
                }
            }
            vc
        };

        let instance_resolved = store.paths().resolve(*instance_path).clone();

        // 2b. Strip local sources and opinions on surviving descendants.
        for &desc_path in &descendants {
            // Skip stripping for descendants under variant-introduced children.
            // Variant opinions are their own arc (V in LIVERPS) and survive
            // instancing, so their descendant local sources should too.
            let desc_resolved = store.paths().resolve(desc_path);
            let is_under_variant_child = desc_resolved
                .strip_prefix(&instance_resolved)
                .and_then(|rel| rel.first().copied())
                .is_some_and(|first_name| variant_children.contains(&first_name));
            if is_under_variant_child {
                continue;
            }

            let Some(desc_index) = prims.get_mut(&desc_path) else {
                continue;
            };

            // Only strip LOCAL sources that are namespace-descendants of
            // identity paths. Variant/inherit/reference sources survive.
            desc_index.sources.retain(|source| {
                if !source.is_local {
                    return true; // non-local sources always survive
                }
                !is_identity_descendant(store, source.layer_id, source.lookup_path, identity_paths)
            });

            // Strip LOCAL opinions from identity-descendant sources.
            for opinions in desc_index.opinions_by_field.values_mut() {
                opinions.retain(|op| {
                    if !op.key.is_local {
                        return true;
                    }
                    !is_identity_descendant(
                        store,
                        op.key.layer_id,
                        op.key.lookup_path,
                        identity_paths,
                    )
                });
            }

            // Remove empty field entries.
            desc_index
                .opinions_by_field
                .retain(|_, ops| !ops.is_empty());
        }

        // 2c. Strip children of the instance that are identity-only.
        if let Some(child_list) = children.get_mut(instance_path) {
            child_list.retain(|child| {
                let child_leaf = store.paths().resolve(*child).leaf();
                child_leaf.is_some_and(|name| non_identity_children.contains(&name))
            });
        }

        // 2d. Also apply child stripping to descendant prims (recursive
        // instances or descendant prims with identity-introduced children).
        for &desc_path in &descendants {
            if let Some(child_list) = children.get_mut(&desc_path) {
                child_list
                    .retain(|child| prims.get(child).is_some_and(|idx| !idx.sources.is_empty()));
            }
        }

        // 2e. Remove subtrees of stripped children. A descendant is removed
        // if its first namespace component under the instance is not a
        // surviving child.
        let surviving_child_paths: HashSet<PathId> = children
            .get(instance_path)
            .map(|list| list.iter().copied().collect())
            .unwrap_or_default();

        for &desc_path in &descendants {
            let desc_resolved = store.paths().resolve(desc_path);
            let Some(rel) = desc_resolved.strip_prefix(&instance_resolved) else {
                continue;
            };
            if let Some(first_name) = rel.first().copied() {
                let child_path_obj = instance_resolved.join(&[first_name]);
                if let Some(child_id) = store.paths().lookup(&child_path_obj)
                    && !surviving_child_paths.contains(&child_id)
                {
                    prims.remove(&desc_path);
                    children.remove(&desc_path);
                }
            }
        }
    }
}

/// Collects child names introduced by non-identity sources for an instance prim.
///
/// A child survives instancing if it appears in the `authored_children` of any
/// source that is NOT an identity source (local or instanceable). This includes
/// children from reference targets, inherit classes, and variant branches on
/// non-identity sources.
fn collect_non_identity_children(
    store: &dyn LayerStore,
    instance_path: PathId,
    identity_paths: &[(LayerId, PathId)],
    prims: &HashMap<PathId, PrimIndex>,
    authored_children_opinions: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
) -> HashSet<TokenId> {
    use hashbrown::HashSet;

    let identity_set: HashSet<(LayerId, PathId)> = identity_paths.iter().copied().collect();
    let mut surviving = HashSet::new();

    // Check authored_children_opinions: these include both local and variant
    // authored_children with their OpinionKey provenance.
    if let Some(opinions) = authored_children_opinions.get(&instance_path) {
        for (key, children) in opinions {
            // A non-identity opinion contributes surviving children.
            // Identity = source whose (layer_id, spec_path) is in identity_paths
            // (either local or with instanceable=true).
            let is_identity = identity_set.contains(&(key.layer_id, key.lookup_path));
            if !is_identity {
                for child in children {
                    surviving.insert(*child);
                }
            }
        }
    }

    // Also check variant-introduced children directly from the PrimSpec
    // variant branches. These may not all be in authored_children_opinions
    // because variant_spec.authored_children for the prim itself are not
    // forwarded there — only child_authored_children are.
    if let Some(index) = prims.get(&instance_path) {
        // Resolve variant selections from all sources.
        let mut selections: HashMap<TokenId, TokenId> = HashMap::new();
        for source in &index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            for (set, variant) in &spec.variant_selections {
                selections.entry(*set).or_insert(*variant);
            }
        }

        // Collect variant children from non-identity sources.
        for source in &index.sources {
            let is_identity = identity_set.contains(&(source.layer_id, source.lookup_path));
            if is_identity {
                continue;
            }
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            for (set, set_spec) in &spec.variant_sets {
                let Some(selected) = selections.get(set) else {
                    continue;
                };
                let Some(variant_spec) = set_spec.variants.get(selected) else {
                    continue;
                };
                for child in &variant_spec.authored_children {
                    surviving.insert(*child);
                }
            }
        }

        // Also add variant children from identity sources' variant branches,
        // because variants are their own arc (V in LIVERPS) and should survive
        // instancing stripping even when authored on identity sources.
        for source in &index.sources {
            let is_identity = identity_set.contains(&(source.layer_id, source.lookup_path));
            if !is_identity {
                continue;
            }
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            for (set, set_spec) in &spec.variant_sets {
                let Some(selected) = selections.get(set) else {
                    continue;
                };
                let Some(variant_spec) = set_spec.variants.get(selected) else {
                    continue;
                };
                for child in &variant_spec.authored_children {
                    surviving.insert(*child);
                }
            }
        }

        // Collect authored_children from non-identity sources directly from
        // the PrimSpec (supplements authored_children_opinions).
        for source in &index.sources {
            let is_identity = identity_set.contains(&(source.layer_id, source.lookup_path));
            if is_identity {
                continue;
            }
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            for child in &spec.authored_children {
                surviving.insert(*child);
            }
        }
    }

    surviving
}

/// Checks if `(layer_id, spec_path)` has a `spec_path` that is a descendant of
/// any identity path from the same layer.
fn is_identity_descendant(
    store: &dyn LayerStore,
    layer_id: LayerId,
    spec_path: PathId,
    identity_paths: &[(LayerId, PathId)],
) -> bool {
    let spec_resolved = store.paths().resolve(spec_path);
    for &(id_layer, id_path) in identity_paths {
        if layer_id != id_layer {
            continue;
        }
        let id_resolved = store.paths().resolve(id_path);
        // spec_path must be a STRICT descendant (not equal to) the identity path.
        if id_resolved.is_prefix_of(spec_resolved) && spec_path != id_path {
            return true;
        }
    }
    false
}

/// Removes deactivated prims and their namespace descendants from the stage.
///
/// A prim is deactivated when its strongest `active` opinion across all
/// contributing sources resolves to `false`. When a prim is deactivated,
/// both it and all its namespace descendants are removed from the prim index
/// and children map.
///
/// Spec: AOUSD Core §7.6 (active metadata), §11 (stage population).
fn prune_deactivated(
    store: &dyn LayerStore,
    prims: &mut HashMap<PathId, PrimIndex>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    let mut deactivated: Vec<PathId> = Vec::new();

    let all_paths: Vec<PathId> = prims.keys().copied().collect();
    for &prim_path in &all_paths {
        let Some(index) = prims.get(&prim_path) else {
            continue;
        };

        // Resolve active: strongest opinion wins.
        let mut active_value: Option<bool> = None;
        for source in &index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.lookup_path) else {
                continue;
            };
            if let Some(val) = spec.active {
                active_value = Some(val);
                break; // strongest wins
            }
        }

        if active_value == Some(false) {
            deactivated.push(prim_path);
        }
    }

    // For each deactivated prim, collect all its namespace descendants
    // and remove them all.
    let mut to_remove: HashSet<PathId> = HashSet::new();
    for &deact_path in &deactivated {
        to_remove.insert(deact_path);
        let deact_resolved = store.paths().resolve(deact_path);
        for &path in &all_paths {
            if path != deact_path && deact_resolved.is_prefix_of(store.paths().resolve(path)) {
                to_remove.insert(path);
            }
        }
    }

    // Remove from prims and children.
    for path in &to_remove {
        prims.remove(path);
        children.remove(path);
    }

    // Remove deactivated paths from parent children lists.
    for child_list in children.values_mut() {
        child_list.retain(|c| !to_remove.contains(c));
    }
}

/// Resolves variant selections considering both the local layer stack and
/// referenced layers (weaker selections from references fill in gaps).
///
/// Spec: AOUSD Core §10.5 (variant selection), §9 (LIVERPS strength ordering).
fn resolve_full_variant_selections(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    path: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selections = resolve_variant_selections_for_prim(store, local_stack, path);
    for (set, variant) in resolve_variant_child_selections_for_prim(store, local_stack, path) {
        selections.entry(set).or_insert(variant);
    }

    // Ancestor variant selections apply to namespace descendants unless a more
    // local selection overrides them.
    //
    // Spec: AOUSD Core §10.5 (variant selections authored on a prim determine
    // which descendant variant branches participate in composition).
    let path_obj = store.paths().resolve(path).clone();
    let mut ancestors = Vec::new();
    let mut cursor = path_obj.parent();
    while let Some(parent) = cursor {
        ancestors.push(parent.clone());
        cursor = parent.parent();
    }
    ancestors.reverse();
    for ancestor in ancestors {
        let Some(ancestor_id) = store.paths().lookup(&ancestor) else {
            continue;
        };
        for (set, variant) in resolve_full_variant_selections(store, local_stack, ancestor_id) {
            selections.entry(set).or_insert(variant);
        }
    }

    // Also gather selections from inherit targets (weaker than local, per LIVERPS).
    let inherits = resolve_inherits_for_prim(store, local_stack, path);
    for inherit_target in inherits.iter().copied() {
        let inherit_selections =
            resolve_variant_selections_for_prim(store, local_stack, inherit_target);
        for (set, variant) in inherit_selections {
            selections.entry(set).or_insert(variant);
        }
    }

    // Gather selections introduced by selected local/inherit variant branches
    // before consulting weaker reference and payload targets.
    loop {
        let mut new_selections = HashMap::new();
        let check_paths = core::iter::once(path).chain(inherits.iter().copied());
        for check_path in check_paths {
            for layer_id in &local_stack.layers {
                let Some(layer) = store.layer(*layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(&check_path) else {
                    continue;
                };
                for (set, selected_variant) in &selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            if !selections.contains_key(inner_set) {
                                new_selections.entry(*inner_set).or_insert(*inner_variant);
                            }
                        }
                    }
                }
            }
        }
        if new_selections.is_empty() {
            break;
        }
        selections.extend(new_selections);
    }

    // Also gather selections from reference targets (weaker).
    let refs = {
        let mut ops = Vec::new();
        for layer_id in &local_stack.layers {
            let Some(layer) = store.layer(*layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&path) else {
                continue;
            };
            ops.push(spec.references.clone());
        }
        crate::listop::resolve_list_chain::<Reference>(&[], ops)
    };

    let mut ref_stacks: Vec<(LayerStack, PathId)> = Vec::new();
    for reference in refs {
        let ref_stack = LayerStack::gather(store, reference.layer);
        let Some(reference_path) = resolve_reference_target_path(store, &reference) else {
            continue;
        };
        let ref_selections = resolve_variant_selections_for_prim(store, &ref_stack, reference_path);
        for (set, variant) in ref_selections {
            selections.entry(set).or_insert(variant);
        }
        ref_stacks.push((ref_stack, reference_path));
    }

    loop {
        let mut new_selections = HashMap::new();
        for (ref_stack, ref_path) in &ref_stacks {
            for layer_id in &ref_stack.layers {
                let Some(layer) = store.layer(*layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(ref_path) else {
                    continue;
                };
                for (set, selected_variant) in &selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            if !selections.contains_key(inner_set) {
                                new_selections.entry(*inner_set).or_insert(*inner_variant);
                            }
                        }
                    }
                }
            }
        }
        if new_selections.is_empty() {
            break;
        }
        for (set, variant) in new_selections {
            selections.entry(set).or_insert(variant);
        }
    }

    // Also gather selections from payload targets (weaker than references,
    // stronger than specializes in LIVERPS).
    let payloads = {
        let mut ops = Vec::new();
        for layer_id in &local_stack.layers {
            let Some(layer) = store.layer(*layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&path) else {
                continue;
            };
            ops.push(spec.payloads.clone());
        }
        crate::listop::resolve_list_chain::<Reference>(&[], ops)
    };

    let mut payload_stacks: Vec<(LayerStack, PathId)> = Vec::new();
    for payload in payloads {
        let payload_stack = LayerStack::gather(store, payload.layer);
        let Some(payload_path) = resolve_reference_target_path(store, &payload) else {
            continue;
        };
        let payload_selections =
            resolve_variant_selections_for_prim(store, &payload_stack, payload_path);
        for (set, variant) in payload_selections {
            selections.entry(set).or_insert(variant);
        }
        payload_stacks.push((payload_stack, payload_path));
    }

    loop {
        let mut new_selections = HashMap::new();
        for (payload_stack, payload_path) in &payload_stacks {
            for layer_id in &payload_stack.layers {
                let Some(layer) = store.layer(*layer_id) else {
                    continue;
                };
                let Some(spec) = layer.prims.get(payload_path) else {
                    continue;
                };
                for (set, selected_variant) in &selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected_variant)
                    {
                        for (inner_set, inner_variant) in &variant_spec.variant_selections {
                            new_selections.entry(*inner_set).or_insert(*inner_variant);
                        }
                    }
                }
            }
        }
        if new_selections.is_empty() {
            break;
        }
        for (set, variant) in new_selections {
            selections.entry(set).or_insert(variant);
        }
    }

    selections
}

fn resolve_variant_child_selections_for_prim(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    prim: PathId,
) -> HashMap<TokenId, TokenId> {
    let leaf = store.paths().resolve(prim).leaf();
    let parent = store.paths().resolve(prim).parent();
    let (Some(leaf), Some(parent)) = (leaf, parent) else {
        return HashMap::new();
    };
    let Some(parent_id) = store.paths().lookup(&parent) else {
        return HashMap::new();
    };

    let parent_selections = resolve_full_variant_selections(store, local_stack, parent_id);
    let mut selected = HashMap::new();
    for layer_id in &local_stack.layers {
        let Some(layer) = store.layer(*layer_id) else {
            continue;
        };
        let Some(parent_spec) = layer.prims.get(&parent_id) else {
            continue;
        };
        for (set, variant) in &parent_selections {
            if let Some(set_spec) = parent_spec.variant_sets.get(set)
                && let Some(variant_spec) = set_spec.variants.get(variant)
                && let Some(child_selections) = variant_spec.child_variant_selections.get(&leaf)
            {
                for (child_set, child_variant) in child_selections {
                    selected.entry(*child_set).or_insert(*child_variant);
                }
            }
        }
    }

    selected
}

fn resolve_forwarded_variant_selections(
    store: &dyn LayerStore,
    stronger_stack: &LayerStack,
    selection_path: PathId,
    weaker_stack: &LayerStack,
    source_path: PathId,
) -> HashMap<TokenId, TokenId> {
    let mut selections = resolve_full_variant_selections(store, stronger_stack, selection_path);
    for (set, variant) in resolve_full_variant_selections(store, weaker_stack, source_path) {
        selections.entry(set).or_insert(variant);
    }
    selections
}

fn add_local_and_variant_opinions(
    store: &dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    for path in paths.iter().copied() {
        let selections = resolve_full_variant_selections(store, local_stack, path);
        let namespace_depth =
            u16::try_from(store.paths().resolve(path).depth()).unwrap_or(u16::MAX);

        for (layer_strength_idx, layer_id) in local_stack.layers.iter().copied().enumerate() {
            let Some(layer) = store.layer(layer_id).cloned() else {
                continue;
            };
            let Some(spec) = layer.prims.get(&path) else {
                continue;
            };

            if let Some(d) = deps.as_deref_mut() {
                d.add_layer_opinion(layer_id, path);
            }

            let accumulated_offset = local_stack.offset_at(layer_strength_idx);
            let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
            out.get_mut(&path)
                .expect("path exists")
                .add_source(OpinionKey {
                    is_local: true,
                    arc_kind: ArcKind::Local,
                    nested_arc_kind: None,
                    namespace_depth,
                    authored: true,
                    arc_list_index: 0,
                    layer_strength,
                    layer_id,
                    lookup_path: path,
                    spec_path: prim_spec_path(store, path, &spec.outer_variant_sites),
                });

            for entry in &spec.fields {
                let key = OpinionKey {
                    is_local: true,
                    arc_kind: ArcKind::Local,
                    nested_arc_kind: None,
                    namespace_depth,
                    authored: true,
                    arc_list_index: 0,
                    layer_strength,
                    layer_id,
                    lookup_path: path,
                    spec_path: property_spec_path(
                        store,
                        path,
                        &spec.outer_variant_sites,
                        entry.name,
                    ),
                };
                let index = out.get_mut(&path).expect("path exists");
                if let Some(property_type) = &entry.property_type {
                    index.add_property_type(entry.name, key.clone(), property_type.clone());
                }
                index.add_opinion(Opinion {
                    key,
                    field: entry.name,
                    value: entry.value.clone(),
                    layer_offset: accumulated_offset,
                });
            }

            if !spec.authored_children.is_empty() {
                authored_children_out.entry(path).or_default().push((
                    OpinionKey {
                        is_local: true,
                        arc_kind: ArcKind::Local,
                        nested_arc_kind: None,
                        namespace_depth,
                        authored: true,
                        arc_list_index: 0,
                        layer_strength,
                        layer_id,
                        lookup_path: path,
                        spec_path: prim_spec_path(store, path, &spec.outer_variant_sites),
                    },
                    spec.authored_children.clone(),
                ));
            }

            if let Some(order) = &spec.prim_order {
                prim_order_out.entry(path).or_default().push((
                    OpinionKey {
                        is_local: true,
                        arc_kind: ArcKind::Local,
                        nested_arc_kind: None,
                        namespace_depth,
                        authored: true,
                        arc_list_index: 0,
                        layer_strength,
                        layer_id,
                        lookup_path: path,
                        spec_path: prim_spec_path(store, path, &spec.outer_variant_sites),
                    },
                    order.clone(),
                ));
            }

            for (set, selected_variant) in &selections {
                let Some(set_spec) = spec.variant_sets.get(set) else {
                    continue;
                };
                let Some(variant_spec) = set_spec.variants.get(selected_variant) else {
                    continue;
                };
                let branch_selections = combined_variant_sites(
                    &variant_spec.outer_variant_sites,
                    VariantSelectionSite {
                        host_path: path,
                        set: *set,
                        variant: *selected_variant,
                    },
                );

                out.get_mut(&path)
                    .expect("path exists")
                    .add_source(OpinionKey {
                        is_local: false,
                        arc_kind: ArcKind::Variants,
                        nested_arc_kind: None,
                        namespace_depth,
                        authored: true,
                        arc_list_index: 0,
                        layer_strength,
                        layer_id,
                        lookup_path: path,
                        spec_path: variant_spec_path(store, path, &branch_selections),
                    });

                for entry in &variant_spec.fields {
                    let key = OpinionKey {
                        is_local: false,
                        arc_kind: ArcKind::Variants,
                        nested_arc_kind: None,
                        namespace_depth,
                        authored: true,
                        arc_list_index: 0,
                        layer_strength,
                        layer_id,
                        lookup_path: path,
                        spec_path: variant_property_spec_path(
                            store,
                            path,
                            &branch_selections,
                            entry.name,
                        ),
                    };
                    let index = out.get_mut(&path).expect("path exists");
                    if let Some(property_type) = &entry.property_type {
                        index.add_property_type(entry.name, key.clone(), property_type.clone());
                    }
                    index.add_opinion(Opinion {
                        key,
                        field: entry.name,
                        value: entry.value.clone(),
                        layer_offset: accumulated_offset,
                    });
                }

                // Forward variant-scoped child prim fields.
                let path_obj = store.paths().resolve(path).clone();
                for child_tok in &variant_spec.authored_children {
                    let child_path = path_obj.join(&[*child_tok]);
                    if let Some(child_path_id) = store.paths().lookup(&child_path)
                        && out.contains_key(&child_path_id)
                    {
                        let child_ns_depth =
                            u16::try_from(store.paths().resolve(child_path_id).depth())
                                .unwrap_or(u16::MAX);
                        let child_outer = variant_spec
                            .required_outer_variant_sites
                            .get(child_tok)
                            .cloned()
                            .unwrap_or_else(|| variant_spec.outer_variant_sites.clone());
                        let child_selections = combined_variant_sites(
                            &child_outer,
                            VariantSelectionSite {
                                host_path: path,
                                set: *set,
                                variant: *selected_variant,
                            },
                        );
                        out.get_mut(&child_path_id)
                            .expect("path exists")
                            .add_source(OpinionKey {
                                is_local: false,
                                arc_kind: ArcKind::Variants,
                                nested_arc_kind: None,
                                namespace_depth: child_ns_depth,
                                authored: true,
                                arc_list_index: 0,
                                layer_strength,
                                layer_id,
                                lookup_path: child_path_id,
                                spec_path: variant_spec_path(
                                    store,
                                    child_path_id,
                                    &child_selections,
                                ),
                            });
                    }
                }
                for (child_tok, child_fields) in &variant_spec.child_fields {
                    let child_path = path_obj.join(&[*child_tok]);
                    if let Some(child_path_id) = store.paths().lookup(&child_path)
                        && out.contains_key(&child_path_id)
                    {
                        let child_ns_depth =
                            u16::try_from(store.paths().resolve(child_path_id).depth())
                                .unwrap_or(u16::MAX);
                        let child_outer = variant_spec
                            .required_outer_variant_sites
                            .get(child_tok)
                            .cloned()
                            .unwrap_or_else(|| variant_spec.outer_variant_sites.clone());
                        let child_selections = combined_variant_sites(
                            &child_outer,
                            VariantSelectionSite {
                                host_path: path,
                                set: *set,
                                variant: *selected_variant,
                            },
                        );
                        for entry in child_fields {
                            let key = OpinionKey {
                                is_local: false,
                                arc_kind: ArcKind::Variants,
                                nested_arc_kind: None,
                                namespace_depth: child_ns_depth,
                                authored: true,
                                arc_list_index: 0,
                                layer_strength,
                                layer_id,
                                lookup_path: child_path_id,
                                spec_path: variant_property_spec_path(
                                    store,
                                    child_path_id,
                                    &child_selections,
                                    entry.name,
                                ),
                            };
                            let index = out.get_mut(&child_path_id).expect("path exists");
                            if let Some(property_type) = &entry.property_type {
                                index.add_property_type(
                                    entry.name,
                                    key.clone(),
                                    property_type.clone(),
                                );
                            }
                            index.add_opinion(Opinion {
                                key,
                                field: entry.name,
                                value: entry.value.clone(),
                                layer_offset: accumulated_offset,
                            });
                        }
                        out.get_mut(&child_path_id)
                            .expect("path exists")
                            .add_source(OpinionKey {
                                is_local: false,
                                arc_kind: ArcKind::Variants,
                                nested_arc_kind: None,
                                namespace_depth: child_ns_depth,
                                authored: true,
                                arc_list_index: 0,
                                layer_strength,
                                layer_id,
                                lookup_path: child_path_id,
                                spec_path: variant_spec_path(
                                    store,
                                    child_path_id,
                                    &child_selections,
                                ),
                            });
                    }
                }

                // Forward variant-scoped child_authored_children as
                // authored_children opinions on the child path.
                for (child_tok, gc_list) in &variant_spec.child_authored_children {
                    let child_path = path_obj.join(&[*child_tok]);
                    if let Some(child_path_id) = store.paths().lookup(&child_path)
                        && out.contains_key(&child_path_id)
                    {
                        let child_ns_depth =
                            u16::try_from(store.paths().resolve(child_path_id).depth())
                                .unwrap_or(u16::MAX);
                        let child_outer = variant_spec
                            .required_outer_variant_sites
                            .get(child_tok)
                            .cloned()
                            .unwrap_or_else(|| variant_spec.outer_variant_sites.clone());
                        let child_selections = combined_variant_sites(
                            &child_outer,
                            VariantSelectionSite {
                                host_path: path,
                                set: *set,
                                variant: *selected_variant,
                            },
                        );
                        authored_children_out
                            .entry(child_path_id)
                            .or_default()
                            .push((
                                OpinionKey {
                                    is_local: false,
                                    arc_kind: ArcKind::Variants,
                                    nested_arc_kind: None,
                                    namespace_depth: child_ns_depth,
                                    authored: true,
                                    arc_list_index: 0,
                                    layer_strength,
                                    layer_id,
                                    lookup_path: child_path_id,
                                    spec_path: variant_spec_path(
                                        store,
                                        child_path_id,
                                        &child_selections,
                                    ),
                                },
                                gc_list.clone(),
                            ));
                    }
                }
            }
        }
    }
}

fn add_reference_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Spec: AOUSD Core §10 (references arcs). For v0.1 we expand references
    // recursively so that nested references contribute opinions.
    let mut visited: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_specializes: HashSet<(PathId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        let refs = resolve_references_for_prim(store, local_stack, dest_root);
        // Also resolve variant child references with full selection chaining.
        let variant_child_refs =
            resolve_variant_child_references(store, local_stack, local_stack, dest_root);
        let all_refs = refs.into_iter().chain(variant_child_refs);
        for (arc_list_index, reference) in all_refs.enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            let Some(reference_path) = resolve_reference_target_path(store, &reference) else {
                continue;
            };
            if let Some(d) = deps.as_deref_mut() {
                d.add_arc(ArcDependency {
                    source: reference_path,
                    target: dest_root,
                    arc_kind: ArcKind::References,
                    layer: reference.layer,
                });
            }
            add_reference_edge_opinions(
                store,
                local_stack,
                dest_root,
                reference,
                namespace_depth,
                arc_list_index,
                out,
                &mut visited,
                &mut visited_inherits,
                &mut visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                deps.as_deref_mut(),
            );
        }
    }
}

fn add_inherit_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Spec: AOUSD Core §10 (inherits arc).
    let mut visited: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_specializes: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_refs: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        let inherits = resolve_inherits_for_prim(store, local_stack, dest_root);
        for (arc_list_index, inherited_root) in inherits.into_iter().enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            if let Some(d) = deps.as_deref_mut() {
                d.add_arc(ArcDependency {
                    source: inherited_root,
                    target: dest_root,
                    arc_kind: ArcKind::Inherits,
                    layer: local_stack.layers[0],
                });
            }
            add_inherit_edge_opinions(
                store,
                local_stack,
                dest_root,
                inherited_root,
                None,
                namespace_depth,
                arc_list_index,
                out,
                &mut visited,
                &mut visited_specializes,
                &mut visited_refs,
                prim_order_out,
                authored_children_out,
                None,
                None,
                LayerOffset::IDENTITY,
                deps.as_deref_mut(),
            );
        }
    }
}

fn add_inherit_edge_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    dest_root: PathId,
    inherited_root: PathId,
    outer_arc_kind: Option<ArcKind>,
    namespace_depth: u16,
    arc_list_index: u16,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, PathId)>,
    visited_specializes: &mut HashSet<(PathId, PathId)>,
    visited_refs: &mut HashSet<(PathId, LayerId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    // Optional reference namespace for remapping field values (dest, src).
    ref_remap: Option<(&crate::path::Path, &crate::path::Path)>,
    // Optional source-namespace remap for provenance spec paths (dest, src).
    provenance_remap: Option<(PathId, PathId)>,
    // Accumulated offset from outer arcs (references/payloads). Composed with
    // each layer's sublayer offset to produce the final opinion offset.
    base_offset: LayerOffset,
    mut deps: Option<&mut DependencyBuilder>,
) {
    if !visited.insert((dest_root, inherited_root)) {
        return;
    }

    let base_path = store.paths().resolve(dest_root).clone();
    let inherited_path = store.paths().resolve(inherited_root).clone();

    let mut remote_paths: Vec<PathId> = local_stack
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

    let mut mapping: Vec<(PathId, PathId)> = Vec::new();
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&inherited_path) else {
                continue;
            };
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(base_path.join(&rel));
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }

    let (arc_kind, nested_arc_kind) = match outer_arc_kind {
        Some(outer) => (outer, Some(ArcKind::Inherits)),
        None => (ArcKind::Inherits, None),
    };

    for (layer_strength_idx, layer_id) in local_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let layer_offset = base_offset.compose(local_stack.offset_at(layer_strength_idx));
        let mut pending: Vec<(
            PathId,
            PathId,
            SpecPath,
            TokenId,
            FieldValue,
            Option<PropertyType>,
        )> = Vec::new();
        let mut pending_sources = Vec::new();
        {
            let Some(layer) = store.layer(layer_id).cloned() else {
                continue;
            };

            for (remote_path_id, dest_path_id) in &mapping {
                let Some(spec) = layer.prims.get(remote_path_id) else {
                    continue;
                };
                let stronger_selections =
                    resolve_full_variant_selections(store, local_stack, *dest_path_id);
                if let Some(d) = deps.as_deref_mut() {
                    d.add_layer_opinion(layer_id, *dest_path_id);
                }
                if let Some(order) = &spec.prim_order {
                    prim_order_out.entry(*dest_path_id).or_default().push((
                        OpinionKey {
                            is_local: false,
                            arc_kind,
                            nested_arc_kind,
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_prim_spec_path(
                                store,
                                *remote_path_id,
                                &spec.outer_variant_sites,
                                provenance_remap,
                                &stronger_selections,
                            ),
                        },
                        order.clone(),
                    ));
                }

                if !spec.authored_children.is_empty() {
                    authored_children_out
                        .entry(*dest_path_id)
                        .or_default()
                        .push((
                            OpinionKey {
                                is_local: false,
                                arc_kind,
                                nested_arc_kind,
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength,
                                layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_prim_spec_path(
                                    store,
                                    *remote_path_id,
                                    &spec.outer_variant_sites,
                                    provenance_remap,
                                    &stronger_selections,
                                ),
                            },
                            spec.authored_children.clone(),
                        ));
                }

                pending_sources.push((
                    *dest_path_id,
                    OpinionKey {
                        is_local: false,
                        arc_kind,
                        nested_arc_kind,
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength,
                        layer_id,
                        lookup_path: *remote_path_id,
                        spec_path: normalized_prim_spec_path(
                            store,
                            *remote_path_id,
                            &spec.outer_variant_sites,
                            provenance_remap,
                            &stronger_selections,
                        ),
                    },
                ));
                for entry in &spec.fields {
                    pending.push((
                        *dest_path_id,
                        *remote_path_id,
                        normalized_property_spec_path(
                            store,
                            *remote_path_id,
                            &spec.outer_variant_sites,
                            entry.name,
                            provenance_remap,
                            &stronger_selections,
                        ),
                        entry.name,
                        entry.value.clone(),
                        entry.property_type.clone(),
                    ));
                }

                // Forward variant opinions from selected variants through inherits.
                let inherits_selections = resolve_forwarded_variant_selections(
                    store,
                    local_stack,
                    *dest_path_id,
                    local_stack,
                    *remote_path_id,
                );
                for (set, selected) in &inherits_selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected)
                    {
                        let branch_selections = combined_variant_sites(
                            &variant_spec.outer_variant_sites,
                            VariantSelectionSite {
                                host_path: *remote_path_id,
                                set: *set,
                                variant: *selected,
                            },
                        );
                        pending_sources.push((
                            *dest_path_id,
                            OpinionKey {
                                is_local: false,
                                arc_kind,
                                nested_arc_kind: nested_arc_kind.or(Some(ArcKind::Variants)),
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength,
                                layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_variant_spec_path(
                                    store,
                                    *remote_path_id,
                                    &branch_selections,
                                    provenance_remap,
                                    &stronger_selections,
                                ),
                            },
                        ));
                        for entry in &variant_spec.fields {
                            pending.push((
                                *dest_path_id,
                                *remote_path_id,
                                normalized_variant_property_spec_path(
                                    store,
                                    *remote_path_id,
                                    &branch_selections,
                                    entry.name,
                                    provenance_remap,
                                    &stronger_selections,
                                ),
                                entry.name,
                                entry.value.clone(),
                                entry.property_type.clone(),
                            ));
                        }
                    }
                }

                // Forward child_fields from the parent's variant specs through
                // inherits. When the inherited prim is a child whose parent has
                // variant sets with child_fields targeting this child, those
                // fields need to propagate through the inherit arc.
                let remote_path = store.paths().resolve(*remote_path_id).clone();
                if let Some(remote_leaf) = remote_path.leaf()
                    && let Some(remote_parent) = remote_path.parent()
                    && let Some(remote_parent_id) = store.paths().lookup(&remote_parent)
                    && let Some(parent_spec) = layer.prims.get(&remote_parent_id)
                {
                    let parent_selections =
                        resolve_variant_selections_for_prim(store, local_stack, remote_parent_id);
                    for (set, selected) in &parent_selections {
                        if let Some(set_spec) = parent_spec.variant_sets.get(set)
                            && let Some(variant_spec) = set_spec.variants.get(selected)
                            && let Some(child_fields) = variant_spec.child_fields.get(&remote_leaf)
                        {
                            let child_outer = variant_spec
                                .required_outer_variant_sites
                                .get(&remote_leaf)
                                .cloned()
                                .unwrap_or_else(|| variant_spec.outer_variant_sites.clone());
                            let child_selections = combined_variant_sites(
                                &child_outer,
                                VariantSelectionSite {
                                    host_path: remote_parent_id,
                                    set: *set,
                                    variant: *selected,
                                },
                            );
                            for entry in child_fields {
                                pending.push((
                                    *dest_path_id,
                                    *remote_path_id,
                                    normalized_variant_property_spec_path(
                                        store,
                                        *remote_path_id,
                                        &child_selections,
                                        entry.name,
                                        provenance_remap,
                                        &stronger_selections,
                                    ),
                                    entry.name,
                                    entry.value.clone(),
                                    entry.property_type.clone(),
                                ));
                            }
                        }
                    }
                }
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }

        for (dest_path_id, remote_path_id, spec_path, field, value, property_type) in pending {
            let mut value = remap_field_value_paths(store, &base_path, &inherited_path, value);
            // Also apply reference namespace remapping if within a reference context.
            if let Some((ref_dest, ref_src)) = ref_remap {
                value = remap_field_value_paths(store, ref_dest, ref_src, value);
            }
            let key = OpinionKey {
                is_local: false,
                arc_kind,
                nested_arc_kind,
                namespace_depth,
                authored: true,
                arc_list_index,
                layer_strength,
                layer_id,
                lookup_path: remote_path_id,
                spec_path,
            };
            let index = out.get_mut(&dest_path_id).expect("path exists");
            if let Some(property_type) = property_type {
                index.add_property_type(field, key.clone(), property_type);
            }
            index.add_opinion(Opinion {
                key,
                field,
                value,
                layer_offset,
            });
        }
    }

    // Propagate already-accumulated PrimIndex sources from mapped source
    // paths to dest paths. This handles cases where the source path has
    // opinions from other composition arcs (e.g., references) that were
    // added by earlier processing. Without this, opinions from layers using
    // different namespace roots (as in reference contexts) would be missed.
    for &(remote_path_id, dest_path_id) in &mapping {
        let src_index = out.get(&remote_path_id).cloned();
        if let Some(src_index) = src_index {
            let stronger_selections =
                resolve_full_variant_selections(store, local_stack, dest_path_id);
            for source in &src_index.sources {
                if source.arc_kind == ArcKind::Local {
                    continue;
                }
                let spec_path = normalize_forwarded_spec_path(
                    store,
                    &source.spec_path,
                    provenance_remap,
                    &stronger_selections,
                );
                out.get_mut(&dest_path_id)
                    .expect("path exists")
                    .add_source(OpinionKey {
                        is_local: false,
                        arc_kind,
                        nested_arc_kind: Some(source.arc_kind),
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength: source.layer_strength,
                        layer_id: source.layer_id,
                        lookup_path: source.lookup_path,
                        spec_path,
                    });
            }
            for (field, opinions) in &src_index.opinions_by_field {
                for opinion in opinions {
                    if opinion.key.arc_kind == ArcKind::Local {
                        continue;
                    }
                    let spec_path = normalize_forwarded_spec_path(
                        store,
                        &opinion.key.spec_path,
                        provenance_remap,
                        &stronger_selections,
                    );
                    out.get_mut(&dest_path_id)
                        .expect("path exists")
                        .add_opinion(Opinion {
                            key: OpinionKey {
                                is_local: false,
                                arc_kind,
                                nested_arc_kind: Some(opinion.key.arc_kind),
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength: opinion.key.layer_strength,
                                layer_id: opinion.key.layer_id,
                                lookup_path: opinion.key.lookup_path,
                                spec_path,
                            },
                            field: *field,
                            value: opinion.value.clone(),
                            layer_offset: opinion.layer_offset,
                        });
                }
            }
        }
    }

    for &(remote_path_id, dest_path_id) in &mapping {
        let nested_inherits = resolve_inherits_for_prim(store, local_stack, remote_path_id);
        for (nested_index, nested) in nested_inherits.into_iter().enumerate() {
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // Inherit arcs authored inside inherited namespace may refer to
            // paths within that same namespace. When those specs are mapped
            // onto the destination prim, the inherit targets participate in
            // the destination namespace as well.
            //
            // We apply both:
            // - the translated target (to pick up local opinions at the
            //   destination path), and
            // - the original target (to pick up the class opinions authored
            //   at the source path).
            //
            // Spec: AOUSD Core §10 (inherits arc), including namespace mapping
            // behavior for inherited class namespaces.
            let translated = remap_path_id(store, &base_path, &inherited_path, nested);
            if translated != nested {
                add_inherit_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    translated,
                    outer_arc_kind,
                    namespace_depth,
                    nested_index,
                    out,
                    visited,
                    visited_specializes,
                    visited_refs,
                    prim_order_out,
                    authored_children_out,
                    ref_remap,
                    None,
                    base_offset,
                    deps.as_deref_mut(),
                );
            }

            // Also allow translation relative to the parent mapping site.
            // This handles cases where the inherited class’s own inherits
            // target is a sibling rather than a descendant (e.g. /Looks/Metal
            // inherits /Looks/Material which inherits /Looks/BaseMaterial —
            // the parent remap /Looks → /Model/Looks correctly translates
            // /Looks/BaseMaterial → /Model/Looks/BaseMaterial).
            //
            // Spec: AOUSD Core §10 (inherits arc) and supplemental fixtures
            // involving nested classes (e.g. `BasicLocalAndGlobalClassCombination_root`).
            if let (Some(base_parent), Some(inherited_parent)) =
                (base_path.parent(), inherited_path.parent())
            {
                let parent_translated =
                    remap_path_id(store, &base_parent, &inherited_parent, nested);
                if parent_translated != translated && parent_translated != nested {
                    add_inherit_edge_opinions(
                        store,
                        local_stack,
                        dest_path_id,
                        parent_translated,
                        outer_arc_kind,
                        namespace_depth,
                        nested_index,
                        out,
                        visited,
                        visited_specializes,
                        visited_refs,
                        prim_order_out,
                        authored_children_out,
                        ref_remap,
                        None,
                        base_offset,
                        deps.as_deref_mut(),
                    );
                }
            }
            add_inherit_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                nested,
                outer_arc_kind,
                namespace_depth,
                nested_index,
                out,
                visited,
                visited_specializes,
                visited_refs,
                prim_order_out,
                authored_children_out,
                ref_remap,
                None,
                base_offset,
                deps.as_deref_mut(),
            );
        }

        // Propagate specializes from the inherited class.
        //
        // When an inherited class specializes another class, those opinions
        // propagate at specializes strength. This completes the LIVERPS chain
        // for inherits: inherits sees the full composition of the inherited
        // namespace including its specializes.
        //
        // Spec: AOUSD Core §10 (LIVERPS composition ordering).
        let nested_specializes = resolve_specializes_for_prim(store, local_stack, remote_path_id);
        for (spec_index, specialized) in nested_specializes.into_iter().enumerate() {
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated = remap_path_id(store, &base_path, &inherited_path, specialized);
            if translated != specialized {
                add_specializes_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    dest_path_id,
                    translated,
                    outer_arc_kind,
                    namespace_depth,
                    spec_index,
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    base_offset,
                    deps.as_deref_mut(),
                );
            }

            if let (Some(base_parent), Some(inherited_parent)) =
                (base_path.parent(), inherited_path.parent())
            {
                let parent_translated =
                    remap_path_id(store, &base_parent, &inherited_parent, specialized);
                if parent_translated != translated && parent_translated != specialized {
                    add_specializes_edge_opinions(
                        store,
                        local_stack,
                        dest_path_id,
                        dest_path_id,
                        parent_translated,
                        outer_arc_kind,
                        namespace_depth,
                        spec_index,
                        out,
                        visited_specializes,
                        prim_order_out,
                        authored_children_out,
                        None,
                        base_offset,
                        deps.as_deref_mut(),
                    );
                }
            }

            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                dest_path_id,
                specialized,
                outer_arc_kind,
                namespace_depth,
                spec_index,
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                base_offset,
                deps.as_deref_mut(),
            );
        }

        // Propagate references from the inherited class.
        //
        // When an inherited class has references, those reference opinions
        // propagate through the inherits arc. This completes the LIVERPS chain
        // for inherits: the inherited namespace's references contribute opinions.
        //
        // Spec: AOUSD Core §10 (LIVERPS composition ordering).
        let nested_refs = resolve_references_for_prim(store, local_stack, remote_path_id);
        for (ref_index, nested_ref) in nested_refs.into_iter().enumerate() {
            let ref_index = u16::try_from(ref_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                nested_ref,
                namespace_depth,
                ref_index,
                out,
                visited_refs,
                visited,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                deps.as_deref_mut(),
            );
        }
    }

    // Propagate opinions for paths that exist in the PrimIndex (from reference
    // expansion) but not in any layer's PrimSpec. These are reference-introduced
    // children of the inherited source that need to be mapped to the destination.
    let mapping_set: HashSet<PathId> = mapping.iter().map(|(r, _)| *r).collect();
    let all_out_paths: Vec<PathId> = out.keys().copied().collect();
    for src_path_id in all_out_paths {
        if mapping_set.contains(&src_path_id) {
            continue;
        }
        let rel: Vec<_> = {
            let src_path = store.paths().resolve(src_path_id);
            let Some(rel) = src_path.strip_prefix(&inherited_path) else {
                continue;
            };
            if rel.is_empty() {
                continue;
            }
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(base_path.join(&rel));
        if !out.contains_key(&dest_path_id) {
            continue;
        }

        // Copy sources and opinions from the source PrimIndex entry.
        let src_index = out.get(&src_path_id).cloned();
        if let Some(src_index) = src_index {
            for source in &src_index.sources {
                let spec_path =
                    remap_spec_path(store, &source.spec_path, &base_path, &inherited_path);
                out.get_mut(&dest_path_id)
                    .expect("path exists")
                    .add_source(OpinionKey {
                        is_local: false,
                        arc_kind,
                        nested_arc_kind: Some(source.arc_kind),
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength: source.layer_strength,
                        layer_id: source.layer_id,
                        lookup_path: source.lookup_path,
                        spec_path,
                    });
            }
            for (field, opinions) in &src_index.opinions_by_field {
                for opinion in opinions {
                    let spec_path =
                        remap_spec_path(store, &opinion.key.spec_path, &base_path, &inherited_path);
                    out.get_mut(&dest_path_id)
                        .expect("path exists")
                        .add_opinion(Opinion {
                            key: OpinionKey {
                                is_local: false,
                                arc_kind,
                                nested_arc_kind: Some(opinion.key.arc_kind),
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength: opinion.key.layer_strength,
                                layer_id: opinion.key.layer_id,
                                lookup_path: opinion.key.lookup_path,
                                spec_path,
                            },
                            field: *field,
                            value: opinion.value.clone(),
                            layer_offset: opinion.layer_offset,
                        });
                }
            }
        }
    }
}

fn remap_field_value_paths(
    store: &mut dyn LayerStore,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
    value: FieldValue,
) -> FieldValue {
    match value {
        FieldValue::PathListOp(list) => {
            let mut out = list;
            out.explicit = out.explicit.map(|v| {
                v.into_iter()
                    .map(|p| remap_target_path(store, dest_root, src_root, p))
                    .collect()
            });
            out.prepend = out
                .prepend
                .into_iter()
                .map(|p| remap_target_path(store, dest_root, src_root, p))
                .collect();
            out.append = out
                .append
                .into_iter()
                .map(|p| remap_target_path(store, dest_root, src_root, p))
                .collect();
            out.delete = out
                .delete
                .into_iter()
                .map(|p| remap_target_path(store, dest_root, src_root, p))
                .collect();
            FieldValue::PathListOp(out)
        }
        other => other,
    }
}

fn remap_target_path(
    store: &mut dyn LayerStore,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
    path: TargetPath,
) -> TargetPath {
    match path {
        TargetPath::Prim(path) => TargetPath::Prim(remap_path_id(store, dest_root, src_root, path)),
        TargetPath::Property(path) => {
            TargetPath::Property(remap_property_path(store, dest_root, src_root, path))
        }
    }
}

fn remap_property_path(
    store: &mut dyn LayerStore,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
    path: PropertyPath,
) -> PropertyPath {
    PropertyPath::new(
        remap_path_id(store, dest_root, src_root, path.prim_path()),
        path.property(),
    )
}

fn remap_path_id(
    store: &mut dyn LayerStore,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
    path: PathId,
) -> PathId {
    let rel: Option<Vec<_>> = {
        let p = store.paths().resolve(path);
        p.strip_prefix(src_root).map(<[_]>::to_vec)
    };
    if let Some(rel) = rel {
        return store.paths_mut().intern(dest_root.join(&rel));
    }

    path
}

fn spec_path_selection_sites(
    store: &mut dyn LayerStore,
    spec_path: &SpecPath,
) -> Vec<VariantSelectionSite> {
    let mut sites = Vec::new();
    let mut prefix = Vec::new();
    for component in spec_path.components().iter().copied() {
        match component {
            crate::spec_path::SpecComponent::Prim(segment) => prefix.push(segment),
            crate::spec_path::SpecComponent::VariantSelection { set, variant } => {
                let host_path = store
                    .paths_mut()
                    .intern(crate::path::Path::root().join(&prefix));
                sites.push(VariantSelectionSite {
                    host_path,
                    set,
                    variant,
                });
            }
        }
    }
    sites
}

fn remap_spec_path(
    store: &mut dyn LayerStore,
    spec_path: &SpecPath,
    dest_root: &crate::path::Path,
    src_root: &crate::path::Path,
) -> SpecPath {
    let prim_path = remap_path_id(store, dest_root, src_root, spec_path.prim_path());
    let selection_sites = spec_path_selection_sites(store, spec_path)
        .into_iter()
        .map(|site| VariantSelectionSite {
            host_path: remap_path_id(store, dest_root, src_root, site.host_path),
            set: site.set,
            variant: site.variant,
        })
        .collect::<Vec<_>>();
    let mut out = if selection_sites.is_empty() {
        SpecPath::from_prim_path(prim_path, store.paths())
    } else {
        SpecPath::from_variant_selection_sites(prim_path, &selection_sites, store.paths())
    };
    if let Some(property) = spec_path.property() {
        out = out.with_property(property);
    }
    out
}

fn add_reference_edge_opinions(
    store: &mut dyn LayerStore,
    stage_stack: &LayerStack,
    dest_root: PathId,
    reference: Reference,
    namespace_depth: u16,
    arc_list_index: u16,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut HashSet<(PathId, PathId)>,
    visited_specializes: &mut HashSet<(PathId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    provenance_remap: Option<(PathId, PathId)>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    if !out.contains_key(&dest_root) {
        return;
    }
    let Some(reference_path) = resolve_reference_target_path(store, &reference) else {
        return;
    };
    if !visited.insert((dest_root, reference.layer, reference_path)) {
        return;
    }

    let remote_stack = LayerStack::gather(store, reference.layer);
    let combined_stack = LayerStack {
        layers: stage_stack
            .layers
            .iter()
            .copied()
            .chain(remote_stack.layers.iter().copied())
            .collect(),
        offsets: stage_stack
            .offsets
            .iter()
            .copied()
            .chain(remote_stack.offsets.iter().copied())
            .collect(),
    };
    let target_root = store.paths().resolve(reference_path).clone();
    let dest_root_path = store.paths().resolve(dest_root).clone();

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

    let mut mapping: Vec<(PathId, PathId)> = Vec::new();
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&target_root) else {
                continue;
            };
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(dest_root_path.join(&rel));
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }

    // Compose the reference's own offset with each remote layer's sublayer offset.
    // This gives the total time remapping for opinions from each layer in the
    // referenced layer stack.
    //
    // Spec: §12.3.2.1 (sublayer offsets compose when nested).
    for (layer_strength_idx, remote_layer_id) in remote_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let ref_offset = reference
            .layer_offset
            .compose(remote_stack.offset_at(layer_strength_idx));
        let Some(remote_layer) = store.layer(remote_layer_id).cloned() else {
            continue;
        };

        let mut pending_sources = Vec::new();
        let mut pending_fields: Vec<(
            PathId,
            TokenId,
            OpinionKey,
            FieldValue,
            Option<PropertyType>,
            LayerOffset,
        )> = Vec::new();
        for (remote_path_id, dest_path_id) in &mapping {
            let Some(remote_spec) = remote_layer.prims.get(remote_path_id) else {
                continue;
            };
            let stronger_selections =
                resolve_full_variant_selections(store, stage_stack, *dest_path_id);
            if let Some(d) = deps.as_deref_mut() {
                d.add_layer_opinion(remote_layer_id, *dest_path_id);
            }
            let base_key = OpinionKey {
                is_local: false,
                arc_kind: ArcKind::References,
                nested_arc_kind: None,
                namespace_depth,
                authored: true,
                arc_list_index,
                layer_strength,
                layer_id: remote_layer_id,
                lookup_path: *remote_path_id,
                spec_path: normalized_prim_spec_path(
                    store,
                    *remote_path_id,
                    &remote_spec.outer_variant_sites,
                    provenance_remap,
                    &stronger_selections,
                ),
            };
            pending_sources.push((*dest_path_id, base_key.clone()));

            for entry in &remote_spec.fields {
                pending_fields.push((
                    *dest_path_id,
                    entry.name,
                    base_key
                        .clone()
                        .with_spec_path(normalized_property_spec_path(
                            store,
                            *remote_path_id,
                            &remote_spec.outer_variant_sites,
                            entry.name,
                            provenance_remap,
                            &stronger_selections,
                        )),
                    entry.value.clone(),
                    entry.property_type.clone(),
                    ref_offset,
                ));
            }

            // Forward variant opinions from selected variants.
            // Variant selections are resolved using the combined stack
            // (referencing layer selections take precedence).
            {
                let selections = resolve_forwarded_variant_selections(
                    store,
                    stage_stack,
                    *dest_path_id,
                    &remote_stack,
                    *remote_path_id,
                );
                for (set, selected) in &selections {
                    if let Some(set_spec) = remote_spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected)
                    {
                        let branch_selections = combined_variant_sites(
                            &variant_spec.outer_variant_sites,
                            VariantSelectionSite {
                                host_path: *remote_path_id,
                                set: *set,
                                variant: *selected,
                            },
                        );
                        pending_sources.push((
                            *dest_path_id,
                            OpinionKey {
                                is_local: false,
                                arc_kind: ArcKind::References,
                                nested_arc_kind: Some(ArcKind::Variants),
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength,
                                layer_id: remote_layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_variant_spec_path(
                                    store,
                                    *remote_path_id,
                                    &branch_selections,
                                    provenance_remap,
                                    &stronger_selections,
                                ),
                            },
                        ));
                        for entry in &variant_spec.fields {
                            pending_fields.push((
                                *dest_path_id,
                                entry.name,
                                OpinionKey {
                                    is_local: false,
                                    arc_kind: ArcKind::References,
                                    nested_arc_kind: Some(ArcKind::Variants),
                                    namespace_depth,
                                    authored: true,
                                    arc_list_index,
                                    layer_strength,
                                    layer_id: remote_layer_id,
                                    lookup_path: *remote_path_id,
                                    spec_path: normalized_variant_property_spec_path(
                                        store,
                                        *remote_path_id,
                                        &branch_selections,
                                        entry.name,
                                        provenance_remap,
                                        &stronger_selections,
                                    ),
                                },
                                entry.value.clone(),
                                entry.property_type.clone(),
                                ref_offset,
                            ));
                        }

                        // Forward child_authored_children as authored_children
                        // opinions on child paths.
                        let ref_path_obj = store.paths().resolve(*dest_path_id).clone();
                        for (child_tok, gc_list) in &variant_spec.child_authored_children {
                            let child_path = ref_path_obj.join(&[*child_tok]);
                            if let Some(child_path_id) = store.paths().lookup(&child_path)
                                && out.contains_key(&child_path_id)
                            {
                                let child_ns =
                                    u16::try_from(store.paths().resolve(child_path_id).depth())
                                        .unwrap_or(u16::MAX);
                                let remote_child_source = store
                                    .paths()
                                    .lookup(
                                        &store.paths().resolve(*remote_path_id).join(&[*child_tok]),
                                    )
                                    .unwrap_or(*remote_path_id);
                                authored_children_out
                                    .entry(child_path_id)
                                    .or_default()
                                    .push((
                                        OpinionKey {
                                            is_local: false,
                                            arc_kind: ArcKind::References,
                                            nested_arc_kind: Some(ArcKind::Variants),
                                            namespace_depth: child_ns,
                                            authored: true,
                                            arc_list_index,
                                            layer_strength,
                                            layer_id: remote_layer_id,
                                            lookup_path: *remote_path_id,
                                            spec_path: normalized_variant_spec_path(
                                                store,
                                                remote_child_source,
                                                &branch_selections,
                                                provenance_remap,
                                                &stronger_selections,
                                            ),
                                        },
                                        gc_list.clone(),
                                    ));
                            }
                        }

                        // Forward child_fields to child paths.
                        for (child_tok, child_fields) in &variant_spec.child_fields {
                            let child_path = ref_path_obj.join(&[*child_tok]);
                            if let Some(child_path_id) = store.paths().lookup(&child_path)
                                && out.contains_key(&child_path_id)
                            {
                                let child_ns =
                                    u16::try_from(store.paths().resolve(child_path_id).depth())
                                        .unwrap_or(u16::MAX);
                                let remote_child_source = store
                                    .paths()
                                    .lookup(
                                        &store.paths().resolve(*remote_path_id).join(&[*child_tok]),
                                    )
                                    .unwrap_or(*remote_path_id);
                                let child_outer = variant_spec
                                    .required_outer_variant_sites
                                    .get(child_tok)
                                    .cloned()
                                    .unwrap_or_else(|| variant_spec.outer_variant_sites.clone());
                                let child_selections = combined_variant_sites(
                                    &child_outer,
                                    VariantSelectionSite {
                                        host_path: *remote_path_id,
                                        set: *set,
                                        variant: *selected,
                                    },
                                );
                                for entry in child_fields {
                                    pending_fields.push((
                                        child_path_id,
                                        entry.name,
                                        OpinionKey {
                                            is_local: false,
                                            arc_kind: ArcKind::References,
                                            nested_arc_kind: Some(ArcKind::Variants),
                                            namespace_depth: child_ns,
                                            authored: true,
                                            arc_list_index,
                                            layer_strength,
                                            layer_id: remote_layer_id,
                                            lookup_path: *remote_path_id,
                                            spec_path: normalized_variant_property_spec_path(
                                                store,
                                                remote_child_source,
                                                &child_selections,
                                                entry.name,
                                                provenance_remap,
                                                &stronger_selections,
                                            ),
                                        },
                                        entry.value.clone(),
                                        entry.property_type.clone(),
                                        ref_offset,
                                    ));
                                }
                                out.get_mut(&child_path_id)
                                    .expect("path exists")
                                    .add_source(OpinionKey {
                                        is_local: false,
                                        arc_kind: ArcKind::References,
                                        nested_arc_kind: Some(ArcKind::Variants),
                                        namespace_depth: child_ns,
                                        authored: true,
                                        arc_list_index,
                                        layer_strength,
                                        layer_id: remote_layer_id,
                                        lookup_path: *remote_path_id,
                                        spec_path: normalized_variant_spec_path(
                                            store,
                                            remote_child_source,
                                            &child_selections,
                                            provenance_remap,
                                            &stronger_selections,
                                        ),
                                    });
                            }
                        }
                    }
                }
            }

            if let Some(order) = &remote_spec.prim_order {
                prim_order_out.entry(*dest_path_id).or_default().push((
                    OpinionKey {
                        is_local: false,
                        arc_kind: ArcKind::References,
                        nested_arc_kind: None,
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength,
                        layer_id: remote_layer_id,
                        lookup_path: *remote_path_id,
                        spec_path: prim_spec_path(
                            store,
                            *remote_path_id,
                            &remote_spec.outer_variant_sites,
                        ),
                    },
                    order.clone(),
                ));
            }

            if !remote_spec.authored_children.is_empty() {
                authored_children_out
                    .entry(*dest_path_id)
                    .or_default()
                    .push((
                        OpinionKey {
                            is_local: false,
                            arc_kind: ArcKind::References,
                            nested_arc_kind: None,
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: prim_spec_path(
                                store,
                                *remote_path_id,
                                &remote_spec.outer_variant_sites,
                            ),
                        },
                        remote_spec.authored_children.clone(),
                    ));
            }

            let selections = resolve_forwarded_variant_selections(
                store,
                stage_stack,
                *dest_path_id,
                &remote_stack,
                *remote_path_id,
            );
            for (set, selected) in &selections {
                if let Some(set_spec) = remote_spec.variant_sets.get(set)
                    && let Some(variant_spec) = set_spec.variants.get(selected)
                {
                    let branch_selections = combined_variant_sites(
                        &variant_spec.outer_variant_sites,
                        VariantSelectionSite {
                            host_path: *remote_path_id,
                            set: *set,
                            variant: *selected,
                        },
                    );
                    pending_sources.push((
                        *dest_path_id,
                        OpinionKey {
                            is_local: false,
                            arc_kind: ArcKind::References,
                            nested_arc_kind: Some(ArcKind::Variants),
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_variant_spec_path(
                                store,
                                *remote_path_id,
                                &branch_selections,
                                provenance_remap,
                                &stronger_selections,
                            ),
                        },
                    ));

                    for entry in &variant_spec.fields {
                        let key = OpinionKey {
                            is_local: false,
                            arc_kind: ArcKind::References,
                            nested_arc_kind: Some(ArcKind::Variants),
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_variant_property_spec_path(
                                store,
                                *remote_path_id,
                                &branch_selections,
                                entry.name,
                                provenance_remap,
                                &stronger_selections,
                            ),
                        };
                        let index = out.get_mut(dest_path_id).expect("path exists");
                        if let Some(property_type) = &entry.property_type {
                            index.add_property_type(entry.name, key.clone(), property_type.clone());
                        }
                        index.add_opinion(Opinion {
                            key,
                            field: entry.name,
                            value: entry.value.clone(),
                            layer_offset: ref_offset,
                        });
                    }

                    let payload_path_obj = store.paths().resolve(*dest_path_id).clone();
                    for (child_tok, gc_list) in &variant_spec.child_authored_children {
                        let child_path = payload_path_obj.join(&[*child_tok]);
                        if let Some(child_path_id) = store.paths().lookup(&child_path)
                            && out.contains_key(&child_path_id)
                        {
                            let child_ns =
                                u16::try_from(store.paths().resolve(child_path_id).depth())
                                    .unwrap_or(u16::MAX);
                            let remote_child_source = store
                                .paths()
                                .lookup(&store.paths().resolve(*remote_path_id).join(&[*child_tok]))
                                .unwrap_or(*remote_path_id);
                            authored_children_out
                                .entry(child_path_id)
                                .or_default()
                                .push((
                                    OpinionKey {
                                        is_local: false,
                                        arc_kind: ArcKind::References,
                                        nested_arc_kind: Some(ArcKind::Variants),
                                        namespace_depth: child_ns,
                                        authored: true,
                                        arc_list_index,
                                        layer_strength,
                                        layer_id: remote_layer_id,
                                        lookup_path: *remote_path_id,
                                        spec_path: normalized_variant_spec_path(
                                            store,
                                            remote_child_source,
                                            &branch_selections,
                                            provenance_remap,
                                            &stronger_selections,
                                        ),
                                    },
                                    gc_list.clone(),
                                ));
                        }
                    }

                    for (child_tok, child_fields) in &variant_spec.child_fields {
                        let child_path = payload_path_obj.join(&[*child_tok]);
                        if let Some(child_path_id) = store.paths().lookup(&child_path)
                            && out.contains_key(&child_path_id)
                        {
                            let child_ns =
                                u16::try_from(store.paths().resolve(child_path_id).depth())
                                    .unwrap_or(u16::MAX);
                            let remote_child_source = store
                                .paths()
                                .lookup(&store.paths().resolve(*remote_path_id).join(&[*child_tok]))
                                .unwrap_or(*remote_path_id);
                            let child_outer = variant_spec
                                .required_outer_variant_sites
                                .get(child_tok)
                                .cloned()
                                .unwrap_or_else(|| variant_spec.outer_variant_sites.clone());
                            let child_selections = combined_variant_sites(
                                &child_outer,
                                VariantSelectionSite {
                                    host_path: *remote_path_id,
                                    set: *set,
                                    variant: *selected,
                                },
                            );
                            out.get_mut(&child_path_id)
                                .expect("path exists")
                                .add_source(OpinionKey {
                                    is_local: false,
                                    arc_kind: ArcKind::References,
                                    nested_arc_kind: Some(ArcKind::Variants),
                                    namespace_depth: child_ns,
                                    authored: true,
                                    arc_list_index,
                                    layer_strength,
                                    layer_id: remote_layer_id,
                                    lookup_path: *remote_path_id,
                                    spec_path: normalized_variant_spec_path(
                                        store,
                                        remote_child_source,
                                        &child_selections,
                                        provenance_remap,
                                        &stronger_selections,
                                    ),
                                });
                            for entry in child_fields {
                                let key = OpinionKey {
                                    is_local: false,
                                    arc_kind: ArcKind::References,
                                    nested_arc_kind: Some(ArcKind::Variants),
                                    namespace_depth: child_ns,
                                    authored: true,
                                    arc_list_index,
                                    layer_strength,
                                    layer_id: remote_layer_id,
                                    lookup_path: *remote_path_id,
                                    spec_path: normalized_variant_property_spec_path(
                                        store,
                                        remote_child_source,
                                        &child_selections,
                                        entry.name,
                                        provenance_remap,
                                        &stronger_selections,
                                    ),
                                };
                                let index = out.get_mut(&child_path_id).expect("path exists");
                                if let Some(property_type) = &entry.property_type {
                                    index.add_property_type(
                                        entry.name,
                                        key.clone(),
                                        property_type.clone(),
                                    );
                                }
                                index.add_opinion(Opinion {
                                    key,
                                    field: entry.name,
                                    value: entry.value.clone(),
                                    layer_offset: ref_offset,
                                });
                            }
                        }
                    }
                }
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }
        for (dest_path_id, field, key, value, property_type, offset) in pending_fields {
            let value = remap_field_value_paths(store, &dest_root_path, &target_root, value);
            let index = out.get_mut(&dest_path_id).expect("path exists");
            if let Some(property_type) = property_type {
                index.add_property_type(field, key.clone(), property_type);
            }
            index.add_opinion(Opinion {
                key,
                field,
                value,
                layer_offset: offset,
            });
        }
    }

    for &(remote_path_id, dest_path_id) in &mapping {
        let inherits = resolve_inherits_for_prim(store, &remote_stack, remote_path_id);
        for (inherit_index, inherited_root) in inherits.into_iter().enumerate() {
            let inherit_index = u16::try_from(inherit_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // Inherit paths authored inside referenced content are translated
            // into the destination namespace (so local opinions on the
            // destination path participate).
            //
            // Spec: AOUSD Core §10 (references/inherits), via path translation
            // into the referencing namespace.
            let translated = remap_path_id(store, &dest_root_path, &target_root, inherited_root);
            let ref_remap = Some((&dest_root_path, &target_root));
            if translated != inherited_root {
                add_inherit_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    translated,
                    Some(ArcKind::References),
                    namespace_depth,
                    inherit_index,
                    out,
                    visited_inherits,
                    visited_specializes,
                    visited,
                    prim_order_out,
                    authored_children_out,
                    ref_remap,
                    None,
                    reference.layer_offset,
                    deps.as_deref_mut(),
                );
            }

            add_inherit_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                inherited_root,
                Some(ArcKind::References),
                namespace_depth,
                inherit_index,
                out,
                visited_inherits,
                visited_specializes,
                visited,
                prim_order_out,
                authored_children_out,
                ref_remap,
                None,
                reference.layer_offset,
                deps.as_deref_mut(),
            );
        }

        // Use direct refs only — variant branch and child refs are resolved
        // separately below with the combined_stack for proper selection handling.
        let nested = resolve_direct_references_for_prim(store, &remote_stack, remote_path_id);
        // Also resolve variant-scoped child references using combined_stack
        // for selections, so referencing layer's variant selections override
        // the referenced layer's defaults.
        let variant_child_refs =
            resolve_variant_child_references(store, &remote_stack, &combined_stack, remote_path_id);
        // Also resolve variant branch-level references (arcs on the variant
        // branch header itself, e.g. `"full" (add references = ...) {}`).
        let variant_branch_refs = resolve_variant_branch_references(
            store,
            &remote_stack,
            &combined_stack,
            remote_path_id,
        );
        let all_nested = nested
            .into_iter()
            .chain(variant_child_refs)
            .chain(variant_branch_refs);
        for (nested_index, nested_ref) in all_nested.enumerate() {
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_ref,
                namespace_depth,
                nested_index,
                out,
                visited,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                deps.as_deref_mut(),
            );
        }

        // Handle nested payloads inside referenced content.
        let nested_payloads = resolve_payloads_for_prim(store, &remote_stack, remote_path_id);
        for (nested_index, nested_payload) in nested_payloads.into_iter().enumerate() {
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_payload,
                namespace_depth,
                nested_index,
                out,
                visited,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                deps.as_deref_mut(),
            );
        }

        // Handle nested specializes inside referenced content.
        //
        // Spec: AOUSD Core §10 (specializes arcs within referenced layers
        // contribute opinions at the Specializes position, nested under
        // the References arc).
        let specializes = resolve_specializes_for_prim(store, &remote_stack, remote_path_id);
        for (spec_index, specialized_root) in specializes.into_iter().enumerate() {
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated = remap_path_id(store, &dest_root_path, &target_root, specialized_root);
            if translated != specialized_root {
                add_specializes_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    dest_path_id,
                    translated,
                    Some(ArcKind::References),
                    namespace_depth,
                    spec_index,
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    reference.layer_offset,
                    deps.as_deref_mut(),
                );
            }

            add_specializes_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                remote_path_id,
                specialized_root,
                Some(ArcKind::References),
                namespace_depth,
                spec_index,
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                reference.layer_offset,
                deps.as_deref_mut(),
            );
        }
    }

    // Late-copy accumulated sources after nested arc expansion. This lets
    // descendant paths pick up weaker opinions introduced while composing the
    // referenced namespace itself.
    for &(remote_path_id, dest_path_id) in &mapping {
        let src_index = out.get(&remote_path_id).cloned();
        if let Some(src_index) = src_index {
            let stronger_selections =
                resolve_full_variant_selections(store, stage_stack, dest_path_id);
            for source in &src_index.sources {
                if source.arc_kind == ArcKind::Local {
                    continue;
                }
                let spec_path = normalize_forwarded_spec_path(
                    store,
                    &source.spec_path,
                    provenance_remap,
                    &stronger_selections,
                );
                out.get_mut(&dest_path_id)
                    .expect("path exists")
                    .add_source(OpinionKey {
                        is_local: false,
                        arc_kind: ArcKind::References,
                        nested_arc_kind: Some(source.arc_kind),
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength: source.layer_strength,
                        layer_id: source.layer_id,
                        lookup_path: source.lookup_path,
                        spec_path,
                    });
            }
            for (field, opinions) in &src_index.opinions_by_field {
                for opinion in opinions {
                    if opinion.key.arc_kind == ArcKind::Local {
                        continue;
                    }
                    let spec_path = normalize_forwarded_spec_path(
                        store,
                        &opinion.key.spec_path,
                        provenance_remap,
                        &stronger_selections,
                    );
                    out.get_mut(&dest_path_id)
                        .expect("path exists")
                        .add_opinion(Opinion {
                            key: OpinionKey {
                                is_local: false,
                                arc_kind: ArcKind::References,
                                nested_arc_kind: Some(opinion.key.arc_kind),
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength: opinion.key.layer_strength,
                                layer_id: opinion.key.layer_id,
                                lookup_path: opinion.key.lookup_path,
                                spec_path,
                            },
                            field: *field,
                            value: opinion.value.clone(),
                            layer_offset: opinion.layer_offset,
                        });
                }
            }
        }
    }

    // Post-process: remap any PathListOp values in opinions on mapped
    // dest prims that still reference the source namespace. This covers
    // field values brought in by nested arcs (inherits, nested references)
    // within this reference context.
    for (_, dest_path_id) in &mapping {
        let Some(index) = out.get_mut(dest_path_id) else {
            continue;
        };
        for opinions in index.opinions_by_field.values_mut() {
            for opinion in opinions.iter_mut() {
                if matches!(opinion.value, FieldValue::PathListOp(_)) {
                    let old = core::mem::replace(
                        &mut opinion.value,
                        FieldValue::Value(crate::doc::Value::Null),
                    );
                    opinion.value =
                        remap_field_value_paths(store, &dest_root_path, &target_root, old);
                }
            }
        }
    }
}

fn add_payload_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Spec: AOUSD Core §10 (payloads arc, §5.1.22). Payloads are structurally
    // identical to references for composition purposes but sit at a weaker
    // position in LIVERPS (between References and Specializes).
    let mut visited: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_specializes: HashSet<(PathId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        let payloads = resolve_payloads_for_prim(store, local_stack, dest_root);
        // Also resolve variant branch-level payloads.
        let branch_payloads =
            resolve_variant_branch_payloads(store, local_stack, local_stack, dest_root);
        let all_payloads = payloads.into_iter().chain(branch_payloads);
        for (arc_list_index, payload) in all_payloads.enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            let Some(payload_path) = resolve_reference_target_path(store, &payload) else {
                continue;
            };
            if let Some(d) = deps.as_deref_mut() {
                d.add_arc(ArcDependency {
                    source: payload_path,
                    target: dest_root,
                    arc_kind: ArcKind::Payloads,
                    layer: payload.layer,
                });
            }
            add_payload_edge_opinions(
                store,
                local_stack,
                dest_root,
                payload,
                namespace_depth,
                arc_list_index,
                out,
                &mut visited,
                &mut visited_inherits,
                &mut visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                deps.as_deref_mut(),
            );
        }
    }
}

fn add_payload_edge_opinions(
    store: &mut dyn LayerStore,
    stage_stack: &LayerStack,
    dest_root: PathId,
    reference: Reference,
    namespace_depth: u16,
    arc_list_index: u16,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, LayerId, PathId)>,
    visited_inherits: &mut HashSet<(PathId, PathId)>,
    visited_specializes: &mut HashSet<(PathId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    provenance_remap: Option<(PathId, PathId)>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Payloads mirror reference edge opinions with ArcKind::Payloads.
    if !out.contains_key(&dest_root) {
        return;
    }
    let Some(reference_path) = resolve_reference_target_path(store, &reference) else {
        return;
    };
    if !visited.insert((dest_root, reference.layer, reference_path)) {
        return;
    }

    let remote_stack = LayerStack::gather(store, reference.layer);
    let combined_stack = LayerStack {
        layers: stage_stack
            .layers
            .iter()
            .copied()
            .chain(remote_stack.layers.iter().copied())
            .collect(),
        offsets: stage_stack
            .offsets
            .iter()
            .copied()
            .chain(remote_stack.offsets.iter().copied())
            .collect(),
    };
    let target_root = store.paths().resolve(reference_path).clone();
    let dest_root_path = store.paths().resolve(dest_root).clone();

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

    let mut mapping: Vec<(PathId, PathId)> = Vec::new();
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&target_root) else {
                continue;
            };
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(dest_root_path.join(&rel));
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }

    for (layer_strength_idx, remote_layer_id) in remote_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let payload_offset = reference
            .layer_offset
            .compose(remote_stack.offset_at(layer_strength_idx));
        let Some(remote_layer) = store.layer(remote_layer_id).cloned() else {
            continue;
        };

        let mut pending_sources = Vec::new();
        for (remote_path_id, dest_path_id) in &mapping {
            let Some(remote_spec) = remote_layer.prims.get(remote_path_id) else {
                continue;
            };
            let stronger_selections =
                resolve_full_variant_selections(store, stage_stack, *dest_path_id);
            if let Some(d) = deps.as_deref_mut() {
                d.add_layer_opinion(remote_layer_id, *dest_path_id);
            }
            pending_sources.push((
                *dest_path_id,
                OpinionKey {
                    is_local: false,
                    arc_kind: ArcKind::Payloads,
                    nested_arc_kind: None,
                    namespace_depth,
                    authored: true,
                    arc_list_index,
                    layer_strength,
                    layer_id: remote_layer_id,
                    lookup_path: *remote_path_id,
                    spec_path: normalized_prim_spec_path(
                        store,
                        *remote_path_id,
                        &remote_spec.outer_variant_sites,
                        provenance_remap,
                        &stronger_selections,
                    ),
                },
            ));

            for entry in &remote_spec.fields {
                let key = OpinionKey {
                    is_local: false,
                    arc_kind: ArcKind::Payloads,
                    nested_arc_kind: None,
                    namespace_depth,
                    authored: true,
                    arc_list_index,
                    layer_strength,
                    layer_id: remote_layer_id,
                    lookup_path: *remote_path_id,
                    spec_path: normalized_property_spec_path(
                        store,
                        *remote_path_id,
                        &remote_spec.outer_variant_sites,
                        entry.name,
                        provenance_remap,
                        &stronger_selections,
                    ),
                };
                let index = out.get_mut(dest_path_id).expect("path exists");
                if let Some(property_type) = &entry.property_type {
                    index.add_property_type(entry.name, key.clone(), property_type.clone());
                }
                index.add_opinion(Opinion {
                    key,
                    field: entry.name,
                    value: entry.value.clone(),
                    layer_offset: payload_offset,
                });
            }

            if let Some(order) = &remote_spec.prim_order {
                prim_order_out.entry(*dest_path_id).or_default().push((
                    OpinionKey {
                        is_local: false,
                        arc_kind: ArcKind::Payloads,
                        nested_arc_kind: None,
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength,
                        layer_id: remote_layer_id,
                        lookup_path: *remote_path_id,
                        spec_path: normalized_prim_spec_path(
                            store,
                            *remote_path_id,
                            &remote_spec.outer_variant_sites,
                            provenance_remap,
                            &stronger_selections,
                        ),
                    },
                    order.clone(),
                ));
            }

            if !remote_spec.authored_children.is_empty() {
                authored_children_out
                    .entry(*dest_path_id)
                    .or_default()
                    .push((
                        OpinionKey {
                            is_local: false,
                            arc_kind: ArcKind::Payloads,
                            nested_arc_kind: None,
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_prim_spec_path(
                                store,
                                *remote_path_id,
                                &remote_spec.outer_variant_sites,
                                provenance_remap,
                                &stronger_selections,
                            ),
                        },
                        remote_spec.authored_children.clone(),
                    ));
            }

            let selections = resolve_forwarded_variant_selections(
                store,
                stage_stack,
                *dest_path_id,
                &remote_stack,
                *remote_path_id,
            );
            for (set, selected) in &selections {
                if let Some(set_spec) = remote_spec.variant_sets.get(set)
                    && let Some(variant_spec) = set_spec.variants.get(selected)
                {
                    let branch_selections = combined_variant_sites(
                        &variant_spec.outer_variant_sites,
                        VariantSelectionSite {
                            host_path: *remote_path_id,
                            set: *set,
                            variant: *selected,
                        },
                    );
                    pending_sources.push((
                        *dest_path_id,
                        OpinionKey {
                            is_local: false,
                            arc_kind: ArcKind::Payloads,
                            nested_arc_kind: Some(ArcKind::Variants),
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_variant_spec_path(
                                store,
                                *remote_path_id,
                                &branch_selections,
                                provenance_remap,
                                &stronger_selections,
                            ),
                        },
                    ));

                    for entry in &variant_spec.fields {
                        let key = OpinionKey {
                            is_local: false,
                            arc_kind: ArcKind::Payloads,
                            nested_arc_kind: Some(ArcKind::Variants),
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id: remote_layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_variant_property_spec_path(
                                store,
                                *remote_path_id,
                                &branch_selections,
                                entry.name,
                                provenance_remap,
                                &stronger_selections,
                            ),
                        };
                        let index = out.get_mut(dest_path_id).expect("path exists");
                        if let Some(property_type) = &entry.property_type {
                            index.add_property_type(entry.name, key.clone(), property_type.clone());
                        }
                        index.add_opinion(Opinion {
                            key,
                            field: entry.name,
                            value: entry.value.clone(),
                            layer_offset: payload_offset,
                        });
                    }
                }
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }
    }

    // Handle nested arcs inside payload targets.
    for (remote_path_id, dest_path_id) in mapping {
        let inherits = resolve_inherits_for_prim(store, &remote_stack, remote_path_id);
        for (inherit_index, inherited_root) in inherits.into_iter().enumerate() {
            let inherit_index = u16::try_from(inherit_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated = remap_path_id(store, &dest_root_path, &target_root, inherited_root);
            let ref_remap = Some((&dest_root_path, &target_root));
            if translated != inherited_root {
                add_inherit_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    translated,
                    Some(ArcKind::Payloads),
                    namespace_depth,
                    inherit_index,
                    out,
                    visited_inherits,
                    visited_specializes,
                    visited,
                    prim_order_out,
                    authored_children_out,
                    ref_remap,
                    None,
                    reference.layer_offset,
                    deps.as_deref_mut(),
                );
            }

            add_inherit_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                inherited_root,
                Some(ArcKind::Payloads),
                namespace_depth,
                inherit_index,
                out,
                visited_inherits,
                visited_specializes,
                visited,
                prim_order_out,
                authored_children_out,
                ref_remap,
                None,
                reference.layer_offset,
                deps.as_deref_mut(),
            );
        }

        // Use direct refs only — variant branch refs are handled by
        // add_reference_edge_opinions internally with proper selection stacks.
        let nested = resolve_direct_references_for_prim(store, &remote_stack, remote_path_id);
        for (nested_index, nested_ref) in nested.into_iter().enumerate() {
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_reference_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_ref,
                namespace_depth,
                nested_index,
                out,
                visited,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                deps.as_deref_mut(),
            );
        }

        // Handle nested payloads inside payload targets.
        let nested_payloads = resolve_payloads_for_prim(store, &remote_stack, remote_path_id);
        for (nested_index, nested_payload) in nested_payloads.into_iter().enumerate() {
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            add_payload_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                nested_payload,
                namespace_depth,
                nested_index,
                out,
                visited,
                visited_inherits,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                deps.as_deref_mut(),
            );
        }

        // Handle nested specializes inside payload targets.
        let specializes = resolve_specializes_for_prim(store, &remote_stack, remote_path_id);
        for (spec_index, specialized_root) in specializes.into_iter().enumerate() {
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated = remap_path_id(store, &dest_root_path, &target_root, specialized_root);
            if translated != specialized_root {
                add_specializes_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    dest_path_id,
                    translated,
                    Some(ArcKind::Payloads),
                    namespace_depth,
                    spec_index,
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                    None,
                    reference.layer_offset,
                    deps.as_deref_mut(),
                );
            }

            add_specializes_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                remote_path_id,
                specialized_root,
                Some(ArcKind::Payloads),
                namespace_depth,
                spec_index,
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
                None,
                reference.layer_offset,
                deps.as_deref_mut(),
            );
        }
    }
}

fn add_specializes_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    paths: &BTreeSet<PathId>,
    out: &mut HashMap<PathId, PrimIndex>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    mut deps: Option<&mut DependencyBuilder>,
) {
    // Spec: AOUSD Core §10 (specializes arc, §5.1.33). Specializes mirrors
    // inherits but sits at the weakest position in LIVERPS.
    let mut visited: HashSet<(PathId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        let specializes = resolve_specializes_for_prim(store, local_stack, dest_root);
        for (arc_list_index, specialized_root) in specializes.into_iter().enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
            if let Some(d) = deps.as_deref_mut() {
                d.add_arc(ArcDependency {
                    source: specialized_root,
                    target: dest_root,
                    arc_kind: ArcKind::Specializes,
                    layer: local_stack.layers[0],
                });
            }
            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_root,
                dest_root,
                specialized_root,
                None,
                namespace_depth,
                arc_list_index,
                out,
                &mut visited,
                prim_order_out,
                authored_children_out,
                None,
                LayerOffset::IDENTITY,
                deps.as_deref_mut(),
            );
        }
    }
}

/// Specializes edge opinions mirror inherits but use [`ArcKind::Specializes`].
fn add_specializes_edge_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    dest_root: PathId,
    selection_root: PathId,
    specialized_root: PathId,
    outer_arc_kind: Option<ArcKind>,
    namespace_depth: u16,
    arc_list_index: u16,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    provenance_remap: Option<(PathId, PathId)>,
    // Accumulated offset from outer arcs (references/payloads).
    base_offset: LayerOffset,
    mut deps: Option<&mut DependencyBuilder>,
) {
    if !visited.insert((dest_root, specialized_root)) {
        return;
    }

    let base_path = store.paths().resolve(dest_root).clone();
    let selection_base_path = store.paths().resolve(selection_root).clone();
    let specialized_path = store.paths().resolve(specialized_root).clone();

    let mut remote_paths: Vec<PathId> = local_stack
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

    let mut mapping: Vec<(PathId, PathId)> = Vec::new();
    for remote_path_id in remote_paths {
        let rel: Vec<_> = {
            let remote_path = store.paths().resolve(remote_path_id);
            let Some(rel) = remote_path.strip_prefix(&specialized_path) else {
                continue;
            };
            rel.to_vec()
        };
        let dest_path_id = store.paths_mut().intern(base_path.join(&rel));
        if out.contains_key(&dest_path_id) {
            mapping.push((remote_path_id, dest_path_id));
        }
    }

    let (arc_kind, nested_arc_kind) = match outer_arc_kind {
        Some(outer) => (outer, Some(ArcKind::Specializes)),
        None => (ArcKind::Specializes, None),
    };

    for (layer_strength_idx, layer_id) in local_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength_idx).unwrap_or(u16::MAX);
        let layer_offset = base_offset.compose(local_stack.offset_at(layer_strength_idx));
        let mut pending: Vec<(
            PathId,
            PathId,
            SpecPath,
            TokenId,
            FieldValue,
            Option<PropertyType>,
        )> = Vec::new();
        let mut pending_sources = Vec::new();
        {
            let Some(layer) = store.layer(layer_id).cloned() else {
                continue;
            };

            for (remote_path_id, dest_path_id) in &mapping {
                let Some(spec) = layer.prims.get(remote_path_id) else {
                    continue;
                };
                let selection_path_id = {
                    let rel = store
                        .paths()
                        .resolve(*remote_path_id)
                        .strip_prefix(&specialized_path)
                        .expect("mapping source should stay under specialized root")
                        .to_vec();
                    store
                        .paths()
                        .lookup(&selection_base_path.join(&rel))
                        .unwrap_or(selection_root)
                };
                let stronger_selections =
                    resolve_full_variant_selections(store, local_stack, selection_path_id);
                if let Some(d) = deps.as_deref_mut() {
                    d.add_layer_opinion(layer_id, *dest_path_id);
                }
                if let Some(order) = &spec.prim_order {
                    prim_order_out.entry(*dest_path_id).or_default().push((
                        OpinionKey {
                            is_local: false,
                            arc_kind,
                            nested_arc_kind,
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id,
                            lookup_path: *remote_path_id,
                            spec_path: normalized_prim_spec_path(
                                store,
                                *remote_path_id,
                                &spec.outer_variant_sites,
                                provenance_remap,
                                &stronger_selections,
                            ),
                        },
                        order.clone(),
                    ));
                }

                if !spec.authored_children.is_empty() {
                    authored_children_out
                        .entry(*dest_path_id)
                        .or_default()
                        .push((
                            OpinionKey {
                                is_local: false,
                                arc_kind,
                                nested_arc_kind,
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength,
                                layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_prim_spec_path(
                                    store,
                                    *remote_path_id,
                                    &spec.outer_variant_sites,
                                    provenance_remap,
                                    &stronger_selections,
                                ),
                            },
                            spec.authored_children.clone(),
                        ));
                }

                pending_sources.push((
                    *dest_path_id,
                    OpinionKey {
                        is_local: false,
                        arc_kind,
                        nested_arc_kind,
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength,
                        layer_id,
                        lookup_path: *remote_path_id,
                        spec_path: normalized_prim_spec_path(
                            store,
                            *remote_path_id,
                            &spec.outer_variant_sites,
                            provenance_remap,
                            &stronger_selections,
                        ),
                    },
                ));
                for entry in &spec.fields {
                    pending.push((
                        *dest_path_id,
                        *remote_path_id,
                        normalized_property_spec_path(
                            store,
                            *remote_path_id,
                            &spec.outer_variant_sites,
                            entry.name,
                            provenance_remap,
                            &stronger_selections,
                        ),
                        entry.name,
                        entry.value.clone(),
                        entry.property_type.clone(),
                    ));
                }

                // Forward variant opinions from selected variants through specializes.
                let spec_selections = resolve_forwarded_variant_selections(
                    store,
                    local_stack,
                    selection_path_id,
                    local_stack,
                    *remote_path_id,
                );
                for (set, selected) in &spec_selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected)
                    {
                        let branch_selections = combined_variant_sites(
                            &variant_spec.outer_variant_sites,
                            VariantSelectionSite {
                                host_path: *remote_path_id,
                                set: *set,
                                variant: *selected,
                            },
                        );
                        pending_sources.push((
                            *dest_path_id,
                            OpinionKey {
                                is_local: false,
                                arc_kind,
                                nested_arc_kind: nested_arc_kind.or(Some(ArcKind::Variants)),
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength,
                                layer_id,
                                lookup_path: *remote_path_id,
                                spec_path: normalized_variant_spec_path(
                                    store,
                                    *remote_path_id,
                                    &branch_selections,
                                    provenance_remap,
                                    &stronger_selections,
                                ),
                            },
                        ));
                        for entry in &variant_spec.fields {
                            pending.push((
                                *dest_path_id,
                                *remote_path_id,
                                normalized_variant_property_spec_path(
                                    store,
                                    *remote_path_id,
                                    &branch_selections,
                                    entry.name,
                                    provenance_remap,
                                    &stronger_selections,
                                ),
                                entry.name,
                                entry.value.clone(),
                                entry.property_type.clone(),
                            ));
                        }
                    }
                }
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }

        for (dest_path_id, remote_path_id, spec_path, field, value, property_type) in pending {
            let value = remap_field_value_paths(store, &base_path, &specialized_path, value);
            let key = OpinionKey {
                is_local: false,
                arc_kind,
                nested_arc_kind,
                namespace_depth,
                authored: true,
                arc_list_index,
                layer_strength,
                layer_id,
                lookup_path: remote_path_id,
                spec_path,
            };
            let index = out.get_mut(&dest_path_id).expect("path exists");
            if let Some(property_type) = property_type {
                index.add_property_type(field, key.clone(), property_type);
            }
            index.add_opinion(Opinion {
                key,
                field,
                value,
                layer_offset,
            });
        }
    }

    // Propagate already-accumulated PrimIndex sources from mapped source
    // paths to dest paths — mirrors the same logic in add_inherit_edge_opinions.
    for &(remote_path_id, dest_path_id) in &mapping {
        let src_index = out.get(&remote_path_id).cloned();
        if let Some(src_index) = src_index {
            let selection_path_id = {
                let rel = store
                    .paths()
                    .resolve(remote_path_id)
                    .strip_prefix(&specialized_path)
                    .expect("mapping source should stay under specialized root")
                    .to_vec();
                store
                    .paths()
                    .lookup(&selection_base_path.join(&rel))
                    .unwrap_or(selection_root)
            };
            let stronger_selections =
                resolve_full_variant_selections(store, local_stack, selection_path_id);
            for source in &src_index.sources {
                if source.arc_kind == ArcKind::Local {
                    continue;
                }
                let spec_path = normalize_forwarded_spec_path(
                    store,
                    &source.spec_path,
                    provenance_remap,
                    &stronger_selections,
                );
                out.get_mut(&dest_path_id)
                    .expect("path exists")
                    .add_source(OpinionKey {
                        is_local: false,
                        arc_kind,
                        nested_arc_kind: Some(source.arc_kind),
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength: source.layer_strength,
                        layer_id: source.layer_id,
                        lookup_path: source.lookup_path,
                        spec_path,
                    });
            }
            for (field, opinions) in &src_index.opinions_by_field {
                for opinion in opinions {
                    if opinion.key.arc_kind == ArcKind::Local {
                        continue;
                    }
                    let spec_path = normalize_forwarded_spec_path(
                        store,
                        &opinion.key.spec_path,
                        provenance_remap,
                        &stronger_selections,
                    );
                    out.get_mut(&dest_path_id)
                        .expect("path exists")
                        .add_opinion(Opinion {
                            key: OpinionKey {
                                is_local: false,
                                arc_kind,
                                nested_arc_kind: Some(opinion.key.arc_kind),
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength: opinion.key.layer_strength,
                                layer_id: opinion.key.layer_id,
                                lookup_path: opinion.key.lookup_path,
                                spec_path,
                            },
                            field: *field,
                            value: opinion.value.clone(),
                            layer_offset: opinion.layer_offset,
                        });
                }
            }
        }
    }

    // Handle nested specializes arcs.
    for &(remote_path_id, dest_path_id) in &mapping {
        let selection_path_id = {
            let rel = store
                .paths()
                .resolve(remote_path_id)
                .strip_prefix(&specialized_path)
                .expect("mapping source should stay under specialized root")
                .to_vec();
            store
                .paths()
                .lookup(&selection_base_path.join(&rel))
                .unwrap_or(selection_root)
        };
        let nested_specializes = resolve_specializes_for_prim(store, local_stack, remote_path_id);
        for (nested_index, nested) in nested_specializes.into_iter().enumerate() {
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated = remap_path_id(store, &base_path, &specialized_path, nested);
            if translated != nested {
                add_specializes_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    selection_path_id,
                    translated,
                    outer_arc_kind,
                    namespace_depth,
                    nested_index,
                    out,
                    visited,
                    prim_order_out,
                    authored_children_out,
                    None,
                    base_offset,
                    deps.as_deref_mut(),
                );
            }

            // Parent-level remap for sibling specializes targets.
            if let (Some(base_parent), Some(specialized_parent)) =
                (base_path.parent(), specialized_path.parent())
            {
                let parent_translated =
                    remap_path_id(store, &base_parent, &specialized_parent, nested);
                if parent_translated != translated && parent_translated != nested {
                    add_specializes_edge_opinions(
                        store,
                        local_stack,
                        dest_path_id,
                        selection_path_id,
                        parent_translated,
                        outer_arc_kind,
                        namespace_depth,
                        nested_index,
                        out,
                        visited,
                        prim_order_out,
                        authored_children_out,
                        None,
                        base_offset,
                        deps.as_deref_mut(),
                    );
                }
            }

            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                selection_path_id,
                nested,
                outer_arc_kind,
                namespace_depth,
                nested_index,
                out,
                visited,
                prim_order_out,
                authored_children_out,
                None,
                base_offset,
                deps.as_deref_mut(),
            );
        }
    }

    // Propagate inherits from the specialized class.
    //
    // Specializes propagates through all levels of referencing per the spec.
    // When a specialized class inherits from other classes, those classes
    // form a hierarchy that is also propagated. Their opinions remain weaker
    // than the specialized class but still participate.
    //
    // Spec: AOUSD Core §10 (specializes arc propagation).
    for &(remote_path_id, dest_path_id) in &mapping {
        let selection_path_id = {
            let rel = store
                .paths()
                .resolve(remote_path_id)
                .strip_prefix(&specialized_path)
                .expect("mapping source should stay under specialized root")
                .to_vec();
            store
                .paths()
                .lookup(&selection_base_path.join(&rel))
                .unwrap_or(selection_root)
        };
        let nested_inherits = resolve_inherits_for_prim(store, local_stack, remote_path_id);
        for (nested_index, inherited) in nested_inherits.into_iter().enumerate() {
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated = remap_path_id(store, &base_path, &specialized_path, inherited);
            if translated != inherited {
                add_specializes_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    selection_path_id,
                    translated,
                    outer_arc_kind,
                    namespace_depth,
                    nested_index,
                    out,
                    visited,
                    prim_order_out,
                    authored_children_out,
                    None,
                    base_offset,
                    deps.as_deref_mut(),
                );
            }

            // Also try parent-level remap. This handles the case where
            // an inherited class is a sibling of the specialized class (e.g.
            // /Looks/Metal specializes, /Looks/Material inherited — they share
            // /Looks). The parent remap uses the reference namespace mapping
            // to find the correct translated path (e.g. /Model/Looks/Material).
            if let (Some(base_parent), Some(specialized_parent)) =
                (base_path.parent(), specialized_path.parent())
            {
                let parent_translated =
                    remap_path_id(store, &base_parent, &specialized_parent, inherited);
                if parent_translated != translated && parent_translated != inherited {
                    add_specializes_edge_opinions(
                        store,
                        local_stack,
                        dest_path_id,
                        selection_path_id,
                        parent_translated,
                        outer_arc_kind,
                        namespace_depth,
                        nested_index,
                        out,
                        visited,
                        prim_order_out,
                        authored_children_out,
                        None,
                        base_offset,
                        deps.as_deref_mut(),
                    );
                }
            }

            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                selection_path_id,
                inherited,
                outer_arc_kind,
                namespace_depth,
                nested_index,
                out,
                visited,
                prim_order_out,
                authored_children_out,
                None,
                base_offset,
                deps.as_deref_mut(),
            );
        }
    }

    // Propagate references from the specialized class.
    //
    // When a specialized class references other prims, those referenced
    // opinions propagate at specializes strength. This handles cases like
    // ShinyPlastic_BlueShinyPlastic specializes ShinyPlastic which
    // references ShinyPlasticLook.
    //
    // Spec: AOUSD Core §10 (specializes propagation through all arcs).
    let mut visited_refs: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    for &(remote_path_id, dest_path_id) in &mapping {
        let refs = resolve_references_for_prim(store, local_stack, remote_path_id);
        for (ref_index, reference) in refs.into_iter().enumerate() {
            let ref_index = u16::try_from(ref_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            add_reference_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                reference,
                namespace_depth,
                ref_index,
                out,
                &mut visited_refs,
                &mut visited_inherits,
                visited,
                prim_order_out,
                authored_children_out,
                None,
                deps.as_deref_mut(),
            );
        }
    }

    // Late-copy accumulated sources after nested arc propagation so specializes
    // can inherit weaker referenced opinions authored on the specialized prim.
    for &(remote_path_id, dest_path_id) in &mapping {
        let selection_path_id = {
            let rel = store
                .paths()
                .resolve(remote_path_id)
                .strip_prefix(&specialized_path)
                .expect("mapping source should stay under specialized root")
                .to_vec();
            store
                .paths()
                .lookup(&selection_base_path.join(&rel))
                .unwrap_or(selection_root)
        };
        let stronger_selections =
            resolve_full_variant_selections(store, local_stack, selection_path_id);
        let src_index = out.get(&remote_path_id).cloned();
        if let Some(src_index) = src_index {
            for source in &src_index.sources {
                if source.arc_kind == ArcKind::Local {
                    continue;
                }
                let spec_path = normalize_forwarded_spec_path(
                    store,
                    &source.spec_path,
                    provenance_remap,
                    &stronger_selections,
                );
                out.get_mut(&dest_path_id)
                    .expect("path exists")
                    .add_source(OpinionKey {
                        is_local: false,
                        arc_kind,
                        nested_arc_kind: Some(source.arc_kind),
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength: source.layer_strength,
                        layer_id: source.layer_id,
                        lookup_path: source.lookup_path,
                        spec_path,
                    });
            }
            for (field, opinions) in &src_index.opinions_by_field {
                for opinion in opinions {
                    if opinion.key.arc_kind == ArcKind::Local {
                        continue;
                    }
                    let spec_path = normalize_forwarded_spec_path(
                        store,
                        &opinion.key.spec_path,
                        provenance_remap,
                        &stronger_selections,
                    );
                    out.get_mut(&dest_path_id)
                        .expect("path exists")
                        .add_opinion(Opinion {
                            key: OpinionKey {
                                is_local: false,
                                arc_kind,
                                nested_arc_kind: Some(opinion.key.arc_kind),
                                namespace_depth,
                                authored: true,
                                arc_list_index,
                                layer_strength: opinion.key.layer_strength,
                                layer_id: opinion.key.layer_id,
                                lookup_path: opinion.key.lookup_path,
                                spec_path,
                            },
                            field: *field,
                            value: opinion.value.clone(),
                            layer_offset: opinion.layer_offset,
                        });
                }
            }
        }
    }
}

fn apply_child_order(
    store: &dyn LayerStore,
    authored_children: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    prim_order: &HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    children: &mut HashMap<PathId, Vec<PathId>>,
) {
    for (parent, list) in children.iter_mut() {
        if let Some(opinions) = authored_children.get(parent) {
            apply_authored_children_base_order(store, list, opinions);
        }
        if let Some(opinions) = prim_order.get(parent) {
            apply_prim_order_chain(store, list, opinions);
        };
    }
}

fn apply_authored_children_base_order(
    store: &dyn LayerStore,
    children: &mut Vec<PathId>,
    opinions: &[(OpinionKey, Vec<TokenId>)],
) {
    // Builds child ordering by processing opinions weakest-first (the weakest
    // source establishes the baseline child order; stronger sources append
    // unique children).
    //
    // The ordering groups opinions by layer, using the combined-stack position
    // derived from inherit/specializes opinions (which walk the full combined
    // stack and carry the correct `layer_strength`). Within each layer group,
    // local opinions come first, then direct opinions, then nested (inherit)
    // opinions.
    //
    // Spec: AOUSD Core §11 (stage population) and supplemental suite composition
    // fixtures that rely on authoring order in referenced layers.
    let mut by_name = HashMap::<TokenId, PathId>::new();
    for child in children.iter().copied() {
        if let Some(name) = store.paths().resolve(child).leaf() {
            by_name.insert(name, child);
        }
    }

    // Build a layer ordering map from inherit/specializes opinions. These
    // opinions walk the combined stack and their `layer_strength` reflects the
    // correct position of each layer in the unified composition order.
    let mut layer_position: HashMap<LayerId, u16> = HashMap::new();
    for (key, _) in opinions {
        if key.nested_arc_kind.is_some() {
            layer_position
                .entry(key.layer_id)
                .and_modify(|pos| *pos = (*pos).min(key.layer_strength))
                .or_insert(key.layer_strength);
        }
    }

    // For layers not seen in inherit opinions, assign a position based on
    // namespace_depth (deeper introduction site = more deeply nested reference
    // = weaker, i.e. higher position number).
    let max_inherit_pos = layer_position.values().copied().max().unwrap_or(0);
    for (key, _) in opinions {
        layer_position.entry(key.layer_id).or_insert_with(|| {
            if key.is_local {
                0
            } else {
                // Place after all inherit-discovered layers, offset by
                // namespace_depth to preserve relative ordering among
                // layers that only have direct reference opinions.
                max_inherit_pos + 1 + key.namespace_depth
            }
        });
    }

    // Sort opinions strongest-first: by layer position (lower = stronger),
    // then local > direct > nested within the same layer.
    let mut sorted: Vec<_> = opinions.iter().collect();
    sorted.sort_by(|a, b| {
        let pos_a = layer_position
            .get(&a.0.layer_id)
            .copied()
            .unwrap_or(u16::MAX);
        let pos_b = layer_position
            .get(&b.0.layer_id)
            .copied()
            .unwrap_or(u16::MAX);
        let pos = pos_a.cmp(&pos_b);
        if pos != Ordering::Equal {
            return pos;
        }
        // Within the same layer: local first, then direct, then nested.
        match (a.0.is_local, b.0.is_local) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        match (a.0.nested_arc_kind, b.0.nested_arc_kind) {
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            _ => {}
        }
        a.0.layer_strength.cmp(&b.0.layer_strength)
    });

    let mut out = Vec::new();
    let mut seen = HashSet::<PathId>::new();

    // Process weakest-first.
    for (_key, names) in sorted.iter().rev() {
        for name in names.iter() {
            let Some(child_id) = by_name.get(name).copied() else {
                continue;
            };
            if seen.insert(child_id) {
                out.push(child_id);
            }
        }
    }

    // Append remaining children not covered by any opinion.
    for child_id in children.iter().copied() {
        if seen.insert(child_id) {
            out.push(child_id);
        }
    }

    *children = out;
}

fn apply_prim_order_chain(
    store: &dyn LayerStore,
    children: &mut Vec<PathId>,
    opinions: &[(OpinionKey, Vec<TokenId>)],
) {
    // `reorder nameChildren = [...]` composes as a chain of reorder operations
    // across the prim stack (weak-to-strong), rather than as a single strongest
    // scalar field.
    //
    // This matches the supplemental composition fixtures (e.g.
    // `BasicListEditing_root`).
    let mut sorted = opinions.to_vec();
    sorted.sort_by(|a, b| a.0.cmp_strongest_first(&b.0));
    for (_, order) in sorted.into_iter().rev() {
        apply_reorder_op(store, children, &order);
    }
}

fn apply_reorder_op(store: &dyn LayerStore, children: &mut Vec<PathId>, order: &[TokenId]) {
    let mut by_name = HashMap::<TokenId, PathId>::new();
    for child in children.iter().copied() {
        if let Some(name) = store.paths().resolve(child).leaf() {
            by_name.insert(name, child);
        }
    }

    let order: Vec<TokenId> = order
        .iter()
        .copied()
        .filter(|name| by_name.contains_key(name))
        .collect();
    let Some((&first, rest)) = order.split_first() else {
        return;
    };

    let order_set: HashSet<TokenId> = order.iter().copied().collect();
    let mut prefix = Vec::new();
    let mut segments: HashMap<TokenId, Vec<PathId>> = HashMap::new();
    let mut current = None;
    for child in children.iter().copied() {
        let Some(name) = store.paths().resolve(child).leaf() else {
            continue;
        };
        if order_set.contains(&name) {
            segments.entry(name).or_default();
            current = Some(name);
        } else if let Some(owner) = current {
            segments.entry(owner).or_default().push(child);
        } else {
            prefix.push(child);
        }
    }

    let mut out = Vec::with_capacity(children.len());
    out.push(by_name[&first]);
    out.extend(prefix);
    if let Some(seg) = segments.get(&first) {
        out.extend(seg.iter().copied());
    }

    for name in rest {
        out.push(by_name[name]);
        if let Some(seg) = segments.get(name) {
            out.extend(seg.iter().copied());
        }
    }

    // Preserve any remaining children (shouldn't happen if `prefix+segments`
    // covered everything, but keep deterministic behavior for partial lists).
    let mut seen = HashSet::<PathId>::new();
    for id in out.iter().copied() {
        seen.insert(id);
    }
    for id in children.iter().copied() {
        if seen.insert(id) {
            out.push(id);
        }
    }

    *children = out;
}

#[cfg(test)]
mod child_order_tests {
    extern crate std;
    use super::*;
    use crate::doc::{InMemoryStore, Layer, PrimSpec, VariantSetSpec, VariantSpec};
    use crate::path::Path;
    use crate::prim_index::OpinionKey;
    use alloc::vec;

    #[test]
    fn test_layer_grouped_sort() {
        let mut store = InMemoryStore::default();
        let root_layer = LayerId(1);
        let set_layer = LayerId(2);
        let prop_layer = LayerId(3);

        let from_root = store.tokens.intern("From_root");
        let from_set = store.tokens.intern("From_set");
        let from_class_root = store.tokens.intern("From_class_in_root");
        let from_class_set = store.tokens.intern("From_class_in_set");
        let geom_tok = store.tokens.intern("geom");

        let c_geom = store
            .paths
            .intern(Path::parse_absolute("/P/I/geom", &mut store.tokens).unwrap());
        let c_fr = store
            .paths
            .intern(Path::parse_absolute("/P/I/From_root", &mut store.tokens).unwrap());
        let c_fs = store
            .paths
            .intern(Path::parse_absolute("/P/I/From_set", &mut store.tokens).unwrap());
        let c_fcr = store
            .paths
            .intern(Path::parse_absolute("/P/I/From_class_in_root", &mut store.tokens).unwrap());
        let c_fcs = store
            .paths
            .intern(Path::parse_absolute("/P/I/From_class_in_set", &mut store.tokens).unwrap());

        let sp_local = store
            .paths
            .intern(Path::parse_absolute("/P/I", &mut store.tokens).unwrap());
        let sp_set = store
            .paths
            .intern(Path::parse_absolute("/S/I", &mut store.tokens).unwrap());
        let sp_prop = store
            .paths
            .intern(Path::parse_absolute("/Prop", &mut store.tokens).unwrap());
        let sp_class = store
            .paths
            .intern(Path::parse_absolute("/_C", &mut store.tokens).unwrap());

        let opinions: Vec<(OpinionKey, Vec<TokenId>)> = vec![
            (
                OpinionKey {
                    is_local: true,
                    arc_kind: ArcKind::Local,
                    nested_arc_kind: None,
                    namespace_depth: 2,
                    authored: true,
                    arc_list_index: 0,
                    layer_strength: 0,
                    layer_id: root_layer,
                    lookup_path: sp_local,
                    spec_path: prim_spec_path(&store, sp_local, &[]),
                },
                vec![from_root, geom_tok],
            ),
            (
                OpinionKey {
                    is_local: false,
                    arc_kind: ArcKind::References,
                    nested_arc_kind: None,
                    namespace_depth: 1,
                    authored: true,
                    arc_list_index: 0,
                    layer_strength: 0,
                    layer_id: set_layer,
                    lookup_path: sp_set,
                    spec_path: prim_spec_path(&store, sp_set, &[]),
                },
                vec![from_set, geom_tok],
            ),
            (
                OpinionKey {
                    is_local: false,
                    arc_kind: ArcKind::References,
                    nested_arc_kind: None,
                    namespace_depth: 2,
                    authored: true,
                    arc_list_index: 0,
                    layer_strength: 0,
                    layer_id: prop_layer,
                    lookup_path: sp_prop,
                    spec_path: prim_spec_path(&store, sp_prop, &[]),
                },
                vec![geom_tok],
            ),
            (
                OpinionKey {
                    is_local: false,
                    arc_kind: ArcKind::References,
                    nested_arc_kind: Some(ArcKind::Inherits),
                    namespace_depth: 2,
                    authored: true,
                    arc_list_index: 0,
                    layer_strength: 0,
                    layer_id: root_layer,
                    lookup_path: sp_class,
                    spec_path: prim_spec_path(&store, sp_class, &[]),
                },
                vec![from_class_root, geom_tok],
            ),
            (
                OpinionKey {
                    is_local: false,
                    arc_kind: ArcKind::References,
                    nested_arc_kind: Some(ArcKind::Inherits),
                    namespace_depth: 2,
                    authored: true,
                    arc_list_index: 0,
                    layer_strength: 1,
                    layer_id: set_layer,
                    lookup_path: sp_class,
                    spec_path: prim_spec_path(&store, sp_class, &[]),
                },
                vec![from_class_set, geom_tok],
            ),
        ];

        let mut children = vec![c_geom, c_fr, c_fs, c_fcr, c_fcs];
        apply_authored_children_base_order(&store, &mut children, &opinions);

        let result: Vec<&str> = children
            .iter()
            .map(|c| {
                store
                    .tokens
                    .resolve(store.paths.resolve(*c).leaf().unwrap())
            })
            .collect();

        assert_eq!(
            result,
            vec![
                "geom",
                "From_class_in_set",
                "From_set",
                "From_class_in_root",
                "From_root"
            ]
        );
    }

    /// Test that deeply nested variant children are ordered correctly:
    /// children from deeper nesting levels (more `required_outer_variant_sites`)
    /// come before shallower ones.
    #[test]
    fn test_nested_variant_child_ordering() {
        let mut store = InMemoryStore::default();
        let layer_id = LayerId(1);

        // Intern tokens.
        let standin = store.tokens.intern("standin");
        let shading = store.tokens.intern("shadingVariant");
        let anim = store.tokens.intern("anim");
        let spooky = store.tokens.intern("spooky");

        let sphere = store.tokens.intern("anim_spooky_sphere");
        let anim_sphere = store.tokens.intern("anim_spooky_anim_sphere");

        // Create paths.
        let parent_path = store
            .paths
            .intern(Path::parse_absolute("/D", &mut store.tokens).unwrap());
        let child_sphere = store
            .paths
            .intern(Path::parse_absolute("/D/anim_spooky_sphere", &mut store.tokens).unwrap());
        let child_anim_sphere = store
            .paths
            .intern(Path::parse_absolute("/D/anim_spooky_anim_sphere", &mut store.tokens).unwrap());

        // Build PrimSpec with nested variant sets.
        let mut d_spec = PrimSpec {
            variant_set_order: vec![standin, shading],
            ..PrimSpec::default()
        };
        d_spec.variant_selections.insert(standin, anim);
        d_spec.variant_selections.insert(shading, spooky);

        // shadingVariant=spooky: anim_spooky_sphere with required outer variant site
        let mut shading_spooky = VariantSpec::default();
        shading_spooky.authored_children.push(sphere);
        shading_spooky.required_outer_variant_sites.insert(
            sphere,
            vec![VariantSelectionSite {
                host_path: parent_path,
                set: standin,
                variant: anim,
            }],
        );

        let mut shading_set = VariantSetSpec::default();
        shading_set.variants.insert(spooky, shading_spooky);
        d_spec.variant_sets.insert(shading, shading_set);

        // standin=anim: anim_spooky_anim_sphere with two required outer sites
        let mut standin_anim = VariantSpec::default();
        standin_anim.authored_children.push(anim_sphere);
        standin_anim.required_outer_variant_sites.insert(
            anim_sphere,
            vec![
                VariantSelectionSite {
                    host_path: parent_path,
                    set: standin,
                    variant: anim,
                },
                VariantSelectionSite {
                    host_path: parent_path,
                    set: shading,
                    variant: spooky,
                },
            ],
        );

        let mut standin_set = VariantSetSpec::default();
        standin_set.variants.insert(anim, standin_anim);
        d_spec.variant_sets.insert(standin, standin_set);

        // Build layer.
        let mut layer = Layer::new(layer_id);
        layer.prims.insert(parent_path, d_spec);

        // Add child PrimSpecs.
        layer.prims.insert(child_sphere, PrimSpec::default());
        layer.prims.insert(child_anim_sphere, PrimSpec::default());

        store.insert_layer(layer);

        // Build prim index.
        let mut prims = HashMap::new();
        prims.insert(
            parent_path,
            PrimIndex {
                opinions_by_field: HashMap::new(),
                property_types_by_field: HashMap::new(),
                sources: vec![OpinionKey {
                    is_local: true,
                    arc_kind: ArcKind::Local,
                    nested_arc_kind: None,
                    namespace_depth: 1,
                    authored: true,
                    arc_list_index: 0,
                    layer_strength: 0,
                    layer_id,
                    lookup_path: parent_path,
                    spec_path: prim_spec_path(&store, parent_path, &[]),
                }],
            },
        );

        // Children list (initial order from population).
        let mut children = HashMap::new();
        children.insert(parent_path, vec![child_sphere, child_anim_sphere]);

        filter_variant_children(&store, &prims, &mut children);

        let result: Vec<&str> = children[&parent_path]
            .iter()
            .map(|c| {
                store
                    .tokens
                    .resolve(store.paths.resolve(*c).leaf().unwrap())
            })
            .collect();

        // Deeper nesting (depth 2) should come before shallower (depth 1).
        assert_eq!(
            result,
            vec!["anim_spooky_anim_sphere", "anim_spooky_sphere"]
        );
    }
}
