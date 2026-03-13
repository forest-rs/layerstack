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
        resolve_specializes_for_prim, resolve_variant_selections_for_prim,
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

    Stage::from_parts(prims, children, options.with_provenance)
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
        let selections = resolve_variant_selections_for_prim(store, local_stack, path);
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

    for (remote_path_id, dest_path_id) in mapping {
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
                prim_order_out,
                authored_children_out,
            );
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
                prim_order_out,
                authored_children_out,
            );
        }

        let nested = resolve_references_for_prim(store, &remote_stack, remote_path_id);
        for (nested_index, nested_ref) in nested.into_iter().enumerate() {
            let nested_index = u16::try_from(nested_index).unwrap_or(u16::MAX);
            let namespace_depth =
                u16::try_from(store.paths().resolve(dest_path_id).depth()).unwrap_or(u16::MAX);
            // Pass combined_stack so that nested references can discover
            // opinions from intermediate reference layers (e.g. inherits and
            // specializes arcs that resolve paths defined in the parent
            // reference's layer).
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
