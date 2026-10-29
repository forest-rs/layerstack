// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Coordinated root-stack edits. Composition supplies namespace mappings; the
//! caller supplies the set of stage snapshots. No stage or asset is discovered
//! through hidden global state. AOUSD Core §8 and §10.3.2.6; OpenUSD
//! `UsdNamespaceEditor::_ProcessPrimEdit` / `PcpGatherDependentNamespaceEdits`.

use super::*;
use crate::prim_indexing::Boundaries;
use crate::sort;
use crate::{
    ArcKind, HashMap, HashSet, NodeId, Relocate, prim_indexing::ChildIndex,
    stage::preparation::PreparationIndex,
};

pub(super) fn prepare(
    primary: &Stage,
    dependents: &[&Stage],
    store: &mut dyn LayerStore,
    target: &EditTarget,
    source: TargetPath,
    destination: TargetPath,
) -> Result<NamespaceEdit, NamespaceError> {
    prepare_with(
        primary,
        dependents,
        store,
        target,
        source,
        destination,
        true,
    )
}

/// [`prepare`] by composing and visiting every stage and layer whole, as
/// before stages retained preparation evidence: the oracle the retained
/// proofs are tested against.
#[cfg(test)]
pub(super) fn prepare_by_composition(
    primary: &Stage,
    dependents: &[&Stage],
    store: &mut dyn LayerStore,
    target: &EditTarget,
    source: TargetPath,
    destination: TargetPath,
) -> Result<NamespaceEdit, NamespaceError> {
    prepare_with(
        primary,
        dependents,
        store,
        target,
        source,
        destination,
        false,
    )
}

/// [`prepare`], from the evidence the stages retain
/// ([`crate::stage::preparation`]) when `use_retained` and every stage
/// keeps it, otherwise by composing and visiting them whole. Both find the
/// same edit, or the same error.
fn prepare_with(
    primary: &Stage,
    dependents: &[&Stage],
    store: &mut dyn LayerStore,
    target: &EditTarget,
    source: TargetPath,
    destination: TargetPath,
    use_retained: bool,
) -> Result<NamespaceEdit, NamespaceError> {
    validate_paths(store, source, destination)?;
    validate_stage(primary, store, source, destination)?;
    // A variant's edit target maps into its branch (`/V{v=x}F` for
    // `/V/F`), across the variant arc: `UsdNamespaceEditor` refuses edit
    // targets that map paths across composition arcs
    // (`_EditProcessor::_GatherLayersToEdit`,
    // `pxr/usd/usd/namespaceEditor.cpp`).
    let (from, to) = (
        map(target, store, source)?,
        map(target, store, destination)?,
    );
    if !primary.layer_stack().contains(&target.layer())
        || from.has_variant_selections()
        || to.has_variant_selections()
        || spec_target(&from) != source
        || spec_target(&to) != destination
    {
        return Err(NamespaceError::Unmappable);
    }
    let stages: Vec<_> = core::iter::once(primary)
        .chain(dependents.iter().copied())
        .collect();
    // The evidence of every stage, or of none.
    let retained: Option<Vec<&PreparationIndex>> = if use_retained {
        stages
            .iter()
            .map(|stage| stage.preparation(store.paths()))
            .collect()
    } else {
        None
    };
    let indexes = retained.as_deref().unwrap_or_default();
    let root_layers = primary.layer_stack();
    let mut inspected = Vec::new();
    // Whether every snapshot was proven current by its layer generations.
    let mut current = retained.is_some();
    for (i, stage) in stages.iter().enumerate() {
        current &= validate_snapshot(stage, store, indexes.get(i).copied())?;
        let root = stage.root_layer().ok_or(NamespaceError::NoSuchObject)?;
        inspected.extend(match indexes.get(i) {
            Some(index) => retained::reachable_layers(store, indexes, index, root),
            None => reachable_layers(stage, store, root),
        });
        inspected.extend(stage.layer_stack());
    }
    sort::sort_unstable(&mut inspected);
    inspected.dedup();
    let mut links = HashMap::new();
    for id in &inspected {
        let layer = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        if retained.is_some() {
            // Only specs that author arcs can fail these checks.
            let slots = retained::links_of(indexes, layer);
            for spec in slots.iter().filter_map(|path| layer.prims.get(path)) {
                validate_arcs(store, spec)?;
            }
            links.insert(*id, slots);
            continue;
        }
        for spec in layer.prims.values() {
            validate_arcs(store, spec)?;
        }
    }

    let relocate = requires_relocate(primary, store, source)?;
    if let Some(property) = source.property_path()
        && !root_layers.iter().any(|id| {
            store
                .layer(*id)
                .is_some_and(|l| l.property(property).is_some())
        })
    {
        return Err(NamespaceError::NoSuchObject);
    }
    if source.property_path().is_some() && relocate {
        return Err(NamespaceError::UnsupportedComposition(
            "properties with contributing composition-arc opinions cannot be relocated",
        ));
    }
    let source_move = PathMove::new(source, destination);
    let mut local_moves: HashMap<LayerId, Vec<PathMove>> = HashMap::new();
    let mut local_variant_moves: HashMap<LayerId, Vec<VariantMove>> = HashMap::new();
    let mut stage_moves = Vec::new();
    for (i, stage) in stages.iter().enumerate() {
        let shares_local_namespace = stage
            .layer_stack()
            .iter()
            .any(|id| root_layers.contains(id))
            && match source {
                TargetPath::Prim(path) => stage.has_prim(path),
                TargetPath::Property(path) => stage.has_property_path(path),
            };
        let mut mappings = if i == 0 || shares_local_namespace {
            alloc::vec![source_move.clone()]
        } else {
            Vec::new()
        };
        let mut variant_moves = Vec::new();
        gather_dependent_moves(
            stage,
            indexes.get(i).copied(),
            store,
            root_layers,
            &source_move,
            &mut mappings,
            &mut variant_moves,
        )?;
        normalize_moves(store, &mut mappings)?;
        for mapping in &mappings {
            validate_stage(stage, store, mapping.from, mapping.to)?;
            if mapping.from.property_path().is_none() {
                let from = mapping.from.prim_path();
                let inactive = if retained.is_some() {
                    // The snapshot holds the prims beneath `from` as its
                    // descendants, inactive ones included.
                    stage
                        .traverse_all(from)
                        .any(|path| inactive_prim(stage, store, path))
                } else {
                    stage.prim_paths().any(|path| {
                        store
                            .paths()
                            .resolve(from)
                            .is_prefix_of(store.paths().resolve(path))
                            && inactive_prim(stage, store, path)
                    })
                };
                if inactive {
                    return Err(NamespaceError::UnsupportedComposition(
                        "namespace moves containing inactive composed descendants are not supported",
                    ));
                }
            }
        }
        for id in stage.layer_stack() {
            let moves = local_moves.entry(*id).or_default();
            for mapping in &mappings {
                if !moves
                    .iter()
                    .any(|m| m.from == mapping.from && m.to == mapping.to)
                {
                    moves.push(mapping.clone());
                }
            }
            normalize_moves(store, moves)?;
            let within = local_variant_moves.entry(*id).or_default();
            for mapping in &variant_moves {
                if !within.contains(mapping) {
                    within.push(mapping.clone());
                }
            }
        }
        stage_moves.push(mappings);
    }
    // With retained evidence, each layer is edited as a working copy of
    // the slots the edit can change: those at and beneath the paths it
    // moves, their ancestors, and those that author paths.
    let mut copied: HashMap<LayerId, Vec<PathId>> = HashMap::new();
    let mut edited = HashMap::new();
    let mut dormant: HashMap<LayerId, Dormant> = HashMap::new();
    for id in &inspected {
        let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        let mut layer = match (indexes.first(), links.get(id)) {
            (Some(index), Some(slots)) => {
                let slots = working_slots(store, index, original, slots, local_moves.get(id));
                let copy = retained::working_copy(original, &slots);
                copied.insert(*id, slots);
                copy
            }
            _ => original.clone(),
        };
        let mut unused = dormant_variants(store, &stages, indexes, &layer);
        if let Some(moves) = local_moves.get(id) {
            for mapping in moves {
                validate_authored(&layer, store, mapping)?;
                move_local(store, &mut layer, mapping)?;
                unused = unused.after(store, mapping, &[]);
            }
            for moved in local_variant_moves.get(id).into_iter().flatten() {
                if moves.iter().any(|m| {
                    *m != moved.mapping
                        && m.from.property_path().is_none()
                        && moved.overlaps(store, m.from.prim_path())
                }) {
                    return Err(NamespaceError::UnsupportedComposition(
                        "dependent namespace in a variant of moved namespace is ambiguous",
                    ));
                }
                move_in_variant(store, &mut layer, moved)?;
                unused = unused.after(store, &moved.mapping, &moved.within);
            }
        }
        dormant.insert(*id, unused);
        edited.insert(*id, layer);
    }
    if relocate {
        update_relocates(&mut edited, root_layers, target.layer(), &source_move)?;
    } else {
        // Existing relocate targets must follow moves of their containing local
        // namespace too. Source paths already prohibited by relocates never
        // become new authored local specs.
        update_existing_relocates(store, &mut edited, &local_moves)?;
    }
    for id in &inspected {
        let layer = edited.get_mut(id).unwrap();
        let unused = &dormant[id];
        let moves = local_moves.get(id).map_or(&[][..], Vec::as_slice);
        // A layer used by multiple stages cannot have two meanings for a path.
        // The normalized union has already checked that its maps agree.
        for mapping in moves {
            for (path, spec) in &mut layer.prims {
                repair_prim(
                    store,
                    spec,
                    *path,
                    *id,
                    target.layer(),
                    Some(mapping),
                    &source_move,
                    unused,
                )?;
            }
            for (path, branches) in &mut layer.variant_prims {
                for spec in branches {
                    repair_prim(
                        store,
                        spec,
                        *path,
                        *id,
                        target.layer(),
                        Some(mapping),
                        &source_move,
                        unused,
                    )?;
                }
            }
            let root = store_root_path(store);
            repair_fields(store, &mut layer.metadata, Some(mapping), root, false)?;
            repair_default_prim(store, layer, mapping);
        }
        // External reference/payload targets use the source stack's namespace,
        // never the dependent stage namespace. Source assets otherwise stay put.
        for edited_id in root_layers {
            for (path, spec) in &mut layer.prims {
                repair_prim(
                    store,
                    spec,
                    *path,
                    *id,
                    *edited_id,
                    None,
                    &source_move,
                    unused,
                )?;
            }
            for (path, branches) in &mut layer.variant_prims {
                for spec in branches {
                    repair_prim(
                        store,
                        spec,
                        *path,
                        *id,
                        *edited_id,
                        None,
                        &source_move,
                        unused,
                    )?;
                }
            }
        }
    }
    // The slots each working copy may differ from its layer at.
    let delta: HashMap<LayerId, Vec<PathId>> = copied
        .iter()
        .map(|(id, slots)| (*id, retained::copy_slots(slots, &edited[id])))
        .collect();
    if retained.is_some() {
        let mut edits = HashMap::new();
        for (id, paths) in &delta {
            let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
            if let Some(edit) = retained::layer_edit(original, &edited[id], paths) {
                edits.insert(*id, edit);
            }
        }
        validate_projection_retained(
            store,
            &edited,
            &edits,
            &stages,
            &stage_moves,
            indexes,
            current,
        )?;
    } else {
        validate_projection(store, &edited, &stages, &stage_moves)?;
    }
    let mut transaction = Transaction::new();
    let mut layers = Vec::new();
    let mut specs = Vec::new();
    let mut relocates = Vec::new();
    for id in inspected {
        let original = store.layer(id).unwrap();
        let layer = &edited[&id];
        transaction.expect_generation(id, original.generation());
        let previous = transaction.len();
        match delta.get(&id) {
            Some(paths) => {
                retained::append_delta_at(original, layer, paths, &mut transaction, &mut specs);
            }
            None => append_delta(original, layer, &mut transaction, &mut specs),
        }
        if original.default_prim != layer.default_prim || !original.metadata.same(&layer.metadata) {
            push(
                &mut transaction,
                Raw::LayerFields {
                    layer: id,
                    default_prim: layer.default_prim,
                    metadata: layer.metadata.clone(),
                },
                Raw::LayerFields {
                    layer: id,
                    default_prim: original.default_prim,
                    metadata: original.metadata.clone(),
                },
            );
        }
        if original.relocates != layer.relocates {
            relocates.push((id, layer.relocates().to_vec()));
            push(
                &mut transaction,
                Raw::LayerRelocates {
                    layer: id,
                    relocates: layer.relocates.clone(),
                },
                Raw::LayerRelocates {
                    layer: id,
                    relocates: original.relocates.clone(),
                },
            );
        }
        if transaction.len() != previous {
            layers.push(id);
        }
    }
    sort::sort_unstable_by(&mut specs, |a, b| {
        a.0.cmp(&b.0).then(a.1.prim_path().cmp(&b.1.prim_path()))
    });
    Ok(NamespaceEdit {
        transaction,
        source,
        destination: Some(destination),
        layers,
        specs,
        namespace_moves: stages
            .iter()
            .zip(&stage_moves)
            .flat_map(|(s, moves)| {
                moves
                    .iter()
                    .map(|m| (s.root_layer().unwrap(), m.from, m.to))
            })
            .collect(),
        relocates,
    })
}

/// The variant specs of `layer`, before the edit, that no prim index of
/// `stages` uses ([`Dormant`]).
///
/// In a layer of a stage's root layer stack, a variant spec is used where
/// the prim at its path has a node at its site in that stack: OpenUSD
/// fixes paths only in the specs of a prim's local and local variant
/// nodes (`_GatherPathBearingFieldEditsForStage`), and a variant spec
/// reached through another arc (`/World/Copy` referencing `/World/A` with
/// another selection) is that arc's to keep. In any other layer the edit
/// repairs only reference and payload targets, so only the variant specs
/// authoring those are looked up, among the prims with a node at their
/// path. AOUSD Core §10.4 (prim index nodes), §10.5 (variants).
fn dormant_variants(
    store: &dyn LayerStore,
    stages: &[&Stage],
    indexes: &[&PreparationIndex],
    layer: &Layer,
) -> Dormant {
    let local = stages.iter().any(|s| s.layer_stack().contains(&layer.id));
    let mut out = Dormant::default();
    if layer.variant_prims.is_empty() && layer.prims.values().all(|p| p.variant_sets().is_empty()) {
        return out;
    }
    let used = |path: PathId, sites: &[VariantSelectionSite]| {
        let at_site = |node: crate::prim_index_graph::PrimNode<'_>| {
            node.site().prim_path() == path
                && node
                    .site()
                    .variant_selection_sites()
                    .eq(sites.iter().copied())
        };
        if local {
            return stages.iter().any(|stage| {
                stage.layer_stack().contains(&layer.id)
                    && stage.explain_prim_graph(path).is_some_and(|graph| {
                        let stack = graph.node(NodeId::ROOT).map(|n| n.layer_stack_identifier());
                        graph.nodes().any(|(_, node)| {
                            at_site(node) && Some(node.layer_stack_identifier()) == stack
                        })
                    })
            });
        }
        stages.iter().enumerate().any(|(i, stage)| {
            let prims: Vec<PathId> = match indexes.get(i) {
                Some(index) => index.prims_with_nodes_at(path).collect(),
                None => stage.prim_paths().collect(),
            };
            prims.into_iter().any(|prim| {
                stage.explain_prim_graph(prim).is_some_and(|graph| {
                    graph.nodes().any(|(_, node)| {
                        at_site(node)
                            && LayerStack::gather_identifier(store, node.layer_stack_identifier())
                                .layers
                                .contains(&layer.id)
                    })
                })
            })
        })
    };
    // Outside the root layer stack, only arcs are repaired.
    let repairable = |refs: &ListOp<Reference>, payloads: &ListOp<Reference>| {
        local || refs.items().next().is_some() || payloads.items().next().is_some()
    };
    let mut visit = |path: PathId, sites: &mut Vec<VariantSelectionSite>, sets: &VariantSets| {
        fn walk(
            path: PathId,
            sites: &mut Vec<VariantSelectionSite>,
            sets: &VariantSets,
            check: &mut dyn FnMut(PathId, &[VariantSelectionSite], &VariantSpec),
        ) {
            for (set, spec) in sets {
                for (variant, branch) in &spec.variants {
                    sites.push(VariantSelectionSite {
                        host_path: path,
                        set: *set,
                        variant: *variant,
                    });
                    check(path, sites, branch);
                    walk(path, sites, &branch.variant_sets, check);
                    sites.pop();
                }
            }
        }
        walk(path, sites, sets, &mut |path, sites, branch| {
            if repairable(&branch.references, &branch.payloads) && !used(path, sites) {
                out.insert(path, sites.to_vec());
            }
        });
    };
    let mut unused_specs = Vec::new();
    let specs = layer.prims.iter().map(|(p, s)| (*p, s)).chain(
        layer
            .variant_prims
            .iter()
            .flat_map(|(p, specs)| specs.iter().map(move |s| (*p, s))),
    );
    for (path, spec) in specs {
        let mut sites = spec.outer_variant_sites().to_vec();
        if !sites.is_empty() && repairable(&spec.references, &spec.payloads) && !used(path, &sites)
        {
            // Everything inside it is unused too.
            unused_specs.push((path, sites));
            continue;
        }
        visit(path, &mut sites, spec.variant_sets());
    }
    for (path, sites) in unused_specs {
        out.insert(path, sites);
    }
    out
}

/// Rejects a spec whose references or payloads target an unloaded or
/// unresolved layer, or an asset expression. A spec inside a variant
/// passes: a selected variant's unresolved arc is a composition error the
/// snapshot check rejects, and one in a variant no prim selects composes
/// nothing for the edit to keep.
fn validate_arcs(store: &dyn LayerStore, spec: &PrimSpec) -> Result<(), NamespaceError> {
    if !spec.outer_variant_sites().is_empty() {
        return Ok(());
    }
    if spec
        .references
        .items()
        .chain(spec.payloads.items())
        .any(|r| {
            r.stored_target_layer(store)
                .is_none_or(|target| store.layer(target).is_none())
                || r.is_expression()
        })
    {
        return Err(NamespaceError::UnsupportedComposition(
            "unloaded or expression-based assets require resolved dependency coordination",
        ));
    }
    Ok(())
}

/// The slots of `layer` a namespace edit making `moves` in it can change
/// or must inspect: the slots that author paths (`links`), and for each
/// move, the slots at and beneath its source and destination and their
/// ancestors.
fn working_slots(
    store: &dyn LayerStore,
    index: &PreparationIndex,
    layer: &Layer,
    links: &HashSet<PathId>,
    moves: Option<&Vec<PathMove>>,
) -> Vec<PathId> {
    let mut slots: HashSet<PathId> = links.clone();
    for mapping in moves.into_iter().flatten() {
        let (from, to) = (mapping.from.prim_path(), mapping.to.prim_path());
        slots.extend(retained::slots_beneath(
            store,
            index.children(),
            layer,
            from,
        ));
        if mapping.to.property_path().is_none() {
            slots.extend(retained::slots_beneath(store, index.children(), layer, to));
        }
        slots.extend(retained::at_or_above(store, from));
        slots.extend(retained::at_or_above(store, to));
    }
    let mut slots: Vec<PathId> = slots
        .into_iter()
        .filter(|path| layer.prims.contains_key(path) || layer.variant_prims.contains_key(path))
        .collect();
    sort::sort_unstable(&mut slots);
    slots
}

/// Rejects a snapshot that does not reflect `store`, or whose composition
/// preparation cannot coordinate. A snapshot whose `index` records the
/// generations of every layer it reads is current when they all still
/// match; any other is composed again and compared.
fn validate_snapshot(
    stage: &Stage,
    store: &mut dyn LayerStore,
    index: Option<&PreparationIndex>,
) -> Result<bool, NamespaceError> {
    if stage.store_identity() != Some(&store.identity()) {
        return Err(NamespaceError::UnsupportedComposition(
            "dependent stages must share the store's token/path domains",
        ));
    }
    if stage.population_mask().is_some()
        || !stage.muted_layers().is_empty()
        || stage
            .load_rules()
            .rules()
            .any(|(_, r)| r != crate::PayloadRule::All)
        || !stage.composition_errors().is_empty()
    {
        return Err(NamespaceError::UnsupportedComposition(
            "masked, muted, unloaded or unresolved composition requires explicit dependency coordination",
        ));
    }
    if index.is_some_and(|index| index.is_current(store)) {
        return Ok(true);
    }
    let fresh = Stage::compose(
        store,
        stage.root_layer().ok_or(NamespaceError::NoSuchObject)?,
        stage.options().clone(),
    );
    let mut old: Vec<_> = stage.prim_paths().collect();
    let mut new: Vec<_> = fresh.prim_paths().collect();
    sort::sort_unstable(&mut old);
    sort::sort_unstable(&mut new);
    if old != new
        || old.iter().any(|p| {
            stage.prim_stack(*p) != fresh.prim_stack(*p)
                || graph_signature(stage, *p) != graph_signature(&fresh, *p)
        })
    {
        return Err(NamespaceError::UnsupportedComposition(
            "namespace preparation requires current stage snapshots",
        ));
    }
    Ok(false)
}

/// The namespace moves in `stage` that follow `source` through arcs into
/// the layers `source_layers`: one for each prim whose composition graph
/// has a node other than its local one at or beneath the moved site. The
/// prims are visited in [`PathId`] order, so the first that cannot be
/// mapped is always the one reported; with retained evidence (`index`),
/// only the prims it lists with such nodes, otherwise every prim.
fn gather_dependent_moves(
    stage: &Stage,
    index: Option<&PreparationIndex>,
    store: &mut dyn LayerStore,
    source_layers: &[LayerId],
    source: &PathMove,
    out: &mut Vec<PathMove>,
    variant_out: &mut Vec<VariantMove>,
) -> Result<(), NamespaceError> {
    let raw_from = store.paths().resolve(source.from.prim_path()).clone();
    let raw_to = store.paths().resolve(source.to.prim_path()).clone();
    let prims: Vec<PathId> = match index {
        Some(index) => {
            let mut prims: Vec<PathId> = index
                .prims_with_nodes_beneath(source.from.prim_path())
                .collect();
            sort::sort_unstable(&mut prims);
            prims
        }
        None => {
            let mut prims: Vec<PathId> = stage.prim_paths().collect();
            sort::sort_unstable(&mut prims);
            prims
        }
    };
    for prim in prims {
        let Some(graph) = stage.explain_prim_graph(prim) else {
            continue;
        };
        let composed = store.paths().resolve(prim).clone();
        for (_, node) in graph.nodes() {
            // A variant arc names no path: its site follows its host's
            // (`PcpArcTypeVariant` in
            // `_ProcessDependentNodePathAtIntroductionChange`,
            // `pxr/usd/pcp/dependentNamespaceEditUtils.cpp`).
            if matches!(node.arc_kind(), ArcKind::Local | ArcKind::Variants) {
                continue;
            }
            let stack = LayerStack::gather_identifier(store, node.layer_stack_identifier());
            if !stack.layers.iter().any(|id| source_layers.contains(id)) {
                continue;
            }
            let site = store.paths().resolve(node.site().prim_path()).clone();
            let Some(suffix) = site.strip_prefix(&raw_from) else {
                continue;
            };
            if source.from.property_path().is_some() && !suffix.is_empty() {
                continue;
            }
            // An arc authored in the local variants the prim selects hangs
            // beneath their nodes rather than the root, and is implied to
            // the root at the same site, through the identity map of those
            // variants (`_EvalImpliedClasses`, `pxr/usd/pcp/primIndex.cpp`):
            // the arc it is implied from is the one to follow.
            let in_variants = node.parent() != Some(NodeId::ROOT);
            if node.is_implied()
                && node
                    .origin()
                    .and_then(|id| graph.node(id))
                    .is_some_and(|origin| {
                        origin.site() == node.site()
                            && origin.layer_stack_identifier() == node.layer_stack_identifier()
                            && origin.parent() != Some(NodeId::ROOT)
                            && authored_in_local_variants(graph, origin)
                    })
            {
                continue;
            }
            if node.site().has_variant_selections()
                || !authored_in_local_variants(graph, node)
                || node.is_implied()
            {
                return Err(NamespaceError::UnsupportedComposition(
                    "dependent namespace through nested or variant arcs requires an unambiguous mapping",
                ));
            }
            let mount_depth = usize::from(node.namespace_depth());
            if mount_depth > composed.depth() || composed.depth() - mount_depth > site.depth() {
                return Err(NamespaceError::Unmappable);
            }
            let raw_mount = Path::root()
                .join(&site.segments()[..site.depth() - (composed.depth() - mount_depth)]);
            // Moving the referenced root changes its target, not its mount.
            if source.from.property_path().is_none() && raw_from.is_prefix_of(&raw_mount) {
                continue;
            }
            let Some(dest_suffix) = raw_to.strip_prefix(&raw_mount) else {
                return Err(NamespaceError::UnsupportedComposition(
                    "dependent namespace reparenting outside an arc's source domain is not supported",
                ));
            };
            if suffix.len() > composed.depth() {
                return Err(NamespaceError::Unmappable);
            }
            let from = store
                .paths()
                .intern(Path::root().join(&composed.segments()[..composed.depth() - suffix.len()]));
            let to = store.paths().intern(
                Path::root()
                    .join(&composed.segments()[..mount_depth])
                    .join(dest_suffix),
            );
            let mapping = if let (Some(a), Some(b)) =
                (source.from.property_path(), source.to.property_path())
            {
                PathMove::new(
                    TargetPath::Property(PropertyPath::new(from, a.property())),
                    TargetPath::Property(PropertyPath::new(to, b.property())),
                )
            } else {
                PathMove::new(TargetPath::Prim(from), TargetPath::Prim(to))
            };
            if mapping.from == mapping.to {
                continue;
            }
            // The local specs to move are at the sites of the nodes above
            // the arc's, each in its own branches: OpenUSD moves the specs
            // of every node on the way up to the root
            // (`_PrimIndexDependentNodeEditProcessor::_AddSpecMoveEdits`).
            if in_variants {
                let from_path = store.paths().resolve(from).clone();
                let mut parent = node.parent();
                while let Some(above) = parent.filter(|id| *id != NodeId::ROOT) {
                    let above = graph.node(above).ok_or(NamespaceError::Unmappable)?;
                    let within: Vec<VariantSelectionSite> = above
                        .site()
                        .variant_selection_sites()
                        .filter(|site| {
                            let host = store.paths().resolve(site.host_path);
                            host.is_prefix_of(&from_path) && host.depth() < from_path.depth()
                        })
                        .collect();
                    let moved = VariantMove {
                        mapping: mapping.clone(),
                        within,
                    };
                    if !moved.within.is_empty() && !variant_out.contains(&moved) {
                        variant_out.push(moved);
                    }
                    parent = above.parent();
                }
            }
            if !out
                .iter()
                .any(|m| m.from == mapping.from && m.to == mapping.to)
            {
                out.push(mapping);
            }
        }
    }
    Ok(())
}

/// Whether `node`'s arc is authored at the root's site or inside the local
/// variants it selects: every node above it is a variant node of the root
/// layer stack.
fn authored_in_local_variants(
    graph: &crate::PrimIndexGraph,
    node: crate::prim_index_graph::PrimNode<'_>,
) -> bool {
    let Some(root) = graph.node(NodeId::ROOT) else {
        return false;
    };
    let mut parent = node.parent();
    while let Some(id) = parent.filter(|id| *id != NodeId::ROOT) {
        let Some(above) = graph.node(id) else {
            return false;
        };
        if above.arc_kind() != ArcKind::Variants
            || above.layer_stack_identifier() != root.layer_stack_identifier()
        {
            return false;
        }
        parent = above.parent();
    }
    parent == Some(NodeId::ROOT)
}

fn normalize_moves(
    store: &mut dyn LayerStore,
    moves: &mut Vec<PathMove>,
) -> Result<(), NamespaceError> {
    sort::sort_by_key(moves, |m| {
        (store.paths().resolve(m.from.prim_path()).depth(), m.from)
    });
    let mut normalized: Vec<PathMove> = Vec::new();
    for mapping in moves.drain(..) {
        let mut covered = false;
        for earlier in &normalized {
            if earlier.target(store, mapping.from) != mapping.from {
                if earlier.target(store, mapping.from) != mapping.to {
                    return Err(NamespaceError::UnsupportedComposition(
                        "shared layer has conflicting dependent namespace maps",
                    ));
                }
                covered = true;
            }
            if earlier.target(store, mapping.to) != mapping.to
                || mapping.target(store, earlier.to) != earlier.to
            {
                return Err(NamespaceError::UnsupportedComposition(
                    "overlapping dependent namespace destinations are ambiguous",
                ));
            }
        }
        if !covered {
            normalized.push(mapping);
        }
    }
    *moves = normalized;
    Ok(())
}

fn requires_relocate(
    stage: &Stage,
    store: &dyn LayerStore,
    source: TargetPath,
) -> Result<bool, NamespaceError> {
    let graph = stage
        .explain_prim_graph(source.prim_path())
        .ok_or(NamespaceError::NoSuchObject)?;
    let opinions = stage.prim_stack(source.prim_path()).unwrap_or_default();
    let depth = store.paths().resolve(source.prim_path()).depth();
    if source.property_path().is_none()
        && stage.layer_stack().iter().any(|id| {
            store.layer(*id).is_some_and(|l| {
                l.relocates()
                    .iter()
                    .any(|r| r.target == Some(source.prim_path()))
            })
        })
    {
        return Ok(true);
    }
    for (id, node) in graph.nodes() {
        if id == NodeId::ROOT {
            continue;
        }
        let stack = LayerStack::gather_identifier(store, node.layer_stack_identifier());
        let contributes = opinions.iter().any(|(l, path)| {
            stack.layers.contains(l)
                && path == node.site()
                && source.property_path().is_none_or(|p| {
                    store
                        .layer(*l)
                        .and_then(|layer| spec_at(layer, &Loc::of(path)))
                        .is_some_and(|s| s.properties().iter().any(|e| e.name == p.property()))
                })
        });
        if !contributes {
            continue;
        }
        // Properties never carry an arc with them. For prims, a direct arc
        // moves with its local prim spec, while an ancestral arc does not.
        // Pcp HasSpecs/IsInert is represented by membership in the actual
        // composed opinion stack, including nested nodes beneath this child.
        if source.property_path().is_some() {
            return Ok(true);
        }
        let mut child = node;
        while let Some(parent) = child.parent().filter(|p| *p != NodeId::ROOT) {
            child = graph.node(parent).ok_or(NamespaceError::Unmappable)?;
        }
        if child.arc_kind() == ArcKind::Relocates || usize::from(child.namespace_depth()) < depth {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Rejects a move of namespace holding an authored inactive opinion
/// outside every variant: the descendants it hides need coordination.
/// One inside a variant a prim index selects makes its prim inactive,
/// which the composed checks reject; one in a variant none selects
/// changes nothing composed and moves with its prim.
fn validate_authored(
    layer: &Layer,
    store: &dyn LayerStore,
    mapping: &PathMove,
) -> Result<(), NamespaceError> {
    for (path, spec) in &layer.prims {
        let relevant = spec.outer_variant_sites().is_empty()
            && store
                .paths()
                .resolve(mapping.from.prim_path())
                .is_prefix_of(store.paths().resolve(*path));
        if relevant && spec.active == Some(false) {
            return Err(NamespaceError::UnsupportedComposition(
                "namespace moves containing inactive opinions are not supported",
            ));
        }
    }
    Ok(())
}

fn move_local(
    store: &mut dyn LayerStore,
    layer: &mut Layer,
    mapping: &PathMove,
) -> Result<(), NamespaceError> {
    let has_source = if let Some(p) = mapping.from.property_path() {
        layer.property(p).is_some()
    } else {
        // Specs at the moved path inside an ancestor's variant stay with
        // that variant ([`move_specs`]): the edit relocates their prim.
        let from = store.paths().resolve(mapping.from.prim_path());
        let beneath = |p: &PathId| from.is_prefix_of(store.paths().resolve(*p));
        layer
            .prims
            .iter()
            .chain(
                layer
                    .variant_prims
                    .iter()
                    .flat_map(|(p, specs)| specs.iter().map(move |s| (p, s))),
            )
            .any(|(p, s)| beneath(p) && moves_with(store, from, &[], s))
    };
    if !has_source {
        return Ok(());
    }
    let parent = if mapping.to.property_path().is_some() {
        store.paths().resolve(mapping.to.prim_path()).clone()
    } else {
        store
            .paths()
            .resolve(mapping.to.prim_path())
            .parent()
            .unwrap()
    };
    ensure_ancestors(store, layer, parent);
    if layer.prim_spec_in(mapping.from.prim_path(), &[]).is_none() {
        layer.insert_prim(mapping.from.prim_path(), PrimSpec::over());
    }
    let from = SpecPath::from_prim_path(mapping.from.prim_path());
    let to = SpecPath::from_prim_path(mapping.to.prim_path());
    let from = mapping
        .from
        .property_path()
        .map_or(from.clone(), |p| from.with_property(p.property()));
    let to = mapping
        .to
        .property_path()
        .map_or(to.clone(), |p| to.with_property(p.property()));
    move_specs(store, layer, &from, &to)
}
/// A dependent move of the local specs inside the branches `within`, all
/// hosted above the moved path: those of an arc authored in a selected
/// variant of an ancestor, whose composed namespace moves with the arc's
/// target (`/Host{v=on}Inner/Kid` when `/Host/Inner` inherits a moved
/// `/B/Kid`).
#[derive(Clone, PartialEq)]
struct VariantMove {
    mapping: PathMove,
    within: Vec<VariantSelectionSite>,
}

impl VariantMove {
    /// Whether moving the namespace at `path` moves these specs or the
    /// variants hosting them.
    fn overlaps(&self, store: &dyn LayerStore, path: PathId) -> bool {
        let path = store.paths().resolve(path);
        path.is_prefix_of(store.paths().resolve(self.mapping.from.prim_path()))
            || path.is_prefix_of(store.paths().resolve(self.mapping.to.prim_path()))
            || self
                .within
                .iter()
                .any(|site| path.is_prefix_of(store.paths().resolve(site.host_path)))
    }
}

/// Moves `layer`'s specs at `moved`'s source inside its branches, as
/// [`move_local`] moves those outside every branch. A layer with no spec
/// there has none to move; the destination's parent must have one.
fn move_in_variant(
    store: &mut dyn LayerStore,
    layer: &mut Layer,
    moved: &VariantMove,
) -> Result<(), NamespaceError> {
    let paths = store.paths();
    let from = SpecPath::from_variant_selection_sites(
        moved.mapping.from.prim_path(),
        &moved.within,
        paths,
    );
    let to =
        SpecPath::from_variant_selection_sites(moved.mapping.to.prim_path(), &moved.within, paths);
    let (from, to) = match (
        moved.mapping.from.property_path(),
        moved.mapping.to.property_path(),
    ) {
        (Some(a), Some(b)) => (
            from.with_property(a.property()),
            to.with_property(b.property()),
        ),
        _ => (from, to),
    };
    let Some(spec) = spec_at(layer, &Loc::of(&from)) else {
        return Ok(());
    };
    if from
        .property()
        .is_some_and(|name| !spec.properties().iter().any(|p| p.name == name))
    {
        return Ok(());
    }
    move_specs(store, layer, &from, &to)
}

fn ensure_ancestors(store: &mut dyn LayerStore, layer: &mut Layer, path: Path) {
    if path.segments().is_empty() {
        return;
    }
    let parent = path.parent().unwrap();
    ensure_ancestors(store, layer, parent.clone());
    let name = path.leaf().unwrap();
    let id = store.paths().intern(path);
    if layer.prim_spec_in(id, &[]).is_some() {
        return;
    }
    layer.insert_prim(id, PrimSpec::over());
    // The new over is its parent's last child, as `SdfJustCreatePrimInLayer`
    // lists the specs it creates (`pxr/usd/sdf/primSpec.cpp`), which
    // `UsdNamespaceEditor` calls for a reparent's new parent. AOUSD Core
    // §7.6.2.2.1 (`primChildren`).
    let parent = store.paths().intern(parent);
    if let Some(spec) = layer
        .prims
        .get_mut(&parent)
        .filter(|spec| spec.outer_variant_sites().is_empty())
        && !spec.authored_children.contains(&name)
    {
        spec.authored_children.push(name);
    }
}

fn update_relocates(
    layers: &mut HashMap<LayerId, Layer>,
    roots: &[LayerId],
    target: LayerId,
    mapping: &PathMove,
) -> Result<(), NamespaceError> {
    let mut old_source = None;
    for id in roots {
        for r in layers[id].relocates() {
            if r.target == Some(mapping.from.prim_path()) {
                if old_source.is_some_and(|p| p != r.source) {
                    return Err(NamespaceError::UnsupportedComposition(
                        "conflicting authored relocate targets",
                    ));
                }
                old_source = Some(r.source);
            }
        }
    }
    if let Some(source) = old_source {
        for id in roots {
            if let Some(relocates) = &mut layers.get_mut(id).unwrap().relocates {
                relocates.retain(|r| r.source != source);
            }
        }
        if source != mapping.to.prim_path() {
            layers
                .get_mut(&target)
                .unwrap()
                .relocates_mut()
                .push(Relocate {
                    source,
                    target: Some(mapping.to.prim_path()),
                });
        }
    } else {
        layers
            .get_mut(&target)
            .unwrap()
            .relocates_mut()
            .push(Relocate {
                source: mapping.from.prim_path(),
                target: Some(mapping.to.prim_path()),
            });
    }
    // A relocation source is prohibited from holding local opinions. Local
    // opinions were moved above; unrelated source-layer assets were not.
    Ok(())
}
fn update_existing_relocates(
    store: &mut dyn LayerStore,
    layers: &mut HashMap<LayerId, Layer>,
    moves: &HashMap<LayerId, Vec<PathMove>>,
) -> Result<(), NamespaceError> {
    for (id, mappings) in moves {
        for mapping in mappings {
            for r in layers.get_mut(id).unwrap().relocates.iter_mut().flatten() {
                r.source = mapping.prim(store, r.source);
                if let Some(target) = &mut r.target {
                    *target = mapping.prim(store, *target);
                }
                if r.target == Some(r.source) {
                    return Err(NamespaceError::UnsupportedComposition(
                        "namespace edit produced an identity relocate",
                    ));
                }
            }
        }
    }
    Ok(())
}
fn repair_default_prim(store: &mut dyn LayerStore, layer: &mut Layer, mapping: &PathMove) {
    if mapping.from.property_path().is_some() {
        return;
    }
    let Some(path) = layer.default_prim_path(store.tokens()) else {
        return;
    };
    let old = store.paths().intern(path);
    let new = mapping.prim(store, old);
    if old != new {
        let path = store.paths().resolve(new);
        let text = if path.depth() == 1 {
            alloc::string::String::from(store.tokens().resolve(path.leaf().unwrap()))
        } else {
            path.display(store.tokens())
        };
        layer.default_prim = Some(store.tokens().intern(&text));
    }
}

/// The store, with edited copies of some of its layers in place of theirs.
pub(super) struct Overlay<'a> {
    pub(super) store: &'a mut dyn LayerStore,
    pub(super) layers: &'a HashMap<LayerId, Layer>,
}
impl LayerStore for Overlay<'_> {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.get(&id).or_else(|| self.store.layer(id))
    }
    fn is_layer_pending(&self, id: LayerId) -> bool {
        !self.layers.contains_key(&id) && self.store.is_layer_pending(id)
    }
    fn layer_mut(&mut self, _: LayerId) -> Option<&mut Layer> {
        None
    }
    fn tokens(&self) -> &crate::TokenInterner {
        self.store.tokens()
    }
    fn paths(&self) -> &crate::PathInterner {
        self.store.paths()
    }
    fn layer_identifier(&self, id: LayerId) -> Option<&str> {
        self.store.layer_identifier(id)
    }
    fn asset_layer(&self, anchor: LayerId, path: &str) -> Option<LayerId> {
        self.store.asset_layer(anchor, path)
    }
    fn asset_availability(&self, anchor: LayerId, path: &str) -> crate::AssetAvailability {
        self.store.asset_availability(anchor, path)
    }
}
/// What composing only part of each projected stage needs: the stages'
/// retained evidence, what the edit changes in each layer, and an index of
/// the projected layers' child specs.
struct ProjectionScope<'a> {
    indexes: &'a [&'a PreparationIndex],
    edits: &'a HashMap<LayerId, retained::LayerEdit>,
    children: Option<ChildIndex>,
}

impl ProjectionScope<'_> {
    /// The prims of `stage`, the `i`th stage, whose composition the edit
    /// can change, with the namespace beneath them; `None` when they
    /// cannot be told apart from the rest of the stage.
    ///
    /// A prim's index reads the specs at its nodes' sites, and beneath
    /// them for its descendants, so a changed slot changes the prim at
    /// the slot's path, the prims with a node at the slot's site, and the
    /// namespace beneath the prims with a node at an ancestor of it. A
    /// slot whose child list alone changes adds or removes only children
    /// whose own slots changed. A `defaultPrim` follows the prim it names,
    /// whose slot changes with it. Relocates and layer metadata reach
    /// beyond the slots that author them, and instances, dynamic payloads
    /// and value clips share or read composition elsewhere; for those the
    /// whole stage is composed.
    ///
    /// Spec: AOUSD Core §10 (composition arcs and their sites), §11
    /// (population). OpenUSD: `PcpChanges::DidChange` maps the same
    /// significant changes to the prim indexes depending on their sites.
    fn roots(
        &self,
        store: &dyn LayerStore,
        stage: &Stage,
        i: usize,
        moves: &[PathMove],
    ) -> Option<Vec<PathId>> {
        let index = self.indexes.get(i)?;
        if self.edits.values().any(|edit| edit.fields)
            || stage.composes_instances()
            || !stage.dynamic_layers().is_empty()
            || !stage.clip_layers().is_empty()
            || stage
                .used_layers(false)
                .iter()
                .any(|id| store.layer(*id).is_some_and(|l| !l.relocates().is_empty()))
        {
            return None;
        }
        let mut roots = Vec::new();
        for (id, edit) in self.edits {
            let local = stage.layer_stack().contains(id);
            for &slot in &edit.structural {
                if local {
                    roots.push(slot);
                }
                let path = store.paths().resolve(slot).clone();
                for site in retained::at_or_above(store, slot) {
                    let suffix = path
                        .strip_prefix(store.paths().resolve(site))
                        .expect("ancestor");
                    for prim in index.prims_with_nodes_at(site) {
                        roots.push(store.paths().intern_joined(prim, suffix));
                    }
                }
            }
        }
        for mapping in moves {
            roots.push(mapping.from.prim_path());
            roots.push(mapping.to.prim_path());
        }
        Some(retained::minimal_roots(store, &roots))
    }

    /// The projection of `stage`, the `i`th stage, composing only the
    /// prims [`Self::roots`] finds, the prims with arcs to them, and
    /// their ancestors; `None` when the whole stage must be composed.
    fn compose(
        &self,
        store: &mut dyn LayerStore,
        stage: &Stage,
        i: usize,
        moves: &[PathMove],
    ) -> Option<Stage> {
        let index = self.indexes.get(i)?;
        let mut roots = self.roots(store, stage, i, moves)?;
        let pseudo_root = store.paths().intern(Path::root());
        let fresh;
        let children = match &self.children {
            Some(children) => children,
            None => {
                fresh = ChildIndex::default();
                &fresh
            }
        };
        loop {
            if roots.contains(&pseudo_root) {
                return None;
            }
            let options = crate::StageOptions {
                mask: Some(crate::PopulationMask {
                    include: roots.clone(),
                }),
                with_dependencies: false,
                ..stage.options().clone()
            };
            let partial = Stage::compose_retained(
                store,
                stage.root_layer()?,
                options,
                false,
                &Boundaries::default(),
                children,
            );
            if partial.composes_instances() {
                return None;
            }
            // Prims with arcs to the composed namespace, before or after
            // the edit, and parents the edit leaves without children.
            let within: HashSet<PathId> = roots.iter().copied().collect();
            let mut more = Vec::new();
            for &root in &roots {
                for prim in stage.traverse_all(root).chain(partial.traverse_all(root)) {
                    more.extend(
                        index
                            .prims_with_nodes_at(prim)
                            .filter(|p| !retained::is_beneath(store, *p, &within)),
                    );
                }
                if let Some(parent) = store.paths().parent(root)
                    && parent != pseudo_root
                    && stage.has_prim(parent)
                    && !partial.has_prim(parent)
                    && !retained::is_beneath(store, parent, &within)
                {
                    more.push(parent);
                }
            }
            if more.is_empty() {
                return Some(partial);
            }
            roots.extend(more);
            roots = retained::minimal_roots(store, &roots);
        }
    }
}

/// Composes each stage over the edited `layers` and rejects an edit that
/// would make its composition invalid or not map its namespace as
/// planned.
fn validate_projection(
    store: &mut dyn LayerStore,
    layers: &HashMap<LayerId, Layer>,
    stages: &[&Stage],
    mappings: &[Vec<PathMove>],
) -> Result<(), NamespaceError> {
    let mut overlay = Overlay { store, layers };
    validate_projected_stages(&mut overlay, stages, mappings, None)
}

/// [`validate_projection`] for the `edits` of the working copies `edited`:
/// the projected layers are composed in place in `store`, or copied whole
/// when the store does not lend them, and where every snapshot is
/// `current`, a stage whose prims the edit changes can be told apart
/// composes only those.
fn validate_projection_retained(
    store: &mut dyn LayerStore,
    edited: &HashMap<LayerId, Layer>,
    edits: &HashMap<LayerId, retained::LayerEdit>,
    stages: &[&Stage],
    mappings: &[Vec<PathMove>],
    indexes: &[&PreparationIndex],
    current: bool,
) -> Result<(), NamespaceError> {
    let scope = if current {
        let mut projected = Vec::new();
        for (id, edit) in edits {
            let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
            projected.push((original, &edited[id], edit.slots.as_slice()));
        }
        let children = indexes
            .first()
            .and_then(|index| index.children().projected(store, &projected));
        Some(ProjectionScope {
            indexes,
            edits,
            children,
        })
    } else {
        None
    };
    let mut in_place = retained::InPlace::project(store, edited, edits);
    if in_place.complete() {
        return validate_projected_stages(in_place.store(), stages, mappings, scope.as_ref());
    }
    drop(in_place);
    let mut layers = HashMap::new();
    for (id, edit) in edits {
        let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        layers.insert(*id, retained::materialize(original, &edited[id], edit));
    }
    let mut overlay = Overlay {
        store,
        layers: &layers,
    };
    validate_projected_stages(&mut overlay, stages, mappings, scope.as_ref())
}

/// Composes each stage over the projected `store`, only in part where
/// `scope` allows, and rejects an edit that would make its composition
/// invalid or not map its namespace as planned.
fn validate_projected_stages(
    store: &mut dyn LayerStore,
    stages: &[&Stage],
    mappings: &[Vec<PathMove>],
    scope: Option<&ProjectionScope<'_>>,
) -> Result<(), NamespaceError> {
    for (i, (stage, moves)) in stages.iter().zip(mappings).enumerate() {
        let projected = match scope.and_then(|scope| scope.compose(store, stage, i, moves)) {
            Some(projected) => {
                // Every test that prepares an edit checks the part against
                // the whole.
                #[cfg(test)]
                {
                    let whole =
                        Stage::compose(store, stage.root_layer().unwrap(), stage.options().clone());
                    assert_eq!(
                        projection_findings(&projected, moves),
                        projection_findings(&whole, moves),
                        "a partial projection finds what the whole projection finds"
                    );
                }
                projected
            }
            None => Stage::compose(store, stage.root_layer().unwrap(), stage.options().clone()),
        };
        if !projected.composition_errors().is_empty() {
            return Err(NamespaceError::UnsupportedComposition(
                "prepared namespace edit would create invalid composition",
            ));
        }
        for mapping in moves {
            let exists = |p| match p {
                TargetPath::Prim(p) => projected.has_prim(p),
                TargetPath::Property(p) => projected.has_property_path(p),
            };
            if exists(mapping.from) || !exists(mapping.to) {
                return Err(NamespaceError::UnsupportedComposition(
                    "prepared namespace edit did not preserve its composed namespace mapping",
                ));
            }
        }
    }
    Ok(())
}

/// What validating a projection reads from it: whether it composes
/// without errors, and whether each move's source and destination exist.
#[cfg(test)]
fn projection_findings(projected: &Stage, moves: &[PathMove]) -> (bool, Vec<(bool, bool)>) {
    let exists = |p| match p {
        TargetPath::Prim(p) => projected.has_prim(p),
        TargetPath::Property(p) => projected.has_property_path(p),
    };
    (
        projected.composition_errors().is_empty(),
        moves
            .iter()
            .map(|m| (exists(m.from), exists(m.to)))
            .collect(),
    )
}

#[derive(PartialEq)]
struct GraphNodeEvidence {
    kind: ArcKind,
    parent: Option<NodeId>,
    site: SpecPath,
    stack: crate::LayerStackIdentifier,
    depth: u16,
    sibling: u16,
    implied: bool,
}
fn graph_signature(stage: &Stage, path: PathId) -> Vec<GraphNodeEvidence> {
    stage.explain_prim_graph(path).map_or_else(Vec::new, |g| {
        g.nodes()
            .map(|(_, n)| GraphNodeEvidence {
                kind: n.arc_kind(),
                parent: n.parent(),
                site: n.site().clone(),
                stack: n.layer_stack_identifier(),
                depth: n.namespace_depth(),
                sibling: n.sibling_index(),
                implied: n.is_implied(),
            })
            .collect()
    })
}
