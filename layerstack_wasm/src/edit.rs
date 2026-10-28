// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Edits as the wrapper sends them, and their lowering to one
//! [`Transaction`].
//!
//! The wrapper fills an [`EditBatch`] with one push per edit of the
//! `Edit[]` it was given, paths and names as text and values as
//! [`ValueInput`]s in numbered slots, and hands the batch to `apply`.
//! Nothing is checked while pushing: [`lower`] checks every edit against
//! the store and the composed stage, and each failure names the index of
//! its edit. Each edit lowers to the [`Transaction`] edit of the OpenUSD
//! authoring call it is named after, so its semantics are the
//! transaction's.
//!
//! Spec: AOUSD Core §7 (scene description), §12.4 (list ops).

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use layerstack::{
    EditTarget, FieldValue, InMemoryStore, LayerId, LayerOffset, ListOp, ListPosition, PathId,
    PropertyKind, PropertyPath, PropertySpec, PropertyType, Reference, ReferenceSource,
    ReferenceTarget, Specifier, StageView, Transaction, Value, Variability,
};
use wasm_bindgen::prelude::*;

use crate::{
    array::ArrayHandle,
    error::Error,
    value::{self, DictionaryEntry, InputData, ValueInput},
};

/// One edit of an [`EditBatch`], as the wrapper sent it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EditInput {
    /// `UsdStage::DefinePrim`, `OverridePrim` or `CreateClassPrim` by
    /// `specifier` (`def`, `over`, `class`).
    DefinePrim {
        path: String,
        specifier: String,
        type_name: Option<String>,
    },
    /// `UsdStage::RemovePrim` in the target layer: its spec and everything
    /// beneath it.
    RemovePrim { path: String },
    /// `UsdPrim::RemoveProperty` in the target layer.
    RemoveProperty { path: String },
    /// `UsdPrim::CreateAttribute`, with an optional default: a spec the
    /// target layer has is kept, and a new one copies the schema
    /// definition or composed declaration when there is one
    /// ([`ensure_property_spec`]).
    CreateAttribute {
        path: String,
        type_name: String,
        uniform: bool,
        custom: bool,
        value: Option<u32>,
    },
    /// `UsdAttribute::Set` at the default time or at a time code.
    SetValue {
        path: String,
        time: Option<f64>,
        value: u32,
    },
    /// `UsdAttribute::ClearAtTime`: the default, or one time sample.
    ClearValue { path: String, time: Option<f64> },
    /// `UsdAttribute::Block`.
    BlockValue { path: String },
    /// `UsdRelationship::SetTargets`.
    SetTargets { path: String, targets: Vec<String> },
    /// `UsdReferences::AddReference` / `UsdPayloads::AddPayload`.
    AddArc {
        payload: bool,
        path: String,
        asset: Option<String>,
        prim_path: Option<String>,
        position: String,
        offset: f64,
        scale: f64,
    },
    /// `UsdReferences::RemoveReference` / `UsdPayloads::RemovePayload`.
    RemoveArc {
        payload: bool,
        path: String,
        asset: Option<String>,
        prim_path: Option<String>,
    },
    /// `UsdVariantSet::SetVariantSelection`, or `ClearVariantSelection`
    /// for `None`.
    SetVariantSelection {
        path: String,
        set: String,
        variant: Option<String>,
    },
    /// `UsdPrim::SetActive`.
    SetActive { path: String, active: bool },
    /// `UsdPrim::SetInstanceable`.
    SetInstanceable { path: String, instanceable: bool },
    /// `UsdModelAPI::SetKind`.
    SetKind { path: String, kind: String },
    /// `UsdObject::SetMetadata` on a prim or property.
    SetMetadata {
        path: String,
        key: String,
        value: u32,
    },
    /// `UsdObject::ClearMetadata` on a prim or property.
    ClearMetadata { path: String, key: String },
}

impl EditInput {
    /// The prim or property path the edit is about.
    pub(crate) fn path(&self) -> &str {
        match self {
            Self::DefinePrim { path, .. }
            | Self::RemovePrim { path }
            | Self::RemoveProperty { path }
            | Self::CreateAttribute { path, .. }
            | Self::SetValue { path, .. }
            | Self::ClearValue { path, .. }
            | Self::BlockValue { path }
            | Self::SetTargets { path, .. }
            | Self::AddArc { path, .. }
            | Self::RemoveArc { path, .. }
            | Self::SetVariantSelection { path, .. }
            | Self::SetActive { path, .. }
            | Self::SetInstanceable { path, .. }
            | Self::SetKind { path, .. }
            | Self::SetMetadata { path, .. }
            | Self::ClearMetadata { path, .. } => path,
        }
    }
}

/// The edits of one `apply`, filled by the wrapper: the native object
/// behind the `Edit[]` it lowers.
///
/// Each push appends one edit; values go into numbered slots first
/// (`valueNumbers` and its siblings return the slot), which the edits that
/// take a value name. Pushing checks nothing; `Stage.apply` checks every
/// edit and names the index of the one that fails.
#[wasm_bindgen]
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditBatch {
    pub(crate) edits: Vec<EditInput>,
    pub(crate) values: Vec<ValueInput>,
}

impl EditBatch {
    fn value(&mut self, type_name: String, data: InputData) -> u32 {
        self.values.push(ValueInput { type_name, data });
        u32::try_from(self.values.len() - 1).unwrap_or(u32::MAX)
    }
}

#[wasm_bindgen]
impl EditBatch {
    /// An empty batch.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of edits pushed.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn length(&self) -> u32 {
        u32::try_from(self.edits.len()).unwrap_or(u32::MAX)
    }

    /// A value of `type_name` from numbers; returns its slot.
    #[wasm_bindgen(js_name = valueNumbers)]
    pub fn value_numbers(&mut self, type_name: String, data: Vec<f64>) -> u32 {
        self.value(type_name, InputData::Numbers(data))
    }

    /// A value of `type_name` from a `Float32Array`; returns its slot.
    #[wasm_bindgen(js_name = valueFloats)]
    pub fn value_floats(&mut self, type_name: String, data: Vec<f32>) -> u32 {
        self.value(type_name, InputData::Floats(data))
    }

    /// A value of `type_name` from `bigint`s; returns its slot.
    #[wasm_bindgen(js_name = valueInt64)]
    pub fn value_int64(&mut self, type_name: String, data: Vec<i64>) -> u32 {
        self.value(type_name, InputData::Int64(data))
    }

    /// A value of `type_name` from unsigned `bigint`s; returns its slot.
    #[wasm_bindgen(js_name = valueUInt64)]
    pub fn value_uint64(&mut self, type_name: String, data: Vec<u64>) -> u32 {
        self.value(type_name, InputData::UInt64(data))
    }

    /// A value of a half type from raw half float bits; returns its slot.
    #[wasm_bindgen(js_name = valueHalfBits)]
    pub fn value_half_bits(&mut self, type_name: String, data: Vec<u16>) -> u32 {
        self.value(type_name, InputData::HalfBits(data))
    }

    /// A value of a text type from strings; returns its slot.
    #[wasm_bindgen(js_name = valueText)]
    pub fn value_text(&mut self, type_name: String, data: Vec<String>) -> u32 {
        self.value(type_name, InputData::Text(data))
    }

    /// A `dictionary` value whose entry `keys[i]` maps to the value in slot
    /// `slots[i]`, filled before it; returns its slot. A key without a slot
    /// (`keys` longer than `slots`) is refused when the batch is applied.
    #[wasm_bindgen(js_name = valueDictionary)]
    pub fn value_dictionary(
        &mut self,
        type_name: String,
        keys: Vec<String>,
        slots: Vec<u32>,
    ) -> u32 {
        let entries = keys
            .into_iter()
            .enumerate()
            .map(|(index, key)| DictionaryEntry {
                key,
                slot: slots.get(index).copied().unwrap_or(u32::MAX),
            })
            .collect();
        self.value(type_name, InputData::Dictionary(entries))
    }

    /// Fills a value slot with a pinned array, shared rather than copied
    /// when `type_name`'s storage is the array's; returns the slot.
    #[wasm_bindgen(js_name = valueArray)]
    pub fn value_array(&mut self, type_name: String, handle: &ArrayHandle) -> u32 {
        self.value(type_name, InputData::Shared(handle.array().clone()))
    }

    /// Pushes a `definePrim` edit.
    #[wasm_bindgen(js_name = definePrim)]
    pub fn define_prim(&mut self, path: String, specifier: String, type_name: Option<String>) {
        self.edits.push(EditInput::DefinePrim {
            path,
            specifier,
            type_name,
        });
    }

    /// Pushes a `removePrim` edit.
    #[wasm_bindgen(js_name = removePrim)]
    pub fn remove_prim(&mut self, path: String) {
        self.edits.push(EditInput::RemovePrim { path });
    }

    /// Pushes a `removeProperty` edit.
    #[wasm_bindgen(js_name = removeProperty)]
    pub fn remove_property(&mut self, path: String) {
        self.edits.push(EditInput::RemoveProperty { path });
    }

    /// Pushes a `createAttribute` edit.
    #[wasm_bindgen(js_name = createAttribute)]
    pub fn create_attribute(
        &mut self,
        path: String,
        type_name: String,
        uniform: bool,
        custom: bool,
        value: Option<u32>,
    ) {
        self.edits.push(EditInput::CreateAttribute {
            path,
            type_name,
            uniform,
            custom,
            value,
        });
    }

    /// Pushes a `setValue` edit.
    #[wasm_bindgen(js_name = setValue)]
    pub fn set_value(&mut self, path: String, time: Option<f64>, value: u32) {
        self.edits.push(EditInput::SetValue { path, time, value });
    }

    /// Pushes a `clearValue` edit.
    #[wasm_bindgen(js_name = clearValue)]
    pub fn clear_value(&mut self, path: String, time: Option<f64>) {
        self.edits.push(EditInput::ClearValue { path, time });
    }

    /// Pushes a `blockValue` edit.
    #[wasm_bindgen(js_name = blockValue)]
    pub fn block_value(&mut self, path: String) {
        self.edits.push(EditInput::BlockValue { path });
    }

    /// Pushes a `setTargets` edit.
    #[wasm_bindgen(js_name = setTargets)]
    pub fn set_targets(&mut self, path: String, targets: Vec<String>) {
        self.edits.push(EditInput::SetTargets { path, targets });
    }

    /// Pushes an `addPayload` edit when `payload`, else an
    /// `addReference` edit.
    #[wasm_bindgen(js_name = addArc)]
    pub fn add_arc(
        &mut self,
        payload: bool,
        path: String,
        asset: Option<String>,
        prim_path: Option<String>,
        position: String,
        offset: f64,
        scale: f64,
    ) {
        self.edits.push(EditInput::AddArc {
            payload,
            path,
            asset,
            prim_path,
            position,
            offset,
            scale,
        });
    }

    /// Pushes a `removePayload` edit when `payload`, else a
    /// `removeReference` edit.
    #[wasm_bindgen(js_name = removeArc)]
    pub fn remove_arc(
        &mut self,
        payload: bool,
        path: String,
        asset: Option<String>,
        prim_path: Option<String>,
    ) {
        self.edits.push(EditInput::RemoveArc {
            payload,
            path,
            asset,
            prim_path,
        });
    }

    /// Pushes a `setVariantSelection` edit.
    #[wasm_bindgen(js_name = setVariantSelection)]
    pub fn set_variant_selection(&mut self, path: String, set: String, variant: Option<String>) {
        self.edits
            .push(EditInput::SetVariantSelection { path, set, variant });
    }

    /// Pushes a `setActive` edit.
    #[wasm_bindgen(js_name = setActive)]
    pub fn set_active(&mut self, path: String, active: bool) {
        self.edits.push(EditInput::SetActive { path, active });
    }

    /// Pushes a `setInstanceable` edit.
    #[wasm_bindgen(js_name = setInstanceable)]
    pub fn set_instanceable(&mut self, path: String, instanceable: bool) {
        self.edits
            .push(EditInput::SetInstanceable { path, instanceable });
    }

    /// Pushes a `setKind` edit.
    #[wasm_bindgen(js_name = setKind)]
    pub fn set_kind(&mut self, path: String, kind: String) {
        self.edits.push(EditInput::SetKind { path, kind });
    }

    /// Pushes a `setMetadata` edit.
    #[wasm_bindgen(js_name = setMetadata)]
    pub fn set_metadata(&mut self, path: String, key: String, value: u32) {
        self.edits.push(EditInput::SetMetadata { path, key, value });
    }

    /// Pushes a `clearMetadata` edit.
    #[wasm_bindgen(js_name = clearMetadata)]
    pub fn clear_metadata(&mut self, path: String, key: String) {
        self.edits.push(EditInput::ClearMetadata { path, key });
    }
}

/// A batch lowered to one transaction, with the edit each of its
/// transaction edits came from.
#[derive(Debug)]
pub(crate) struct Lowered {
    pub(crate) transaction: Transaction,
    /// For each transaction edit, the index of the batch edit it lowers.
    origins: Vec<u32>,
}

impl Lowered {
    /// The index of the batch edit transaction edit `op` lowers.
    pub(crate) fn edit_of(&self, op: usize) -> Option<u32> {
        self.origins.get(op).copied()
    }
}

/// What lowering reads: the store, the composed stage before the batch,
/// and the target layer with the layer stack it is in.
pub(crate) struct Scope<'a> {
    pub(crate) store: &'a InMemoryStore,
    pub(crate) view: StageView<'a>,
    pub(crate) target: LayerId,
    /// The stage's layer stack, strongest first.
    pub(crate) stack: &'a [LayerId],
}

/// Lowers `batch` to one transaction whose edits are addressed through
/// [`EditTarget::for_layer`] of the target layer, so stage paths are
/// the layer's own and time codes are stage times.
///
/// # Errors
///
/// The first edit that does not lower, with its index: `path.invalid`
/// for a malformed path or a prim path where a property path is asked
/// for (and the reverse), `argument.invalid` for an unknown specifier,
/// list position or value slot, and the `value.*` errors of
/// [`value::decode`].
pub(crate) fn lower(batch: &EditBatch, scope: &Scope<'_>) -> Result<Lowered, Error> {
    let mut lowered = Lowered {
        transaction: Transaction::new(),
        origins: Vec::new(),
    };
    // Property specs this batch creates, so a later edit does not stamp
    // them again.
    let mut ensured = Vec::new();
    for (index, edit) in batch.edits.iter().enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        lower_one(edit, batch, scope, &mut lowered.transaction, &mut ensured)
            .map_err(|error| error.at_edit(index).at_path(edit.path()))?;
        let ops = lowered.transaction.len();
        lowered.origins.resize(ops, index);
    }
    Ok(lowered)
}

fn lower_one(
    edit: &EditInput,
    batch: &EditBatch,
    scope: &Scope<'_>,
    txn: &mut Transaction,
    ensured: &mut Vec<String>,
) -> Result<(), Error> {
    let store = scope.store;
    let at = EditTarget::for_layer(scope.target);
    let prim = |path: &str| -> Result<PathId, Error> {
        if path.contains('.') {
            return Err(Error::new(
                "path.invalid",
                "not a prim path, such as /World/Rock",
            ));
        }
        store
            .try_path(path)
            .map_err(|error| Error::new("path.invalid", format!("not a prim path: {error}")))
    };
    let property = |path: &str| {
        store.try_property_path(path).map_err(|error| {
            Error::new(
                "path.invalid",
                format!("not a property path, such as /World/Rock.size: {error}"),
            )
        })
    };
    let value = |slot: u32| value::decode_slot(&batch.values, slot, store.tokens());
    match edit {
        EditInput::DefinePrim {
            path,
            specifier,
            type_name,
        } => {
            let path_id = prim(path)?;
            let specifier = match specifier.as_str() {
                "def" => Specifier::Def,
                "over" => Specifier::Over,
                "class" => Specifier::Class,
                other => {
                    return Err(Error::new(
                        "argument.invalid",
                        format!("`{other}` is not a specifier: def, over or class"),
                    ));
                }
            };
            define_prim(scope, txn, path, path_id, specifier, type_name.as_deref());
        }
        EditInput::RemovePrim { path } => {
            txn.remove_spec(at.prim(prim(path)?));
        }
        EditInput::RemoveProperty { path } => {
            txn.remove_spec(at.property(property(path)?));
        }
        EditInput::CreateAttribute {
            path,
            type_name,
            uniform,
            custom,
            value: slot,
        } => {
            let property = property(path)?;
            let ty = PropertyType::parse(type_name).map_err(|_| {
                Error::new(
                    "value.unknown_type",
                    format!("`{type_name}` is not a registered value type name"),
                )
            })?;
            let value = slot.map(value).transpose()?;
            if !ensure_property_spec(scope, txn, ensured, path, property) {
                let mut spec = PropertySpec::typed_attribute(ty);
                if *uniform {
                    spec = spec.uniform();
                }
                if *custom {
                    spec = spec.custom();
                }
                txn.create_property(at.property(property), spec);
                ensured.push(path.clone());
            }
            if let Some(value) = value {
                txn.set_default(at.property(property), value);
            }
        }
        EditInput::SetValue {
            path,
            time,
            value: slot,
        } => {
            let at = at.property(property(path)?);
            let value = value(*slot)?;
            match time {
                Some(time) => txn.set_time_sample(at, check_time(*time)?, value),
                None => txn.set_default(at, value),
            };
        }
        EditInput::ClearValue { path, time } => {
            let at = at.property(property(path)?);
            match time {
                Some(time) => txn.remove_time_sample(at, check_time(*time)?),
                None => txn.clear_default(at),
            };
        }
        EditInput::BlockValue { path } => {
            txn.block_attribute(at.property(property(path)?));
        }
        EditInput::SetTargets { path, targets } => {
            let at = at.property(property(path)?);
            let mut list = Vec::with_capacity(targets.len());
            for target in targets {
                list.push(store.try_target_path(target).map_err(|error| {
                    Error::new(
                        "path.invalid",
                        format!("`{target}` is not a target path: {error}"),
                    )
                })?);
            }
            txn.set_targets(
                at,
                ListOp {
                    explicit: Some(list),
                    ..ListOp::default()
                },
            );
        }
        EditInput::AddArc {
            payload,
            path,
            asset,
            prim_path,
            position,
            offset,
            scale,
        } => {
            let at_prim = at.prim(prim(path)?);
            let position = match position.as_str() {
                "frontOfPrepend" => ListPosition::FrontOfPrepend,
                "backOfPrepend" => ListPosition::BackOfPrepend,
                "frontOfAppend" => ListPosition::FrontOfAppend,
                "backOfAppend" => ListPosition::BackOfAppend,
                other => {
                    return Err(Error::new(
                        "argument.invalid",
                        format!(
                            "`{other}` is not a list position: frontOfPrepend, backOfPrepend, \
                             frontOfAppend or backOfAppend"
                        ),
                    ));
                }
            };
            if !(offset.is_finite() && scale.is_finite() && *scale > 0.0) {
                return Err(Error::new(
                    "argument.invalid",
                    "a layer offset needs a finite offset and a finite, positive scale",
                ));
            }
            let arc = arc(
                scope,
                asset.as_deref(),
                prim_path.as_deref(),
                LayerOffset {
                    offset: *offset,
                    scale: *scale,
                },
            )?;
            if *payload {
                txn.add_payload(at_prim, arc, position);
            } else {
                txn.add_reference(at_prim, arc, position);
            }
        }
        EditInput::RemoveArc {
            payload,
            path,
            asset,
            prim_path,
        } => {
            let path_id = prim(path)?;
            let built = arc(
                scope,
                asset.as_deref(),
                prim_path.as_deref(),
                LayerOffset::IDENTITY,
            )?;
            let arc = authored_arc(scope, path_id, *payload, &built).unwrap_or(built);
            if *payload {
                txn.remove_payload(at.prim(path_id), arc);
            } else {
                txn.remove_reference(at.prim(path_id), arc);
            }
        }
        EditInput::SetVariantSelection { path, set, variant } => {
            let at = at.prim(prim(path)?);
            let set = store.tokens().intern(set);
            // OpenUSD clears the selection for an empty variant name.
            let variant = variant
                .as_deref()
                .filter(|name| !name.is_empty())
                .map(|name| store.tokens().intern(name));
            txn.set_variant_selection(at, set, variant);
        }
        EditInput::SetActive { path, active } => {
            txn.set_active(at.prim(prim(path)?), *active);
        }
        EditInput::SetInstanceable { path, instanceable } => {
            txn.set_instanceable(at.prim(prim(path)?), *instanceable);
        }
        EditInput::SetKind { path, kind } => {
            let at = at.prim(prim(path)?);
            let key = store.tokens().intern("kind");
            let kind = store.tokens().intern(kind);
            txn.set_metadata(at, key, FieldValue::Value(Value::Token(kind)));
        }
        EditInput::SetMetadata {
            path,
            key,
            value: slot,
        } => {
            let value = value(*slot)?;
            let at = if path.contains('.') {
                let property = property(path)?;
                ensure_property_spec(scope, txn, ensured, path, property);
                at.property(property)
            } else {
                at.prim(prim(path)?)
            };
            let key = store.tokens().intern(key);
            txn.set_metadata(at, key, FieldValue::Value(value));
        }
        EditInput::ClearMetadata { path, key } => {
            let at = if path.contains('.') {
                at.property(property(path)?)
            } else {
                at.prim(prim(path)?)
            };
            let key = store.tokens().intern(key);
            txn.clear_metadata(at, key);
        }
    }
    Ok(())
}

/// Makes sure the target layer has a spec for the property at `path`, as
/// OpenUSD's `UsdStage::_CreatePropertySpecForEditing` does before it
/// authors one of the property's fields: a spec the layer (or an earlier
/// edit of the batch) has stays; otherwise one is stamped from the prim's
/// schema definition of the property (not `custom`), else from the stage's
/// composed declaration of it. Returns whether the spec exists or was
/// stamped; `false` when nothing defines or declares the property.
///
/// Spec: AOUSD Core §12.2.2–§12.2.4 (property declarations), §13.3.2.3
/// (the prim definition).
fn ensure_property_spec(
    scope: &Scope<'_>,
    txn: &mut Transaction,
    ensured: &mut Vec<String>,
    path: &str,
    property: PropertyPath,
) -> bool {
    let authored = scope
        .store
        .layer(scope.target)
        .is_some_and(|layer| layer.property(property).is_some());
    if authored || ensured.iter().any(|done| done == path) {
        return true;
    }
    let Some(composed) = scope.view.property(path) else {
        return false;
    };
    let (kind, type_name, variability, custom) = match composed.definition() {
        Some(definition) => (
            definition.kind,
            definition.type_name.clone(),
            definition.variability,
            false,
        ),
        None => {
            let Some(declared) = composed.declaration() else {
                return false;
            };
            (
                declared.kind,
                declared.type_name,
                declared.variability,
                declared.custom,
            )
        }
    };
    let mut spec = match (kind, type_name) {
        (PropertyKind::Relationship, _) => PropertySpec::relationship(),
        (PropertyKind::Attribute, Some(ty)) => PropertySpec::typed_attribute(ty),
        (PropertyKind::Attribute, None) => return false,
    };
    if variability == Variability::Uniform {
        spec = spec.uniform();
    }
    if custom {
        spec = spec.custom();
    }
    txn.create_property(EditTarget::for_layer(scope.target).property(property), spec);
    ensured.push(path.to_string());
    true
}

/// `time` as a time code, refusing a NaN or an infinity.
fn check_time(time: f64) -> Result<f64, Error> {
    if time.is_finite() {
        Ok(time)
    } else {
        Err(Error::new(
            "argument.invalid",
            "a time code must be a finite number",
        ))
    }
}

/// Lowers a define as OpenUSD's stage calls do, reading the composed
/// stage as it was before the batch:
///
/// - `def` (`UsdStage::DefinePrim`): nothing when the prim is defined
///   and `type_name` is absent or its type already; otherwise authors
///   `def` (and the type name, when given) on its spec, and `def` on each
///   ancestor that does not exist or is not defined.
/// - `over` (`UsdStage::OverridePrim`): nothing when the prim exists;
///   otherwise authors an `over` spec (ancestors as `over`s).
/// - `class` (`UsdStage::CreateClassPrim`): nothing when the prim exists
///   as a class; otherwise authors `class` on its spec.
///
/// A type name given with `over` or `class` is authored too.
///
/// Spec: AOUSD Core §7.6 (specifier), §12.2.1 (specifier resolution).
fn define_prim(
    scope: &Scope<'_>,
    txn: &mut Transaction,
    path: &str,
    path_id: PathId,
    specifier: Specifier,
    type_name: Option<&str>,
) {
    let at = EditTarget::for_layer(scope.target);
    let composed = scope.view.prim(path);
    let type_matches =
        |prim: &layerstack::Prim<'_>| type_name.is_none_or(|name| prim.type_name() == Some(name));
    let done = match specifier {
        Specifier::Def => composed.is_some_and(|prim| prim.is_defined() && type_matches(&prim)),
        Specifier::Over => composed.is_some() && type_name.is_none(),
        Specifier::Class => {
            composed.is_some_and(|prim| prim.specifier() == Specifier::Class && type_matches(&prim))
        }
    };
    if done {
        return;
    }
    if specifier == Specifier::Def {
        let mut ancestor = String::new();
        let names: Vec<&str> = path.split('/').filter(|name| !name.is_empty()).collect();
        for name in &names[..names.len().saturating_sub(1)] {
            ancestor.push('/');
            ancestor.push_str(name);
            let defined = scope
                .view
                .prim(ancestor.as_str())
                .is_some_and(|prim| prim.is_defined());
            if !defined {
                txn.set_specifier(at.prim(scope.store.path(&ancestor)), Specifier::Def);
            }
        }
    }
    let prim_at = at.prim(path_id);
    if specifier == Specifier::Over && composed.is_none() {
        txn.create_prim(prim_at.clone(), Specifier::Over, None);
    } else {
        txn.set_specifier(prim_at.clone(), specifier);
    }
    if let Some(name) = type_name {
        txn.set_type_name(prim_at, scope.store.tokens().intern(name));
    }
}

/// The reference or payload an add or remove names: an external arc to
/// `asset` resolved against the target layer, or an internal arc
/// without one; to `prim_path`, or to the default prim without one.
fn arc(
    scope: &Scope<'_>,
    asset: Option<&str>,
    prim_path: Option<&str>,
    offset: LayerOffset,
) -> Result<Reference, Error> {
    let target = match prim_path {
        Some(path) => ReferenceTarget::Prim(scope.store.try_path(path).map_err(|error| {
            Error::new(
                "path.invalid",
                format!("`{path}` is not an arc's prim path: {error}"),
            )
        })?),
        None => ReferenceTarget::DefaultPrim,
    };
    let mut arc = match asset.filter(|asset| !asset.is_empty()) {
        Some(asset) => Reference::deferred(scope.target, asset, target, offset),
        None => Reference::from_source(
            ReferenceSource::Layer {
                layer: scope.target,
                asset: None,
            },
            target,
        ),
    };
    arc.layer_offset = offset;
    Ok(arc)
}

/// The arc a layer of the stack authors on `path` that names what `wanted`
/// names (its asset path as authored and its target), as it is authored:
/// a removal must name the authored item, which records how the arc was
/// resolved, not only what was written.
fn authored_arc(
    scope: &Scope<'_>,
    path: PathId,
    payload: bool,
    wanted: &Reference,
) -> Option<Reference> {
    let asset = |arc: &Reference| -> Option<String> {
        match arc.source() {
            ReferenceSource::Layer { asset, .. } => asset.map(ToString::to_string),
            ReferenceSource::Asset { path, .. } | ReferenceSource::Unresolved { path } => {
                Some(path.to_string())
            }
            ReferenceSource::Expression { text, .. } => Some(text.to_string()),
        }
    };
    let wanted_asset = asset(wanted);
    let order = core::iter::once(scope.target).chain(
        scope
            .stack
            .iter()
            .copied()
            .filter(|layer| *layer != scope.target),
    );
    for layer in order {
        let Some(spec) = scope.store.layer(layer).and_then(|layer| layer.prim(path)) else {
            continue;
        };
        let list = if payload {
            &*spec.payloads
        } else {
            &*spec.references
        };
        let found = list
            .explicit
            .iter()
            .flatten()
            .chain(&list.prepend)
            .chain(&list.append)
            .chain(&list.add)
            .chain(&list.delete)
            .find(|arc| arc.target == wanted.target && asset(arc) == wanted_asset);
        if let Some(found) = found {
            return Some(found.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec, vec::Vec};

    use layerstack::LayerId;

    use super::*;
    use crate::{
        receipt::EditReceipt,
        stage::{BROWSER_LIMITS, Stage},
        wire::Wire,
    };

    const SCENE: &str = r#"#usda 1.0
(
    defaultPrim = "World"
)
def Xform "World" (
    variants = {
        string shade = "light"
    }
    prepend variantSets = "shade"
) {
    double size = 2
    double spin.timeSamples = {
        1: 10,
        2: 20,
    }
    rel lookAt = </World/Rock>

    def Sphere "Rock" {
    }

    variantSet "shade" = {
        "dark" (
            prepend references = @./dark.usda@</Dark>
        ) {
        }
        "light" {
            color3f tint = (1, 1, 1)
        }
    }
}

def "Spare" (
    prepend references = @./spare.usda@</Part>
) {
}
"#;
    const DARK: &str = "#usda 1.0\ndef \"Dark\" {\n    color3f tint = (0.1, 0.1, 0.1)\n}\n";
    const SPARE: &str = "#usda 1.0\ndef \"Part\" {\n    int count = 3\n}\n";

    fn open() -> Stage {
        let mut stage = Stage::open(
            "scene.usda",
            SCENE.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        stage
            .document_mut()
            .storage_mut()
            .supply("spare.usda", SPARE.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        stage
    }

    fn root(stage: &Stage) -> LayerId {
        stage.document().stage().stage().root_layer().unwrap()
    }

    /// The USDA text of layer `id`.
    fn text(stage: &Stage, id: LayerId) -> String {
        let store = stage.document().store();
        layerstack_usda::save::save_usda(store.layer(id).unwrap(), store.tokens(), store.paths())
            .unwrap()
    }

    /// Every composed prim with its fields, attribute values, samples,
    /// relationships, variant sets and kind, as text.
    fn snapshot(stage: &Stage) -> String {
        let mut out = String::new();
        let mut queue = vec![String::from("/")];
        while let Some(path) = queue.pop() {
            out.push_str(&format!("{path} {:?}\n", stage.prim(&path).unwrap()));
            if path != "/" {
                out.push_str(&format!(
                    "  variants {:?} kind {:?}\n",
                    stage.variant_sets(&path).unwrap(),
                    stage.metadata(&path, "kind").unwrap()
                ));
                for name in stage.attribute_names(&path).unwrap() {
                    let property = format!("{path}.{name}");
                    out.push_str(&format!(
                        "  .{name} {:?} {:?} {:?}\n",
                        stage.attribute(&property).unwrap(),
                        stage.get(&property, None).unwrap(),
                        stage.time_samples(&property).unwrap(),
                    ));
                }
                out.push_str(&format!(
                    "  lookAt {:?}\n",
                    stage.relationship(&format!("{path}.lookAt")).unwrap()
                ));
            }
            queue.extend(stage.children(&path).unwrap().into_iter().rev());
        }
        out
    }

    /// Applies `batch` to the root layer, checks `effect`, undoes it and
    /// checks the layer text and composed stage are exactly as before,
    /// then redoes it and checks `effect` again.
    fn round_trip(batch: EditBatch, effect: impl Fn(&Stage)) -> Stage {
        let mut stage = open();
        let target = root(&stage);
        let (text_before, before) = (text(&stage, target), snapshot(&stage));
        let mut batch = batch;
        let mut receipt = stage.apply(target.0, &mut batch).unwrap();
        assert!(batch.edits.is_empty(), "apply takes the edits");
        assert_ne!(
            (text(&stage, target), snapshot(&stage)),
            (text_before.clone(), before.clone()),
            "the edit changes something"
        );
        effect(&stage);
        let mut redo = stage.undo(&mut receipt).unwrap();
        assert_eq!(text(&stage, target), text_before, "undo restores the text");
        assert_eq!(snapshot(&stage), before, "undo restores the composition");
        assert!(!receipt.undoable(), "a receipt undoes once");
        assert_eq!(
            stage.undo(&mut receipt).unwrap_err().code,
            "receipt.released"
        );
        let _again: EditReceipt = stage.undo(&mut redo).unwrap();
        effect(&stage);
        stage
    }

    fn scalar(type_name: &str, value: Wire) -> Wire {
        Wire::List(vec![Wire::text(type_name), Wire::text("s"), value])
    }

    fn numbers(batch: &mut EditBatch, type_name: &str, data: &[f64]) -> u32 {
        batch.value_numbers(type_name.to_string(), data.to_vec())
    }

    fn prim_field(stage: &Stage, path: &str, index: usize) -> Wire {
        stage.prim(path).unwrap().as_list().unwrap()[index].clone()
    }

    #[test]
    fn define_prim_defines_it_and_its_ancestors() {
        let mut batch = EditBatch::new();
        batch.define_prim("/Garden/Moss".into(), "def".into(), Some("Cube".into()));
        let stage = round_trip(batch, |stage| {
            assert_eq!(prim_field(stage, "/Garden/Moss", 0), Wire::text("Cube"));
            assert_eq!(prim_field(stage, "/Garden", 1), Wire::text("def"));
        });
        assert!(text(&stage, root(&stage)).contains("def \"Garden\""));
    }

    #[test]
    fn define_prim_on_a_defined_prim_retypes_or_does_nothing() {
        let mut stage = open();
        let target = root(&stage);
        let before = text(&stage, target);
        let mut batch = EditBatch::new();
        batch.define_prim("/World/Rock".into(), "def".into(), None);
        batch.define_prim("/World/Rock".into(), "def".into(), Some("Sphere".into()));
        let receipt = stage.apply(target.0, &mut batch).unwrap();
        assert_eq!(
            text(&stage, target),
            before,
            "UsdStage::DefinePrim finds it defined"
        );
        assert!(receipt.undoable(), "an empty transaction still undoes");
        let mut retype = EditBatch::new();
        retype.define_prim("/World/Rock".into(), "def".into(), Some("Cube".into()));
        round_trip(retype, |stage| {
            assert_eq!(prim_field(stage, "/World/Rock", 0), Wire::text("Cube"));
        });
        let mut over = EditBatch::new();
        over.define_prim("/Elsewhere".into(), "over".into(), None);
        round_trip(over, |stage| {
            assert_eq!(prim_field(stage, "/Elsewhere", 1), Wire::text("over"));
        });
        let mut class = EditBatch::new();
        class.define_prim("/Base".into(), "class".into(), None);
        round_trip(class, |stage| {
            assert_eq!(prim_field(stage, "/Base", 4), Wire::Bool(true), "abstract");
        });
    }

    /// The root layer's specs after each define, as C++ OpenUSD 26.11's
    /// `UsdStage::DefinePrim`, `OverridePrim` and `CreateClassPrim` leave
    /// them (`.local/perf-evidence/wasm-edits/define_prim_oracle.py`):
    /// `-` for no spec, else the specifier and type name.
    #[test]
    fn define_prim_matches_the_stage_calls() {
        const DEFINES: &str = "#usda 1.0\ndef Xform \"World\" {\n    def Sphere \"Rock\" {\n    }\n}\nover \"Loose\" {\n    over \"Inner\" {\n    }\n}\n";
        /// One define: the edit, whether it changes the layer, and each
        /// spec after it.
        struct Case {
            path: &'static str,
            specifier: &'static str,
            type_name: Option<&'static str>,
            changes: bool,
            specs: &'static [(&'static str, &'static str)],
        }
        fn case(
            path: &'static str,
            specifier: &'static str,
            type_name: Option<&'static str>,
            changes: bool,
            specs: &'static [(&'static str, &'static str)],
        ) -> Case {
            Case {
                path,
                specifier,
                type_name,
                changes,
                specs,
            }
        }
        let cases = [
            case(
                "/Garden/Moss",
                "def",
                Some("Cube"),
                true,
                &[("/Garden", "def:"), ("/Garden/Moss", "def:Cube")],
            ),
            case(
                "/World/Rock",
                "def",
                None,
                false,
                &[("/World/Rock", "def:Sphere")],
            ),
            case(
                "/World/Rock",
                "def",
                Some("Sphere"),
                false,
                &[("/World/Rock", "def:Sphere")],
            ),
            case(
                "/World/Rock",
                "def",
                Some("Cube"),
                true,
                &[("/World/Rock", "def:Cube")],
            ),
            case(
                "/Loose/Inner/Leaf",
                "def",
                None,
                true,
                &[
                    ("/Loose", "def:"),
                    ("/Loose/Inner", "def:"),
                    ("/Loose/Inner/Leaf", "def:"),
                ],
            ),
            case(
                "/Elsewhere/Deep",
                "over",
                None,
                true,
                &[("/Elsewhere", "over:"), ("/Elsewhere/Deep", "over:")],
            ),
            case(
                "/World/Rock",
                "over",
                None,
                false,
                &[("/World/Rock", "def:Sphere")],
            ),
            case("/Base", "class", None, true, &[("/Base", "class:")]),
            case("/Loose", "class", None, true, &[("/Loose", "class:")]),
        ];
        for Case {
            path,
            specifier,
            type_name,
            changes,
            specs,
        } in cases
        {
            let mut stage = Stage::open(
                "defines.usda",
                DEFINES.as_bytes().to_vec(),
                false,
                false,
                BROWSER_LIMITS,
            )
            .unwrap();
            let target = root(&stage);
            let before = text(&stage, target);
            let mut batch = EditBatch::new();
            batch.define_prim(path.into(), specifier.into(), type_name.map(Into::into));
            let mut receipt = stage.apply(target.0, &mut batch).unwrap();
            assert_eq!(
                text(&stage, target) != before,
                changes,
                "{specifier} {path}"
            );
            let store = stage.document().store();
            for (spec_path, expected) in specs {
                let spec = store.layer(target).unwrap().prim(store.path(spec_path));
                let found = spec.map_or(String::from("-"), |spec| {
                    let specifier = match spec.specifier {
                        Some(Specifier::Def) => "def",
                        Some(Specifier::Over) | None => "over",
                        Some(Specifier::Class) => "class",
                    };
                    let type_name = spec
                        .type_name
                        .map_or("", |name| store.tokens().resolve(name));
                    format!("{specifier}:{type_name}")
                });
                assert_eq!(found, *expected, "{specifier} {path}: {spec_path}");
            }
            stage.undo(&mut receipt).unwrap();
            assert_eq!(text(&stage, target), before, "{specifier} {path} undoes");
        }
    }

    /// The session layer's attribute specs after each create, as C++
    /// OpenUSD 26.11's `UsdPrim::CreateAttribute` and `SetMetadata` leave
    /// them (`.local/perf-evidence/wasm-edits/create_attribute_oracle.py`).
    #[test]
    fn create_attribute_matches_the_stage_calls() {
        const ROCK: &str =
            "#usda 1.0\ndef Sphere \"Rock\" {\n    custom uniform float glow = 1\n}\n";
        type Push = fn(&mut EditBatch);
        let cases: [(&str, Push, &str); 7] = [
            (
                "new",
                |b| b.create_attribute("/Rock.height".into(), "float".into(), false, true, None),
                "/Rock.height=custom varying float",
            ),
            (
                "new not custom uniform",
                |b| b.create_attribute("/Rock.height".into(), "float".into(), true, false, None),
                "/Rock.height=uniform float",
            ),
            (
                "schema-defined, other type",
                |b| b.create_attribute("/Rock.radius".into(), "float".into(), false, true, None),
                "/Rock.radius=varying double",
            ),
            (
                "declared weaker, other type",
                |b| b.create_attribute("/Rock.glow".into(), "double".into(), false, false, None),
                "/Rock.glow=custom uniform float",
            ),
            (
                "twice",
                |b| {
                    b.create_attribute("/Rock.height".into(), "float".into(), false, true, None);
                    b.create_attribute("/Rock.height".into(), "double".into(), false, true, None);
                },
                "/Rock.height=custom varying float",
            ),
            (
                "metadata on schema property",
                |b| {
                    let hidden = b.value_numbers("bool".into(), vec![1.0]);
                    b.set_metadata("/Rock.radius".into(), "hidden".into(), hidden);
                },
                "/Rock.radius=varying double",
            ),
            (
                "metadata on declared property",
                |b| {
                    let hidden = b.value_numbers("bool".into(), vec![1.0]);
                    b.set_metadata("/Rock.glow".into(), "hidden".into(), hidden);
                },
                "/Rock.glow=custom uniform float",
            ),
        ];
        for (label, push, expected) in cases {
            let mut stage = Stage::open(
                "rock.usda",
                ROCK.as_bytes().to_vec(),
                false,
                true,
                BROWSER_LIMITS,
            )
            .unwrap();
            let session = stage.edit_layer().unwrap();
            let before = text(&stage, session);
            let mut batch = EditBatch::new();
            push(&mut batch);
            let mut receipt = stage.apply(session.0, &mut batch).unwrap();
            let (path, _) = expected.split_once('=').unwrap();
            let store = stage.document().store();
            let spec = store
                .layer(session)
                .unwrap()
                .property(store.property_path(path))
                .unwrap();
            let found = format!(
                "{path}={}{} {}",
                if spec.custom { "custom " } else { "" },
                match spec.variability {
                    Variability::Varying => "varying",
                    Variability::Uniform => "uniform",
                },
                spec.type_name.as_ref().unwrap().type_name(),
            );
            assert_eq!(found, expected, "{label}");
            stage.undo(&mut receipt).unwrap();
            assert_eq!(text(&stage, session), before, "{label} undoes");
        }
    }

    #[test]
    fn remove_prim_and_property() {
        let mut batch = EditBatch::new();
        batch.remove_prim("/World/Rock".into());
        round_trip(batch, |stage| {
            assert_eq!(stage.prim("/World/Rock").unwrap(), Wire::Null);
        });
        let mut batch = EditBatch::new();
        batch.remove_property("/World.size".into());
        round_trip(batch, |stage| {
            assert_eq!(stage.attribute("/World.size").unwrap(), Wire::Null);
        });
    }

    #[test]
    fn create_attribute_with_type_variability_custom_and_default() {
        let mut batch = EditBatch::new();
        let value = batch.value_floats("color3f".into(), vec![0.5, 0.25, 1.0]);
        batch.create_attribute(
            "/World/Rock.glow".into(),
            "color3f".into(),
            true,
            false,
            Some(value),
        );
        let stage = round_trip(batch, |stage| {
            let row = stage.attribute("/World/Rock.glow").unwrap();
            let row = row.as_list().unwrap();
            assert_eq!(row[0], Wire::text("color3f"));
            assert_eq!(row[1], Wire::text("uniform"));
            assert_eq!(row[2], Wire::Bool(false));
            assert_eq!(
                stage.get("/World/Rock.glow", None).unwrap(),
                Wire::List(vec![
                    Wire::text("color3f"),
                    Wire::text("t"),
                    Wire::List(vec![
                        Wire::Number(0.5),
                        Wire::Number(0.25),
                        Wire::Number(1.0)
                    ]),
                ])
            );
        });
        assert!(text(&stage, root(&stage)).contains("uniform color3f glow = (0.5, 0.25, 1)"));
    }

    #[test]
    fn set_value_at_the_default_and_at_a_time() {
        let mut batch = EditBatch::new();
        let three = numbers(&mut batch, "double", &[3.0]);
        let fifty = numbers(&mut batch, "double", &[50.0]);
        batch.set_value("/World.size".into(), None, three);
        batch.set_value("/World.spin".into(), Some(5.0), fifty);
        round_trip(batch, |stage| {
            assert_eq!(
                stage.get("/World.size", None).unwrap(),
                scalar("double", Wire::Number(3.0))
            );
            assert_eq!(stage.time_samples("/World.spin").unwrap(), [1.0, 2.0, 5.0]);
        });
    }

    #[test]
    fn set_value_of_an_array_creates_the_spec_with_the_composed_type() {
        let mut batch = EditBatch::new();
        let points = batch.value_floats("point3f[]".into(), vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        batch.create_attribute(
            "/World/Rock.points".into(),
            "point3f[]".into(),
            false,
            false,
            None,
        );
        batch.set_value("/World/Rock.points".into(), None, points);
        round_trip(batch, |stage| {
            let got = stage.get("/World/Rock.points", None).unwrap();
            assert_eq!(got.as_list().unwrap()[4], Wire::Number(2.0), "two points");
        });
    }

    #[test]
    fn clear_and_block_values() {
        let mut batch = EditBatch::new();
        batch.clear_value("/World.size".into(), None);
        batch.clear_value("/World.spin".into(), Some(1.0));
        round_trip(batch, |stage| {
            assert_eq!(stage.get("/World.size", None).unwrap(), Wire::Null);
            assert_eq!(stage.time_samples("/World.spin").unwrap(), [2.0]);
        });
        let mut batch = EditBatch::new();
        batch.block_value("/World.spin".into());
        round_trip(batch, |stage| {
            assert!(stage.time_samples("/World.spin").unwrap().is_empty());
            assert_eq!(stage.get("/World.spin", Some(1.0)).unwrap(), Wire::Null);
        });
    }

    #[test]
    fn set_relationship_targets() {
        let mut batch = EditBatch::new();
        batch.set_targets(
            "/World.lookAt".into(),
            vec!["/Spare".into(), "/World.size".into()],
        );
        round_trip(batch, |stage| {
            assert_eq!(
                stage.relationship("/World.lookAt").unwrap(),
                Wire::List(vec![
                    Wire::Bool(false),
                    Wire::List(vec![Wire::text("/Spare"), Wire::text("/World.size")]),
                ])
            );
        });
        let mut batch = EditBatch::new();
        batch.set_targets("/Spare.aim".into(), vec!["/World".into()]);
        round_trip(batch, |stage| {
            let rel = stage.relationship("/Spare.aim").unwrap();
            assert_eq!(
                rel.as_list().unwrap()[0],
                Wire::Bool(true),
                "created custom"
            );
        });
    }

    #[test]
    fn add_and_remove_references_and_payloads() {
        let mut batch = EditBatch::new();
        batch.add_arc(
            false,
            "/World/Rock".into(),
            Some("./spare.usda".into()),
            Some("/Part".into()),
            "frontOfPrepend".into(),
            0.0,
            1.0,
        );
        round_trip(batch, |stage| {
            assert_eq!(
                stage.get("/World/Rock.count", None).unwrap(),
                scalar("int", Wire::Number(3.0))
            );
        });
        let mut batch = EditBatch::new();
        batch.add_arc(
            true,
            "/World/Rock".into(),
            None,
            Some("/Spare".into()),
            "backOfAppend".into(),
            0.0,
            1.0,
        );
        let stage = round_trip(batch, |stage| {
            assert_eq!(
                stage.get("/World/Rock.count", None).unwrap(),
                scalar("int", Wire::Number(3.0)),
                "the internal payload reaches /Spare, which references the part"
            );
        });
        assert!(text(&stage, root(&stage)).contains("append payload = </Spare>"));
        let mut batch = EditBatch::new();
        batch.remove_arc(
            false,
            "/Spare".into(),
            Some("./spare.usda".into()),
            Some("/Part".into()),
        );
        round_trip(batch, |stage| {
            assert_eq!(stage.get("/Spare.count", None).unwrap(), Wire::Null);
        });
    }

    #[test]
    fn variant_selection_asks_for_the_assets_of_the_variant() {
        let mut stage = open();
        let target = root(&stage);
        let before = (text(&stage, target), snapshot(&stage));
        let mut batch = EditBatch::new();
        batch.set_variant_selection("/World".into(), "shade".into(), Some("dark".into()));
        let mut receipt = stage.apply(target.0, &mut batch).unwrap();
        let record = receipt.record.as_list().unwrap().to_vec();
        let wanted = record[4].as_list().unwrap();
        assert_eq!(
            wanted.len(),
            1,
            "the dark variant references an unsupplied asset"
        );
        assert_eq!(wanted[0].as_list().unwrap()[0].as_text(), Some("dark.usda"));
        assert_eq!(stage.get("/World.tint", None).unwrap(), Wire::Null);
        stage
            .document_mut()
            .storage_mut()
            .supply("dark.usda", DARK.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        let tint = stage.get("/World.tint", None).unwrap();
        assert_eq!(
            tint.as_list().unwrap()[2].as_list().unwrap()[0],
            Wire::Number(f64::from(0.1_f32))
        );
        stage.undo(&mut receipt).unwrap();
        assert_eq!(text(&stage, target), before.0);
        assert_eq!(snapshot(&stage), before.1);
        let mut clear = EditBatch::new();
        clear.set_variant_selection("/World".into(), "shade".into(), None);
        round_trip(clear, |stage| {
            let sets = stage.variant_sets("/World").unwrap();
            assert_eq!(sets.as_list().unwrap()[0].as_list().unwrap()[1], Wire::Null);
        });
    }

    #[test]
    fn active_instanceable_kind_and_metadata() {
        let mut batch = EditBatch::new();
        batch.set_active("/World/Rock".into(), false);
        round_trip(batch, |stage| {
            assert_eq!(prim_field(stage, "/World/Rock", 2), Wire::Bool(false));
        });
        let mut batch = EditBatch::new();
        batch.set_instanceable("/Spare".into(), true);
        round_trip(batch, |stage| {
            assert_eq!(
                prim_field(stage, "/Spare", 5),
                Wire::Bool(true),
                "an instance"
            );
        });
        let mut batch = EditBatch::new();
        batch.set_kind("/World".into(), "assembly".into());
        round_trip(batch, |stage| {
            assert_eq!(
                stage.metadata("/World", "kind").unwrap(),
                scalar("token", Wire::text("assembly"))
            );
        });
        let mut batch = EditBatch::new();
        let note = batch.value_text("string".into(), vec!["the main one".into()]);
        let hidden = numbers(&mut batch, "bool", &[1.0]);
        batch.set_metadata("/World".into(), "documentation".into(), note);
        batch.set_metadata("/World.size".into(), "hidden".into(), hidden);
        round_trip(batch, |stage| {
            assert_eq!(
                stage.metadata("/World", "documentation").unwrap(),
                scalar("string", Wire::text("the main one"))
            );
            assert_eq!(
                stage.metadata("/World.size", "hidden").unwrap(),
                scalar("bool", Wire::Bool(true))
            );
        });
        let mut batch = EditBatch::new();
        batch.clear_metadata("/World".into(), "defaultPrim".into());
        batch.clear_metadata("/World".into(), "kind".into());
        let mut stage = open();
        let target = root(&stage);
        let before = text(&stage, target);
        stage.apply(target.0, &mut batch).unwrap();
        assert_eq!(
            text(&stage, target),
            before,
            "clearing what is not authored"
        );
    }

    #[test]
    fn dictionary_metadata_is_authored_whole() {
        let mut batch = EditBatch::new();
        let name = batch.value_text("string".into(), vec!["moss".into()]);
        let count = numbers(&mut batch, "int", &[3.0]);
        let inner = batch.value_dictionary("dictionary".into(), vec!["count".into()], vec![count]);
        let data = batch.value_dictionary(
            "dictionary".into(),
            vec!["name".into(), "inner".into()],
            vec![name, inner],
        );
        batch.set_metadata("/World".into(), "customData".into(), data);
        round_trip(batch, |stage| {
            let entries = Wire::List(vec![
                Wire::List(vec![
                    Wire::text("inner"),
                    Wire::List(vec![
                        Wire::text("dictionary"),
                        Wire::text("d"),
                        Wire::List(vec![Wire::List(vec![
                            Wire::text("count"),
                            scalar("int", Wire::Number(3.0)),
                        ])]),
                    ]),
                ]),
                Wire::List(vec![
                    Wire::text("name"),
                    scalar("string", Wire::text("moss")),
                ]),
            ]);
            assert_eq!(
                stage.metadata("/World", "customData").unwrap(),
                Wire::List(vec![Wire::text("dictionary"), Wire::text("d"), entries])
            );
        });
        let mut stage = open();
        let target = root(&stage);
        let mut batch = EditBatch::new();
        let missing = batch.value_dictionary("dictionary".into(), vec!["a".into()], vec![]);
        batch.set_metadata("/World".into(), "customData".into(), missing);
        let error = stage.apply(target.0, &mut batch).unwrap_err();
        assert_eq!((error.code, error.edit), ("argument.invalid", Some(0)));
    }

    #[test]
    fn failures_name_the_edit_and_change_nothing() {
        let mut stage = open();
        let target = root(&stage);
        let before = text(&stage, target);
        let mut batch = EditBatch::new();
        let three = numbers(&mut batch, "double", &[3.0]);
        let wrong = numbers(&mut batch, "float", &[3.0]);
        batch.set_value("/World.size".into(), None, three);
        batch.set_value("/World.size".into(), Some(4.0), wrong);
        let error = stage.apply(target.0, &mut batch).unwrap_err();
        assert_eq!(error.code, "edit.rejected.type_mismatch");
        assert_eq!(error.edit, Some(1));
        assert_eq!(error.path.as_deref(), Some("/World.size"));
        assert_eq!(text(&stage, target), before, "nothing applied");

        let mut batch = EditBatch::new();
        batch.set_active("/World/Rock".into(), true);
        batch.remove_prim("/World.size".into());
        let error = stage.apply(target.0, &mut batch).unwrap_err();
        assert_eq!((error.code, error.edit), ("path.invalid", Some(1)));

        let mut batch = EditBatch::new();
        let text_value = batch.value_text("float".into(), vec!["x".into()]);
        batch.set_value("/World.size".into(), None, text_value);
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "value.kind"
        );
        let mut batch = EditBatch::new();
        batch.define_prim("/X".into(), "maybe".into(), None);
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "argument.invalid"
        );
        let mut batch = EditBatch::new();
        batch.set_value("/World.size".into(), None, 7);
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "argument.invalid",
            "no such slot"
        );
    }

    #[test]
    fn targets_must_be_editable_layers_of_the_stack() {
        let mut stage = open();
        let spare = stage
            .document()
            .store()
            .layer_ids()
            .into_iter()
            .find(|id| stage.document().identifier(*id) == Some("spare.usda"))
            .unwrap();
        let mut batch = EditBatch::new();
        batch.set_active("/Part".into(), false);
        assert_eq!(
            stage.apply(spare.0, &mut batch).unwrap_err().code,
            "layer.outside_stack"
        );
        assert_eq!(
            stage
                .apply(u64::MAX, &mut EditBatch::new())
                .unwrap_err()
                .code,
            "layer.unknown"
        );
    }

    #[test]
    fn a_session_layer_takes_edits_in_front_of_the_root() {
        let mut stage = Stage::open(
            "scene.usda",
            SCENE.as_bytes().to_vec(),
            false,
            true,
            BROWSER_LIMITS,
        )
        .unwrap();
        let session = stage.edit_layer().unwrap();
        assert_ne!(session, root(&stage));
        let root_text = text(&stage, root(&stage));
        let mut batch = EditBatch::new();
        let three = numbers(&mut batch, "double", &[3.0]);
        batch.set_value("/World.size".into(), None, three);
        stage.apply(session.0, &mut batch).unwrap();
        assert_eq!(
            stage.get("/World.size", None).unwrap(),
            scalar("double", Wire::Number(3.0))
        );
        assert_eq!(
            text(&stage, root(&stage)),
            root_text,
            "the root is untouched"
        );
        assert!(text(&stage, session).contains("double size = 3"));
        let layers = stage.layers();
        assert_eq!(layers.as_list().unwrap()[3], Wire::UInt64(session.0));
    }

    #[test]
    fn a_stale_undo_changes_nothing_and_keeps_its_inverse() {
        let mut stage = open();
        let target = root(&stage);
        let mut first = EditBatch::new();
        let three = numbers(&mut first, "double", &[3.0]);
        first.set_value("/World.size".into(), None, three);
        let mut receipt = stage.apply(target.0, &mut first).unwrap();
        let mut later = EditBatch::new();
        let four = numbers(&mut later, "double", &[4.0]);
        later.set_value("/World.size".into(), None, four);
        stage.apply(target.0, &mut later).unwrap();
        let before = text(&stage, target);
        let error = stage.undo(&mut receipt).unwrap_err();
        assert_eq!(error.code, "edit.stale");
        assert_eq!(text(&stage, target), before);
        assert!(receipt.undoable(), "a stale receipt keeps its inverse");

        let mut other = open();
        assert_eq!(
            other.undo(&mut receipt).unwrap_err().code,
            "receipt.affinity"
        );
    }

    #[test]
    fn changes_drain_in_order_after_each_call() {
        let mut stage = open();
        let loaded = stage.take_changes(true);
        assert_eq!(
            loaded.as_list().unwrap()[0].as_list().unwrap()[2],
            Wire::List(vec![Wire::text("/Spare")]),
            "loading the supplied reference resynced its prim"
        );
        assert_eq!(stage.take_changes(true), Wire::List(Vec::new()));
        let target = root(&stage);
        let mut batch = EditBatch::new();
        batch.define_prim("/Garden".into(), "def".into(), None);
        let three = numbers(&mut batch, "double", &[3.0]);
        batch.set_value("/World.size".into(), None, three);
        stage.apply(target.0, &mut batch).unwrap();
        let changes = stage.take_changes(true);
        let batches = changes.as_list().unwrap();
        assert_eq!(batches.len(), 1);
        let fields = batches[0].as_list().unwrap();
        assert_eq!(
            fields[0],
            Wire::List(vec![Wire::text("/Garden")]),
            "created"
        );
        assert!(
            fields[3].as_list().unwrap().contains(&Wire::text("/World"))
                || fields[2].as_list().unwrap().contains(&Wire::text("/World")),
            "{fields:?}"
        );
        assert_eq!(stage.take_changes(true), Wire::List(Vec::new()));
        let mut batch = EditBatch::new();
        batch.set_active("/Garden".into(), false);
        stage.apply(target.0, &mut batch).unwrap();
        assert_eq!(stage.take_changes(false), Wire::List(Vec::new()));
        assert_eq!(stage.take_changes(true), Wire::List(Vec::new()), "consumed");
    }
}
