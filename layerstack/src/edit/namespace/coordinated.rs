// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Coordinated root-stack edits. Composition supplies namespace mappings; the
//! caller supplies the set of stage snapshots. No stage or asset is discovered
//! through hidden global state. AOUSD Core §8 and §10.3.2.6; OpenUSD
//! `UsdNamespaceEditor::_ProcessPrimEdit` / `PcpGatherDependentNamespaceEdits`.

use super::*;
use crate::{ArcKind, HashMap, HashSet, NodeId, Relocate, stage::preparation::PreparationIndex};

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
    if !primary.layer_stack().contains(&target.layer())
        || spec_target(&map(target, store, source)?) != source
        || spec_target(&map(target, store, destination)?) != destination
    {
        return Err(NamespaceError::Unmappable);
    }
    let stages: Vec<_> = core::iter::once(primary)
        .chain(dependents.iter().copied())
        .collect();
    // The evidence of every stage, or of none.
    let retained: Option<Vec<&PreparationIndex>> = if use_retained {
        stages.iter().map(|stage| stage.preparation()).collect()
    } else {
        None
    };
    let indexes = retained.as_deref().unwrap_or_default();
    let root_layers = primary.layer_stack();
    let mut inspected = Vec::new();
    for (i, stage) in stages.iter().enumerate() {
        validate_snapshot(stage, store, indexes.get(i).copied())?;
        let root = stage.root_layer().ok_or(NamespaceError::NoSuchObject)?;
        inspected.extend(match indexes.get(i) {
            Some(index) => retained::reachable_layers(store, indexes, index, root),
            None => reachable_layers(stage, store, root),
        });
        inspected.extend(stage.layer_stack());
    }
    inspected.sort_unstable();
    inspected.dedup();
    let mut links = HashMap::new();
    for id in &inspected {
        let layer = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        if retained.is_some() {
            // Only specs that author arcs or variant sets can fail these
            // checks.
            let slots = retained::links_of(indexes, layer);
            let branched = !layer.variant_prims.is_empty()
                || slots.iter().any(|path| {
                    layer
                        .prims
                        .get(path)
                        .is_some_and(|p| !p.variant_sets.is_empty())
                });
            if branched {
                return Err(NamespaceError::UnsupportedComposition(
                    "dormant variant composition requires explicit dependency coordination",
                ));
            }
            for spec in slots.iter().filter_map(|path| layer.prims.get(path)) {
                validate_arcs(store, spec)?;
            }
            links.insert(*id, slots);
            continue;
        }
        if !layer.variant_prims.is_empty()
            || layer.prims.values().any(|p| !p.variant_sets.is_empty())
        {
            return Err(NamespaceError::UnsupportedComposition(
                "dormant variant composition requires explicit dependency coordination",
            ));
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
        gather_dependent_moves(
            stage,
            indexes.get(i).copied(),
            store,
            root_layers,
            &source_move,
            &mut mappings,
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
        }
        stage_moves.push(mappings);
    }
    // With retained evidence, each layer is edited as a working copy of
    // the slots the edit can change: those at and beneath the paths it
    // moves, their ancestors, and those that author paths.
    let mut copied: HashMap<LayerId, Vec<PathId>> = HashMap::new();
    let mut edited = HashMap::new();
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
        if let Some(moves) = local_moves.get(id) {
            for mapping in moves {
                validate_authored(&layer, store, mapping)?;
                move_local(store, &mut layer, mapping)?;
            }
        }
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
                repair_prim(store, spec, *path, *id, *edited_id, None, &source_move)?;
            }
            for (path, branches) in &mut layer.variant_prims {
                for spec in branches {
                    repair_prim(store, spec, *path, *id, *edited_id, None, &source_move)?;
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
        let mut projected = HashMap::new();
        for (id, paths) in &delta {
            let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
            if let Some(layer) = retained::project(original, &edited[id], paths) {
                projected.insert(*id, layer);
            }
        }
        validate_projection(store, &projected, &stages, &stage_moves)?;
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
            relocates.push((id, layer.relocates.clone()));
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
    specs.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.prim_path().cmp(&b.1.prim_path())));
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

/// Rejects a spec whose references or payloads target an unloaded or
/// unresolved layer, or an asset expression.
fn validate_arcs(store: &dyn LayerStore, spec: &PrimSpec) -> Result<(), NamespaceError> {
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
    slots.sort_unstable();
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
) -> Result<(), NamespaceError> {
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
        return Ok(());
    }
    let fresh = Stage::compose(
        store,
        stage.root_layer().ok_or(NamespaceError::NoSuchObject)?,
        stage.options().clone(),
    );
    let mut old: Vec<_> = stage.prim_paths().collect();
    let mut new: Vec<_> = fresh.prim_paths().collect();
    old.sort_unstable();
    new.sort_unstable();
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
    Ok(())
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
) -> Result<(), NamespaceError> {
    let raw_from = store.paths().resolve(source.from.prim_path()).clone();
    let raw_to = store.paths().resolve(source.to.prim_path()).clone();
    let prims: Vec<PathId> = match index {
        Some(index) => {
            let mut prims: Vec<PathId> = index
                .prims_with_nodes_beneath(source.from.prim_path())
                .collect();
            prims.sort_unstable();
            prims
        }
        None => {
            let mut prims: Vec<PathId> = stage.prim_paths().collect();
            prims.sort_unstable();
            prims
        }
    };
    for prim in prims {
        let Some(graph) = stage.explain_prim_graph(prim) else {
            continue;
        };
        let composed = store.paths().resolve(prim).clone();
        for (_, node) in graph.nodes() {
            if node.arc_kind() == ArcKind::Local {
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
            if node.site().has_variant_selections()
                || node.parent() != Some(NodeId::ROOT)
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
                .paths_mut()
                .intern(Path::root().join(&composed.segments()[..composed.depth() - suffix.len()]));
            let to = store.paths_mut().intern(
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
            if mapping.from != mapping.to
                && !out
                    .iter()
                    .any(|m| m.from == mapping.from && m.to == mapping.to)
            {
                out.push(mapping);
            }
        }
    }
    Ok(())
}

fn normalize_moves(
    store: &mut dyn LayerStore,
    moves: &mut Vec<PathMove>,
) -> Result<(), NamespaceError> {
    moves.sort_by_key(|m| (store.paths().resolve(m.from.prim_path()).depth(), m.from));
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
                l.relocates
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

fn validate_authored(
    layer: &Layer,
    store: &dyn LayerStore,
    mapping: &PathMove,
) -> Result<(), NamespaceError> {
    for (path, spec) in layer.prims.iter().chain(
        layer
            .variant_prims
            .iter()
            .flat_map(|(p, ss)| ss.iter().map(move |s| (p, s))),
    ) {
        let relevant = store
            .paths()
            .resolve(mapping.from.prim_path())
            .is_prefix_of(store.paths().resolve(*path));
        if !relevant {
            continue;
        }
        if !spec.outer_variant_sites.is_empty() || !spec.variant_sets.is_empty() {
            return Err(NamespaceError::UnsupportedComposition(
                "namespace moves containing dormant variant branches are not supported",
            ));
        }
        if spec.active == Some(false) {
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
        layer
            .prims
            .keys()
            .chain(layer.variant_prims.keys())
            .any(|p| {
                store
                    .paths()
                    .resolve(mapping.from.prim_path())
                    .is_prefix_of(store.paths().resolve(*p))
            })
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
    if !layer.prims.contains_key(&mapping.from.prim_path()) {
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
fn ensure_ancestors(store: &mut dyn LayerStore, layer: &mut Layer, path: Path) {
    if path.segments().is_empty() {
        return;
    }
    ensure_ancestors(store, layer, path.parent().unwrap());
    let id = store.paths_mut().intern(path);
    if !layer.prims.contains_key(&id) {
        layer.insert_prim(id, PrimSpec::over());
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
        for r in &layers[id].relocates {
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
            layers
                .get_mut(id)
                .unwrap()
                .relocates
                .retain(|r| r.source != source);
        }
        if source != mapping.to.prim_path() {
            layers.get_mut(&target).unwrap().relocates.push(Relocate {
                source,
                target: Some(mapping.to.prim_path()),
            });
        }
    } else {
        layers.get_mut(&target).unwrap().relocates.push(Relocate {
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
            for r in &mut layers.get_mut(id).unwrap().relocates {
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
    let Some(path) = layer.default_prim_path(store.tokens_mut()) else {
        return;
    };
    let old = store.paths_mut().intern(path);
    let new = mapping.prim(store, old);
    if old != new {
        let path = store.paths().resolve(new);
        let text = if path.depth() == 1 {
            alloc::string::String::from(store.tokens().resolve(path.leaf().unwrap()))
        } else {
            path.display(store.tokens())
        };
        layer.default_prim = Some(store.tokens_mut().intern(&text));
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
    fn layer_mut(&mut self, _: LayerId) -> Option<&mut Layer> {
        None
    }
    fn tokens(&self) -> &crate::TokenInterner {
        self.store.tokens()
    }
    fn tokens_mut(&mut self) -> &mut crate::TokenInterner {
        self.store.tokens_mut()
    }
    fn paths(&self) -> &crate::PathInterner {
        self.store.paths()
    }
    fn paths_mut(&mut self) -> &mut crate::PathInterner {
        self.store.paths_mut()
    }
    fn asset_layer(&self, anchor: LayerId, path: &str) -> Option<LayerId> {
        self.store.asset_layer(anchor, path)
    }
    fn asset_availability(&self, anchor: LayerId, path: &str) -> crate::AssetAvailability {
        self.store.asset_availability(anchor, path)
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
    for (stage, moves) in stages.iter().zip(mappings) {
        let projected = Stage::compose(
            &mut overlay,
            stage.root_layer().unwrap(),
            stage.options().clone(),
        );
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
