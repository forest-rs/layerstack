// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Copying a prim spec with everything beneath it, or a property spec, to
//! another path of the same or another layer.
//!
//! The copy replaces whatever the destination holds. Paths the copied
//! specs author that point into the copied namespace are remapped to the
//! destination, as a namespace move remaps them: relationship targets,
//! connections, inherits, specializes, internal arcs and path expressions.
//!
//! OpenUSD: `SdfCopySpec`.

use crate::sort;
use alloc::vec::Vec;

use super::{
    apply::{Journal, Raw, add_child, ensure, insert_prim_spec, layer, remove_subtree, resolve},
    error::Rejection,
    namespace::{Dormant, PathMove, move_specs, repair_prim},
    spec::{Loc, spec_at},
    target::Address,
};
use crate::{
    doc::{Layer, LayerId, LayerStore, PrimSpec, Reference, VariantSets},
    listop::ListOp,
    path::{PathId, PropertyPath, TargetPath},
    property::get_property,
    spec_path::SpecPath,
};

/// Copies the spec `from` addresses to `to` (see
/// [`Transaction::copy_spec`](super::Transaction::copy_spec)).
pub(crate) fn copy_spec(
    store: &mut dyn LayerStore,
    from: &Address,
    to: &Address,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let (source, _) = resolve(store, from)?;
    let (destination, _) = resolve(store, to)?;
    if source.has_variant_selections() || destination.has_variant_selections() {
        return Err(Rejection::VariantCopy);
    }
    match (source.property(), destination.property()) {
        (None, None) => copy_prim(
            store,
            (from.layer(), source.prim_path()),
            (to.layer(), destination.prim_path()),
            journal,
        ),
        (Some(a), Some(b)) => copy_property(
            store,
            (from.layer(), PropertyPath::new(source.prim_path(), a)),
            (to.layer(), PropertyPath::new(destination.prim_path(), b)),
            journal,
        ),
        _ => Err(Rejection::CopyBetweenKinds),
    }
}

/// The paths a copy remaps: `from` to `to`, with everything beneath.
fn remap(
    store: &mut dyn LayerStore,
    spec: &mut PrimSpec,
    path: PathId,
    author: LayerId,
    mapping: &PathMove,
) -> Result<(), Rejection> {
    // `LayerId::UNRESOLVED` names no layer: external arcs, which name
    // paths of their own asset, are left as they are.
    repair_prim(
        store,
        spec,
        path,
        author,
        LayerId::UNRESOLVED,
        Some(mapping),
        mapping,
        &Dormant::default(),
    )
    .map_err(|_| Rejection::UnsupportedCopy)
}

fn copy_prim(
    store: &mut dyn LayerStore,
    (from_layer, from): (LayerId, PathId),
    (to_layer, to): (LayerId, PathId),
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let paths = store.paths();
    let (source, destination) = (paths.resolve(from).clone(), paths.resolve(to).clone());
    if from_layer == to_layer
        && (source.is_prefix_of(&destination) || destination.is_prefix_of(&source))
    {
        return Err(Rejection::CopyIntoSource);
    }
    let src = layer(store, from_layer)?;
    let Some(root) = src
        .prims
        .get(&from)
        .filter(|s| s.outer_variant_sites().is_empty())
    else {
        return Err(Rejection::NoSuchSpec(SpecPath::from_prim_path(from)));
    };
    // The specs of the copied namespace: the prim's and every spec beneath
    // it, outside its ancestors' variant branches.
    let owned = |spec: &PrimSpec| {
        spec.outer_variant_sites()
            .iter()
            .all(|site| source.is_prefix_of(paths.resolve(site.host_path)))
    };
    let mut scratch = Layer::new(from_layer);
    scratch.insert_prim(from, root.clone());
    for (path, spec) in &src.prims {
        if *path != from && source.is_prefix_of(paths.resolve(*path)) && owned(spec) {
            scratch.prims.insert(*path, spec.clone());
        }
    }
    for (path, branches) in &src.variant_prims {
        if source.is_prefix_of(paths.resolve(*path)) {
            let kept: Vec<_> = branches.iter().filter(|s| owned(s)).cloned().collect();
            if !kept.is_empty() {
                scratch.variant_prims.insert(*path, kept);
            }
        }
    }
    // Move the copies to the destination's namespace, then remap the paths
    // they author, as a namespace move does.
    let parent_path = destination.parent().expect("a prim below the pseudo-root");
    let parent = store.paths().intern(parent_path);
    scratch.insert_prim(parent, PrimSpec::over());
    let from_spec = SpecPath::from_prim_path(from);
    let to_spec = SpecPath::from_prim_path(to);
    move_specs(store, &mut scratch, &from_spec, &to_spec)
        .map_err(|_| Rejection::UnsupportedCopy)?;
    scratch.prims.remove(&parent);
    let mapping = PathMove::for_copy(TargetPath::Prim(from), TargetPath::Prim(to));
    let mut copies: Vec<(PathId, PrimSpec)> = scratch
        .prims
        .into_iter()
        .chain(
            scratch
                .variant_prims
                .into_iter()
                .flat_map(|(path, specs)| specs.into_iter().map(move |spec| (path, spec))),
        )
        .collect();
    for (path, spec) in &mut copies {
        remap(store, spec, *path, from_layer, &mapping)?;
        if from_layer != to_layer {
            reanchor_internal_arcs(spec, from_layer, to_layer);
        }
    }
    sort::sort_by_key(&mut copies, |(path, spec)| {
        (
            store.paths().resolve(*path).depth(),
            spec.outer_variant_sites().len(),
        )
    });

    let destination_loc = Loc::Prim {
        path: to,
        sites: Vec::new(),
    };
    if spec_at(layer(store, to_layer)?, &destination_loc).is_some() {
        remove_subtree(store, to_layer, &destination_loc, journal)?;
    }
    let (parent_loc, name) = destination_loc
        .parent(store.paths())
        .expect("a prim below the pseudo-root");
    ensure(store, to_layer, &parent_loc, journal)?;
    for (path, spec) in copies {
        insert_prim_spec(store, to_layer, path, spec, journal)?;
    }
    add_child(store, to_layer, &parent_loc, name, journal)
}

/// Re-anchors the internal arcs of a spec copied to another layer there: an
/// internal arc targets the layer stack of the layer that authors it.
///
/// Spec: AOUSD Core §10.3.2.1 (internal references).
fn reanchor_internal_arcs(spec: &mut PrimSpec, from: LayerId, to: LayerId) {
    fn arcs(list: &mut ListOp<Reference>, from: LayerId, to: LayerId) {
        for item in list.lists_mut().flatten() {
            if item.layer == from && item.asset.is_none() {
                item.layer = to;
            }
        }
    }
    fn variants(sets: &mut VariantSets, from: LayerId, to: LayerId) {
        for variant in sets.values_mut().flat_map(|set| set.variants.values_mut()) {
            if let Some(op) = variant.references.get_mut() {
                arcs(op, from, to);
            }
            if let Some(op) = variant.payloads.get_mut() {
                arcs(op, from, to);
            }
            variants(&mut variant.variant_sets, from, to);
        }
    }
    if let Some(op) = spec.references.get_mut() {
        arcs(op, from, to);
    }
    if let Some(op) = spec.payloads.get_mut() {
        arcs(op, from, to);
    }
    if let Some(rare) = spec.authored_rare_mut() {
        variants(&mut rare.variant_sets, from, to);
    }
}

fn copy_property(
    store: &mut dyn LayerStore,
    (from_layer, from): (LayerId, PropertyPath),
    (to_layer, to): (LayerId, PropertyPath),
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let src = layer(store, from_layer)?;
    let Some(property) = src
        .prims
        .get(&from.prim_path())
        .and_then(|spec| get_property(&spec.properties, from.property()))
        .cloned()
    else {
        return Err(Rejection::NoSuchSpec(
            SpecPath::from_prim_path(from.prim_path()).with_property(from.property()),
        ));
    };
    // Remap the copy's paths as its prim's: targets of the source prim's
    // namespace follow it to the destination prim, as OpenUSD remaps them.
    let mut carrier = PrimSpec::over().with_property(to.property(), property);
    let mapping = PathMove::for_copy(
        TargetPath::Prim(from.prim_path()),
        TargetPath::Prim(to.prim_path()),
    );
    remap(store, &mut carrier, to.prim_path(), from_layer, &mapping)?;
    let property = carrier
        .remove_property(to.property())
        .expect("the carrier holds the copy");
    let loc = Loc::Prim {
        path: to.prim_path(),
        sites: Vec::new(),
    };
    ensure(store, to_layer, &loc, journal)?;
    let spec = spec_at(layer(store, to_layer)?, &loc).expect("ensured prim spec");
    let index = spec
        .properties()
        .iter()
        .position(|entry| entry.name == to.property());
    if let Some(index) = index {
        journal.run(
            store,
            Raw::Property {
                layer: to_layer,
                loc: loc.clone(),
                name: to.property(),
                spec: None,
                index,
            },
        )?;
    }
    let index = index.unwrap_or_else(|| {
        spec_at(store.layer(to_layer).expect("checked layer"), &loc)
            .expect("ensured prim spec")
            .properties()
            .len()
    });
    journal.run(
        store,
        Raw::Property {
            layer: to_layer,
            loc,
            name: to.property(),
            spec: Some(property),
            index,
        },
    )
}
