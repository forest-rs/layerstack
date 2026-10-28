// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Variant set authoring: adding a variant set to a prim or variant spec
//! and listing it in its `variantSetNames`, and removing a variant set
//! spec with everything in it.
//!
//! A variant set composes only where a site lists it in
//! `variantSetNames`; its spec holds the variants. The two are authored
//! apart: adding a set creates its spec and lists it, as
//! `UsdVariantSets::AddVariantSet` does, while removing a set's spec
//! leaves the list as it is, as `SdfPrimSpec::RemoveVariantSet` does.
//!
//! Spec: AOUSD Core §7.3.6 (prim and variant specs contain variant set
//! specs), §7.6.2.3.5 (`variantSetNames`), §10.5 (variant selection).

use crate::sort;
use alloc::vec::Vec;

use super::{
    apply::{Journal, Raw, ensure, layer, remove_subtree, resolve},
    error::Rejection,
    prim_fields::{ListPosition, PrimValue, insert_item, prim_address},
    spec::{Loc, SpecRef, spec_at},
    target::Address,
};
use crate::{
    doc::{LayerId, LayerStore, VariantSetSpec},
    ident::is_identifier,
    interner::TokenId,
    listop::ListOp,
    spec_path::VariantSelectionSite,
};

/// The `variantSetNames` of the prim or variant spec `spec`.
fn names(spec: SpecRef<'_>) -> &ListOp<TokenId> {
    match spec {
        SpecRef::Prim(spec) => spec.variant_set_names(),
        SpecRef::Variant(spec) => &spec.variant_set_names,
    }
}

/// Writes `names` as the `variantSetNames` of the spec at `loc`, unless it
/// holds them already.
fn write_names(
    store: &mut dyn LayerStore,
    id: LayerId,
    loc: Loc,
    names: ListOp<TokenId>,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let current = spec_at(layer(store, id)?, &loc).map(|spec| self::names(spec).clone());
    if current.as_ref() == Some(&names) {
        return Ok(());
    }
    journal.run(
        store,
        Raw::PrimField {
            layer: id,
            loc,
            value: PrimValue::VariantSetNames(names),
        },
    )
}

/// Lists `set` at the front of the `variantSetNames` of the existing spec
/// at `holder`, as the list editor's `Prepend` does: at the front of the
/// explicit list when there is one, and otherwise at the front of the
/// prepended items, no longer deleted. An item already listed there moves
/// to the front.
///
/// OpenUSD: `SdfListEditorProxy::Prepend`, which `SdfCreatePrimInLayer`
/// calls for a variant set it creates (`_FindOrCreateVariantSpec` in
/// `pxr/usd/sdf/primSpec.cpp`).
pub(super) fn prepend_variant_set_name(
    store: &mut dyn LayerStore,
    id: LayerId,
    holder: Loc,
    set: TokenId,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let Some(spec) = spec_at(layer(store, id)?, &holder) else {
        return Err(Rejection::Diverged(holder.spec_path(store.paths())));
    };
    let mut list = names(spec).clone();
    let items = match &mut list.explicit {
        Some(explicit) => explicit,
        None => {
            list.delete.retain(|name| *name != set);
            &mut list.prepend
        }
    };
    items.retain(|name| *name != set);
    items.insert(0, set);
    write_names(store, id, holder, list, journal)
}

/// Adds the variant set `name` to the prim or variant spec at `at` (see
/// [`Transaction::add_variant_set`](super::Transaction::add_variant_set)).
pub(crate) fn add_variant_set(
    store: &mut dyn LayerStore,
    at: &Address,
    name: TokenId,
    position: ListPosition,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    if !is_identifier(store.tokens().resolve(name)) {
        return Err(Rejection::InvalidVariantSetName(name));
    }
    let loc = prim_address(store, at)?;
    let id = at.layer();
    ensure(store, id, &loc, journal)?;
    let Some(spec) = spec_at(layer(store, id)?, &loc) else {
        return Err(Rejection::Diverged(loc.spec_path(store.paths())));
    };
    let sets = spec.variant_sets();
    let (has_set, last) = (sets.contains_key(&name), sets.len());
    let mut list = names(spec).clone();
    if !has_set {
        // A new set spec goes after the others: `SdfVariantSetSpec::New`
        // appends it to `variantSetChildren` (`Sdf_ChildrenUtils::CreateSpec`,
        // `pxr/usd/sdf/childrenUtils.cpp`).
        journal.run(
            store,
            Raw::VariantSet {
                layer: id,
                host: loc.clone(),
                set: name,
                spec: Some(VariantSetSpec::default()),
                index: last,
            },
        )?;
    }
    insert_item(&mut list, name, position);
    write_names(store, id, loc, list, journal)
}

/// Removes the variant set `name` of the prim or variant spec at `at`
/// (see
/// [`Transaction::remove_variant_set`](super::Transaction::remove_variant_set)).
pub(crate) fn remove_variant_set(
    store: &mut dyn LayerStore,
    at: &Address,
    name: TokenId,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let (path, loc) = resolve(store, at)?;
    if path.property().is_some() {
        return Err(Rejection::NotAPrim(path));
    }
    let id = at.layer();
    let Some(spec) = spec_at(layer(store, id)?, &loc) else {
        return Ok(());
    };
    let Some(set) = spec.variant_sets().get(&name) else {
        return Ok(());
    };
    let mut variants: Vec<TokenId> = set.variants.keys().copied().collect();
    sort::sort_unstable(&mut variants);
    let host = loc.prim_path();
    for variant in variants {
        let mut sites = loc.sites().to_vec();
        sites.push(VariantSelectionSite {
            host_path: host,
            set: name,
            variant,
        });
        remove_subtree(store, id, &Loc::Variant { host, sites }, journal)?;
    }
    journal.run(
        store,
        Raw::VariantSet {
            layer: id,
            host: loc,
            set: name,
            spec: None,
            index: 0,
        },
    )
}
