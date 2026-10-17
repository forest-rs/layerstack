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
use crate::{HashMap, Relocate};

pub(super) fn prepare(
    stage: &Stage,
    store: &mut dyn LayerStore,
    target: &EditTarget,
    object: TargetPath,
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
        let id = store.paths_mut().intern(prim.clone());
        if stage.is_instance(id) && TargetPath::Prim(id) != object {
            return Err(NamespaceError::InstanceProxy);
        }
        ancestor = prim.parent();
    }
    if !stage.layer_stack().contains(&target.layer())
        || spec_target(&map(target, store, object)?) != object
    {
        return Err(NamespaceError::Unmappable);
    }
    let root = stage.root_layer().ok_or(NamespaceError::NoSuchObject)?;
    let stack = stage.layer_stack().to_vec();
    let mut edited = HashMap::new();
    for id in &stack {
        let mut layer = store
            .layer(*id)
            .ok_or(NamespaceError::NoSuchObject)?
            .clone();
        remove_specs(store, &mut layer, object);
        edited.insert(*id, layer);
    }
    let still_composed = {
        let mut overlay = Overlay {
            store: &mut *store,
            layers: &edited,
        };
        exists(&Stage::compose(&mut overlay, root, stage.options().clone()))
    };
    if still_composed {
        relocate_away(store, &mut edited, &stack, target.layer(), object)?;
    }
    for layer in edited.values_mut() {
        repair(store, layer, object)?;
    }
    let projected = {
        let mut overlay = Overlay {
            store: &mut *store,
            layers: &edited,
        };
        Stage::compose(&mut overlay, root, stage.options().clone())
    };
    if exists(&projected) {
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
        append_delta(original, layer, &mut transaction, &mut specs);
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
        source: object,
        destination: None,
        layers,
        specs,
        namespace_moves: Vec::new(),
        relocates,
    })
}

/// Removes `object`'s specs from `layer`: for a prim, every spec at or
/// beneath it outside its ancestors' variant branches (a branch of an
/// ancestor is an arc, so its specs stay) and its name from its parent's
/// children; for a property, the property of the prim's own spec.
fn remove_specs(store: &mut dyn LayerStore, layer: &mut Layer, object: TargetPath) {
    let prim = object.prim_path();
    if let TargetPath::Property(property) = object {
        if let Some(spec) = layer.prims.get_mut(&prim) {
            spec.remove_property(property.property());
        }
        return;
    }
    let root = store.paths().resolve(prim).clone();
    let owned = |store: &dyn LayerStore, spec: &PrimSpec| {
        spec.outer_variant_sites
            .iter()
            .all(|site| root.is_prefix_of(store.paths().resolve(site.host_path)))
    };
    let paths: Vec<PathId> = layer
        .prims
        .keys()
        .chain(layer.variant_prims.keys())
        .copied()
        .filter(|path| root.is_prefix_of(store.paths().resolve(*path)))
        .collect();
    let mut removed_roots = Vec::new();
    for path in paths {
        if layer
            .prims
            .get(&path)
            .is_some_and(|spec| owned(store, spec))
            && let Some(spec) = layer.prims.remove(&path)
            && path == prim
        {
            removed_roots.push(spec.outer_variant_sites);
        }
        if let Some(branches) = layer.variant_prims.get_mut(&path) {
            let mut kept = Vec::new();
            for spec in core::mem::take(branches) {
                if owned(store, &spec) {
                    if path == prim {
                        removed_roots.push(spec.outer_variant_sites);
                    }
                } else {
                    kept.push(spec);
                }
            }
            if kept.is_empty() {
                layer.variant_prims.remove(&path);
            } else {
                *branches = kept;
            }
        }
    }
    for sites in removed_roots {
        let loc = Loc::Prim { path: prim, sites };
        if let Some((parent, name)) = loc.parent(store.paths_mut())
            && let Some(mut spec) = spec_at_mut(layer, &parent)
        {
            spec.children().retain(|child| *child != name);
        }
    }
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
        for relocate in &mut layers.get_mut(id).expect("edited stack layer").relocates {
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
            .relocates
            .push(Relocate {
                source: prim,
                target: None,
            });
    }
    Ok(())
}

/// Whether the authored path `path` names the deleted `object`, or for a
/// prim anything beneath it.
fn names(store: &dyn LayerStore, object: TargetPath, path: TargetPath) -> bool {
    match object {
        TargetPath::Property(_) => path == object,
        TargetPath::Prim(prim) => store
            .paths()
            .resolve(prim)
            .is_prefix_of(store.paths().resolve(path.prim_path())),
    }
}

/// Drops every authored path in `layer` that names the deleted `object`.
fn repair(
    store: &mut dyn LayerStore,
    layer: &mut Layer,
    object: TargetPath,
) -> Result<(), NamespaceError> {
    let author = layer.id;
    for (path, spec) in &mut layer.prims {
        repair_prim(store, spec, *path, author, object)?;
    }
    for (path, branches) in &mut layer.variant_prims {
        for spec in branches {
            repair_prim(store, spec, *path, author, object)?;
        }
    }
    let root = store_root_path(store);
    repair_fields(store, &mut layer.metadata, root, object)?;
    if let TargetPath::Prim(_) = object
        && let Some(path) = layer.default_prim_path(store.tokens_mut())
    {
        let default = store.paths_mut().intern(path);
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
) -> Result<(), NamespaceError> {
    repair_owner(
        store,
        Owner {
            properties: &mut spec.properties,
            fields: &mut spec.fields,
            references: &mut spec.references,
            payloads: &mut spec.payloads,
            inherits: &mut spec.inherits,
            specializes: &mut spec.specializes,
        },
        path,
        author,
        object,
    )?;
    repair_variant_sets(store, &mut spec.variant_sets, path, author, object)
}

fn repair_variant_sets(
    store: &mut dyn LayerStore,
    sets: &mut HashMap<TokenId, VariantSetSpec>,
    path: PathId,
    author: LayerId,
    object: TargetPath,
) -> Result<(), NamespaceError> {
    for set in sets.values_mut() {
        for variant in set.variants.values_mut() {
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
            repair_variant_sets(store, &mut variant.variant_sets, path, author, object)?;
        }
    }
    Ok(())
}

/// The members of a prim or variant spec that hold paths.
struct Owner<'a> {
    properties: &'a mut Vec<PropertyEntry>,
    fields: &'a mut Vec<FieldEntry>,
    references: &'a mut ListOp<Reference>,
    payloads: &'a mut ListOp<Reference>,
    inherits: &'a mut ListOp<PathId>,
    specializes: &'a mut ListOp<PathId>,
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
        if let Some(targets) = &mut spec.targets {
            retain(targets, |target| !names(store, object, *target));
        }
        if let Some(value) = &mut spec.default {
            repair_value(store, value, path, object)?;
        }
        if let Some(samples) = &mut spec.time_samples {
            for (_, value) in samples.make_mut() {
                repair_value(store, value, path, object)?;
            }
        }
        repair_fields(store, spec.metadata.make_mut(), path, object)?;
    }
    repair_fields(store, owner.fields, path, object)?;
    if let TargetPath::Prim(_) = object {
        retain(owner.inherits, |p| {
            !names(store, object, TargetPath::Prim(*p))
        });
        retain(owner.specializes, |p| {
            !names(store, object, TargetPath::Prim(*p))
        });
        for arcs in [owner.references, owner.payloads] {
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
