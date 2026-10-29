// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition-aware deletes of a prim or property, lowered to one atomic
//! transaction.
//!
//! The root layer stack's specs of the object are removed. A prim that
//! still composes from across an arc (a reference, payload, variant
//! branch or relocate) is removed from namespace with a relocate to `<>`;
//! a property cannot be. Every authored path naming the object (and for a
//! prim, anything beneath it) is then dropped: relationship targets,
//! attribute connections, inherits, specializes, internal references and
//! payloads, path expression terms and the layers' `defaultPrim`.
//!
//! Spec: AOUSD Core §8 (namespace), §10.3.2.6 (relocates), §12.4 (list
//! ops). OpenUSD: `UsdNamespaceEditor::DeletePrimAtPath` and
//! `DeletePropertyAtPath`.

use super::coordinated::Overlay;
use super::*;
use crate::prim_indexing::Boundaries;
use crate::sort;
use crate::{
    HashMap, HashSet, Relocate, SparseListOp, VariantSets,
    spec_path::VariantSelectionSite,
    stage::preparation::{PreparationIndex, layer_links},
};

pub(super) fn prepare(
    stage: &Stage,
    store: &mut dyn LayerStore,
    target: &EditTarget,
    object: TargetPath,
) -> Result<NamespaceEdit, NamespaceError> {
    prepare_with(stage, store, target, object, true)
}

/// [`prepare`] by copying and composing every layer whole, as before
/// stages retained preparation evidence: the oracle the retained proofs
/// are tested against.
#[cfg(test)]
pub(super) fn prepare_by_composition(
    stage: &Stage,
    store: &mut dyn LayerStore,
    target: &EditTarget,
    object: TargetPath,
) -> Result<NamespaceEdit, NamespaceError> {
    prepare_with(stage, store, target, object, false)
}

/// [`prepare`], editing working copies of the slots the delete can change
/// and composing only `object` and its ancestors when `use_retained` and
/// the stage retains preparation evidence; otherwise copying and composing
/// every layer whole. Both find the same edit, or the same error.
fn prepare_with(
    stage: &Stage,
    store: &mut dyn LayerStore,
    target: &EditTarget,
    object: TargetPath,
    use_retained: bool,
) -> Result<NamespaceEdit, NamespaceError> {
    let path = store.paths().resolve(object.prim_path()).clone();
    if path.segments().is_empty() {
        return Err(NamespaceError::InvalidPaths);
    }
    let exists = |stage: &Stage| match object {
        TargetPath::Prim(prim) => stage.has_prim(prim),
        TargetPath::Property(property) => stage.has_property_path(property),
    };
    if !exists(stage) {
        return Err(NamespaceError::NoSuchObject);
    }
    // An object beneath an instance belongs to its prototype.
    let mut ancestor = match object {
        TargetPath::Prim(_) => path.parent(),
        TargetPath::Property(_) => Some(path.clone()),
    };
    while let Some(prim) = ancestor {
        let id = store.paths().intern(prim.clone());
        if stage.is_instance(id) && TargetPath::Prim(id) != object {
            return Err(NamespaceError::InstanceProxy);
        }
        ancestor = prim.parent();
    }
    // A variant's edit target maps into its branch (`/V{v=x}F` for
    // `/V/F`), across the variant arc: `UsdNamespaceEditor` refuses edit
    // targets that map paths across composition arcs
    // (`_EditProcessor::_GatherLayersToEdit`,
    // `pxr/usd/usd/namespaceEditor.cpp`).
    let mapped = map(target, store, object)?;
    if !stage.layer_stack().contains(&target.layer())
        || mapped.has_variant_selections()
        || spec_target(&mapped) != object
    {
        return Err(NamespaceError::Unmappable);
    }
    let root = stage.root_layer().ok_or(NamespaceError::NoSuchObject)?;
    let stack = stage.layer_stack().to_vec();
    let index = if use_retained {
        stage.preparation(store.paths())
    } else {
        None
    };
    // With retained evidence, each layer is edited as a working copy of
    // the slots the delete can change: those at and beneath the object,
    // its ancestors, and those that author paths.
    let mut copied = HashMap::new();
    let mut edited = HashMap::new();
    let mut dormant = HashMap::new();
    let indexes: Vec<&PreparationIndex> = index.into_iter().collect();
    let dependents = dependents(stage, store, index, object);
    for id in &stack {
        let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        let mut layer = match index {
            Some(index) => {
                let slots = working_slots(store, index, original, object, &dependents);
                let copy = retained::working_copy(original, &slots);
                copied.insert(*id, slots);
                copy
            }
            None => original.clone(),
        };
        dormant.insert(
            *id,
            coordinated::dormant_variants(store, &[stage], &indexes, &stack, &layer),
        );
        remove_specs(store, &mut layer, object, &[]);
        for (dependent, within) in &dependents {
            remove_specs(store, &mut layer, *dependent, within);
        }
        edited.insert(*id, layer);
    }
    let still_composed = composes(store, stage, root, &edited, &copied, index, object)?;
    if still_composed {
        relocate_away(store, &mut edited, &stack, target.layer(), object)?;
    }
    // In layer stack order, so the first spec that cannot be repaired is
    // always the one reported; once for a layer the stack repeats.
    let mut repaired = HashSet::new();
    for id in &stack {
        if repaired.insert(*id) {
            repair(
                store,
                edited.get_mut(id).expect("edited stack layer"),
                object,
                &dormant[id],
            )?;
        }
    }
    if composes(store, stage, root, &edited, &copied, index, object)? {
        return Err(NamespaceError::UnsupportedComposition(
            "the delete does not remove the object from the composed stage",
        ));
    }

    let mut transaction = Transaction::new();
    let mut layers = Vec::new();
    let mut specs = Vec::new();
    let mut relocates = Vec::new();
    for id in stack {
        let original = store.layer(id).ok_or(NamespaceError::NoSuchObject)?;
        let layer = &edited[&id];
        transaction.expect_generation(id, original.generation());
        let previous = transaction.len();
        match copied.get(&id) {
            Some(slots) => {
                let paths = retained::copy_slots(slots, layer);
                retained::append_delta_at(original, layer, &paths, &mut transaction, &mut specs);
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
        source: object,
        destination: None,
        layers,
        specs,
        namespace_moves: Vec::new(),
        relocates,
    })
}

/// The slots of `layer` the delete of `object` can change or must
/// inspect: those at and beneath its prim and their ancestors, and those
/// that author paths.
fn working_slots(
    store: &dyn LayerStore,
    index: &PreparationIndex,
    layer: &Layer,
    object: TargetPath,
    dependents: &[(TargetPath, Vec<VariantSelectionSite>)],
) -> Vec<PathId> {
    let mut slots: HashSet<PathId> = layer_links(Some(index), layer).into_owned();
    for object in core::iter::once(object).chain(dependents.iter().map(|(d, _)| *d)) {
        let prim = object.prim_path();
        slots.extend(retained::slots_beneath(
            store,
            index.children(),
            layer,
            prim,
        ));
        slots.extend(retained::at_or_above(store, prim));
    }
    let mut slots: Vec<PathId> = slots
        .into_iter()
        .filter(|path| layer.prims.contains_key(path) || layer.variant_prims.contains_key(path))
        .collect();
    sort::sort_unstable(&mut slots);
    slots
}

/// Whether `object` composes in the stage the `edited` layers project.
/// With retained evidence (`index`), the projected layers are composed in
/// place in `store`, or copied whole when the store does not lend them,
/// and only `object` and its ancestors are composed, which a whole
/// composition composes alike; otherwise every edited layer is copied whole
/// and the whole stage composed.
fn composes(
    store: &mut dyn LayerStore,
    stage: &Stage,
    root: LayerId,
    edited: &HashMap<LayerId, Layer>,
    copied: &HashMap<LayerId, Vec<PathId>>,
    index: Option<&PreparationIndex>,
    object: TargetPath,
) -> Result<bool, NamespaceError> {
    let exists = |stage: &Stage| match object {
        TargetPath::Prim(prim) => stage.has_prim(prim),
        TargetPath::Property(property) => stage.has_property_path(property),
    };
    let Some(index) = index else {
        let mut overlay = Overlay {
            store,
            layers: edited,
        };
        return Ok(exists(&Stage::compose(
            &mut overlay,
            root,
            stage.options().clone(),
        )));
    };
    let mut edits = HashMap::new();
    for (id, slots) in copied {
        let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        let paths = retained::copy_slots(slots, &edited[id]);
        if let Some(edit) = retained::layer_edit(original, &edited[id], &paths) {
            edits.insert(*id, edit);
        }
    }
    let mut projected = Vec::new();
    for (id, edit) in &edits {
        let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        projected.push((original, &edited[id], edit.slots.as_slice()));
    }
    let children = index
        .children()
        .projected(store, &projected)
        .unwrap_or_default();
    let options = crate::StageOptions {
        mask: Some(crate::PopulationMask {
            include: alloc::vec![object.prim_path()],
        }),
        with_dependencies: false,
        ..stage.options().clone()
    };
    let compose = |store: &mut dyn LayerStore| {
        let partial = Stage::compose_retained(
            store,
            root,
            options.clone(),
            false,
            &Boundaries::default(),
            &children,
        );
        let found = exists(&partial);
        // Every test that prepares a delete checks the part against the
        // whole.
        #[cfg(test)]
        assert_eq!(
            found,
            exists(&Stage::compose(store, root, stage.options().clone())),
            "a partial projection finds what the whole projection finds"
        );
        found
    };
    let mut in_place = retained::InPlace::project(store, edited, &edits);
    if in_place.complete() {
        return Ok(compose(in_place.store()));
    }
    drop(in_place);
    let mut layers = HashMap::new();
    for (id, edit) in &edits {
        let original = store.layer(*id).ok_or(NamespaceError::NoSuchObject)?;
        layers.insert(*id, retained::materialize(original, &edited[id], edit));
    }
    let mut overlay = Overlay {
        store,
        layers: &layers,
    };
    Ok(compose(&mut overlay))
}

/// Removes `object`'s specs inside the branches `within` (hosted above
/// it; none for the specs outside every branch) from `layer`: for a prim,
/// every spec at or beneath it there (a branch of an ancestor is an arc,
/// so the specs of other branches stay) and its name from its parent's
/// children; for a property, the property of the prim's spec there.
pub(super) fn remove_specs(
    store: &mut dyn LayerStore,
    layer: &mut Layer,
    object: TargetPath,
    within: &[VariantSelectionSite],
) {
    let prim = object.prim_path();
    if let TargetPath::Property(property) = object {
        let loc = Loc::Prim {
            path: prim,
            sites: within.to_vec(),
        };
        if let Some(mut spec) = spec_at_mut(layer, &loc) {
            spec.properties().retain(|p| p.name != property.property());
        }
        return;
    }
    let root = store.paths().resolve(prim).clone();
    let owned = |store: &dyn LayerStore, spec: &PrimSpec| {
        let sites = spec.outer_variant_sites();
        sites.len() >= within.len()
            && sites[..within.len()] == *within
            && sites[within.len()..]
                .iter()
                .all(|site| root.is_prefix_of(store.paths().resolve(site.host_path)))
    };
    // Every path with a spec has one in `prims`; the specs of other
    // branches at it are in `variant_prims` (see `Layer::prims`), so the
    // specs kept at a path are placed again.
    let paths: Vec<PathId> = layer
        .prims
        .keys()
        .copied()
        .filter(|path| root.is_prefix_of(store.paths().resolve(*path)))
        .collect();
    let mut removed_roots = Vec::new();
    let mut kept = Vec::new();
    for path in paths {
        let main = layer.prims.remove(&path);
        let branches = layer.variant_prims.remove(&path).unwrap_or_default();
        for spec in main.into_iter().chain(branches) {
            if !owned(store, &spec) {
                kept.push((path, spec));
            } else if path == prim {
                removed_roots.push(spec.outer_variant_sites().to_vec());
            }
        }
    }
    for (path, spec) in kept {
        layer.insert_prim(path, spec);
    }
    for sites in removed_roots {
        let loc = Loc::Prim { path: prim, sites };
        if let Some((parent, name)) = loc.parent(store.paths())
            && let Some(mut spec) = spec_at_mut(layer, &parent)
        {
            spec.children().retain(|child| *child != name);
        }
    }
}

/// The local specs that compose over the deleted namespace through an
/// internal arc, which go with it: for each composed prim with a node at
/// or beneath the deleted prim (for a property, at its prim) reached
/// through an arc of the root layer stack authored at the prim, at an
/// ancestor or in the local variants they select, that prim (or its
/// property), outside every branch and inside each of those variants. A
/// prim whose own arc reaches the deleted namespace keeps its specs: the
/// repairs drop the arc, and the prims beneath it that composed over the
/// deleted prim's children go, through the arc's nodes in their indexes.
///
/// OpenUSD: `PcpGatherDependentNamespaceEdits`, which deletes the specs at
/// every node from such a node up to the root
/// (`_PrimIndexDependentNodeEditProcessor::_AddSpecMoveEdits` with an
/// empty new path), and for an arc to the deleted prim itself the specs
/// over its children (`_ProcessDependentNodePathAtIntroductionChange`).
/// Nested and implied arcs are not followed. AOUSD Core §8, §10.
fn dependents(
    stage: &Stage,
    store: &dyn LayerStore,
    index: Option<&PreparationIndex>,
    object: TargetPath,
) -> Vec<(TargetPath, Vec<VariantSelectionSite>)> {
    let deleted = store.paths().resolve(object.prim_path()).clone();
    let mut prims: Vec<PathId> = match index {
        Some(index) => index.prims_with_nodes_beneath(object.prim_path()).collect(),
        None => stage.prim_paths().collect(),
    };
    sort::sort_unstable(&mut prims);
    prims.dedup();
    let mut out: Vec<(TargetPath, Vec<VariantSelectionSite>)> = Vec::new();
    for prim in prims {
        let composed = store.paths().resolve(prim);
        if deleted.is_prefix_of(composed) {
            continue;
        }
        let Some(graph) = stage.explain_prim_graph(prim) else {
            continue;
        };
        let Some(root) = graph.node(crate::NodeId::ROOT) else {
            continue;
        };
        for (_, node) in graph.nodes() {
            if matches!(
                node.arc_kind(),
                crate::ArcKind::Local | crate::ArcKind::Variants
            ) || node.layer_stack_identifier() != root.layer_stack_identifier()
                || node.site().has_variant_selections()
                || coordinated::implied_from_local_variant(graph, node)
                || node.is_implied()
                || !coordinated::authored_in_local_variants(graph, node)
            {
                continue;
            }
            let site = store.paths().resolve(node.site().prim_path());
            let dependent = match object {
                TargetPath::Property(property)
                    if node.site().prim_path() == property.prim_path() =>
                {
                    TargetPath::Property(PropertyPath::new(prim, property.property()))
                }
                // An arc authored at the prim itself is dropped instead.
                TargetPath::Prim(_)
                    if deleted.is_prefix_of(site)
                        && usize::from(node.namespace_depth()) < composed.depth() =>
                {
                    TargetPath::Prim(prim)
                }
                _ => continue,
            };
            let mut branches = coordinated::branches_above(store, graph, node, prim);
            branches.insert(0, Vec::new());
            for within in branches {
                let entry = (dependent, within);
                if !out.contains(&entry) {
                    out.push(entry);
                }
            }
        }
    }
    out
}

/// Removes the prim `object`, which composes from across an arc after its
/// local specs are gone, from namespace: a relocate already moving a prim
/// to it now moves that prim to `<>`, and otherwise `target` authors
/// `object` → `<>`.
///
/// Spec: AOUSD Core §10.3.2.6 (a relocate to an empty target removes its
/// source from namespace). OpenUSD authors the same relocate.
fn relocate_away(
    store: &dyn LayerStore,
    layers: &mut HashMap<LayerId, Layer>,
    stack: &[LayerId],
    target: LayerId,
    object: TargetPath,
) -> Result<(), NamespaceError> {
    let TargetPath::Prim(prim) = object else {
        return Err(NamespaceError::UnsupportedComposition(
            "a property with opinions across composition arcs cannot be deleted",
        ));
    };
    let root = store.paths().resolve(prim);
    if root.depth() == 1 {
        return Err(NamespaceError::UnsupportedComposition(
            "a root prim with opinions across composition arcs cannot be relocated away",
        ));
    }
    let mut moved_here = false;
    for id in stack {
        for relocate in layers
            .get_mut(id)
            .expect("edited stack layer")
            .relocates
            .iter_mut()
            .flatten()
        {
            let beneath =
                |path: PathId| path != prim && root.is_prefix_of(store.paths().resolve(path));
            if beneath(relocate.source) || relocate.target.is_some_and(beneath) {
                return Err(NamespaceError::UnsupportedComposition(
                    "deleting a prim with relocates beneath it is not supported",
                ));
            }
            if relocate.target == Some(prim) {
                relocate.target = None;
                moved_here = true;
            }
        }
    }
    if !moved_here {
        layers
            .get_mut(&target)
            .expect("edited stack layer")
            .relocates_mut()
            .push(Relocate {
                source: prim,
                target: None,
            });
    }
    Ok(())
}

/// Whether the authored path `path` names the deleted `object`, or for a
/// prim anything beneath it.
///
/// As `_PathListOpNamespaceEdit` (`pxr/usd/usd/namespaceEditor.cpp`) drops
/// a list op's item, by `SdfPath::HasPrefix`: a relational attribute or
/// mapper (`/A.b[/C].d`) goes with its owning prim or property, and is
/// kept when the deleted object is named only inside its brackets.
fn names(store: &dyn LayerStore, object: TargetPath, path: TargetPath) -> bool {
    has_prefix(store, path, object)
}

/// Drops every authored path in `layer` that names the deleted `object`,
/// but those in the variants no composed prim index selects (`dormant`),
/// which OpenUSD leaves naming it ([`Dormant`]).
fn repair(
    store: &mut dyn LayerStore,
    layer: &mut Layer,
    object: TargetPath,
    dormant: &Dormant,
) -> Result<(), NamespaceError> {
    let author = layer.id;
    // In path order, so the first spec that cannot be repaired is always
    // the one reported.
    let mut paths: Vec<PathId> = layer
        .prims
        .keys()
        .chain(layer.variant_prims.keys())
        .copied()
        .collect();
    sort::sort_unstable(&mut paths);
    paths.dedup();
    for path in paths {
        if let Some(spec) = layer.prims.get_mut(&path) {
            repair_prim(store, spec, path, author, object, dormant)?;
        }
        for spec in layer.variant_prims.get_mut(&path).into_iter().flatten() {
            repair_prim(store, spec, path, author, object, dormant)?;
        }
    }
    let root = store_root_path(store);
    repair_fields(store, &mut layer.metadata, root, object)?;
    if let TargetPath::Prim(_) = object
        && let Some(path) = layer.default_prim_path(store.tokens())
    {
        let default = store.paths().intern(path);
        if names(store, object, TargetPath::Prim(default)) {
            layer.default_prim = None;
        }
    }
    Ok(())
}

fn repair_prim(
    store: &mut dyn LayerStore,
    spec: &mut PrimSpec,
    path: PathId,
    author: LayerId,
    object: TargetPath,
    dormant: &Dormant,
) -> Result<(), NamespaceError> {
    let outer = spec.outer_variant_sites().to_vec();
    if !outer.is_empty() && dormant.contains(path, &outer) {
        return Ok(());
    }
    // A spec authoring no inherits or specializes has nothing in them to
    // repair: it lends empty ones rather than allocating its rare fields.
    let (mut no_inherits, mut no_specializes) = (SparseListOp::new(), SparseListOp::new());
    let (inherits, specializes) = match spec.rare.as_deref_mut() {
        Some(rare) => (&mut rare.inherits, &mut rare.specializes),
        None => (&mut no_inherits, &mut no_specializes),
    };
    repair_owner(
        store,
        Owner {
            properties: &mut spec.properties,
            fields: &mut spec.fields,
            references: &mut spec.references,
            payloads: &mut spec.payloads,
            inherits,
            specializes,
        },
        path,
        author,
        object,
    )?;
    let mut sites = outer;
    match spec.authored_rare_mut() {
        Some(rare) => repair_variant_sets(
            store,
            &mut rare.variant_sets,
            path,
            author,
            object,
            (&mut sites, dormant),
        ),
        None => Ok(()),
    }
}

/// Repairs the variants in `sets`, held by the prim at `path` inside the
/// branches `sites`, but those `dormant` lists.
fn repair_variant_sets(
    store: &mut dyn LayerStore,
    sets: &mut VariantSets,
    path: PathId,
    author: LayerId,
    object: TargetPath,
    (sites, dormant): (&mut Vec<VariantSelectionSite>, &Dormant),
) -> Result<(), NamespaceError> {
    for (set, spec) in sets.iter_mut() {
        for (name, variant) in spec.variants.iter_mut() {
            sites.push(VariantSelectionSite {
                host_path: path,
                set: *set,
                variant: *name,
            });
            if dormant.contains(path, sites) {
                sites.pop();
                continue;
            }
            repair_owner(
                store,
                Owner {
                    properties: &mut variant.properties,
                    fields: &mut variant.fields,
                    references: &mut variant.references,
                    payloads: &mut variant.payloads,
                    inherits: &mut variant.inherits,
                    specializes: &mut variant.specializes,
                },
                path,
                author,
                object,
            )?;
            repair_variant_sets(
                store,
                &mut variant.variant_sets,
                path,
                author,
                object,
                (sites, dormant),
            )?;
            sites.pop();
        }
    }
    Ok(())
}

/// The members of a prim or variant spec that hold paths.
struct Owner<'a> {
    properties: &'a mut Vec<PropertyEntry>,
    fields: &'a mut Vec<FieldEntry>,
    references: &'a mut SparseListOp<Reference>,
    payloads: &'a mut SparseListOp<Reference>,
    inherits: &'a mut SparseListOp<PathId>,
    specializes: &'a mut SparseListOp<PathId>,
}

fn repair_owner(
    store: &mut dyn LayerStore,
    owner: Owner<'_>,
    path: PathId,
    author: LayerId,
    object: TargetPath,
) -> Result<(), NamespaceError> {
    for entry in owner.properties.iter_mut() {
        let spec = Arc::make_mut(&mut entry.spec);
        repair_path_default(store, spec, object, None);
        if let Some(targets) = &mut spec.targets {
            retain(targets, |target| !names(store, object, *target));
        }
        if let Some(value) = &mut spec.default {
            repair_value(store, value, path, object)?;
        }
        if let Some(samples) = spec.time_samples.get_mut() {
            for (_, value) in samples {
                repair_value(store, value, path, object)?;
            }
        }
        repair_fields(store, spec.metadata.make_mut(), path, object)?;
    }
    repair_fields(store, owner.fields, path, object)?;
    if let TargetPath::Prim(_) = object {
        for paths in [owner.inherits, owner.specializes] {
            if let Some(paths) = paths.get_mut() {
                retain(paths, |p| !names(store, object, TargetPath::Prim(*p)));
            }
        }
        for arcs in [owner.references, owner.payloads] {
            let Some(arcs) = arcs.get_mut() else {
                continue;
            };
            if arcs.items().any(Reference::is_expression) {
                return Err(NamespaceError::UnsupportedComposition(
                    "reference/payload asset expressions require resolved dependency editing",
                ));
            }
            // An internal arc names a path of its own layer stack.
            retain(arcs, |r| {
                let internal = r.layer == author && r.asset.is_none();
                !(internal
                    && matches!(r.target, ReferenceTarget::Prim(p) if names(store, object, TargetPath::Prim(p))))
            });
        }
    }
    Ok(())
}

fn retain<T>(list: &mut ListOp<T>, mut keep: impl FnMut(&T) -> bool) {
    for items in list.lists_mut() {
        items.retain(&mut keep);
    }
}

fn repair_fields(
    store: &mut dyn LayerStore,
    fields: &mut [FieldEntry],
    anchor: PathId,
    object: TargetPath,
) -> Result<(), NamespaceError> {
    for field in fields {
        match &mut field.value {
            FieldValue::Value(value) => repair_value(store, value, anchor, object)?,
            FieldValue::PathListOp(paths) => {
                retain(paths, |path| !names(store, object, *path));
            }
            _ => {}
        }
    }
    Ok(())
}

fn repair_value(
    store: &mut dyn LayerStore,
    value: &mut Value,
    anchor: PathId,
    object: TargetPath,
) -> Result<(), NamespaceError> {
    match value {
        Value::PathExpression(text) => {
            let expression = crate::PathExpression::parse(text).map_err(|_| {
                NamespaceError::UnsupportedComposition(
                    "invalid authored path expression prevents namespace editing",
                )
            })?;
            let property = object
                .property_path()
                .map(|p| store.tokens().resolve(p.property()));
            let edited = expression.remove_namespace(
                &path_names(store, object.prim_path()),
                property,
                &path_names(store, anchor),
            );
            if edited != expression {
                *text = Arc::from(edited.lossless_text());
            }
        }
        Value::Array(values) => {
            for value in values {
                repair_value(store, value, anchor, object)?;
            }
        }
        Value::Dictionary(values) => {
            for (_, value) in values {
                repair_value(store, value, anchor, object)?;
            }
        }
        _ => {}
    }
    Ok(())
}
