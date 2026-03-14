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
        resolve_inherits_for_prim, resolve_payloads_for_prim, resolve_references_for_prim,
        resolve_specializes_for_prim, resolve_variant_branch_references,
        resolve_variant_child_references, resolve_variant_selections_for_prim,
    },
    doc::{FieldValue, LayerId, LayerStore, Reference},
    interner::TokenId,
    layer_stack::LayerStack,
    path::PathId,
    population::populate,
    prim_index::{ArcKind, Opinion, OpinionKey, PrimIndex},
    stage::{Stage, StageOptions},
};

/// Composes a stage from a root layer.
///
/// This implements:
/// - Layer stack gathering (layer is stronger than its sublayers)
/// - Stage population (including prims introduced via references)
/// - Value resolution (scalar + `ListOp`)
pub fn compose_stage(store: &mut dyn LayerStore, root: LayerId, options: StageOptions) -> Stage {
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

    add_local_and_variant_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
    );
    add_inherit_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
    );
    add_reference_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
    );
    add_payload_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
    );
    add_specializes_opinions(
        store,
        &layer_stack,
        &paths,
        &mut prims,
        &mut prim_order_opinions,
        &mut authored_children_opinions,
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

    Stage::from_parts(prims, children, options.with_provenance)
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
            let Some(spec) = layer.prims.get(&source.spec_path) else {
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

        // Then check each source for variant sets.
        for source in &prim_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.spec_path) else {
                continue;
            };

            for (set_name, set_spec) in &spec.variant_sets {
                for (variant_name, variant_spec) in &set_spec.variants {
                    for child in &variant_spec.authored_children {
                        has_variant_sets = true;
                        all_variant_children.insert(*child);
                        if selections.get(set_name) == Some(variant_name) {
                            selected_children.insert(*child);
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

            // Re-order variant children: use authored order from the selected
            // variant branch(es). Children from later variant sets come first.
            // Build an ordered list of selected variant children.
            let mut ordered_variant_children: Vec<TokenId> = Vec::new();

            // Iterate variant sets in reverse order (later sets' children first).
            for set_tok in variant_set_order.iter().rev() {
                let Some(&selected_variant) = selections.get(set_tok) else {
                    continue;
                };
                // Find authored_children for the selected branch from any source.
                for source in &prim_index.sources {
                    let Some(layer) = store.layer(source.layer_id) else {
                        continue;
                    };
                    let Some(spec) = layer.prims.get(&source.spec_path) else {
                        continue;
                    };
                    if let Some(set_spec) = spec.variant_sets.get(set_tok)
                        && let Some(variant_spec) = set_spec.variants.get(&selected_variant)
                    {
                        for child in &variant_spec.authored_children {
                            if !ordered_variant_children.contains(child) {
                                ordered_variant_children.push(*child);
                            }
                        }
                    }
                }
            }

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

        // Resolve grandparent's variant selections.
        let mut gp_selections: HashMap<TokenId, TokenId> = HashMap::new();
        for source in &gp_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.spec_path) else {
                continue;
            };
            for (set, variant) in &spec.variant_selections {
                gp_selections.entry(*set).or_insert(*variant);
            }
        }

        // Collect all and selected grandchild authored children.
        let mut all_gc: HashSet<TokenId> = HashSet::new();
        let mut selected_gc: HashSet<TokenId> = HashSet::new();
        let mut has_gc = false;

        for source in &gp_index.sources {
            let Some(layer) = store.layer(source.layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&source.spec_path) else {
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

        let unselected_gc: HashSet<TokenId> =
            all_gc.difference(&selected_gc).copied().collect();
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
                if source.spec_path != parent_path {
                    inherited_sources.push(source.spec_path);
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
                                if !src_leaves.contains(&leaf)
                                    && prims.contains_key(&sc_id)
                                {
                                    to_remove.insert(leaf);
                                }
                            }
                        }
                    }
                }
            }
        }

        if !to_remove.is_empty() {
            if let Some(child_list) = children.get_mut(&parent_path) {
                child_list.retain(|child| {
                    let leaf = store.paths().resolve(*child).leaf();
                    leaf.map(|l| !to_remove.contains(&l)).unwrap_or(true)
                });
            }
        }

        // Reorder: children from the prim's own arcs come before children
        // inherited from the source prim. Use the source's filtered child
        // list to identify which children are inherited.
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

        if !inherited_leaves.is_empty() {
            if let Some(child_list) = children.get_mut(&parent_path) {
                // Only apply partition+reorder when there are children that
                // exist ONLY as direct children (not from inheritance). When
                // all children also exist in the inherited source, the normal
                // `apply_child_order` with `prim_order` opinions handles
                // ordering correctly.
                let has_direct_only = child_list.iter().any(|child| {
                    let leaf = store.paths().resolve(*child).leaf();
                    leaf.map(|l| !inherited_leaves.contains(&l)).unwrap_or(false)
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
                        let a_pos = a_leaf
                            .and_then(|l| src_order.iter().position(|s| *s == l));
                        let b_pos = b_leaf
                            .and_then(|l| src_order.iter().position(|s| *s == l));
                        a_pos.cmp(&b_pos)
                    });

                    *child_list = direct;
                    child_list.extend(inherited);
                }
            }
        }
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

    // Also gather selections from inherit targets (weaker than local, per LIVERPS).
    let inherits = resolve_inherits_for_prim(store, local_stack, path);
    for inherit_target in inherits {
        let inherit_selections =
            resolve_variant_selections_for_prim(store, local_stack, inherit_target);
        for (set, variant) in inherit_selections {
            selections.entry(set).or_insert(variant);
        }
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

    for reference in refs {
        let ref_stack = LayerStack::gather(store, reference.layer);
        let ref_selections =
            resolve_variant_selections_for_prim(store, &ref_stack, reference.prim_path);
        for (set, variant) in ref_selections {
            selections.entry(set).or_insert(variant);
        }
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
) {
    for path in paths.iter().copied() {
        let selections = resolve_full_variant_selections(store, local_stack, path);
        let namespace_depth =
            u16::try_from(store.paths().resolve(path).depth()).unwrap_or(u16::MAX);

        for (layer_strength, layer_id) in local_stack.layers.iter().copied().enumerate() {
            let Some(layer) = store.layer(layer_id) else {
                continue;
            };
            let Some(spec) = layer.prims.get(&path) else {
                continue;
            };

            let layer_strength = u16::try_from(layer_strength).unwrap_or(u16::MAX);
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
                    spec_path: path,
                });

            for (field, value) in &spec.fields {
                out.get_mut(&path)
                    .expect("path exists")
                    .add_opinion(Opinion {
                        key: OpinionKey {
                            is_local: true,
                            arc_kind: ArcKind::Local,
                            nested_arc_kind: None,
                            namespace_depth,
                            authored: true,
                            arc_list_index: 0,
                            layer_strength,
                            layer_id,
                            spec_path: path,
                        },
                        field: *field,
                        value: value.clone(),
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
                        spec_path: path,
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
                        spec_path: path,
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
                        spec_path: path,
                    });

                for (field, value) in &variant_spec.fields {
                    out.get_mut(&path)
                        .expect("path exists")
                        .add_opinion(Opinion {
                            key: OpinionKey {
                                is_local: false,
                                arc_kind: ArcKind::Variants,
                                nested_arc_kind: None,
                                namespace_depth,
                                authored: true,
                                arc_list_index: 0,
                                layer_strength,
                                layer_id,
                                spec_path: path,
                            },
                            field: *field,
                            value: value.clone(),
                        });
                }

                // Forward variant-scoped child prim fields.
                let path_obj = store.paths().resolve(path).clone();
                for (child_tok, child_fields) in &variant_spec.child_fields {
                    let child_path = path_obj.join(&[*child_tok]);
                    if let Some(child_path_id) = store.paths().lookup(&child_path) {
                        if out.contains_key(&child_path_id) {
                            let child_ns_depth = u16::try_from(
                                store.paths().resolve(child_path_id).depth(),
                            )
                            .unwrap_or(u16::MAX);
                            for (field, value) in child_fields {
                                out.get_mut(&child_path_id)
                                    .expect("path exists")
                                    .add_opinion(Opinion {
                                        key: OpinionKey {
                                            is_local: false,
                                            arc_kind: ArcKind::Variants,
                                            nested_arc_kind: None,
                                            namespace_depth: child_ns_depth,
                                            authored: true,
                                            arc_list_index: 0,
                                            layer_strength,
                                            layer_id,
                                            spec_path: child_path_id,
                                        },
                                        field: *field,
                                        value: value.clone(),
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
                                    spec_path: child_path_id,
                                });
                        }
                    }
                }

                // Forward variant-scoped child_authored_children as
                // authored_children opinions on the child path.
                for (child_tok, gc_list) in &variant_spec.child_authored_children {
                    let child_path = path_obj.join(&[*child_tok]);
                    if let Some(child_path_id) = store.paths().lookup(&child_path) {
                        if out.contains_key(&child_path_id) {
                            let child_ns_depth = u16::try_from(
                                store.paths().resolve(child_path_id).depth(),
                            )
                            .unwrap_or(u16::MAX);
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
                                        spec_path: path,
                                    },
                                    gc_list.clone(),
                                ));
                        }
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
) {
    // Spec: AOUSD Core §10 (references arcs). For v0.1 we expand references
    // recursively so that nested references contribute opinions.
    let mut visited: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_specializes: HashSet<(PathId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        let refs = resolve_references_for_prim(store, local_stack, dest_root);
        for (arc_list_index, reference) in refs.into_iter().enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
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

    for (layer_strength, layer_id) in local_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength).unwrap_or(u16::MAX);
        let mut pending = Vec::new();
        let mut pending_sources = Vec::new();
        {
            let Some(layer) = store.layer(layer_id) else {
                continue;
            };

            for (remote_path_id, dest_path_id) in &mapping {
                let Some(spec) = layer.prims.get(remote_path_id) else {
                    continue;
                };
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
                            spec_path: *remote_path_id,
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
                                spec_path: *remote_path_id,
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
                        spec_path: *remote_path_id,
                    },
                ));
                for (field, value) in &spec.fields {
                    pending.push((*dest_path_id, *remote_path_id, *field, value.clone()));
                }

                // Forward variant opinions from selected variants through inherits.
                let inh_selections =
                    resolve_variant_selections_for_prim(store, local_stack, *remote_path_id);
                for (set, selected) in &inh_selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected)
                    {
                        for (field, value) in &variant_spec.fields {
                            pending.push((
                                *dest_path_id,
                                *remote_path_id,
                                *field,
                                value.clone(),
                            ));
                        }
                    }
                }

                // Forward child_fields from the parent's variant specs through
                // inherits. When the inherited prim is a child whose parent has
                // variant sets with child_fields targeting this child, those
                // fields need to propagate through the inherit arc.
                let remote_path = store.paths().resolve(*remote_path_id).clone();
                if let Some(remote_leaf) = remote_path.leaf() {
                    if let Some(remote_parent) = remote_path.parent() {
                        if let Some(remote_parent_id) = store.paths().lookup(&remote_parent) {
                            if let Some(parent_spec) = layer.prims.get(&remote_parent_id) {
                                let parent_selections =
                                    resolve_variant_selections_for_prim(
                                        store,
                                        local_stack,
                                        remote_parent_id,
                                    );
                                for (set, selected) in &parent_selections {
                                    if let Some(set_spec) = parent_spec.variant_sets.get(set)
                                        && let Some(variant_spec) =
                                            set_spec.variants.get(selected)
                                        && let Some(child_fields) =
                                            variant_spec.child_fields.get(&remote_leaf)
                                    {
                                        for (field, value) in child_fields {
                                            pending.push((
                                                *dest_path_id,
                                                *remote_path_id,
                                                *field,
                                                value.clone(),
                                            ));
                                        }
                                    }
                                }
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

        for (dest_path_id, remote_path_id, field, value) in pending {
            let value = remap_field_value_paths(store, &base_path, &inherited_path, value);
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_opinion(Opinion {
                    key: OpinionKey {
                        is_local: false,
                        arc_kind,
                        nested_arc_kind,
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength,
                        layer_id,
                        spec_path: remote_path_id,
                    },
                    field,
                    value,
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
            for source in &src_index.sources {
                if source.arc_kind == ArcKind::Local {
                    continue;
                }
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
                        spec_path: source.spec_path,
                    });
            }
            for (field, opinions) in &src_index.opinions_by_field {
                for opinion in opinions {
                    if opinion.key.arc_kind == ArcKind::Local {
                        continue;
                    }
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
                                spec_path: opinion.key.spec_path,
                            },
                            field: *field,
                            value: opinion.value.clone(),
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
        let nested_specializes =
            resolve_specializes_for_prim(store, local_stack, remote_path_id);
        for (spec_index, specialized) in nested_specializes.into_iter().enumerate() {
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated =
                remap_path_id(store, &base_path, &inherited_path, specialized);
            if translated != specialized {
                add_specializes_edge_opinions(
                    store,
                    local_stack,
                    dest_path_id,
                    translated,
                    outer_arc_kind,
                    namespace_depth,
                    spec_index,
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
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
                        parent_translated,
                        outer_arc_kind,
                        namespace_depth,
                        spec_index,
                        out,
                        visited_specializes,
                        prim_order_out,
                        authored_children_out,
                    );
                }
            }

            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                specialized,
                outer_arc_kind,
                namespace_depth,
                spec_index,
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
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
                        spec_path: source.spec_path,
                    });
            }
            for (field, opinions) in &src_index.opinions_by_field {
                for opinion in opinions {
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
                                spec_path: opinion.key.spec_path,
                            },
                            field: *field,
                            value: opinion.value.clone(),
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
                    .map(|p| remap_path_id(store, dest_root, src_root, p))
                    .collect()
            });
            out.prepend = out
                .prepend
                .into_iter()
                .map(|p| remap_path_id(store, dest_root, src_root, p))
                .collect();
            out.append = out
                .append
                .into_iter()
                .map(|p| remap_path_id(store, dest_root, src_root, p))
                .collect();
            out.delete = out
                .delete
                .into_iter()
                .map(|p| remap_path_id(store, dest_root, src_root, p))
                .collect();
            FieldValue::PathListOp(out)
        }
        other => other,
    }
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
    let Some(rel) = rel else {
        return path;
    };
    store.paths_mut().intern(dest_root.join(&rel))
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
) {
    if !out.contains_key(&dest_root) {
        return;
    }
    if !visited.insert((dest_root, reference.layer, reference.prim_path)) {
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
    };
    let target_root = store.paths().resolve(reference.prim_path).clone();
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

    for (layer_strength, remote_layer_id) in remote_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength).unwrap_or(u16::MAX);
        let Some(remote_layer) = store.layer(remote_layer_id) else {
            continue;
        };

        let mut pending_sources = Vec::new();
        for (remote_path_id, dest_path_id) in &mapping {
            let Some(remote_spec) = remote_layer.prims.get(remote_path_id) else {
                continue;
            };
            pending_sources.push((
                *dest_path_id,
                OpinionKey {
                    is_local: false,
                    arc_kind: ArcKind::References,
                    nested_arc_kind: None,
                    namespace_depth,
                    authored: true,
                    arc_list_index,
                    layer_strength,
                    layer_id: remote_layer_id,
                    spec_path: *remote_path_id,
                },
            ));

            for (field, value) in &remote_spec.fields {
                out.get_mut(dest_path_id)
                    .expect("path exists")
                    .add_opinion(Opinion {
                        key: OpinionKey {
                            is_local: false,
                            arc_kind: ArcKind::References,
                            nested_arc_kind: None,
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id: remote_layer_id,
                            spec_path: *remote_path_id,
                        },
                        field: *field,
                        value: value.clone(),
                    });
            }

            // Forward variant opinions from selected variants.
            // Variant selections are resolved using the combined stack
            // (referencing layer selections take precedence).
            {
                let selections =
                    resolve_variant_selections_for_prim(store, &combined_stack, *remote_path_id);
                for (set, selected) in &selections {
                    if let Some(set_spec) = remote_spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected)
                    {
                        for (field, value) in &variant_spec.fields {
                            out.get_mut(dest_path_id)
                                .expect("path exists")
                                .add_opinion(Opinion {
                                    key: OpinionKey {
                                        is_local: false,
                                        arc_kind: ArcKind::References,
                                        nested_arc_kind: Some(ArcKind::Variants),
                                        namespace_depth,
                                        authored: true,
                                        arc_list_index,
                                        layer_strength,
                                        layer_id: remote_layer_id,
                                        spec_path: *remote_path_id,
                                    },
                                    field: *field,
                                    value: value.clone(),
                                });
                        }

                        // Forward child_authored_children as authored_children
                        // opinions on child paths.
                        let ref_path_obj = store.paths().resolve(*dest_path_id).clone();
                        for (child_tok, gc_list) in &variant_spec.child_authored_children {
                            let child_path = ref_path_obj.join(&[*child_tok]);
                            if let Some(child_path_id) = store.paths().lookup(&child_path) {
                                if out.contains_key(&child_path_id) {
                                    let child_ns = u16::try_from(
                                        store.paths().resolve(child_path_id).depth(),
                                    )
                                    .unwrap_or(u16::MAX);
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
                                                spec_path: *remote_path_id,
                                            },
                                            gc_list.clone(),
                                        ));
                                }
                            }
                        }

                        // Forward child_fields to child paths.
                        for (child_tok, child_fields) in &variant_spec.child_fields {
                            let child_path = ref_path_obj.join(&[*child_tok]);
                            if let Some(child_path_id) = store.paths().lookup(&child_path) {
                                if out.contains_key(&child_path_id) {
                                    let child_ns = u16::try_from(
                                        store.paths().resolve(child_path_id).depth(),
                                    )
                                    .unwrap_or(u16::MAX);
                                    for (field, value) in child_fields {
                                        out.get_mut(&child_path_id)
                                            .expect("path exists")
                                            .add_opinion(Opinion {
                                                key: OpinionKey {
                                                    is_local: false,
                                                    arc_kind: ArcKind::References,
                                                    nested_arc_kind: Some(ArcKind::Variants),
                                                    namespace_depth: child_ns,
                                                    authored: true,
                                                    arc_list_index,
                                                    layer_strength,
                                                    layer_id: remote_layer_id,
                                                    spec_path: child_path_id,
                                                },
                                                field: *field,
                                                value: value.clone(),
                                            });
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
                                            spec_path: child_path_id,
                                        });
                                }
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
                        spec_path: *remote_path_id,
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
                            spec_path: *remote_path_id,
                        },
                        remote_spec.authored_children.clone(),
                    ));
            }
        }

        for (dest_path_id, key) in pending_sources {
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_source(key);
        }
    }

    for (remote_path_id, dest_path_id) in mapping {
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
            );
        }

        let nested = resolve_references_for_prim(store, &remote_stack, remote_path_id);
        // Also resolve variant-scoped child references using combined_stack
        // for selections, so referencing layer's variant selections override
        // the referenced layer's defaults.
        let variant_child_refs = resolve_variant_child_references(
            store,
            &remote_stack,
            &combined_stack,
            remote_path_id,
        );
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

            let translated =
                remap_path_id(store, &dest_root_path, &target_root, specialized_root);
            if translated != specialized_root {
                add_specializes_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    translated,
                    Some(ArcKind::References),
                    namespace_depth,
                    spec_index,
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                );
            }

            add_specializes_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                specialized_root,
                Some(ArcKind::References),
                namespace_depth,
                spec_index,
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
            );
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
) {
    // Spec: AOUSD Core §10 (payloads arc, §5.1.22). Payloads are structurally
    // identical to references for composition purposes but sit at a weaker
    // position in LIVERPS (between References and Specializes).
    let mut visited: HashSet<(PathId, LayerId, PathId)> = HashSet::new();
    let mut visited_inherits: HashSet<(PathId, PathId)> = HashSet::new();
    let mut visited_specializes: HashSet<(PathId, PathId)> = HashSet::new();
    for dest_root in paths.iter().copied() {
        let payloads = resolve_payloads_for_prim(store, local_stack, dest_root);
        for (arc_list_index, payload) in payloads.into_iter().enumerate() {
            let arc_list_index = u16::try_from(arc_list_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_root).depth()).unwrap_or(u16::MAX);
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
) {
    // Payloads mirror reference edge opinions with ArcKind::Payloads.
    if !out.contains_key(&dest_root) {
        return;
    }
    if !visited.insert((dest_root, reference.layer, reference.prim_path)) {
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
    };
    let target_root = store.paths().resolve(reference.prim_path).clone();
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

    for (layer_strength, remote_layer_id) in remote_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength).unwrap_or(u16::MAX);
        let Some(remote_layer) = store.layer(remote_layer_id) else {
            continue;
        };

        let mut pending_sources = Vec::new();
        for (remote_path_id, dest_path_id) in &mapping {
            let Some(remote_spec) = remote_layer.prims.get(remote_path_id) else {
                continue;
            };
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
                    spec_path: *remote_path_id,
                },
            ));

            for (field, value) in &remote_spec.fields {
                out.get_mut(dest_path_id)
                    .expect("path exists")
                    .add_opinion(Opinion {
                        key: OpinionKey {
                            is_local: false,
                            arc_kind: ArcKind::Payloads,
                            nested_arc_kind: None,
                            namespace_depth,
                            authored: true,
                            arc_list_index,
                            layer_strength,
                            layer_id: remote_layer_id,
                            spec_path: *remote_path_id,
                        },
                        field: *field,
                        value: value.clone(),
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
                        spec_path: *remote_path_id,
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
                            spec_path: *remote_path_id,
                        },
                        remote_spec.authored_children.clone(),
                    ));
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
            );
        }

        let nested = resolve_references_for_prim(store, &remote_stack, remote_path_id);
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
            );
        }

        // Handle nested specializes inside payload targets.
        let specializes = resolve_specializes_for_prim(store, &remote_stack, remote_path_id);
        for (spec_index, specialized_root) in specializes.into_iter().enumerate() {
            let spec_index = u16::try_from(spec_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            let translated =
                remap_path_id(store, &dest_root_path, &target_root, specialized_root);
            if translated != specialized_root {
                add_specializes_edge_opinions(
                    store,
                    stage_stack,
                    dest_path_id,
                    translated,
                    Some(ArcKind::Payloads),
                    namespace_depth,
                    spec_index,
                    out,
                    visited_specializes,
                    prim_order_out,
                    authored_children_out,
                );
            }

            add_specializes_edge_opinions(
                store,
                &combined_stack,
                dest_path_id,
                specialized_root,
                Some(ArcKind::Payloads),
                namespace_depth,
                spec_index,
                out,
                visited_specializes,
                prim_order_out,
                authored_children_out,
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
            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_root,
                specialized_root,
                None,
                namespace_depth,
                arc_list_index,
                out,
                &mut visited,
                prim_order_out,
                authored_children_out,
            );
        }
    }
}

/// Specializes edge opinions mirror inherits but use [`ArcKind::Specializes`].
fn add_specializes_edge_opinions(
    store: &mut dyn LayerStore,
    local_stack: &LayerStack,
    dest_root: PathId,
    specialized_root: PathId,
    outer_arc_kind: Option<ArcKind>,
    namespace_depth: u16,
    arc_list_index: u16,
    out: &mut HashMap<PathId, PrimIndex>,
    visited: &mut HashSet<(PathId, PathId)>,
    prim_order_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
    authored_children_out: &mut HashMap<PathId, Vec<(OpinionKey, Vec<TokenId>)>>,
) {
    if !visited.insert((dest_root, specialized_root)) {
        return;
    }

    let base_path = store.paths().resolve(dest_root).clone();
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

    for (layer_strength, layer_id) in local_stack.layers.iter().copied().enumerate() {
        let layer_strength = u16::try_from(layer_strength).unwrap_or(u16::MAX);
        let mut pending = Vec::new();
        let mut pending_sources = Vec::new();
        {
            let Some(layer) = store.layer(layer_id) else {
                continue;
            };

            for (remote_path_id, dest_path_id) in &mapping {
                let Some(spec) = layer.prims.get(remote_path_id) else {
                    continue;
                };
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
                            spec_path: *remote_path_id,
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
                                spec_path: *remote_path_id,
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
                        spec_path: *remote_path_id,
                    },
                ));
                for (field, value) in &spec.fields {
                    pending.push((*dest_path_id, *remote_path_id, *field, value.clone()));
                }

                // Forward variant opinions from selected variants through specializes.
                let spec_selections =
                    resolve_variant_selections_for_prim(store, local_stack, *remote_path_id);
                for (set, selected) in &spec_selections {
                    if let Some(set_spec) = spec.variant_sets.get(set)
                        && let Some(variant_spec) = set_spec.variants.get(selected)
                    {
                        for (field, value) in &variant_spec.fields {
                            pending.push((
                                *dest_path_id,
                                *remote_path_id,
                                *field,
                                value.clone(),
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

        for (dest_path_id, remote_path_id, field, value) in pending {
            let value = remap_field_value_paths(store, &base_path, &specialized_path, value);
            out.get_mut(&dest_path_id)
                .expect("path exists")
                .add_opinion(Opinion {
                    key: OpinionKey {
                        is_local: false,
                        arc_kind,
                        nested_arc_kind,
                        namespace_depth,
                        authored: true,
                        arc_list_index,
                        layer_strength,
                        layer_id,
                        spec_path: remote_path_id,
                    },
                    field,
                    value,
                });
        }
    }

    // Propagate already-accumulated PrimIndex sources from mapped source
    // paths to dest paths — mirrors the same logic in add_inherit_edge_opinions.
    for &(remote_path_id, dest_path_id) in &mapping {
        let src_index = out.get(&remote_path_id).cloned();
        if let Some(src_index) = src_index {
            for source in &src_index.sources {
                if source.arc_kind == ArcKind::Local {
                    continue;
                }
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
                        spec_path: source.spec_path,
                    });
            }
            for (field, opinions) in &src_index.opinions_by_field {
                for opinion in opinions {
                    if opinion.key.arc_kind == ArcKind::Local {
                        continue;
                    }
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
                                spec_path: opinion.key.spec_path,
                            },
                            field: *field,
                            value: opinion.value.clone(),
                        });
                }
            }
        }
    }

    // Handle nested specializes arcs.
    for &(remote_path_id, dest_path_id) in &mapping {
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
                    translated,
                    outer_arc_kind,
                    namespace_depth,
                    nested_index,
                    out,
                    visited,
                    prim_order_out,
                    authored_children_out,
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
                        parent_translated,
                        outer_arc_kind,
                        namespace_depth,
                        nested_index,
                        out,
                        visited,
                        prim_order_out,
                        authored_children_out,
                    );
                }
            }

            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                nested,
                outer_arc_kind,
                namespace_depth,
                nested_index,
                out,
                visited,
                prim_order_out,
                authored_children_out,
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
                    translated,
                    outer_arc_kind,
                    namespace_depth,
                    nested_index,
                    out,
                    visited,
                    prim_order_out,
                    authored_children_out,
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
                        parent_translated,
                        outer_arc_kind,
                        namespace_depth,
                        nested_index,
                        out,
                        visited,
                        prim_order_out,
                        authored_children_out,
                    );
                }
            }

            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                inherited,
                outer_arc_kind,
                namespace_depth,
                nested_index,
                out,
                visited,
                prim_order_out,
                authored_children_out,
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
    for &(remote_path_id, dest_path_id) in &mapping {
        let refs = resolve_references_for_prim(store, local_stack, remote_path_id);
        for (ref_index, reference) in refs.into_iter().enumerate() {
            let ref_index = u16::try_from(ref_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);

            // Follow the reference: map prims from the reference target
            // to the destination prim, adding opinions at specializes strength.
            add_specializes_edge_opinions(
                store,
                local_stack,
                dest_path_id,
                reference.prim_path,
                outer_arc_kind,
                namespace_depth,
                ref_index,
                out,
                visited,
                prim_order_out,
                authored_children_out,
            );

            // Also try via the reference's own layer stack.
            let ref_stack = LayerStack::gather(store, reference.layer);
            add_specializes_edge_opinions(
                store,
                &ref_stack,
                dest_path_id,
                reference.prim_path,
                outer_arc_kind,
                namespace_depth,
                ref_index,
                out,
                visited,
                prim_order_out,
                authored_children_out,
            );
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
    // Builds a deterministic baseline child order:
    // - Take the first (strongest) contributing authored-children list as-is.
    // - Insert children introduced by weaker sources in lexicographic position.
    //
    // Spec: AOUSD Core §11 (stage population) and supplemental suite composition
    // fixtures that rely on authoring order in referenced layers.
    let mut by_name = HashMap::<TokenId, PathId>::new();
    for child in children.iter().copied() {
        if let Some(name) = store.paths().resolve(child).leaf() {
            by_name.insert(name, child);
        }
    }

    let mut sorted = opinions.to_vec();
    sorted.sort_by(|a, b| a.0.cmp_strongest_first(&b.0));

    // When the strongest opinion is Local and the next is from a Reference arc,
    // and the Local opinion's children are all also present in the Reference
    // opinion, prefer the Reference opinion as the baseline. This handles
    // the case where local `over` children shouldn't override reference-defined
    // child ordering.
    if sorted.len() >= 2 && sorted[0].0.arc_kind == ArcKind::Local {
        let local_names: HashSet<TokenId> = sorted[0].1.iter().copied().collect();
        let local_order = sorted[0].1.clone();
        // Find the first Reference opinion.
        if let Some(ref_idx) = sorted[1..]
            .iter()
            .position(|(k, _)| k.arc_kind == ArcKind::References)
        {
            let ref_idx = ref_idx + 1;
            let ref_names: HashSet<TokenId> = sorted[ref_idx].1.iter().copied().collect();
            // If all local children also exist in the reference target's children,
            // swap: use the reference opinion as the baseline.
            if local_names.is_subset(&ref_names) && local_names != ref_names
                || (local_names == ref_names && local_order != sorted[ref_idx].1)
            {
                sorted.swap(0, ref_idx);
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = HashSet::<PathId>::new();
    for (_, names) in sorted {
        if out.is_empty() {
            // The first contributing list (strongest-first) establishes the
            // baseline ordering and is preserved as-authored.
            for name in &names {
                let Some(child_id) = by_name.get(name).copied() else {
                    continue;
                };
                if seen.insert(child_id) {
                    out.push(child_id);
                }
            }
            continue;
        }

        for name in names {
            let Some(child_id) = by_name.get(&name).copied() else {
                continue;
            };
            if !seen.insert(child_id) {
                continue;
            }
            insert_lexicographic(store, &mut out, child_id);
        }
    }

    for child_id in children.iter().copied() {
        if !seen.insert(child_id) {
            continue;
        }
        if out.is_empty() {
            out.push(child_id);
        } else {
            insert_lexicographic(store, &mut out, child_id);
        }
    }

    *children = out;
}

fn insert_lexicographic(store: &dyn LayerStore, list: &mut Vec<PathId>, child: PathId) {
    // Use token-string ordering (not `TokenId` ordering) for AOUSD-aligned
    // namespace ordering.
    //
    // Spec: AOUSD Core §8 (paths and namespace ordering).
    let child_path = store.paths().resolve(child);
    let idx = list
        .iter()
        .position(|existing| {
            store
                .paths()
                .resolve(*existing)
                .cmp_with_tokens(child_path, store.tokens())
                == Ordering::Greater
        })
        .unwrap_or(list.len());
    list.insert(idx, child);
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
