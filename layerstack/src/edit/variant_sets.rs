// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Variant set authoring: listing a variant set in the `variantSetNames`
//! of the spec holding it.
//!
//! A variant set composes only where a site lists it in
//! `variantSetNames`; its spec holds the variants. The two are authored
//! apart, and creating a set's spec lists it only where OpenUSD's
//! authoring calls do.
//!
//! Spec: AOUSD Core §7.3.6 (prim and variant specs contain variant set
//! specs), §7.6.2.3.5 (`variantSetNames`), §10.5 (variant selection).

use super::{
    apply::{Journal, Raw, layer},
    error::Rejection,
    prim_fields::PrimValue,
    spec::{Loc, SpecRef, spec_at},
};
use crate::{
    doc::{LayerId, LayerStore},
    interner::TokenId,
    listop::ListOp,
};

/// The `variantSetNames` of the prim or variant spec `spec`.
fn names(spec: SpecRef<'_>) -> &ListOp<TokenId> {
    match spec {
        SpecRef::Prim(spec) => &spec.variant_set_names,
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
