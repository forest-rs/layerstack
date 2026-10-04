// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition-aware namespace moves lowered to ordinary atomic transactions.
//!
//! AOUSD Core §8 (namespace), §10 (composition arcs), §12.4 (path list ops).
//! OpenUSD: `UsdNamespaceEditor` validates all required layer edits before
//! changing specs and repairs relationship targets and attribute connections.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::fmt;

use super::{
    EditTarget, Transaction,
    apply::{Guarded, Raw},
    same::Same,
    spec::{Loc, SpecRef, spec_at, spec_at_mut},
    transaction::Op,
};
use crate::{
    FieldEntry, FieldValue, Layer, LayerId, LayerStack, LayerStore, ListOp, Path, PathId, PrimSpec,
    PropertyEntry, PropertyPath, Reference, ReferenceTarget, SpecComponent, SpecPath, Stage,
    TargetPath, TokenId, Value, VariantSetSpec, VariantSpec,
};

/// A validated namespace move, including dependent authored path repairs.
///
/// Prepare against a stage reflecting the current store, inspect the affected
/// layers and specs, then pass [`Self::transaction`] to [`crate::LiveStage::apply`].
/// The transaction checks every inspected layer's generation and returns the
/// usual guarded inverse, which guards each affected prim slot in full.
/// Preparation changes no layer (it may intern paths).
///
/// Moves stay within one edit target's source layer. Split opinions, edits
/// requiring relocates, variant-qualified moves, instance proxies, and dependent
/// namespace moves through other composition arcs return explicit errors.
/// Moves of or beneath inactive roots are rejected until dormant descendants
/// and their composition dependencies can be coordinated.
/// Relationships, shader connections, all path-list-op buckets, internal and
/// external reference/payload targets, inherits and specializes are repaired in
/// the primary stage's reachable layers. Other stages and unloaded assets are
/// outside this editor's scope. Literal path-expression prefixes and expression
/// references are repaired, anchoring relative expressions at their original
/// author before a move. Asset-path variable expressions require resolved
/// dependency editing and are rejected.
#[derive(Clone, Debug, PartialEq)]
pub struct NamespaceEdit {
    transaction: Transaction,
    source: TargetPath,
    destination: TargetPath,
    layers: Vec<LayerId>,
    specs: Vec<(LayerId, SpecPath)>,
}

/// Why a namespace move cannot be prepared. No layer has been modified.
#[derive(Clone, Debug, PartialEq)]
pub enum NamespaceError {
    /// Paths must be distinct non-root prims, or distinct properties.
    InvalidPaths,
    /// The source object has no composed or editable authored spec.
    NoSuchObject,
    /// The destination is already occupied in the stage or source layer.
    Collision,
    /// The destination parent has no authored spec in the edit target.
    MissingParent,
    /// A prim cannot move beneath itself.
    Cycle,
    /// An object beneath an instance root cannot be edited independently.
    InstanceProxy,
    /// The edit target does not map both paths.
    Unmappable,
    /// The moved object's opinions require edits in additional source layers.
    SplitOpinions {
        /// Source layer that contributes the unsupported opinion.
        layer: LayerId,
        /// Authored spec that would require another namespace edit.
        spec: SpecPath,
    },
    /// A composition case cannot yet be handled without changing its meaning.
    UnsupportedComposition(&'static str),
}

impl fmt::Display for NamespaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPaths => "expected two non-root prim paths or two property paths",
            Self::NoSuchObject => "no composed and authored source object",
            Self::Collision => "the destination is occupied",
            Self::MissingParent => "the destination parent has no editable authored spec",
            Self::Cycle => "a prim cannot move beneath itself",
            Self::InstanceProxy => "cannot edit an instance proxy",
            Self::Unmappable => "the edit target does not map both paths",
            Self::SplitOpinions { .. } => {
                "moving split source opinions requires coordinated layer edits"
            }
            Self::UnsupportedComposition(reason) => reason,
        })
    }
}
impl core::error::Error for NamespaceError {}

impl NamespaceEdit {
    /// Prepares a prim rename/reparent or a property rename/reparent.
    ///
    /// Both paths are in stage namespace and map through `target`. The
    /// destination parent must exist in both the composed stage and source
    /// layer; preparation does not create ancestors or author relocates.
    ///
    /// ```
    /// use layerstack::{EditTarget, InMemoryStore, Layer, LayerId, LiveStage,
    ///     NamespaceEdit, PrimSpec, StageOptions, TargetPath};
    /// let mut store = InMemoryStore::default();
    /// let old = store.path("/Old");
    /// let new = store.path("/New");
    /// let mut layer = Layer::new(LayerId(1));
    /// layer.insert_prim(old, PrimSpec::def());
    /// store.insert_layer(layer);
    /// let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let edit = NamespaceEdit::prepare(live.stage(), &mut store,
    ///     &EditTarget::for_layer(LayerId(1)), TargetPath::Prim(old), TargetPath::Prim(new)).unwrap();
    /// let applied = live.apply(&mut store, edit.transaction()).unwrap();
    /// assert!(live.stage().has_prim(new));
    /// live.apply(&mut store, &applied.inverse).unwrap();
    /// assert!(live.stage().has_prim(old));
    /// ```
    pub fn prepare(
        stage: &Stage,
        store: &mut dyn LayerStore,
        target: &EditTarget,
        source: TargetPath,
        destination: TargetPath,
    ) -> Result<Self, NamespaceError> {
        validate_paths(store, source, destination)?;
        validate_stage(stage, store, source, destination)?;
        let from = map(target, store, source)?;
        let to = map(target, store, destination)?;
        if from
            .components()
            .iter()
            .chain(to.components())
            .any(|c| matches!(c, SpecComponent::VariantSelection { .. }))
        {
            return Err(NamespaceError::UnsupportedComposition(
                "variant-qualified namespace moves are not supported",
            ));
        }
        let id = target.layer();
        let root = stage.root_layer().ok_or(NamespaceError::NoSuchObject)?;
        let root_layers = LayerStack::gather(store, root).layers;
        let layers = reachable_layers(stage, store, root);
        if !layers.contains(&id) {
            return Err(NamespaceError::NoSuchObject);
        }
        validate_opinions(stage, store, source, id, &from)?;
        let before = store.layer(id).ok_or(NamespaceError::NoSuchObject)?.clone();
        let mut moved = before.clone();
        move_specs(store, &mut moved, &from, &to)?;
        let source_move = PathMove::new(spec_target(&from), spec_target(&to));
        let stage_move = PathMove::new(source, destination);
        let mut transaction = Transaction::new();
        let mut edited_layers = Vec::new();
        let mut specs = Vec::new();
        for layer_id in layers {
            let original = store
                .layer(layer_id)
                .ok_or(NamespaceError::NoSuchObject)?
                .clone();
            transaction.expect_generation(layer_id, original.generation());
            let mut edited = if layer_id == id {
                moved.clone()
            } else {
                original.clone()
            };
            if !edited.relocates.is_empty() {
                return Err(NamespaceError::UnsupportedComposition(
                    "namespace moves with authored relocates are not supported",
                ));
            }
            let local_move = if layer_id == id {
                Some(&source_move)
            } else if root_layers.contains(&layer_id) {
                Some(&stage_move)
            } else {
                None
            };
            for (path, spec) in &mut edited.prims {
                repair_prim(store, spec, *path, layer_id, id, local_move, &source_move)?;
            }
            for (path, branches) in &mut edited.variant_prims {
                for spec in branches {
                    repair_prim(store, spec, *path, layer_id, id, local_move, &source_move)?;
                }
            }
            let pseudo_root = store_root_path(store);
            repair_fields(store, &mut edited.metadata, local_move, pseudo_root, false)?;
            if let Some(mapping) = local_move.filter(|m| m.from.property_path().is_none())
                && let Some(path) = original.default_prim_path(store.tokens_mut())
            {
                let old = store.paths_mut().intern(path);
                let new = mapping.prim(store, old);
                if old != new {
                    let path = store.paths().resolve(new);
                    let text = if path.segments().len() == 1 {
                        alloc::string::String::from(store.tokens().resolve(path.leaf().unwrap()))
                    } else {
                        path.display(store.tokens())
                    };
                    edited.default_prim = Some(store.tokens_mut().intern(&text));
                }
            }

            let previous_len = transaction.len();
            append_delta(store, &original, &edited, &mut transaction, &mut specs);
            if original.default_prim != edited.default_prim
                || !original.metadata.same(&edited.metadata)
            {
                push(
                    &mut transaction,
                    Raw::LayerFields {
                        layer: layer_id,
                        default_prim: edited.default_prim,
                        metadata: edited.metadata.clone(),
                    },
                    Raw::LayerFields {
                        layer: layer_id,
                        default_prim: original.default_prim,
                        metadata: original.metadata.clone(),
                    },
                );
            }
            if transaction.len() > previous_len {
                edited_layers.push(layer_id);
            }
        }
        edited_layers.sort_unstable();
        specs.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.prim_path().cmp(&b.1.prim_path())));
        Ok(Self {
            transaction,
            source,
            destination,
            layers: edited_layers,
            specs,
        })
    }

    /// Original composed object path.
    #[must_use]
    pub fn source(&self) -> TargetPath {
        self.source
    }
    /// New composed object path.
    #[must_use]
    pub fn destination(&self) -> TargetPath {
        self.destination
    }
    /// Sorted source and dependency layers whose authored content changes.
    #[must_use]
    pub fn layers_to_edit(&self) -> &[LayerId] {
        &self.layers
    }
    /// Prim slots affected by moves and dependent path repairs.
    ///
    /// Each entry includes all variants and properties held by that slot.
    /// Layer metadata changes are reported by [`Self::layers_to_edit`].
    #[must_use]
    pub fn specs_to_edit(&self) -> &[(LayerId, SpecPath)] {
        &self.specs
    }
    /// Atomic, generation-guarded edit; apply through [`crate::LiveStage::apply`].
    #[must_use]
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }
    /// Takes the prepared transaction, preserving its generation guards.
    #[must_use]
    pub fn into_transaction(self) -> Transaction {
        self.transaction
    }
}

fn map(
    target: &EditTarget,
    store: &mut dyn LayerStore,
    path: TargetPath,
) -> Result<SpecPath, NamespaceError> {
    let prim = target
        .map_to_spec_path(path.prim_path(), store.paths_mut())
        .ok_or(NamespaceError::Unmappable)?;
    Ok(match path {
        TargetPath::Prim(_) => prim,
        TargetPath::Property(p) => prim.with_property(p.property()),
    })
}

fn spec_target(path: &SpecPath) -> TargetPath {
    match path.property() {
        Some(name) => TargetPath::Property(PropertyPath::new(path.prim_path(), name)),
        None => TargetPath::Prim(path.prim_path()),
    }
}

fn validate_paths(
    store: &dyn LayerStore,
    from: TargetPath,
    to: TargetPath,
) -> Result<(), NamespaceError> {
    if core::mem::discriminant(&from) != core::mem::discriminant(&to)
        || from == to
        || store
            .paths()
            .resolve(from.prim_path())
            .segments()
            .is_empty()
        || store.paths().resolve(to.prim_path()).segments().is_empty()
    {
        return Err(NamespaceError::InvalidPaths);
    }
    let valid = |p: TargetPath| {
        store
            .paths()
            .resolve(p.prim_path())
            .segments()
            .iter()
            .all(|n| crate::ident::is_identifier(store.tokens().resolve(*n)))
            && p.property_path().is_none_or(|p| {
                store
                    .tokens()
                    .resolve(p.property())
                    .split(':')
                    .all(crate::ident::is_identifier)
            })
    };
    if !valid(from) || !valid(to) {
        return Err(NamespaceError::InvalidPaths);
    }
    if matches!(from, TargetPath::Prim(_))
        && store
            .paths()
            .resolve(from.prim_path())
            .is_prefix_of(store.paths().resolve(to.prim_path()))
    {
        return Err(NamespaceError::Cycle);
    }
    Ok(())
}
fn validate_stage(
    stage: &Stage,
    store: &mut dyn LayerStore,
    from: TargetPath,
    to: TargetPath,
) -> Result<(), NamespaceError> {
    let exists = |p| match p {
        TargetPath::Prim(p) => stage.has_prim(p),
        TargetPath::Property(p) => stage.has_property_path(p),
    };
    if !exists(from) {
        return Err(NamespaceError::NoSuchObject);
    }
    if exists(to) {
        return Err(NamespaceError::Collision);
    }
    let parent = match to {
        TargetPath::Prim(p) => store
            .paths()
            .resolve(p)
            .parent()
            .ok_or(NamespaceError::InvalidPaths)?,
        TargetPath::Property(p) => store.paths().resolve(p.prim_path()).clone(),
    };
    let parent = store.paths_mut().intern(parent);
    if !store.paths().resolve(parent).segments().is_empty() && !stage.has_prim(parent) {
        return Err(NamespaceError::MissingParent);
    }
    for object in [from, to] {
        let mut current = Some(store.paths().resolve(object.prim_path()).clone());
        let mut first = true;
        while let Some(path) = current {
            let at = store.paths_mut().intern(path.clone());
            if inactive_prim(stage, store, at) {
                return Err(NamespaceError::UnsupportedComposition(
                    "namespace moves of or beneath inactive roots require dormant descendant coordination",
                ));
            }
            if stage.is_instance(at) && !first {
                return Err(NamespaceError::InstanceProxy);
            }
            first = false;
            current = path.parent();
        }
    }
    Ok(())
}
fn validate_opinions(
    stage: &Stage,
    store: &dyn LayerStore,
    from: TargetPath,
    layer: LayerId,
    source: &SpecPath,
) -> Result<(), NamespaceError> {
    validate_authored_subtree(stage, store, from, layer, source)?;
    // The snapshot inventory includes retained inactive roots. Default
    // traversal intentionally omits them and is insufficient for validation.
    for prim in stage.prim_paths() {
        let inside = store
            .paths()
            .resolve(from.prim_path())
            .is_prefix_of(store.paths().resolve(prim));
        if inside && from.property_path().is_none() && inactive_prim(stage, store, prim) {
            return Err(NamespaceError::UnsupportedComposition(
                "namespace moves containing inactive roots require dormant descendant coordination",
            ));
        }
        let Some(stack) = stage.prim_stack(prim) else {
            continue;
        };
        if inside && (from.property_path().is_none() || prim == from.prim_path()) {
            for (l, spec) in &stack {
                let relevant = from.property_path().is_none()
                    || store
                        .layer(*l)
                        .and_then(|l| {
                            Loc::lookup(spec, store.paths()).and_then(|loc| spec_at(l, &loc))
                        })
                        .is_some_and(|s| {
                            s.properties()
                                .iter()
                                .any(|p| Some(p.name) == source.property())
                        });
                if relevant
                    && (*l != layer
                        || !spec
                            .components()
                            .iter()
                            .all(|c| matches!(c, SpecComponent::Prim(_))))
                {
                    return Err(NamespaceError::SplitOpinions {
                        layer: *l,
                        spec: spec.clone(),
                    });
                }
            }
        } else if stack.iter().any(|(l, p)| {
            if *l != layer {
                return false;
            }
            if let Some(property) = source.property() {
                p.prim_path() == source.prim_path()
                    && store
                        .layer(*l)
                        .and_then(|l| {
                            Loc::lookup(p, store.paths()).and_then(|loc| spec_at(l, &loc))
                        })
                        .is_some_and(|s| s.properties().iter().any(|p| p.name == property))
            } else {
                store
                    .paths()
                    .resolve(source.prim_path())
                    .is_prefix_of(store.paths().resolve(p.prim_path()))
            }
        }) && !alias_preserves_namespace(stage, store, prim, from, layer, source)
        {
            return Err(NamespaceError::UnsupportedComposition(
                "dependent namespace through another composition arc requires coordinated edits",
            ));
        }
    }
    Ok(())
}

// AOUSD Core §7.6 (active), §11.3.1 (inactive descendants are unpopulated).
// Read the strongest authored active opinion, including selected variants,
// without requiring the stage-controls API from the next stacked branch.
fn inactive_prim(stage: &Stage, store: &dyn LayerStore, prim: PathId) -> bool {
    let active = store.tokens().lookup("active");
    stage
        .prim_stack(prim)
        .unwrap_or_default()
        .into_iter()
        .find_map(|(layer, path)| {
            let spec = Loc::lookup(&path, store.paths())
                .and_then(|loc| spec_at(store.layer(layer)?, &loc))?;
            if let SpecRef::Prim(prim) = spec
                && prim.active.is_some()
            {
                return prim.active;
            }
            active
                .and_then(|key| crate::get_field(spec.fields(), &key))
                .and_then(|field| match field {
                    FieldValue::Value(Value::Bool(value)) => Some(*value),
                    _ => None,
                })
        })
        == Some(false)
}

// Every source layer in the contributing node's stack shares this namespace.
// Inspect authored slots rather than only populated opinions: weaker specs
// behind inactive roots or unselected variants must not remain at the old path.
fn validate_authored_subtree(
    stage: &Stage,
    store: &dyn LayerStore,
    from: TargetPath,
    layer: LayerId,
    source: &SpecPath,
) -> Result<(), NamespaceError> {
    let mut source_layers = LayerStack::gather(store, layer).layers;
    if let Some(graph) = stage.explain_prim_graph(from.prim_path()) {
        for (_, node) in graph.nodes() {
            if node.site().prim_path() == source.prim_path() {
                let stack = LayerStack::gather(store, node.layer_stack());
                if stack.layers.contains(&layer) {
                    source_layers.extend(stack.layers);
                }
            }
        }
    }
    source_layers.sort_unstable();
    source_layers.dedup();
    for id in source_layers {
        let Some(authored) = store.layer(id) else {
            continue;
        };
        for (path, spec) in authored.prims.iter().map(|(p, s)| (*p, s)).chain(
            authored
                .variant_prims
                .iter()
                .flat_map(|(p, ss)| ss.iter().map(|s| (*p, s))),
        ) {
            let relevant = if let Some(property) = source.property() {
                path == source.prim_path() && spec.properties.iter().any(|p| p.name == property)
            } else {
                store
                    .paths()
                    .resolve(source.prim_path())
                    .is_prefix_of(store.paths().resolve(path))
            };
            if !relevant {
                continue;
            }
            if id != layer {
                return Err(NamespaceError::SplitOpinions {
                    layer: id,
                    spec: Loc::Prim {
                        path,
                        sites: spec.outer_variant_sites.clone(),
                    }
                    .spec_path(store.paths()),
                });
            }
            if source.property().is_none() && spec.active == Some(false) {
                return Err(NamespaceError::UnsupportedComposition(
                    "namespace moves containing authored inactive roots require dormant descendant coordination",
                ));
            }
        }
    }
    Ok(())
}

// An arc targeting the moved root keeps its composed mount point. Its authored
// reference/inherit/specialize target changes, but descendants retain their
// names. Moving a descendant inside an arc needs a separate namespace map.
fn alias_preserves_namespace(
    stage: &Stage,
    store: &dyn LayerStore,
    prim: PathId,
    from: TargetPath,
    layer: LayerId,
    source: &SpecPath,
) -> bool {
    if from.property_path().is_some() {
        return false;
    }
    let mut current = Some(store.paths().resolve(prim).clone());
    while let Some(path) = current {
        if let Some(id) = store.paths().lookup(&path)
            && stage.explain_prim_graph(id).is_some_and(|graph| {
                graph.nodes().any(|(_, node)| {
                    node.arc_kind() != crate::ArcKind::Local
                        && usize::from(node.namespace_depth()) == path.segments().len()
                        && node.site().prim_path() == source.prim_path()
                        && LayerStack::gather(store, node.layer_stack())
                            .layers
                            .contains(&layer)
                })
            })
        {
            return true;
        }

        current = path.parent();
    }
    false
}

fn reachable_layers(stage: &Stage, store: &dyn LayerStore, root: LayerId) -> Vec<LayerId> {
    let mut found = Vec::new();
    let mut pending = alloc::vec![root];
    for prim in stage.prim_paths() {
        pending.extend(
            stage
                .prim_stack(prim)
                .unwrap_or_default()
                .into_iter()
                .map(|(layer, _)| layer),
        );
    }
    while let Some(id) = pending.pop() {
        if found.contains(&id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            continue;
        };
        found.push(id);
        pending.extend(layer.sublayers.iter().map(|s| s.layer));
        for spec in layer
            .prims
            .values()
            .chain(layer.variant_prims.values().flatten())
        {
            for refs in [&spec.references, &spec.payloads] {
                collect_ref_layers(refs, &mut pending);
            }
            collect_variant_layers(&spec.variant_sets, &mut pending);
        }
    }
    found.sort_unstable();
    found
}
fn collect_ref_layers(refs: &ListOp<Reference>, layers: &mut Vec<LayerId>) {
    let _ = refs.map_lists(|items| {
        layers.extend(items.iter().map(|r| r.layer));
        Vec::<Reference>::new()
    });
}
fn collect_variant_layers(
    sets: &crate::HashMap<TokenId, VariantSetSpec>,
    layers: &mut Vec<LayerId>,
) {
    for set in sets.values() {
        for v in set.variants.values() {
            collect_ref_layers(&v.references, layers);
            collect_ref_layers(&v.payloads, layers);
            collect_variant_layers(&v.variant_sets, layers);
        }
    }
}

fn move_specs(
    store: &mut dyn LayerStore,
    layer: &mut Layer,
    from: &SpecPath,
    to: &SpecPath,
) -> Result<(), NamespaceError> {
    let from_loc = Loc::of(from, store.paths_mut());
    let to_loc = Loc::of(to, store.paths_mut());
    if spec_at(layer, &from_loc).is_none() {
        return Err(NamespaceError::NoSuchObject);
    }
    if let Some(name) = from.property() {
        let source = spec_at(layer, &from_loc).unwrap();
        let index = source
            .properties()
            .iter()
            .position(|p| p.name == name)
            .ok_or(NamespaceError::NoSuchObject)?;
        let property = source.properties()[index].spec.clone();
        let new_name = to.property().ok_or(NamespaceError::InvalidPaths)?;
        let destination = spec_at(layer, &to_loc).ok_or(NamespaceError::MissingParent)?;
        if destination.properties().iter().any(|p| p.name == new_name) {
            return Err(NamespaceError::Collision);
        }
        spec_at_mut(layer, &from_loc)
            .unwrap()
            .properties()
            .remove(index);
        let mut destination = spec_at_mut(layer, &to_loc).unwrap();
        let new_index = if from.prim_path() == to.prim_path() {
            index
        } else {
            destination.properties().len()
        };
        destination.properties().insert(
            new_index,
            PropertyEntry {
                name: new_name,
                spec: property,
            },
        );
        return Ok(());
    }
    let source_path = store.paths().resolve(from.prim_path()).clone();
    let destination_path = store.paths().resolve(to.prim_path()).clone();
    let parent_path = destination_path
        .parent()
        .ok_or(NamespaceError::InvalidPaths)?;
    let parent = store.paths_mut().intern(parent_path.clone());
    if !parent_path.segments().is_empty() && !layer.prims.contains_key(&parent) {
        return Err(NamespaceError::MissingParent);
    }
    let moved: Vec<_> = layer
        .prims
        .keys()
        .chain(layer.variant_prims.keys())
        .copied()
        .filter(|p| source_path.is_prefix_of(store.paths().resolve(*p)))
        .collect();
    let mut pairs = Vec::new();
    for old in moved {
        if pairs.iter().any(|(p, _)| *p == old) {
            continue;
        }
        let suffix = store
            .paths()
            .resolve(old)
            .strip_prefix(&source_path)
            .unwrap()
            .to_vec();
        let new = store.paths_mut().intern(destination_path.join(&suffix));
        if layer.prims.contains_key(&new) || layer.variant_prims.contains_key(&new) {
            return Err(NamespaceError::Collision);
        }
        pairs.push((old, new));
    }
    for (old, new) in pairs {
        if let Some(spec) = layer.prims.remove(&old) {
            layer.prims.insert(new, spec);
        }
        if let Some(branches) = layer.variant_prims.remove(&old) {
            layer.variant_prims.insert(new, branches);
        }
    }
    let source_parent = store.paths_mut().intern(source_path.parent().unwrap());
    let old_name = source_path.leaf().unwrap();
    let new_name = destination_path.leaf().unwrap();
    let old_index = layer
        .prims
        .get(&source_parent)
        .and_then(|s| s.authored_children.iter().position(|n| *n == old_name));
    if let Some(s) = layer.prims.get_mut(&source_parent) {
        s.authored_children.retain(|n| *n != old_name);
        if let Some(order) = &mut s.prim_order {
            order.retain(|n| *n != old_name);
        }
    }
    if let Some(s) = layer.prims.get_mut(&parent) {
        let index = if parent == source_parent {
            old_index.unwrap_or(s.authored_children.len())
        } else {
            s.authored_children.len()
        };
        s.authored_children
            .insert(index.min(s.authored_children.len()), new_name);
    }
    let path_move = PathMove::new(
        TargetPath::Prim(from.prim_path()),
        TargetPath::Prim(to.prim_path()),
    );
    for spec in layer
        .prims
        .values_mut()
        .chain(layer.variant_prims.values_mut().flatten())
    {
        for site in &mut spec.outer_variant_sites {
            site.host_path = path_move.prim(store, site.host_path);
        }
    }
    Ok(())
}

struct PathMove {
    from: TargetPath,
    to: TargetPath,
}
impl PathMove {
    fn new(from: TargetPath, to: TargetPath) -> Self {
        Self { from, to }
    }
    fn prim(&self, store: &mut dyn LayerStore, path: PathId) -> PathId {
        if self.from.property_path().is_some() {
            return path;
        }
        let Some(suffix) = store
            .paths()
            .resolve(path)
            .strip_prefix(store.paths().resolve(self.from.prim_path()))
            .map(<[_]>::to_vec)
        else {
            return path;
        };
        let result = store.paths().resolve(self.to.prim_path()).join(&suffix);
        store.paths_mut().intern(result)
    }
    fn target(&self, store: &mut dyn LayerStore, path: TargetPath) -> TargetPath {
        if self.from.property_path().is_some() {
            return if path == self.from { self.to } else { path };
        }
        let prim = self.prim(store, path.prim_path());
        match path {
            TargetPath::Prim(_) => TargetPath::Prim(prim),
            TargetPath::Property(p) => TargetPath::Property(PropertyPath::new(prim, p.property())),
        }
    }
}
fn map_list<T>(list: &mut ListOp<T>, mut map: impl FnMut(&mut T)) {
    for bucket in list.lists_mut() {
        for item in bucket {
            map(item);
        }
    }
}
fn repair_refs(
    store: &mut dyn LayerStore,
    refs: &mut ListOp<Reference>,
    author: LayerId,
    edited: LayerId,
    local: Option<&PathMove>,
    source: &PathMove,
) -> Result<(), NamespaceError> {
    if refs.items().any(Reference::is_expression) {
        return Err(NamespaceError::UnsupportedComposition(
            "reference/payload asset expressions require resolved dependency editing",
        ));
    }
    map_list(refs, |r| {
        let mapping = if r.layer == edited {
            Some(source)
        } else if r.layer == author && r.asset.is_none() {
            local
        } else {
            None
        };
        if let (Some(mapping), ReferenceTarget::Prim(path)) = (mapping, r.target.clone()) {
            r.target = ReferenceTarget::Prim(mapping.prim(store, path));
        }
    });
    Ok(())
}
fn repair_properties(
    store: &mut dyn LayerStore,
    properties: &mut [PropertyEntry],
    local: Option<&PathMove>,
    anchor: PathId,
    owner_moves: bool,
) -> Result<(), NamespaceError> {
    for entry in properties {
        let moved_property = local.filter(|m| {
            m.to.property_path()
                .is_some_and(|p| p.prim_path() == anchor && p.property() == entry.name)
        });
        let entry_anchor = moved_property.map_or(anchor, |m| m.from.prim_path());
        let entry_moves =
            owner_moves || moved_property.is_some_and(|m| m.from.prim_path() != m.to.prim_path());
        let spec = Arc::make_mut(&mut entry.spec);
        if let (Some(mapping), Some(targets)) = (local, &mut spec.targets) {
            map_list(targets, |p| *p = mapping.target(store, *p));
        }
        if let Some(value) = &mut spec.default {
            repair_value(store, value, local, entry_anchor, entry_moves)?;
        }
        if let Some(samples) = &mut spec.time_samples {
            for (_, value) in samples.make_mut() {
                repair_value(store, value, local, entry_anchor, entry_moves)?;
            }
        }
        repair_fields(
            store,
            spec.metadata.make_mut(),
            local,
            entry_anchor,
            entry_moves,
        )?;
    }
    Ok(())
}
fn path_names(store: &dyn LayerStore, path: PathId) -> Vec<alloc::string::String> {
    store
        .paths()
        .resolve(path)
        .segments()
        .iter()
        .map(|n| alloc::string::String::from(store.tokens().resolve(*n)))
        .collect()
}
fn store_root_path(store: &mut dyn LayerStore) -> PathId {
    store.paths_mut().intern(Path::root())
}
fn repair_value(
    store: &mut dyn LayerStore,
    value: &mut Value,
    local: Option<&PathMove>,
    anchor: PathId,
    owner_moves: bool,
) -> Result<(), NamespaceError> {
    match value {
        Value::PathExpression(text) => {
            if let Some(mapping) = local {
                let expression = crate::PathExpression::parse(text).map_err(|_| {
                    NamespaceError::UnsupportedComposition(
                        "invalid authored path expression prevents namespace editing",
                    )
                })?;
                let source = path_names(store, mapping.from.prim_path());
                let destination = path_names(store, mapping.to.prim_path());
                let property = mapping
                    .from
                    .property_path()
                    .map(|p| store.tokens().resolve(p.property()));
                let new_property = mapping
                    .to
                    .property_path()
                    .map(|p| store.tokens().resolve(p.property()));
                let edited = expression.edit_namespace(
                    &source,
                    property,
                    &destination,
                    new_property,
                    &path_names(store, anchor),
                    owner_moves,
                );
                if edited != expression {
                    *text = Arc::from(edited.lossless_text());
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                repair_value(store, value, local, anchor, owner_moves)?;
            }
        }
        Value::Dictionary(values) => {
            for (_, value) in values {
                repair_value(store, value, local, anchor, owner_moves)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn repair_fields(
    store: &mut dyn LayerStore,
    fields: &mut [FieldEntry],
    local: Option<&PathMove>,
    anchor: PathId,
    owner_moves: bool,
) -> Result<(), NamespaceError> {
    for field in fields {
        match &mut field.value {
            FieldValue::Value(value) => repair_value(store, value, local, anchor, owner_moves)?,
            FieldValue::PathListOp(paths) => {
                if let Some(mapping) = local {
                    map_list(paths, |p| *p = mapping.target(store, *p));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn repair_prim(
    store: &mut dyn LayerStore,
    spec: &mut PrimSpec,
    path: PathId,
    author: LayerId,
    edited: LayerId,
    local: Option<&PathMove>,
    source: &PathMove,
) -> Result<(), NamespaceError> {
    let anchor = local.map_or(path, |mapping| {
        PathMove::new(mapping.to, mapping.from).prim(store, path)
    });
    let owner_moves = anchor != path;
    repair_properties(store, &mut spec.properties, local, anchor, owner_moves)?;
    repair_fields(store, &mut spec.fields, local, anchor, owner_moves)?;
    repair_refs(store, &mut spec.references, author, edited, local, source)?;
    repair_refs(store, &mut spec.payloads, author, edited, local, source)?;
    if let Some(mapping) = local {
        map_list(&mut spec.inherits, |p| *p = mapping.prim(store, *p));
        map_list(&mut spec.specializes, |p| *p = mapping.prim(store, *p));
    }
    repair_variants(
        store,
        &mut spec.variant_sets,
        author,
        edited,
        local,
        source,
        anchor,
        owner_moves,
    )
}
fn repair_variants(
    store: &mut dyn LayerStore,
    sets: &mut crate::HashMap<TokenId, VariantSetSpec>,
    author: LayerId,
    edited: LayerId,
    local: Option<&PathMove>,
    source: &PathMove,
    anchor: PathId,
    owner_moves: bool,
) -> Result<(), NamespaceError> {
    for set in sets.values_mut() {
        for v in set.variants.values_mut() {
            repair_variant(store, v, author, edited, local, source, anchor, owner_moves)?;
        }
    }
    Ok(())
}
fn repair_variant(
    store: &mut dyn LayerStore,
    spec: &mut VariantSpec,
    author: LayerId,
    edited: LayerId,
    local: Option<&PathMove>,
    source: &PathMove,
    anchor: PathId,
    owner_moves: bool,
) -> Result<(), NamespaceError> {
    repair_properties(store, &mut spec.properties, local, anchor, owner_moves)?;
    repair_fields(store, &mut spec.fields, local, anchor, owner_moves)?;
    repair_refs(store, &mut spec.references, author, edited, local, source)?;
    repair_refs(store, &mut spec.payloads, author, edited, local, source)?;
    if let Some(mapping) = local {
        map_list(&mut spec.inherits, |p| *p = mapping.prim(store, *p));
        map_list(&mut spec.specializes, |p| *p = mapping.prim(store, *p));
    }
    repair_variants(
        store,
        &mut spec.variant_sets,
        author,
        edited,
        local,
        source,
        anchor,
        owner_moves,
    )
}
fn push(transaction: &mut Transaction, step: Raw, written: Raw) {
    transaction
        .ops
        .push(Op::Raw(Box::new(Guarded { step, written })));
}
fn append_delta(
    store: &dyn LayerStore,
    original: &Layer,
    edited: &Layer,
    transaction: &mut Transaction,
    specs: &mut Vec<(LayerId, SpecPath)>,
) {
    let mut paths: Vec<_> = original
        .prims
        .keys()
        .chain(original.variant_prims.keys())
        .chain(edited.prims.keys())
        .chain(edited.variant_prims.keys())
        .copied()
        .collect();
    paths.sort_unstable();
    paths.dedup();
    for path in paths {
        let before = Raw::PrimSlots {
            layer: original.id,
            path,
            main: original.prims.get(&path).cloned(),
            branches: original.variant_prims.get(&path).cloned(),
        };
        let after = Raw::PrimSlots {
            layer: edited.id,
            path,
            main: edited.prims.get(&path).cloned(),
            branches: edited.variant_prims.get(&path).cloned(),
        };
        if !original
            .prims
            .get(&path)
            .cloned()
            .same(&edited.prims.get(&path).cloned())
            || !original
                .variant_prims
                .get(&path)
                .cloned()
                .same(&edited.variant_prims.get(&path).cloned())
        {
            push(transaction, after, before);
            specs.push((original.id, SpecPath::from_prim_path(path, store.paths())));
        }
    }
}
