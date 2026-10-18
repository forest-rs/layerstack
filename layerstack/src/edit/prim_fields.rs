// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim spec fields the layer model keeps in dedicated members: the
//! specifier, the type name, `active`, `instanceable`, child and property
//! order, the composition arcs and `variantSetNames`.
//!
//! Each edit lowers to one [`Raw::PrimField`] step, which replaces the
//! whole field, so an inverse restores it exactly and guards it as one
//! slot. A list edit ([`ArcEdit`]) reads the authored list op, edits it as
//! OpenUSD's `UsdReferences`, `UsdPayloads`, `UsdInherits` and
//! `UsdSpecializes` do, and writes the result.

use alloc::vec::Vec;

use super::{
    apply::{Journal, Raw, ensure, field_index, layer, resolve},
    error::Rejection,
    spec::{Loc, SpecMut, SpecRef, spec_at},
    target::Address,
};
use crate::{
    doc::{FieldValue, LayerStore, Reference, ReferenceTarget, Specifier, Value},
    interner::TokenId,
    listop::ListOp,
    path::{PathId, TargetPath},
};

/// A prim spec field that a [`Transaction`](super::Transaction) authors
/// with a dedicated edit, rather than as metadata.
///
/// Spec: AOUSD Core §7.6.2 (prim spec fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrimField {
    /// `specifier`: `def`, `over` or `class`.
    Specifier,
    /// `typeName`.
    TypeName,
    /// `active`.
    Active,
    /// `instanceable`.
    Instanceable,
    /// `primOrder` (`reorder nameChildren`).
    PrimOrder,
    /// `propertyOrder` (`reorder properties`).
    PropertyOrder,
    /// The `references` list op.
    References,
    /// The `payload` list op.
    Payloads,
    /// The `inheritPaths` list op.
    Inherits,
    /// The `specializes` list op.
    Specializes,
    /// The `variantSetNames` list op (`variantSets` in USDA), of a prim
    /// spec or of a variant spec for the sets nested in it.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`).
    VariantSetNames,
}

/// Where an arc addition puts its item in the spec's list op.
///
/// When the list op is explicit, the item goes to the front of the
/// explicit list for either front position and to its back for either
/// back position. An item the list already holds moves to the position;
/// the other lists of the op are left as they are.
///
/// OpenUSD: `UsdListPosition`, whose default for `AddReference` and the
/// other additions is [`ListPosition::BackOfPrepend`].
///
/// Spec: AOUSD Core §12.4 (list ops).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ListPosition {
    /// The front of the prepended items.
    FrontOfPrepend,
    /// The back of the prepended items.
    #[default]
    BackOfPrepend,
    /// The front of the appended items.
    FrontOfAppend,
    /// The back of the appended items.
    BackOfAppend,
}

/// The authored value of a [`PrimField`]: `None` (or an empty list op)
/// when the field is not authored.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrimValue {
    Specifier(Option<Specifier>),
    TypeName(Option<TokenId>),
    Active(Option<bool>),
    Instanceable(Option<bool>),
    PrimOrder(Option<Vec<TokenId>>),
    PropertyOrder(Option<Vec<TokenId>>),
    References(ListOp<Reference>),
    Payloads(ListOp<Reference>),
    Inherits(ListOp<PathId>),
    Specializes(ListOp<PathId>),
    VariantSetNames(ListOp<TokenId>),
}

impl PrimValue {
    pub(crate) fn field(&self) -> PrimField {
        match self {
            Self::Specifier(_) => PrimField::Specifier,
            Self::TypeName(_) => PrimField::TypeName,
            Self::Active(_) => PrimField::Active,
            Self::Instanceable(_) => PrimField::Instanceable,
            Self::PrimOrder(_) => PrimField::PrimOrder,
            Self::PropertyOrder(_) => PrimField::PropertyOrder,
            Self::References(_) => PrimField::References,
            Self::Payloads(_) => PrimField::Payloads,
            Self::Inherits(_) => PrimField::Inherits,
            Self::Specializes(_) => PrimField::Specializes,
            Self::VariantSetNames(_) => PrimField::VariantSetNames,
        }
    }

    /// Whether this value leaves the field unauthored.
    pub(super) fn is_clear(&self) -> bool {
        match self {
            Self::Specifier(v) => v.is_none(),
            Self::TypeName(v) => v.is_none(),
            Self::Active(v) | Self::Instanceable(v) => v.is_none(),
            Self::PrimOrder(v) | Self::PropertyOrder(v) => v.is_none(),
            Self::References(v) | Self::Payloads(v) => *v == ListOp::default(),
            Self::Inherits(v) | Self::Specializes(v) => *v == ListOp::default(),
            Self::VariantSetNames(v) => *v == ListOp::default(),
        }
    }

    /// The field's unauthored value.
    fn cleared(field: PrimField) -> Self {
        match field {
            PrimField::Specifier => Self::Specifier(None),
            PrimField::TypeName => Self::TypeName(None),
            PrimField::Active => Self::Active(None),
            PrimField::Instanceable => Self::Instanceable(None),
            PrimField::PrimOrder => Self::PrimOrder(None),
            PrimField::PropertyOrder => Self::PropertyOrder(None),
            PrimField::References => Self::References(ListOp::default()),
            PrimField::Payloads => Self::Payloads(ListOp::default()),
            PrimField::Inherits => Self::Inherits(ListOp::default()),
            PrimField::Specializes => Self::Specializes(ListOp::default()),
            PrimField::VariantSetNames => Self::VariantSetNames(ListOp::default()),
        }
    }
}

/// One item of an arc list op, for [`ArcEdit`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ArcItem {
    Reference(Reference),
    Payload(Reference),
    Inherit(PathId),
    Specialize(PathId),
}

impl ArcItem {
    pub(super) fn field(&self) -> PrimField {
        match self {
            Self::Reference(_) => PrimField::References,
            Self::Payload(_) => PrimField::Payloads,
            Self::Inherit(_) => PrimField::Inherits,
            Self::Specialize(_) => PrimField::Specializes,
        }
    }
}

/// Adds an arc at a position, or removes it (`None`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ArcEdit {
    pub(crate) item: ArcItem,
    pub(crate) position: Option<ListPosition>,
}

/// Reads the value of `field` authored by `spec`; `None` for a field a
/// variant spec does not hold.
pub(super) fn read(
    store: &dyn LayerStore,
    spec: SpecRef<'_>,
    field: PrimField,
) -> Option<PrimValue> {
    let variant_bool = |fields: &[crate::doc::FieldEntry], name: &str| {
        let key = store.tokens().lookup(name);
        key.and_then(|key| match crate::doc::get_field(fields, &key) {
            Some(FieldValue::Value(Value::Bool(value))) => Some(*value),
            _ => None,
        })
    };
    Some(match (spec, field) {
        (SpecRef::Prim(spec), PrimField::Specifier) => PrimValue::Specifier(spec.specifier),
        (SpecRef::Prim(spec), PrimField::TypeName) => PrimValue::TypeName(spec.type_name),
        (SpecRef::Prim(spec), PrimField::Active) => PrimValue::Active(spec.active),
        (SpecRef::Prim(spec), PrimField::Instanceable) => {
            PrimValue::Instanceable(spec.instanceable)
        }
        (SpecRef::Prim(spec), PrimField::PrimOrder) => {
            PrimValue::PrimOrder(spec.prim_order.clone())
        }
        (SpecRef::Prim(spec), PrimField::PropertyOrder) => {
            PrimValue::PropertyOrder(spec.property_order.clone())
        }
        (SpecRef::Prim(spec), PrimField::References) => {
            PrimValue::References(spec.references.clone())
        }
        (SpecRef::Prim(spec), PrimField::Payloads) => PrimValue::Payloads(spec.payloads.clone()),
        (SpecRef::Prim(spec), PrimField::Inherits) => PrimValue::Inherits(spec.inherits.clone()),
        (SpecRef::Prim(spec), PrimField::Specializes) => {
            PrimValue::Specializes(spec.specializes.clone())
        }
        (SpecRef::Prim(spec), PrimField::VariantSetNames) => {
            PrimValue::VariantSetNames(spec.variant_set_names.clone())
        }
        (SpecRef::Variant(spec), PrimField::Active) => {
            PrimValue::Active(variant_bool(&spec.fields, "active"))
        }
        (SpecRef::Variant(spec), PrimField::Instanceable) => {
            PrimValue::Instanceable(variant_bool(&spec.fields, "instanceable"))
        }
        (SpecRef::Variant(spec), PrimField::PropertyOrder) => {
            PrimValue::PropertyOrder(spec.property_order.clone())
        }
        (SpecRef::Variant(spec), PrimField::References) => {
            PrimValue::References(spec.references.clone())
        }
        (SpecRef::Variant(spec), PrimField::Payloads) => PrimValue::Payloads(spec.payloads.clone()),
        (SpecRef::Variant(spec), PrimField::Inherits) => PrimValue::Inherits(spec.inherits.clone()),
        (SpecRef::Variant(spec), PrimField::Specializes) => {
            PrimValue::Specializes(spec.specializes.clone())
        }
        (SpecRef::Variant(spec), PrimField::VariantSetNames) => {
            PrimValue::VariantSetNames(spec.variant_set_names.clone())
        }
        (
            SpecRef::Variant(_),
            PrimField::Specifier | PrimField::TypeName | PrimField::PrimOrder,
        ) => return None,
    })
}

/// Stores `value` in `spec`, returning the value it replaces. Variant specs
/// keep `active` and `instanceable` among their fields, which
/// [`set_field`] writes with [`Raw::Field`] instead.
pub(crate) fn swap(spec: SpecMut<'_>, value: PrimValue) -> Result<PrimValue, PrimField> {
    use core::mem::replace;
    Ok(match (spec, value) {
        (SpecMut::Prim(spec), PrimValue::Specifier(v)) => {
            PrimValue::Specifier(replace(&mut spec.specifier, v))
        }
        (SpecMut::Prim(spec), PrimValue::TypeName(v)) => {
            PrimValue::TypeName(replace(&mut spec.type_name, v))
        }
        (SpecMut::Prim(spec), PrimValue::Active(v)) => {
            PrimValue::Active(replace(&mut spec.active, v))
        }
        (SpecMut::Prim(spec), PrimValue::Instanceable(v)) => {
            PrimValue::Instanceable(replace(&mut spec.instanceable, v))
        }
        (SpecMut::Prim(spec), PrimValue::PrimOrder(v)) => {
            PrimValue::PrimOrder(replace(&mut spec.prim_order, v))
        }
        (SpecMut::Prim(spec), PrimValue::PropertyOrder(v)) => {
            PrimValue::PropertyOrder(replace(&mut spec.property_order, v))
        }
        (SpecMut::Prim(spec), PrimValue::References(v)) => {
            PrimValue::References(replace(&mut spec.references, v))
        }
        (SpecMut::Prim(spec), PrimValue::Payloads(v)) => {
            PrimValue::Payloads(replace(&mut spec.payloads, v))
        }
        (SpecMut::Prim(spec), PrimValue::Inherits(v)) => {
            PrimValue::Inherits(replace(&mut spec.inherits, v))
        }
        (SpecMut::Prim(spec), PrimValue::Specializes(v)) => {
            PrimValue::Specializes(replace(&mut spec.specializes, v))
        }
        (SpecMut::Prim(spec), PrimValue::VariantSetNames(v)) => {
            PrimValue::VariantSetNames(replace(&mut spec.variant_set_names, v))
        }
        (SpecMut::Variant(spec), PrimValue::PropertyOrder(v)) => {
            PrimValue::PropertyOrder(replace(&mut spec.property_order, v))
        }
        (SpecMut::Variant(spec), PrimValue::References(v)) => {
            PrimValue::References(replace(&mut spec.references, v))
        }
        (SpecMut::Variant(spec), PrimValue::Payloads(v)) => {
            PrimValue::Payloads(replace(&mut spec.payloads, v))
        }
        (SpecMut::Variant(spec), PrimValue::Inherits(v)) => {
            PrimValue::Inherits(replace(&mut spec.inherits, v))
        }
        (SpecMut::Variant(spec), PrimValue::Specializes(v)) => {
            PrimValue::Specializes(replace(&mut spec.specializes, v))
        }
        (SpecMut::Variant(spec), PrimValue::VariantSetNames(v)) => {
            PrimValue::VariantSetNames(replace(&mut spec.variant_set_names, v))
        }
        (SpecMut::Variant(_), value) => return Err(value.field()),
    })
}

/// The spec path and location `at` addresses, rejecting a property path
/// and the pseudo-root, which holds no prim fields.
pub(super) fn prim_address(store: &mut dyn LayerStore, at: &Address) -> Result<Loc, Rejection> {
    let (path, loc) = resolve(store, at)?;
    if path.property().is_some() {
        return Err(Rejection::NotAPrim(path));
    }
    if matches!(loc, Loc::Prim { path, .. } if store.paths().resolve(path).depth() == 0) {
        return Err(Rejection::PseudoRoot);
    }
    Ok(loc)
}

/// Maps the stage path `path` of an inherit, a specialize or an internal
/// arc through `at`, as relationship targets are mapped. OpenUSD drops a
/// path its edit target does not map and reports success; rejecting it
/// tells the caller nothing was authored. An arc to the pseudo-root is
/// rejected, as OpenUSD rejects it.
///
/// Spec: AOUSD Core §8 (spec paths), §10 (arcs target prims).
fn map_arc_path(
    store: &mut dyn LayerStore,
    at: &Address,
    path: PathId,
) -> Result<PathId, Rejection> {
    if store.paths().resolve(path).depth() == 0 {
        return Err(Rejection::PseudoRootArc);
    }
    match at.map_target(TargetPath::Prim(path), store.paths_mut()) {
        Some(TargetPath::Prim(mapped)) => Ok(mapped),
        _ => Err(Rejection::UnmappableTarget(TargetPath::Prim(path))),
    }
}

/// Maps a reference or payload authored at `at`: an internal arc (no
/// asset, anchored at the addressed layer; see [`Reference::layer`])
/// names a stage path, mapped as [`map_arc_path`] maps it. An external
/// arc names a path in its asset and is authored as given, with its layer
/// offset, which is relative to the authoring layer.
///
/// OpenUSD: `UsdReferences::AddReference` maps the prim path of an
/// internal reference through the edit target.
fn map_reference(
    store: &mut dyn LayerStore,
    at: &Address,
    mut reference: Reference,
) -> Result<Reference, Rejection> {
    if let ReferenceTarget::Prim(path) = reference.target {
        if store.paths().resolve(path).depth() == 0 {
            return Err(Rejection::PseudoRootArc);
        }
        if reference.asset.is_none() && reference.layer == at.layer() {
            reference.target = ReferenceTarget::Prim(map_arc_path(store, at, path)?);
        }
    }
    Ok(reference)
}

/// Maps every item of a whole arc list (see [`map_arc_path`] and
/// [`map_reference`]).
fn map_value(
    store: &mut dyn LayerStore,
    at: &Address,
    value: PrimValue,
) -> Result<PrimValue, Rejection> {
    let paths = |store: &mut dyn LayerStore, mut list: ListOp<PathId>| {
        for items in list.lists_mut() {
            for item in items.iter_mut() {
                *item = map_arc_path(store, at, *item)?;
            }
        }
        Ok::<_, Rejection>(list)
    };
    let references = |store: &mut dyn LayerStore, mut list: ListOp<Reference>| {
        for items in list.lists_mut() {
            for item in items.iter_mut() {
                *item = map_reference(store, at, item.clone())?;
            }
        }
        Ok::<_, Rejection>(list)
    };
    Ok(match value {
        PrimValue::Inherits(list) => PrimValue::Inherits(paths(store, list)?),
        PrimValue::Specializes(list) => PrimValue::Specializes(paths(store, list)?),
        PrimValue::References(list) => PrimValue::References(references(store, list)?),
        PrimValue::Payloads(list) => PrimValue::Payloads(references(store, list)?),
        PrimValue::VariantSetNames(list) => PrimValue::VariantSetNames(unique_items(list)),
        other => other,
    })
}

/// Authors `value` on the prim or variant spec at `at`, creating a missing
/// spec as an `over` unless the value clears the field. Writing the value
/// the spec already holds does nothing.
pub(crate) fn set_field(
    store: &mut dyn LayerStore,
    at: &Address,
    value: PrimValue,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let loc = prim_address(store, at)?;
    let value = map_value(store, at, value)?;
    write(store, at, loc, value, journal)
}

/// Adds or removes one arc of the list op at `at`, as OpenUSD's list
/// editors do (see [`ListPosition`] and [`remove_item`]). Removing an arc
/// from a spec that does not exist authors its deletion on a new `over`.
pub(crate) fn edit_arc(
    store: &mut dyn LayerStore,
    at: &Address,
    edit: &ArcEdit,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let loc = prim_address(store, at)?;
    let field = edit.item.field();
    let current = match spec_at(layer(store, at.layer())?, &loc) {
        Some(spec) => read(store, spec, field).ok_or(Rejection::NotOnVariant(field))?,
        None => PrimValue::cleared(field),
    };
    let item = match edit.item.clone() {
        ArcItem::Reference(r) => ArcItem::Reference(map_reference(store, at, r)?),
        ArcItem::Payload(r) => ArcItem::Payload(map_reference(store, at, r)?),
        ArcItem::Inherit(p) => ArcItem::Inherit(map_arc_path(store, at, p)?),
        ArcItem::Specialize(p) => ArcItem::Specialize(map_arc_path(store, at, p)?),
    };
    let edited = match (current, item) {
        (PrimValue::References(list), ArcItem::Reference(item)) => {
            PrimValue::References(edit_list(list, item, edit.position))
        }
        (PrimValue::Payloads(list), ArcItem::Payload(item)) => {
            PrimValue::Payloads(edit_list(list, item, edit.position))
        }
        (PrimValue::Inherits(list), ArcItem::Inherit(item)) => {
            PrimValue::Inherits(edit_list(list, item, edit.position))
        }
        (PrimValue::Specializes(list), ArcItem::Specialize(item)) => {
            PrimValue::Specializes(edit_list(list, item, edit.position))
        }
        _ => unreachable!("an arc item reads its own list"),
    };
    write(store, at, loc, edited, journal)
}

fn edit_list<T: PartialEq>(
    mut list: ListOp<T>,
    item: T,
    position: Option<ListPosition>,
) -> ListOp<T> {
    match position {
        Some(position) => insert_item(&mut list, item, position),
        None => remove_item(&mut list, item),
    }
    list
}

/// Puts `item` at `position` (see [`ListPosition`]).
///
/// OpenUSD: `Usd_InsertListItem` in `pxr/usd/usd/listEditImpl.h`.
pub(super) fn insert_item<T: PartialEq>(list: &mut ListOp<T>, item: T, position: ListPosition) {
    let front = matches!(
        position,
        ListPosition::FrontOfPrepend | ListPosition::FrontOfAppend
    );
    let items = match (&mut list.explicit, position) {
        (Some(explicit), _) => explicit,
        (None, ListPosition::FrontOfPrepend | ListPosition::BackOfPrepend) => &mut list.prepend,
        (None, ListPosition::FrontOfAppend | ListPosition::BackOfAppend) => &mut list.append,
    };
    items.retain(|existing| *existing != item);
    if front {
        items.insert(0, item);
    } else {
        items.push(item);
    }
}

/// Removes `item`: from the explicit list when the op is explicit, and
/// otherwise from its prepended, appended and added items, deleting it as
/// well (once).
///
/// OpenUSD: `SdfListEditorProxy::Remove`, which `RemoveReference` and the
/// other removals call.
fn remove_item<T: PartialEq>(list: &mut ListOp<T>, item: T) {
    if let Some(explicit) = &mut list.explicit {
        explicit.retain(|existing| *existing != item);
        return;
    }
    list.prepend.retain(|existing| *existing != item);
    list.append.retain(|existing| *existing != item);
    list.add.retain(|existing| *existing != item);
    if !list.delete.contains(&item) {
        list.delete.push(item);
    }
}

/// `list` with the repeats of its explicit, prepended, deleted and appended
/// items removed, keeping the first of each but for the appended items,
/// where the last is kept; added and reordered items are kept as given.
///
/// OpenUSD: `SdfListOp::_MakeUnique`, which the `SdfListOp` setters call
/// (`pxr/usd/sdf/listOp.cpp`).
fn unique_items<T: PartialEq>(mut list: ListOp<T>) -> ListOp<T> {
    fn first<T: PartialEq>(items: &mut Vec<T>) {
        let mut kept: Vec<T> = Vec::with_capacity(items.len());
        for item in items.drain(..) {
            if !kept.contains(&item) {
                kept.push(item);
            }
        }
        *items = kept;
    }
    if let Some(explicit) = &mut list.explicit {
        first(explicit);
    }
    first(&mut list.prepend);
    first(&mut list.delete);
    list.append.reverse();
    first(&mut list.append);
    list.append.reverse();
    list
}

/// Removes the API schema `name` from the `apiSchemas` of the prim spec at
/// `at`, as [`remove_item`] removes a list item, creating the spec as an
/// `over` if needed.
///
/// OpenUSD: `UsdPrim::RemoveAppliedSchema`.
pub(crate) fn remove_applied_schema(
    store: &mut dyn LayerStore,
    at: &Address,
    name: TokenId,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let key = store.tokens_mut().intern("apiSchemas");
    let loc = prim_address(store, at)?;
    let id = at.layer();
    let current = spec_at(layer(store, id)?, &loc).map(|spec| {
        let list = match crate::doc::get_field(spec.fields(), &key) {
            Some(FieldValue::TokenListOp(list)) => list.clone(),
            _ => ListOp::default(),
        };
        (list, field_index(spec.fields(), key))
    });
    let (mut list, index) = current.unwrap_or_default();
    let before = list.clone();
    remove_item(&mut list, name);
    if list == before {
        return Ok(());
    }
    ensure(store, id, &loc, journal)?;
    journal.run(
        store,
        Raw::Field {
            layer: id,
            loc,
            property: None,
            key,
            value: Some(FieldValue::TokenListOp(list)),
            index,
        },
    )
}

/// Writes `value` to the spec at `loc`: a [`Raw::PrimField`] step, or for
/// a variant spec's `active` and `instanceable`, a [`Raw::Field`] step.
fn write(
    store: &mut dyn LayerStore,
    at: &Address,
    loc: Loc,
    value: PrimValue,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let id = at.layer();
    let field = value.field();
    let current = match spec_at(layer(store, id)?, &loc) {
        Some(spec) => Some(read(store, spec, field).ok_or(Rejection::NotOnVariant(field))?),
        None => None,
    };
    match &current {
        Some(current) if *current == value => return Ok(()),
        None if value.is_clear() => return Ok(()),
        None => ensure(store, id, &loc, journal)?,
        Some(_) => {}
    }
    if let Loc::Variant { .. } = loc
        && let PrimValue::Active(v) | PrimValue::Instanceable(v) = value
    {
        let name = if field == PrimField::Active {
            "active"
        } else {
            "instanceable"
        };
        let key = store.tokens_mut().intern(name);
        let spec = spec_at(layer(store, id)?, &loc).expect("ensured spec");
        let index = field_index(spec.fields(), key);
        return journal.run(
            store,
            Raw::Field {
                layer: id,
                loc,
                property: None,
                key,
                value: v.map(|v| FieldValue::Value(Value::Bool(v))),
                index,
            },
        );
    }
    journal.run(
        store,
        Raw::PrimField {
            layer: id,
            loc,
            value,
        },
    )
}
