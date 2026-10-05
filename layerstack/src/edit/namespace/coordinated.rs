// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Coordinated root-stack edits. Composition supplies namespace mappings; the
//! caller supplies the set of stage snapshots. No stage or asset is discovered
//! through hidden global state. AOUSD Core §8 and §10.3.2.6; OpenUSD
//! `UsdNamespaceEditor::_ProcessPrimEdit` / `PcpGatherDependentNamespaceEdits`.

use super::*;
use crate::{ArcKind, HashMap, NodeId, Relocate};

pub(super) fn prepare(
    primary: &Stage,
    dependents: &[&Stage],
    store: &mut dyn LayerStore,
    target: &EditTarget,
    source: TargetPath,
    destination: TargetPath,
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
    let root_layers = primary.layer_stack();
    let mut inspected = Vec::new();
    for stage in &stages {
        validate_snapshot(stage, store)?;
        let root = stage.root_layer().ok_or(NamespaceError::NoSuchObject)?;
        inspected.extend(reachable_layers(stage, store, root));
        inspected.extend(stage.layer_stack());
    }
    inspected.sort_unstable();
    inspected.dedup();
    for id in &inspected {
        let layer = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        if !layer.variant_prims.is_empty()
            || layer.prims.values().any(|p| !p.variant_sets.is_empty())
        {
            return Err(NamespaceError::UnsupportedComposition(
                "dormant variant composition requires explicit dependency coordination",
            ));
        }
        for spec in layer.prims.values() {
            if spec
                .references
                .items()
                .chain(spec.payloads.items())
                .any(|r| store.layer(r.layer).is_none() || r.is_expression())
            {
                return Err(NamespaceError::UnsupportedComposition(
                    "unloaded or expression-based assets require resolved dependency coordination",
                ));
            }
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
        gather_dependent_moves(stage, store, root_layers, &source_move, &mut mappings)?;
        normalize_moves(store, &mut mappings)?;
        for mapping in &mappings {
            validate_stage(stage, store, mapping.from, mapping.to)?;
            if mapping.from.property_path().is_none() {
                for path in stage.prim_paths() {
                    if store
                        .paths()
                        .resolve(mapping.from.prim_path())
                        .is_prefix_of(store.paths().resolve(path))
                        && inactive_prim(stage, store, path)
                    {
                        return Err(NamespaceError::UnsupportedComposition(
                            "namespace moves containing inactive composed descendants are not supported",
                        ));
                    }
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
    let mut edited = HashMap::new();
    for id in &inspected {
        let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        let mut layer = original.clone();
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
    validate_projection(store, &edited, &stages, &stage_moves)?;
    let mut transaction = Transaction::new();
    let mut layers = Vec::new();
    let mut specs = Vec::new();
    let mut relocates = Vec::new();
    for id in inspected {
        let original = store.layer(id).unwrap();
        let layer = &edited[&id];
        transaction.expect_generation(id, original.generation());
        let previous = transaction.len();
        append_delta(store, original, layer, &mut transaction, &mut specs);
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
        destination,
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

fn validate_snapshot(stage: &Stage, store: &mut dyn LayerStore) -> Result<(), NamespaceError> {
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

fn gather_dependent_moves(
    stage: &Stage,
    store: &mut dyn LayerStore,
    source_layers: &[LayerId],
    source: &PathMove,
    out: &mut Vec<PathMove>,
) -> Result<(), NamespaceError> {
    let raw_from = store.paths().resolve(source.from.prim_path()).clone();
    let raw_to = store.paths().resolve(source.to.prim_path()).clone();
    for prim in stage.prim_paths() {
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
            if node
                .site()
                .components()
                .iter()
                .any(|c| matches!(c, SpecComponent::VariantSelection { .. }))
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
                    Loc::lookup(path, store.paths())
                        .and_then(|loc| spec_at(store.layer(*l)?, &loc))
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
    let from = SpecPath::from_prim_path(mapping.from.prim_path(), store.paths());
    let to = SpecPath::from_prim_path(mapping.to.prim_path(), store.paths());
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

struct Overlay<'a> {
    store: &'a mut dyn LayerStore,
    layers: &'a HashMap<LayerId, Layer>,
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
