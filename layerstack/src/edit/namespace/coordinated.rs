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
    ArcKind, HashMap, HashSet, LayerStackIdentifier, NodeId, Relocate, prim_indexing::ChildIndex,
    stage::preparation::PreparationIndex,
};
use core::cell::RefCell;

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
    // With retained evidence, the slots that author paths in each layer of
    // a stage's layer stack, and the slots of each other layer whose arcs
    // the edit repairs.
    let mut links = HashMap::new();
    let mut repairable: HashMap<LayerId, Vec<PathId>> = HashMap::new();
    for id in &inspected {
        let layer = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        if retained.is_some() {
            // Only specs that author arcs can fail these checks.
            let arcs = retained::arcs_of(indexes, layer);
            if arcs
                .main_layers()
                .iter()
                .any(|id| store.layer(*id).is_none() && !primary.is_layer_muted(*id))
            {
                return Err(unresolved_arcs());
            }
            for spec in arcs
                .unresolved()
                .iter()
                .filter_map(|path| layer.prims.get(path))
            {
                validate_arcs(primary, store, spec)?;
            }
            if stages.iter().any(|stage| stage.layer_stack().contains(id)) {
                links.insert(*id, retained::links_of(indexes, layer));
            } else {
                // Outside every layer stack the edit repairs only the arcs
                // into the edited layers.
                let mut slots: Vec<PathId> =
                    arcs.naming(root_layers)
                        .chain(
                            arcs.unresolved().iter().copied().filter(|path| {
                                retained::arcs_into(store, layer, *path, root_layers)
                            }),
                        )
                        .collect();
                sort::sort_unstable(&mut slots);
                slots.dedup();
                repairable.insert(*id, slots);
            }
            continue;
        }
        for spec in layer.prims.values() {
            validate_arcs(primary, store, spec)?;
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
    let mut local_deletes: HashMap<LayerId, Vec<(TargetPath, Vec<VariantSelectionSite>)>> =
        HashMap::new();
    let mut stage_moves = Vec::new();
    let mut chain = Vec::new();
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
        let mut deletes = Vec::new();
        gather_dependent_moves(
            stage,
            indexes.get(i).copied(),
            store,
            root_layers,
            &source_move,
            &mut mappings,
            &mut variant_moves,
            &mut deletes,
            &mut chain,
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
            let gone = local_deletes.entry(*id).or_default();
            for entry in &deletes {
                if !gone.contains(entry) {
                    gone.push(entry.clone());
                }
            }
        }
        stage_moves.push(mappings);
    }
    // The specs of other layer stacks that move or go with nested arcs,
    // by layer. A layer of a stage's own stack edits its namespace through
    // that stage's moves alone.
    let mut remote_moves: HashMap<LayerId, Vec<VariantMove>> = HashMap::new();
    let mut remote_deletes: HashMap<LayerId, Vec<(TargetPath, Vec<VariantSelectionSite>)>> =
        HashMap::new();
    for edit in &chain {
        for id in &edit.layers {
            if stages.iter().any(|stage| stage.layer_stack().contains(id)) {
                return Err(NamespaceError::UnsupportedComposition(
                    "dependent namespace through an arc nested in a stage's layer stack requires an unambiguous mapping",
                ));
            }
            match edit.to {
                Some(to) => {
                    let moved = VariantMove {
                        mapping: PathMove::new(edit.from, to),
                        within: edit.within.clone(),
                    };
                    let moves = remote_moves.entry(*id).or_default();
                    if !moves.contains(&moved) {
                        moves.push(moved);
                    }
                }
                None => {
                    let gone = (edit.from, edit.within.clone());
                    let deletes = remote_deletes.entry(*id).or_default();
                    if !deletes.contains(&gone) {
                        deletes.push(gone);
                    }
                }
            }
        }
    }
    // With retained evidence, each layer is edited as a working copy of
    // the slots the edit can change: those at and beneath the paths it
    // moves, their ancestors, and those that author paths.
    let mut copied: HashMap<LayerId, Vec<PathId>> = HashMap::new();
    let mut edited = HashMap::new();
    let mut dormant: HashMap<LayerId, Dormant> = HashMap::new();
    for id in &inspected {
        let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        let local = stages.iter().any(|stage| stage.layer_stack().contains(id));
        let mut layer = match (indexes.first(), links.get(id), repairable.get(id)) {
            (Some(index), Some(slots), _) => {
                let gone = local_deletes.get(id).map_or(&[][..], Vec::as_slice);
                let slots = working_slots(store, index, original, slots, local_moves.get(id), gone);
                let copy = retained::working_copy(original, &slots);
                copied.insert(*id, slots);
                copy
            }
            (Some(index), None, Some(slots)) => {
                // With the slots the nested arcs' dependent edits change.
                let moves: Vec<PathMove> = remote_moves
                    .get(id)
                    .into_iter()
                    .flatten()
                    .map(|m| m.mapping.clone())
                    .collect();
                let gone = remote_deletes.get(id).map_or(&[][..], Vec::as_slice);
                let mut slots = slots.clone();
                if !moves.is_empty() || !gone.is_empty() {
                    let none = HashSet::new();
                    slots.extend(working_slots(
                        store,
                        index,
                        original,
                        &none,
                        Some(&moves),
                        gone,
                    ));
                    sort::sort_unstable(&mut slots);
                    slots.dedup();
                }
                // A layer outside the layer stacks authoring no arc the
                // edit repairs, and holding no spec it moves, is left as it
                // is, uncopied.
                if slots.is_empty() {
                    continue;
                }
                let copy = retained::working_copy(original, &slots);
                copied.insert(*id, slots);
                copy
            }
            _ => original.clone(),
        };
        let mut unused = dormant_variants(store, &stages, indexes, root_layers, &layer);
        if local {
            repair_path_defaults(
                store,
                &stages,
                &mut layer,
                source,
                Some(destination),
                &unused,
            );
        }
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
        for (gone, within) in local_deletes.get(id).into_iter().flatten() {
            delete::remove_specs(store, &mut layer, *gone, within);
        }
        // `UsdNamespaceEditor` moves each spec with `SdfLayer::Apply`,
        // creating the destination's parent for a reparent
        // (`_ApplyLayerSpecMove`, `pxr/usd/usd/namespaceEditor.cpp`), in
        // other layer stacks as in the stage's.
        for moved in remote_moves.get(id).into_iter().flatten() {
            if moved.within.is_empty() {
                move_local(store, &mut layer, &moved.mapping)?;
            } else {
                move_in_variant(store, &mut layer, moved)?;
            }
            unused = unused.after(store, &moved.mapping, &moved.within);
        }
        for (gone, within) in remote_deletes.get(id).into_iter().flatten() {
            delete::remove_specs(store, &mut layer, *gone, within);
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
        let Some(layer) = edited.get_mut(id) else {
            continue;
        };
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
    // The paths the repairs passed over, in the specs no prim index uses.
    let mut unrepaired_paths = Vec::new();
    for id in &inspected {
        let Some(layer) = edited.get(id) else {
            continue;
        };
        let by = unrepaired::Edited::Move {
            local: local_moves.get(id).map_or(&[][..], Vec::as_slice),
            source: &source_move,
            root_layers,
        };
        unrepaired::collect(&*store, layer, &dormant[id], &by, &mut unrepaired_paths);
    }
    unrepaired::sort(&*store, &mut unrepaired_paths);
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
        // Whether the edit moves namespace whole, deleting nothing and
        // moving nothing within variant branches.
        let whole_moves = local_deletes.values().all(Vec::is_empty)
            && local_variant_moves.values().all(Vec::is_empty)
            && chain.is_empty();
        validate_projection_retained(
            store,
            &edited,
            &edits,
            &stages,
            &stage_moves,
            indexes,
            current && whole_moves,
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
        transaction.expect_generation(id, original.generation());
        let Some(layer) = edited.get(&id) else {
            continue;
        };
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
    NamespaceEdit {
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
        unrepaired: unrepaired_paths,
    }
    .writable(store)
}

/// The variant specs of `layer`, before the edit, that no prim index of
/// `stages` uses ([`Dormant`]), and outside every stage's layer stack the
/// prim specs too.
///
/// In a layer of a stage's root layer stack, a variant spec is used where
/// the prim at its path has a node at its site in that stack: OpenUSD
/// fixes paths only in the specs of a prim's local and local variant
/// nodes (`_GatherPathBearingFieldEditsForStage`), and a variant spec
/// reached through another arc (`/World/Copy` referencing `/World/A` with
/// another selection) is that arc's to keep. In any other layer the edit
/// repairs only reference and payload targets, so only the specs
/// authoring those into the `edited` layers are looked up, among the
/// prims with a node at their path: a prim spec no arc reaches (an
/// asset's `/Unused` beside the `/Asset` a stage references) is as
/// dormant as an unselected variant, as `PcpGatherDependentNamespaceEdits`
/// finds only the sites a prim index depends on. AOUSD Core §10.4 (prim
/// index nodes), §10.5 (variants).
pub(super) fn dormant_variants(
    store: &dyn LayerStore,
    stages: &[&Stage],
    indexes: &[&PreparationIndex],
    edited: &[LayerId],
    layer: &Layer,
) -> Dormant {
    let local = stages.iter().any(|s| s.layer_stack().contains(&layer.id));
    let mut out = Dormant::default();
    // A working copy may hold a variant's child prim spec without the
    // prim hosting the variant set, so each spec's own branches count.
    if local
        && layer.variant_prims.is_empty()
        && layer
            .prims
            .values()
            .all(|p| p.variant_sets().is_empty() && p.outer_variant_sites().is_empty())
    {
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
    // Outside the root layer stack, only the arcs into the edited layers
    // are repaired ([`repair_prim`] with no local move).
    let repairable = |refs: &ListOp<Reference>, payloads: &ListOp<Reference>| {
        local
            || refs.items().chain(payloads.items()).any(|r| {
                edited.contains(&r.layer)
                    || r.stored_target_layer(store)
                        .is_some_and(|target| edited.contains(&target))
            })
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
        if (!sites.is_empty() || !local)
            && repairable(&spec.references, &spec.payloads)
            && !used(path, &sites)
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

/// The error for an arc preparation cannot follow ([`validate_arcs`]).
fn unresolved_arcs() -> NamespaceError {
    NamespaceError::UnsupportedComposition(
        "unloaded or expression-based assets require resolved dependency coordination",
    )
}

/// Rejects a spec whose references or payloads target an unloaded or
/// unresolved layer, or an asset expression. A spec inside a variant
/// passes: a selected variant's unresolved arc is a composition error the
/// snapshot check rejects, and one in a variant no prim selects composes
/// nothing for the edit to keep.
///
/// An arc to a layer `stage` mutes is resolved: it composes nothing, on
/// purpose, and the edit neither reads nor edits that layer
/// (`UsdNamespaceEditor` edits around a `PcpErrorMutedAssetPath`).
fn validate_arcs(
    stage: &Stage,
    store: &dyn LayerStore,
    spec: &PrimSpec,
) -> Result<(), NamespaceError> {
    if !spec.outer_variant_sites().is_empty() {
        return Ok(());
    }
    if spec
        .references
        .items()
        .chain(spec.payloads.items())
        .any(|r| {
            r.stored_target_layer(store)
                .is_none_or(|target| store.layer(target).is_none() && !stage.is_layer_muted(target))
                || r.is_expression()
        })
    {
        return Err(unresolved_arcs());
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
    deletes: &[(TargetPath, Vec<VariantSelectionSite>)],
) -> Vec<PathId> {
    let mut slots: HashSet<PathId> = links.clone();
    for (gone, _) in deletes {
        let prim = gone.prim_path();
        slots.extend(retained::slots_beneath(
            store,
            index.children(),
            layer,
            prim,
        ));
        slots.extend(retained::at_or_above(store, prim));
    }
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
        || stage
            .load_rules()
            .rules()
            .any(|(_, r)| r != crate::PayloadRule::All)
        || blocking_errors(stage)
    {
        return Err(NamespaceError::UnsupportedComposition(
            "masked, unloaded or unresolved composition requires explicit dependency coordination",
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
    deleted_out: &mut Vec<(TargetPath, Vec<VariantSelectionSite>)>,
    chain_out: &mut Vec<ChainEdit>,
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
    // Whether each layer stack the nodes read holds a source layer: the
    // prims beneath a moved group share a few layer stacks among many
    // nodes.
    let mut holds_source: HashMap<LayerStackIdentifier, bool> = HashMap::new();
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
            let holds = *holds_source
                .entry(node.layer_stack_identifier())
                .or_insert_with(|| {
                    LayerStack::gather_identifier(store, node.layer_stack_identifier())
                        .layers
                        .iter()
                        .any(|id| source_layers.contains(id))
                });
            if !holds {
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
            // beneath their nodes rather than the root, with a copy implied
            // to the root.
            if implied_from_local_variant(graph, node) {
                continue;
            }
            if node.site().has_variant_selections() || node.is_implied() {
                return Err(NamespaceError::UnsupportedComposition(
                    "dependent namespace through nested or variant arcs requires an unambiguous mapping",
                ));
            }
            // The levels of the prim beneath the site the arc was
            // introduced at, counted at its parent node, whose site
            // descends with the prim ([`PrimNode::namespace_depth`]).
            let below = graph
                .node(node.parent().unwrap_or(NodeId::ROOT))
                .map(|parent| store.paths().resolve(parent.site().prim_path()).depth())
                .and_then(|depth| depth.checked_sub(usize::from(node.namespace_depth())))
                .ok_or(NamespaceError::Unmappable)?;
            if below > composed.depth() || below > site.depth() {
                return Err(NamespaceError::Unmappable);
            }
            let raw_mount = Path::root().join(&site.segments()[..site.depth() - below]);
            // Moving the referenced root changes its target, not its
            // mount: the arc is repaired where it is authored, in this
            // layer stack or, for an arc nested in another's target, in
            // that one (`_ProcessDependentNodePathAtIntroductionChange`,
            // `pxr/usd/pcp/dependentNamespaceEditUtils.cpp`).
            if source.from.property_path().is_none() && raw_from.is_prefix_of(&raw_mount) {
                continue;
            }
            if suffix.len() > composed.depth() {
                return Err(NamespaceError::Unmappable);
            }
            let from = store
                .paths()
                .intern(Path::root().join(&composed.segments()[..composed.depth() - suffix.len()]));
            // An arc authored in another layer stack beneath the root's
            // (an asset's arc back into the stage) composes over the moved
            // namespace through the specs of every node above it, which
            // move with it.
            let chain = if authored_in_local_variants(graph, node) {
                Vec::new()
            } else {
                let dest = raw_to.strip_prefix(&raw_mount);
                chain_edits(
                    store,
                    graph,
                    node,
                    source_layers,
                    suffix.len(),
                    below,
                    dest,
                    source,
                )?
            };
            // A destination outside the arc's source domain no longer
            // composes here, which deletes the local specs over it
            // (`_MapNewPathToIntroducingNode`,
            // `pxr/usd/pcp/dependentNamespaceEditUtils.cpp`).
            let Some(dest_suffix) = raw_to.strip_prefix(&raw_mount) else {
                push_unique(chain_out, chain);
                let gone = match source.from.property_path() {
                    Some(p) => TargetPath::Property(PropertyPath::new(from, p.property())),
                    None => TargetPath::Prim(from),
                };
                let mut branches = branches_above(store, graph, node, from);
                branches.insert(0, Vec::new());
                for within in branches {
                    let entry = (gone, within);
                    if !deleted_out.contains(&entry) {
                        deleted_out.push(entry);
                    }
                }
                continue;
            };
            let to = store.paths().intern(
                Path::root()
                    .join(&composed.segments()[..composed.depth() - below])
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
            if mapping.from == mapping.to
                || unedited_property_opinion(stage, store, prim, source_layers, source, &chain)
            {
                continue;
            }
            push_unique(chain_out, chain);
            // The local specs to move are at the sites of the nodes above
            // the arc's, each in its own branches: OpenUSD moves the specs
            // of every node on the way up to the root
            // (`_PrimIndexDependentNodeEditProcessor::_AddSpecMoveEdits`).
            for within in branches_above(store, graph, node, from) {
                let moved = VariantMove {
                    mapping: mapping.clone(),
                    within,
                };
                if !variant_out.contains(&moved) {
                    variant_out.push(moved);
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

/// Whether the dependent `prim` composes the moved property from a spec
/// the edit leaves: neither its own local specs nor the moved one
/// (`/World/Copy.a` from `/World/A{look=blue}.a`, where `/World/Copy`
/// references `/World/A` selecting a variant `/World/A` does not).
/// OpenUSD then leaves the prim's specs where they are, as moving them
/// would split its opinions, and warns
/// (`_PrimIndexDependentNodeEditProcessor::_HasUneditedUpstreamSpecConflicts`);
/// the prim keeps composing the property at the old path.
fn unedited_property_opinion(
    stage: &Stage,
    store: &dyn LayerStore,
    prim: PathId,
    source_layers: &[LayerId],
    source: &PathMove,
    chain: &[ChainEdit],
) -> bool {
    let Some(property) = source.from.property_path() else {
        return false;
    };
    let moved = SpecPath::from_prim_path(property.prim_path());
    stage
        .prim_stack(prim)
        .unwrap_or_default()
        .iter()
        .any(|(layer, site)| {
            let own = site.prim_path() == prim && stage.layer_stack().contains(layer);
            let edited = source_layers.contains(layer) && *site == moved;
            // The specs of the nodes between a nested arc and the root,
            // which move with it.
            let carried = chain.iter().any(|edit| {
                edit.layers.contains(layer)
                    && edit.from.prim_path() == site.prim_path()
                    && site
                        .variant_selection_sites()
                        .eq(edit.within.iter().copied())
            });
            !own && !edited
                && !carried
                && store
                    .layer(*layer)
                    .and_then(|l| spec_at(l, &Loc::of(site)))
                    .is_some_and(|s| s.properties().iter().any(|p| p.name == property.property()))
        })
}

/// The branches holding the local specs of the prim at `path` (or an
/// ancestor of it the edit maps) that `node`'s arc composes over, inside
/// the variants above it: for each variant node between `node` and the
/// root, the selections of its site hosted above `path`. Empty for an arc
/// authored outside every variant.
pub(super) fn branches_above(
    store: &dyn LayerStore,
    graph: &crate::PrimIndexGraph,
    node: crate::prim_index_graph::PrimNode<'_>,
    path: PathId,
) -> Vec<Vec<VariantSelectionSite>> {
    let path = store.paths().resolve(path);
    let stack = graph
        .node(NodeId::ROOT)
        .map(|root| root.layer_stack_identifier());
    let mut out = Vec::new();
    let mut parent = node.parent();
    while let Some(above) = parent
        .filter(|id| *id != NodeId::ROOT)
        .and_then(|id| graph.node(id))
    {
        // The nodes of other layer stacks hold no local specs
        // ([`chain_edits`] edits theirs).
        if Some(above.layer_stack_identifier()) != stack {
            parent = above.parent();
            continue;
        }
        let within: Vec<VariantSelectionSite> = above
            .site()
            .variant_selection_sites()
            .filter(|site| {
                let host = store.paths().resolve(site.host_path);
                host.is_prefix_of(path) && host.depth() < path.depth()
            })
            .collect();
        if !within.is_empty() && !out.contains(&within) {
            out.push(within);
        }
        parent = above.parent();
    }
    out
}

/// A dependent edit of the specs a layer stack other than a stage's holds
/// over the edited namespace: those at the site of a node between an arc
/// nested in another arc's target (an asset's arc back into the stage)
/// and the root of a composed prim's graph.
#[derive(Clone, PartialEq)]
pub(super) struct ChainEdit {
    /// The layers of the node's layer stack, each of which edits its specs.
    pub(super) layers: Vec<LayerId>,
    /// The branches the specs are authored in, all hosted above `from`.
    pub(super) within: Vec<VariantSelectionSite>,
    /// The specs' prim, or property, inside `within`.
    pub(super) from: TargetPath,
    /// Where they move, inside the same branches; `None` removes them.
    pub(super) to: Option<TargetPath>,
}

/// Appends each of `edits` that `out` does not already hold.
pub(super) fn push_unique(out: &mut Vec<ChainEdit>, edits: Vec<ChainEdit>) {
    for edit in edits {
        if !out.contains(&edit) {
            out.push(edit);
        }
    }
}

/// The [`ChainEdit`]s of the nodes above `node` outside the root's layer
/// stack, for an edit of the namespace `suffix` levels above `node`'s site
/// in one of `source_layers` (`source`'s property, when it moves one).
/// `node`'s arc was introduced `below` levels above its site, and `dest`
/// is the destination beneath that introduction, `None` where it leaves
/// it.
///
/// Each node's site is the composed prim's in its layer stack, so the
/// namespace edited at `node` is its site `suffix` levels up, and the
/// destination lies beneath its site `below` levels up. A destination
/// outside the arc's domain removes the specs instead. OpenUSD visits the
/// same nodes from the dependent one up to the root, moving or deleting
/// the specs of each (`_PrimIndexDependentNodeEditProcessor::
/// _ProcessNextNodeTask` and `_AddSpecMoveEdits`,
/// `pxr/usd/pcp/dependentNamespaceEditUtils.cpp`), its spec path mapped
/// through each introduction (`_MapNewPathToIntroducingNode`). The root's
/// layer stack has its own moves ([`gather_dependent_moves`]). A node above
/// in a layer stack holding an edited layer would edit that namespace
/// twice, which is refused. AOUSD Core §10 (composition arcs).
#[allow(clippy::too_many_arguments, reason = "the walk's whole context")]
pub(super) fn chain_edits(
    store: &mut dyn LayerStore,
    graph: &crate::PrimIndexGraph,
    node: crate::prim_index_graph::PrimNode<'_>,
    source_layers: &[LayerId],
    suffix: usize,
    below: usize,
    dest: Option<&[TokenId]>,
    source: &PathMove,
) -> Result<Vec<ChainEdit>, NamespaceError> {
    let Some(root) = graph.node(NodeId::ROOT) else {
        return Ok(Vec::new());
    };
    let dest = dest.map(<[TokenId]>::to_vec);
    let mut out = Vec::new();
    let mut parent = node.parent();
    while let Some(above) = parent
        .filter(|id| *id != NodeId::ROOT)
        .and_then(|id| graph.node(id))
    {
        parent = above.parent();
        if above.layer_stack_identifier() == root.layer_stack_identifier() {
            continue;
        }
        let layers = LayerStack::gather_identifier(store, above.layer_stack_identifier()).layers;
        if layers.iter().any(|id| source_layers.contains(id)) {
            return Err(NamespaceError::UnsupportedComposition(
                "dependent namespace through an arc nested in the edited layer stack requires an unambiguous mapping",
            ));
        }
        let site = store.paths().resolve(above.site().prim_path()).clone();
        if suffix > site.depth() || below > site.depth() {
            return Err(NamespaceError::Unmappable);
        }
        let mount = site.depth() - below;
        let within: Vec<VariantSelectionSite> = above.site().variant_selection_sites().collect();
        if within
            .iter()
            .any(|at| store.paths().resolve(at.host_path).depth() > mount)
        {
            return Err(NamespaceError::Unmappable);
        }
        let from = store
            .paths()
            .intern(Path::root().join(&site.segments()[..site.depth() - suffix]));
        let to = dest.as_deref().map(|dest| {
            store
                .paths()
                .intern(Path::root().join(&site.segments()[..mount]).join(dest))
        });
        let (from, to) = match (source.from.property_path(), source.to.property_path()) {
            (Some(a), b) => (
                TargetPath::Property(PropertyPath::new(from, a.property())),
                to.zip(b)
                    .map(|(to, b)| TargetPath::Property(PropertyPath::new(to, b.property()))),
            ),
            (None, _) => (TargetPath::Prim(from), to.map(TargetPath::Prim)),
        };
        if to == Some(from) {
            continue;
        }
        out.push(ChainEdit {
            layers,
            within,
            from,
            to,
        });
    }
    Ok(out)
}

/// Whether `node` is the copy of an arc authored in the local variants its
/// prim selects that Pcp implies to the root at the same site, through
/// those variants' identity map (`_EvalImpliedClasses`,
/// `pxr/usd/pcp/primIndex.cpp`): the arc it is implied from is the one to
/// follow.
pub(super) fn implied_from_local_variant(
    graph: &crate::PrimIndexGraph,
    node: crate::prim_index_graph::PrimNode<'_>,
) -> bool {
    node.is_implied()
        && node
            .origin()
            .and_then(|id| graph.node(id))
            .is_some_and(|origin| {
                origin.site() == node.site()
                    && origin.layer_stack_identifier() == node.layer_stack_identifier()
                    && origin.parent() != Some(NodeId::ROOT)
                    && authored_in_local_variants(graph, origin)
            })
}

/// Whether `node`'s arc is authored at the root's site or inside the local
/// variants it selects: every node above it is a variant node of the root
/// layer stack.
pub(super) fn authored_in_local_variants(
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
    fn layer_read_only(&self, id: LayerId) -> Option<ReadOnlyReason> {
        self.store.layer_read_only(id)
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
    /// Whether a rename may be proven to translate its subtree
    /// ([`Self::translated`]): the edit deletes nothing and moves nothing
    /// within variant branches.
    translatable: bool,
}

/// A prim with a node other than its local one at a site, and the edited
/// layers that node's layer stack holds ([`ProjectionScope::roots`]).
struct SiteReader {
    prim: PathId,
    layers: Vec<LayerId>,
}

/// A stage composed over a projected store, only in part where a
/// [`ProjectionScope`] allows.
struct Projection {
    stage: Stage,
    /// The destination of a rename proven to compose its source's subtree
    /// translated ([`ProjectionScope::translated`]), which the projection
    /// leaves uncomposed.
    translated: Option<PathId>,
}

impl Projection {
    /// Whether `path` exists in the projection, composed or proven.
    fn exists(&self, path: TargetPath) -> bool {
        match path {
            TargetPath::Prim(p) => self.stage.has_prim(p) || self.translated == Some(p),
            TargetPath::Property(p) => self.stage.has_property_path(p),
        }
    }
}

impl ProjectionScope<'_> {
    /// The rename among `moves`, the moves of `stage`, whose destination
    /// subtree composes exactly its source's, translated, when the
    /// composition of no prim beneath the source reads a slot the edit
    /// changes other than those it moves ([`Self::roots`] checks that):
    /// the stage's only move, of a prim to a new name beneath the same
    /// parent, whose composition has no node but its root, in a stage
    /// whose layer stack authors no relocates.
    ///
    /// A prim index reads the specs at its nodes' sites and the arcs they
    /// author. The prims beneath the source read the specs it moves, which
    /// the edit moves in every layer of the stack and repairs as it
    /// translates them, and through their arcs sites in other layer
    /// stacks, or in this one outside the source, which the edit leaves
    /// as they are. With no node above the parent's root, no ancestral
    /// arc maps the prims' names into another namespace, and without
    /// relocates no path of this stack names another, so each prim
    /// beneath the destination composes the graph of its source prim,
    /// translated: the same nodes, arcs and errors, none in a current
    /// snapshot, and the same instancing keys. The same parent keeps every
    /// path outside the subtree an ancestor of one of its prims exactly
    /// when it was before. OpenUSD likewise validates a namespace edit
    /// without composing the moved namespace again
    /// (`UsdNamespaceEditor::CanApplyEdits`).
    ///
    /// Spec: AOUSD Core §8 (namespace), §10 (composition arcs and their
    /// sites), §10.3.2.6 (relocates), §11.3.3 (scene graph instancing).
    fn translated(
        &self,
        store: &dyn LayerStore,
        stage: &Stage,
        moves: &[PathMove],
    ) -> Option<PathMove> {
        let [mapping] = moves else {
            return None;
        };
        let (TargetPath::Prim(from), TargetPath::Prim(to)) = (mapping.from, mapping.to) else {
            return None;
        };
        let parent = store.paths().parent(from)?;
        if !self.translatable
            || store.paths().parent(to) != Some(parent)
            || self.edits.values().any(|edit| !edit.relocated.is_empty())
            || stage
                .layer_stack()
                .iter()
                .any(|id| store.layer(*id).is_none_or(|l| !l.relocates().is_empty()))
            || stage
                .explain_prim_graph(parent)
                .is_some_and(|graph| graph.nodes().any(|(id, _)| id != NodeId::ROOT))
        {
            return None;
        }
        Some(mapping.clone())
    }

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
    /// whose slot changes with it. A spec appearing or disappearing at a
    /// relocation source, even a bare `over`, changes the prims reading
    /// that source as a changed slot there would: the relocated prim
    /// reports any spec there as an opinion it ignores. Layer metadata
    /// reaches beyond the slots that author it, and dynamic payloads and
    /// value clips read composition elsewhere; for those the whole stage
    /// is composed.
    ///
    /// A changed relocate changes the prims depending on the paths it
    /// relocates as a changed slot there would, and the prims with a node
    /// beneath those paths, which name the namespace it moves. Relocates
    /// the edit leaves alone change nothing more: a prim reads them
    /// through the nodes they add to its graph, at relocation sources
    /// the evidence lists like any other site.
    ///
    /// Instances need no more. The instances of one key compose the same
    /// prototype, read through the same sites, so a prototype the edit
    /// changes has a changed site in every instance's graphs, which puts
    /// the changed prims beneath each instance among the roots. An
    /// instance that copies a representative composed only in part, as
    /// the ancestor of a root, misses only prims whose composition the
    /// edit leaves alone: those a current snapshot composes without
    /// errors, and never the moved objects, which are not beneath an
    /// instance.
    ///
    /// Spec: AOUSD Core §10 (composition arcs and their sites),
    /// §10.3.2.6 (relocates), §11 (population), §11.3.3 (scene graph
    /// instancing). OpenUSD: `PcpChanges::DidChange` maps the same
    /// significant changes to the prim indexes depending on their sites,
    /// instance proxies included, and `_DidChangeLayerStackRelocations`
    /// resyncs the prims `FindSiteDependencies` finds at and beneath the
    /// relocated paths.
    ///
    /// A rename proven to translate its subtree ([`Self::translated`])
    /// leaves the destination subtree out of the roots, and is returned.
    fn roots(
        &self,
        store: &dyn LayerStore,
        stage: &Stage,
        i: usize,
        moves: &[PathMove],
    ) -> Option<(Vec<PathId>, Option<PathId>)> {
        let index = self.indexes.get(i)?;
        if self.edits.values().any(|edit| edit.metadata)
            || !stage.dynamic_layers().is_empty()
            || !stage.clip_layers().is_empty()
        {
            return None;
        }
        let used = stage.used_layers(false);
        // The relocation sources as the edit leaves them: a relocated prim
        // reads the specs at its source to report them
        // (`CompositionError::OpinionAtRelocationSource`), so a bare
        // `over` appearing there changes it
        // (`retained::LayerEdit::changed_slots`).
        let mut sources = HashSet::new();
        if self.edits.values().any(|edit| !edit.appeared.is_empty()) {
            for layer in used.iter().filter_map(|id| store.layer(*id)) {
                sources.extend(layer.relocates().iter().map(|r| r.source));
            }
        }
        let mut roots = Vec::new();
        // Whether `prim` has a node whose site `at` admits, in a layer
        // stack that holds `layer`: the evidence lists the prims with a
        // node at each site path, in any layer stack.
        let reads = |prim: PathId, layer: LayerId, at: &dyn Fn(PathId) -> bool| {
            stage.explain_prim_graph(prim).is_none_or(|graph| {
                graph.nodes().any(|(_, node)| {
                    node.arc_kind() != ArcKind::Local
                        && at(node.site().prim_path())
                        && node.layer_strength(layer).is_some()
                })
            })
        };
        // For each site, the prims with a non-local node there in a layer
        // stack holding an edited layer, with those layers: found once per
        // site, as the slots of one edit share ancestors, and the root
        // prims of many assets share a site path.
        let edited: Vec<LayerId> = self.edits.keys().copied().collect();
        let readers: RefCell<HashMap<PathId, Vec<SiteReader>>> = RefCell::default();
        let readers_at = |site: PathId| -> Vec<SiteReader> {
            let mut found = Vec::new();
            for prim in index.prims_with_nodes_at(site) {
                let Some(graph) = stage.explain_prim_graph(prim) else {
                    found.push(SiteReader {
                        prim,
                        layers: edited.clone(),
                    });
                    continue;
                };
                let mut layers = Vec::new();
                for (_, node) in graph.nodes() {
                    if node.arc_kind() == ArcKind::Local || node.site().prim_path() != site {
                        continue;
                    }
                    for &layer in &edited {
                        if node.layer_strength(layer).is_some() && !layers.contains(&layer) {
                            layers.push(layer);
                        }
                    }
                    if layers.len() == edited.len() {
                        break;
                    }
                }
                if !layers.is_empty() {
                    found.push(SiteReader { prim, layers });
                }
            }
            found
        };
        // The prim at `site` in the stage's layer stack when `local`, and
        // the namespace at `site` beneath the prims with a node at or
        // above it in the layer stack of `layer`.
        let depending = |roots: &mut Vec<PathId>, site: PathId, layer: LayerId, local: bool| {
            debug_assert!(edited.contains(&layer), "the readers are of edited layers");
            if local {
                roots.push(site);
            }
            let path = store.paths().resolve(site).clone();
            let mut readers = readers.borrow_mut();
            for above in retained::at_or_above(store, site) {
                let suffix = path
                    .strip_prefix(store.paths().resolve(above))
                    .expect("ancestor");
                let prims = readers.entry(above).or_insert_with(|| readers_at(above));
                for reader in prims.iter().filter(|r| r.layers.contains(&layer)) {
                    roots.push(store.paths().intern_joined(reader.prim, suffix));
                }
            }
        };
        // A rename translating its subtree, whose prims read no slot the
        // edit changes in place: the moved specs alone.
        let mut translated = self.translated(store, stage, moves);
        let moved_roots: HashSet<PathId> = translated
            .iter()
            .flat_map(|m| [m.from.prim_path(), m.to.prim_path()])
            .collect();
        let moved = |path: PathId| retained::is_beneath(store, path, &moved_roots);
        let mut found = Vec::new();
        if translated.is_some() {
            let reads_changed = self.edits.iter().any(|(&id, edit)| {
                let local = stage.layer_stack().contains(&id);
                edit.changed_slots(&sources).any(|slot| {
                    if local && moved(slot) {
                        return false;
                    }
                    found.clear();
                    depending(&mut found, slot, id, local);
                    found.iter().any(|prim| moved(*prim))
                })
            });
            if reads_changed {
                translated = None;
            }
        }
        for (&id, edit) in self.edits {
            let local = stage.layer_stack().contains(&id);
            for slot in edit.changed_slots(&sources) {
                if translated.is_some() && local && moved(slot) {
                    // The moved specs: the prims beneath the source read
                    // them translated; the prims elsewhere with arcs to
                    // them stay roots.
                    found.clear();
                    depending(&mut found, slot, id, local);
                    roots.extend(found.iter().copied().filter(|prim| !moved(*prim)));
                } else {
                    depending(&mut roots, slot, id, local);
                }
            }
            // A source another layer relocates too may move to that
            // layer's target once this one's changes.
            let mut relocated = edit.relocated.clone();
            for layer in used.iter().filter_map(|id| store.layer(*id)) {
                for relocate in layer.relocates() {
                    if edit.relocated.contains(&relocate.source) {
                        relocated.extend(relocate.target);
                    }
                }
            }
            for &path in &relocated {
                depending(&mut roots, path, id, local);
                let beneath = |at| retained::is_beneath(store, at, &HashSet::from_iter([path]));
                roots.extend(
                    index
                        .prims_with_nodes_beneath(path)
                        .filter(|prim| reads(*prim, id, &beneath)),
                );
            }
        }
        for mapping in moves {
            roots.push(mapping.from.prim_path());
            if translated.is_none() {
                roots.push(mapping.to.prim_path());
            }
        }
        let translated = translated.map(|mapping| mapping.to.prim_path());
        Some((retained::minimal_roots(store, &roots), translated))
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
    ) -> Option<Projection> {
        let index = self.indexes.get(i)?;
        let (mut roots, translated) = self.roots(store, stage, i, moves)?;
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
                return Some(Projection {
                    stage: partial,
                    translated,
                });
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
/// composes only those, leaving out a renamed subtree when `translatable`
/// ([`ProjectionScope::translated`]).
#[allow(clippy::too_many_arguments, reason = "the projection's whole context")]
fn validate_projection_retained(
    store: &mut dyn LayerStore,
    edited: &HashMap<LayerId, Layer>,
    edits: &HashMap<LayerId, retained::LayerEdit>,
    stages: &[&Stage],
    mappings: &[Vec<PathMove>],
    indexes: &[&PreparationIndex],
    translatable: bool,
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
            translatable,
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
        let whole = |store: &mut dyn LayerStore| Projection {
            stage: Stage::compose(store, stage.root_layer().unwrap(), stage.options().clone()),
            translated: None,
        };
        let projected = match scope.and_then(|scope| scope.compose(store, stage, i, moves)) {
            Some(projected) => {
                // Every test that prepares an edit checks the part against
                // the whole.
                #[cfg(test)]
                {
                    assert_eq!(
                        projection_findings(&projected, moves),
                        projection_findings(&whole(store), moves),
                        "a partial projection finds what the whole projection finds"
                    );
                }
                projected
            }
            None => whole(store),
        };
        if blocking_errors(&projected.stage) {
            return Err(NamespaceError::UnsupportedComposition(
                "prepared namespace edit would create invalid composition",
            ));
        }
        for mapping in moves {
            if projected.exists(mapping.from) || !projected.exists(mapping.to) {
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
fn projection_findings(projected: &Projection, moves: &[PathMove]) -> (bool, Vec<(bool, bool)>) {
    (
        !blocking_errors(&projected.stage),
        moves
            .iter()
            .map(|m| (projected.exists(m.from), projected.exists(m.to)))
            .collect(),
    )
}

#[derive(PartialEq)]
struct GraphNodeEvidence {
    kind: ArcKind,
    parent: Option<NodeId>,
    site: SpecPath,
    stack: LayerStackIdentifier,
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
