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
    Address, EditTarget, FieldValue, InMemoryStore, Layer, LayerId, LayerOffset, ListOp,
    ListPosition, PathId, PathInterner, PropertyDeclaration, PropertyKind, PropertyPath,
    PropertyType, Reference, ReferenceSource, ReferenceTarget, Relocate, SpecPath, Specifier,
    StageView, SublayerEntry, TargetPath, TokenId, Transaction, Value, Variability,
};
use wasm_bindgen::prelude::*;

use crate::{
    array::ArrayHandle,
    error::Error,
    schema_edit::{self, Refusal, SchemaInput},
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
    /// ([`stamped_declaration`]).
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
    /// `UsdRelationship::AddTarget`, or `UsdAttribute::AddConnection`
    /// with `connection`.
    AddTarget {
        connection: bool,
        path: String,
        target: String,
        position: String,
    },
    /// `UsdRelationship::RemoveTarget`, or
    /// `UsdAttribute::RemoveConnection` with `connection`.
    RemoveTarget {
        connection: bool,
        path: String,
        target: String,
    },
    /// `UsdRelationship::ClearTargets`, or
    /// `UsdAttribute::ClearConnections` with `connection`.
    ClearTargets { connection: bool, path: String },
    /// `SdfCopySpec` of the spec at `path` to `to`, both in the target
    /// layer.
    CopySpec { path: String, to: String },
    /// `SdfLayer::InsertSubLayerPath` of the target's layer; at the end
    /// without an index.
    InsertSublayer {
        asset: String,
        index: Option<u32>,
        offset: f64,
        scale: f64,
    },
    /// `SdfLayer::RemoveSubLayerPath` of the target's layer.
    RemoveSublayer { index: u32 },
    /// `SdfLayer::SetSubLayerOffset` of the target's layer.
    SetSublayerOffset { index: u32, offset: f64, scale: f64 },
    /// `SdfLayer::SetSubLayerPaths` with `subLayerOffsets` of the
    /// target's layer: one entry per asset path.
    SetSublayers {
        assets: Vec<String>,
        offsets: Vec<f64>,
        scales: Vec<f64>,
    },
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
    /// `UsdInherits::AddInherit`, or `UsdSpecializes::AddSpecialize`
    /// with `specialize`.
    AddClassArc {
        specialize: bool,
        path: String,
        prim_path: String,
        position: String,
    },
    /// `UsdInherits::RemoveInherit`, or
    /// `UsdSpecializes::RemoveSpecialize` with `specialize`.
    RemoveClassArc {
        specialize: bool,
        path: String,
        prim_path: String,
    },
    /// `UsdVariantSets::AddVariantSet`.
    AddVariantSet {
        path: String,
        set: String,
        position: String,
    },
    /// `SdfPrimSpec::RemoveVariantSet` in the target layer.
    RemoveVariantSet { path: String, set: String },
    /// `UsdVariantSet::AddVariant`: the set, as `AddVariantSet` adds it,
    /// and its variant spec.
    AddVariant {
        path: String,
        set: String,
        variant: String,
        position: String,
    },
    /// `UsdPrim::SetSpecifier`.
    SetSpecifier { path: String, specifier: String },
    /// `UsdPrim::SetTypeName`, or `ClearTypeName` for `None`.
    SetTypeName {
        path: String,
        type_name: Option<String>,
    },
    /// `UsdPrim::SetChildrenReorder`, or `ClearChildrenReorder` for
    /// `None`; on the pseudo-root, `SdfLayer::SetRootPrimOrder`.
    SetChildrenReorder {
        path: String,
        order: Option<Vec<String>>,
    },
    /// `UsdPrim::SetPropertyOrder`, or `ClearPropertyOrder` for `None`.
    SetPropertyOrder {
        path: String,
        order: Option<Vec<String>>,
    },
    /// `SdfLayer::SetRelocates` of the target's layer, each source to the
    /// target at its index (an empty one for `<>`).
    SetRelocates {
        sources: Vec<String>,
        targets: Vec<String>,
    },
    /// `SdfLayer::ClearRelocates` of the target's layer.
    ClearRelocates,
    /// `UsdPrim::ClearActive`.
    ClearActive { path: String },
    /// `UsdPrim::ClearInstanceable`.
    ClearInstanceable { path: String },
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
    /// `Transaction::set_layer_metadata` of the target's layer.
    SetLayerMetadata { key: String, value: u32 },
    /// `Transaction::clear_layer_metadata` of the target's layer.
    ClearLayerMetadata { key: String },
    /// A schema-shaped edit, lowered through `layerstack_schemas`' edit
    /// handles ([`crate::schema_edit`]).
    Schema(SchemaInput),
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
            | Self::AddTarget { path, .. }
            | Self::RemoveTarget { path, .. }
            | Self::ClearTargets { path, .. }
            | Self::CopySpec { path, .. }
            | Self::AddArc { path, .. }
            | Self::RemoveArc { path, .. }
            | Self::AddClassArc { path, .. }
            | Self::RemoveClassArc { path, .. }
            | Self::AddVariantSet { path, .. }
            | Self::RemoveVariantSet { path, .. }
            | Self::AddVariant { path, .. }
            | Self::SetSpecifier { path, .. }
            | Self::SetTypeName { path, .. }
            | Self::ClearActive { path }
            | Self::ClearInstanceable { path }
            | Self::SetChildrenReorder { path, .. }
            | Self::SetPropertyOrder { path, .. }
            | Self::SetVariantSelection { path, .. }
            | Self::SetActive { path, .. }
            | Self::SetInstanceable { path, .. }
            | Self::SetKind { path, .. }
            | Self::SetMetadata { path, .. }
            | Self::ClearMetadata { path, .. } => path,
            // Layer metadata and sublayers are the pseudo-root spec's.
            Self::SetLayerMetadata { .. }
            | Self::ClearLayerMetadata { .. }
            | Self::InsertSublayer { .. }
            | Self::RemoveSublayer { .. }
            | Self::SetSublayerOffset { .. }
            | Self::SetSublayers { .. }
            | Self::SetRelocates { .. }
            | Self::ClearRelocates => "/",
            Self::Schema(input) => input.path(),
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

    /// Pushes an `addConnection` edit when `connection`, else an
    /// `addTarget` edit.
    #[wasm_bindgen(js_name = addTarget)]
    pub fn add_target(&mut self, connection: bool, path: String, target: String, position: String) {
        self.edits.push(EditInput::AddTarget {
            connection,
            path,
            target,
            position,
        });
    }

    /// Pushes a `removeConnection` edit when `connection`, else a
    /// `removeTarget` edit.
    #[wasm_bindgen(js_name = removeTarget)]
    pub fn remove_target(&mut self, connection: bool, path: String, target: String) {
        self.edits.push(EditInput::RemoveTarget {
            connection,
            path,
            target,
        });
    }

    /// Pushes a `clearConnections` edit when `connection`, else a
    /// `clearTargets` edit.
    #[wasm_bindgen(js_name = clearTargets)]
    pub fn clear_targets(&mut self, connection: bool, path: String) {
        self.edits
            .push(EditInput::ClearTargets { connection, path });
    }

    /// Pushes a `copySpec` edit.
    #[wasm_bindgen(js_name = copySpec)]
    pub fn copy_spec(&mut self, path: String, to: String) {
        self.edits.push(EditInput::CopySpec { path, to });
    }

    /// Pushes an `insertSublayer` edit; at the end without an `index`.
    #[wasm_bindgen(js_name = insertSublayer)]
    pub fn insert_sublayer(&mut self, asset: String, index: Option<u32>, offset: f64, scale: f64) {
        self.edits.push(EditInput::InsertSublayer {
            asset,
            index,
            offset,
            scale,
        });
    }

    /// Pushes a `removeSublayer` edit.
    #[wasm_bindgen(js_name = removeSublayer)]
    pub fn remove_sublayer(&mut self, index: u32) {
        self.edits.push(EditInput::RemoveSublayer { index });
    }

    /// Pushes a `setSublayerOffset` edit.
    #[wasm_bindgen(js_name = setSublayerOffset)]
    pub fn set_sublayer_offset(&mut self, index: u32, offset: f64, scale: f64) {
        self.edits.push(EditInput::SetSublayerOffset {
            index,
            offset,
            scale,
        });
    }

    /// Pushes a `setSublayers` edit: `assets[i]` at `offsets[i]` and
    /// `scales[i]`. Lists of other lengths are refused when the batch is
    /// applied.
    #[wasm_bindgen(js_name = setSublayers)]
    pub fn set_sublayers(&mut self, assets: Vec<String>, offsets: Vec<f64>, scales: Vec<f64>) {
        self.edits.push(EditInput::SetSublayers {
            assets,
            offsets,
            scales,
        });
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

    /// Pushes an `addSpecialize` edit when `specialize`, else an
    /// `addInherit` edit.
    #[wasm_bindgen(js_name = addClassArc)]
    pub fn add_class_arc(
        &mut self,
        specialize: bool,
        path: String,
        prim_path: String,
        position: String,
    ) {
        self.edits.push(EditInput::AddClassArc {
            specialize,
            path,
            prim_path,
            position,
        });
    }

    /// Pushes a `removeSpecialize` edit when `specialize`, else a
    /// `removeInherit` edit.
    #[wasm_bindgen(js_name = removeClassArc)]
    pub fn remove_class_arc(&mut self, specialize: bool, path: String, prim_path: String) {
        self.edits.push(EditInput::RemoveClassArc {
            specialize,
            path,
            prim_path,
        });
    }

    /// Pushes an `addVariantSet` edit.
    #[wasm_bindgen(js_name = addVariantSet)]
    pub fn add_variant_set(&mut self, path: String, set: String, position: String) {
        self.edits.push(EditInput::AddVariantSet {
            path,
            set,
            position,
        });
    }

    /// Pushes a `removeVariantSet` edit.
    #[wasm_bindgen(js_name = removeVariantSet)]
    pub fn remove_variant_set(&mut self, path: String, set: String) {
        self.edits.push(EditInput::RemoveVariantSet { path, set });
    }

    /// Pushes an `addVariant` edit.
    #[wasm_bindgen(js_name = addVariant)]
    pub fn add_variant(&mut self, path: String, set: String, variant: String, position: String) {
        self.edits.push(EditInput::AddVariant {
            path,
            set,
            variant,
            position,
        });
    }

    /// Pushes a `setSpecifier` edit.
    #[wasm_bindgen(js_name = setSpecifier)]
    pub fn set_specifier(&mut self, path: String, specifier: String) {
        self.edits.push(EditInput::SetSpecifier { path, specifier });
    }

    /// Pushes a `setTypeName` edit; `None` clears the type name.
    #[wasm_bindgen(js_name = setTypeName)]
    pub fn set_type_name(&mut self, path: String, type_name: Option<String>) {
        self.edits.push(EditInput::SetTypeName { path, type_name });
    }

    /// Pushes a `setActive` edit that clears the opinion.
    #[wasm_bindgen(js_name = clearActive)]
    pub fn clear_active(&mut self, path: String) {
        self.edits.push(EditInput::ClearActive { path });
    }

    /// Pushes a `setInstanceable` edit that clears the opinion.
    #[wasm_bindgen(js_name = clearInstanceable)]
    pub fn clear_instanceable(&mut self, path: String) {
        self.edits.push(EditInput::ClearInstanceable { path });
    }

    /// Pushes a `setChildrenReorder` edit; `None` clears the order.
    #[wasm_bindgen(js_name = setChildrenReorder)]
    pub fn set_children_reorder(&mut self, path: String, order: Option<Vec<String>>) {
        self.edits
            .push(EditInput::SetChildrenReorder { path, order });
    }

    /// Pushes a `setPropertyOrder` edit; `None` clears the order.
    #[wasm_bindgen(js_name = setPropertyOrder)]
    pub fn set_property_order(&mut self, path: String, order: Option<Vec<String>>) {
        self.edits.push(EditInput::SetPropertyOrder { path, order });
    }

    /// Pushes a `setRelocates` edit: `sources[i]` to `targets[i]`, an
    /// empty target removing its source (`<>`). Lists of other lengths are
    /// refused when the batch is applied.
    #[wasm_bindgen(js_name = setRelocates)]
    pub fn set_relocates(&mut self, sources: Vec<String>, targets: Vec<String>) {
        self.edits
            .push(EditInput::SetRelocates { sources, targets });
    }

    /// Pushes a `setRelocates` edit that clears the layer's relocates.
    #[wasm_bindgen(js_name = clearRelocates)]
    pub fn clear_relocates(&mut self) {
        self.edits.push(EditInput::ClearRelocates);
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

    /// Pushes a `setLayerMetadata` edit.
    #[wasm_bindgen(js_name = setLayerMetadata)]
    pub fn set_layer_metadata(&mut self, key: String, value: u32) {
        self.edits.push(EditInput::SetLayerMetadata { key, value });
    }

    /// Pushes a `clearLayerMetadata` edit.
    #[wasm_bindgen(js_name = clearLayerMetadata)]
    pub fn clear_layer_metadata(&mut self, key: String) {
        self.edits.push(EditInput::ClearLayerMetadata { key });
    }

    /// Pushes a `bindMaterial` edit: a direct binding, or a collection
    /// binding of `collection` named `binding_name`.
    #[wasm_bindgen(js_name = bindMaterial)]
    pub fn bind_material(
        &mut self,
        path: String,
        material: String,
        purpose: String,
        strength: Option<String>,
        collection: Option<String>,
        binding_name: Option<String>,
    ) {
        self.schema(SchemaInput::BindMaterial {
            path,
            material,
            purpose,
            strength,
            collection,
            binding_name,
        });
    }

    /// Pushes an `unbindMaterial` edit.
    #[wasm_bindgen(js_name = unbindMaterial)]
    pub fn unbind_material(&mut self, path: String, purpose: String, binding_name: Option<String>) {
        self.schema(SchemaInput::UnbindMaterial {
            path,
            purpose,
            binding_name,
        });
    }

    /// Pushes a `setShaderInput` edit of the value in slot `value`.
    #[wasm_bindgen(js_name = setShaderInput)]
    pub fn set_shader_input(&mut self, path: String, time: Option<f64>, value: u32) {
        self.schema(SchemaInput::SetShaderInput { path, time, value });
    }

    /// Pushes a `connectToSource` edit.
    #[wasm_bindgen(js_name = connectToSource)]
    pub fn connect_to_source(
        &mut self,
        path: String,
        source: String,
        type_name: Option<String>,
        position: String,
    ) {
        self.schema(SchemaInput::ConnectToSource {
            path,
            source,
            type_name,
            position,
        });
    }

    /// Pushes a `disconnectSource` edit.
    #[wasm_bindgen(js_name = disconnectSource)]
    pub fn disconnect_source(&mut self, path: String, source: Option<String>) {
        self.schema(SchemaInput::DisconnectSource { path, source });
    }

    /// Pushes an `applyAPI` edit.
    #[wasm_bindgen(js_name = applyApi)]
    pub fn apply_api(&mut self, path: String, schema: String, instance: Option<String>) {
        self.schema(SchemaInput::ApplyApi {
            path,
            schema,
            instance,
        });
    }

    /// Pushes a `removeAPI` edit.
    #[wasm_bindgen(js_name = removeApi)]
    pub fn remove_api(&mut self, path: String, schema: String, instance: Option<String>) {
        self.schema(SchemaInput::RemoveApi {
            path,
            schema,
            instance,
        });
    }

    /// Pushes a `setBaseMaterial` edit.
    #[wasm_bindgen(js_name = setBaseMaterial)]
    pub fn set_base_material(&mut self, path: String, base: Option<String>) {
        self.schema(SchemaInput::SetBaseMaterial { path, base });
    }

    /// Pushes a `createGeomSubset` edit, or a `createMaterialSubset` one
    /// with `material_bind`.
    #[wasm_bindgen(js_name = createGeomSubset)]
    #[expect(
        clippy::too_many_arguments,
        reason = "one positional argument per field of the edit"
    )]
    pub fn create_geom_subset(
        &mut self,
        path: String,
        name: String,
        element: String,
        indices: Vec<i32>,
        family: String,
        family_type: Option<String>,
        material_bind: bool,
    ) {
        self.schema(SchemaInput::CreateGeomSubset {
            path,
            name,
            element,
            indices,
            family,
            family_type,
            material_bind,
        });
    }

    /// Pushes a `setSubsetFamilyType` edit.
    #[wasm_bindgen(js_name = setSubsetFamilyType)]
    pub fn set_subset_family_type(&mut self, path: String, family: String, family_type: String) {
        self.schema(SchemaInput::SetSubsetFamilyType {
            path,
            family,
            family_type,
        });
    }

    /// Pushes a `bindCoordSys` edit.
    #[wasm_bindgen(js_name = bindCoordSys)]
    pub fn bind_coord_sys(&mut self, path: String, name: String, target: String) {
        self.schema(SchemaInput::BindCoordSys { path, name, target });
    }

    /// Pushes a `blockCoordSys` edit.
    #[wasm_bindgen(js_name = blockCoordSys)]
    pub fn block_coord_sys(&mut self, path: String, name: String) {
        self.schema(SchemaInput::BlockCoordSys { path, name });
    }

    /// Pushes a `clearCoordSys` edit.
    #[wasm_bindgen(js_name = clearCoordSys)]
    pub fn clear_coord_sys(&mut self, path: String, name: String, remove_spec: bool) {
        self.schema(SchemaInput::ClearCoordSys {
            path,
            name,
            remove_spec,
        });
    }

    /// Pushes a `setSdrMetadata` edit; `None` clears the key.
    #[wasm_bindgen(js_name = setSdrMetadata)]
    pub fn set_sdr_metadata(&mut self, path: String, key: String, value: Option<String>) {
        self.schema(SchemaInput::SetSdrMetadata { path, key, value });
    }

    /// Pushes a common transform edit: every component, or exactly one.
    #[wasm_bindgen(js_name = setCommonTransform)]
    #[expect(
        clippy::too_many_arguments,
        reason = "one positional argument per field of the edit"
    )]
    pub fn set_common_transform(
        &mut self,
        path: String,
        time: Option<f64>,
        translate: Option<Vec<f64>>,
        rotate: Option<Vec<f64>>,
        order: Option<String>,
        scale: Option<Vec<f64>>,
        pivot: Option<Vec<f64>>,
    ) {
        self.schema(SchemaInput::SetCommonTransform {
            path,
            time,
            translate,
            rotate,
            order,
            scale,
            pivot,
        });
    }
}

impl EditBatch {
    fn schema(&mut self, input: SchemaInput) {
        self.edits.push(EditInput::Schema(input));
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

/// What lowering a generic edit reads: the store, the composed stage
/// before the batch, and the edit target with the layer stack its layer
/// is in.
pub(crate) struct Scope<'a> {
    pub(crate) store: &'a InMemoryStore,
    pub(crate) view: StageView<'a>,
    /// Where the edits are written, and how their stage paths and times
    /// map there: a layer of the stage's layer stack, or a layer reached
    /// through an arc of a composed prim.
    pub(crate) target: &'a EditTarget,
    /// The layer stack the target's layer is in, strongest first: the
    /// stage's for a local target, the arc node's for one through an arc.
    pub(crate) stack: &'a [LayerId],
}

/// One edit, checked against the store and ready to collect.
enum Step {
    /// A generic edit's transaction, and what it needs collected through
    /// the schema edit first.
    Generic(Transaction, Option<Need>),
    /// A define (`UsdStage::DefinePrim`, `OverridePrim` or
    /// `CreateClassPrim`, and the type name given), collected through the
    /// schema edit so later edits of the batch see the prim.
    Define(PathId, Specifier, Option<String>),
    /// A schema-shaped edit.
    Schema(schema_edit::Prepared),
}

/// What a generic edit needs collected through the batch's schema edit
/// before its transaction, so that it and the schema-shaped edits see the
/// property specs each other create: a connection's source an earlier
/// `createAttribute` made is not created again, and an attribute a
/// connection created is not refused as undeclared.
#[derive(Debug)]
enum Need {
    /// The property's spec in the target layer, with this declaration,
    /// unless the layer or an earlier edit of the batch already has one
    /// ([`layerstack_schemas::SchemaEdit::declare_property`]).
    Spec(PropertyPath, PropertyDeclaration),
    /// The property, which nothing declares before the batch, must be an
    /// attribute as the earlier edits of the batch leave it
    /// ([`check_connectable`]).
    Attribute(PropertyPath),
}

/// Collects `need` through `edit`.
fn collect_need(edit: &mut layerstack_schemas::SchemaEdit<'_>, need: Need) -> Result<(), Refusal> {
    match need {
        Need::Spec(property, declaration) => {
            edit.declare_property(property, &declaration);
            Ok(())
        }
        Need::Attribute(property) => {
            let name = String::from(edit.tokens().resolve(property.property()));
            match edit.property_kind(property.prim_path(), &name) {
                Some(PropertyKind::Attribute) => Ok(()),
                Some(PropertyKind::Relationship) => Err(Refusal::NotAnAttribute(property)),
                None => Err(Refusal::UndeclaredType(property)),
            }
        }
    }
}

/// Lowers `batch` to one transaction whose edits are addressed through
/// `target`: paths are stage paths and time codes stage times, mapped to
/// the target layer's specs and times when the transaction applies, so an
/// edit the target does not map is rejected then
/// (`edit.rejected.unmappable`).
///
/// Every edit is checked first, reading the stage as composed before the
/// batch. Then the edits are collected in order through one
/// [`layerstack_schemas::SchemaEdit`]: a define (`def`, `over` or `class`)
/// and the schema-shaped edits through its handles, which see the prims,
/// types, applied schemas and property specs earlier edits of the batch
/// define, apply and create; the other edits as their own transactions,
/// appended ([`layerstack_schemas::SchemaEdit::append`]) after the
/// property specs they create, which are created through the schema edit
/// ([`Need`]) so that every later edit sees them.
///
/// # Errors
///
/// The first edit that does not lower, with its index: `path.invalid`
/// for a malformed path or a prim path where a property path is asked
/// for (and the reverse), `argument.invalid` for an unknown specifier,
/// list position or value slot, and the `value.*` errors of
/// [`value::decode`]; then a schema handle's refusal
/// ([`schema_edit::Refusal`]): `prim.not_found`, `prim.not_class`,
/// `material.not_found`, `schema.cannot_edit`, `shading.*`, `subset.*`, `coord_sys.no_binding`,
/// `metadata.*`, and for a connection edit of a property the batch
/// leaves undeclared or a relationship, `edit.rejected.undeclared_type`
/// and `edit.rejected.not_an_attribute`.
pub(crate) fn lower(
    batch: &EditBatch,
    stage: &layerstack::Stage,
    store: &mut InMemoryStore,
    target: &EditTarget,
    stack: &[LayerId],
) -> Result<Lowered, Error> {
    let fail = |index: usize, error: Error| {
        let edit = &batch.edits[index];
        error
            .at_edit(u32::try_from(index).unwrap_or(u32::MAX))
            .at_path(edit.path())
    };
    let mut steps = Vec::with_capacity(batch.edits.len());
    {
        let scope = Scope {
            store: &*store,
            view: stage.view(&*store),
            target,
            stack,
        };
        let inputs = schema_edit::Inputs {
            store: &*store,
            values: &batch.values,
        };
        let mut pending = Pending::default();
        for (index, edit) in batch.edits.iter().enumerate() {
            let step = match edit {
                EditInput::DefinePrim {
                    path,
                    specifier,
                    type_name,
                } => Step::Define(
                    prim_path(store, path).map_err(|error| fail(index, error))?,
                    define_specifier(specifier).map_err(|error| fail(index, error))?,
                    type_name.clone(),
                ),
                EditInput::Schema(input) => Step::Schema(
                    schema_edit::prepare(input, &inputs).map_err(|error| fail(index, error))?,
                ),
                _ => {
                    let mut txn = Transaction::new();
                    let need = lower_one(edit, batch, &scope, &mut txn, &mut pending)
                        .map_err(|error| fail(index, error))?;
                    Step::Generic(txn, need)
                }
            };
            steps.push(step);
        }
    }
    let mut origins = Vec::new();
    let collected = {
        let mut edit = layerstack_schemas::SchemaEdit::new(stage, store, target.clone());
        let mut refused = None;
        for (index, step) in steps.into_iter().enumerate() {
            let result = match step {
                Step::Generic(txn, need) => need
                    .map_or(Ok(()), |need| collect_need(&mut edit, need))
                    .map(|()| {
                        edit.append(txn);
                    }),
                Step::Define(path, specifier, type_name) => {
                    define(&mut edit, path, specifier, type_name.as_deref())
                }
                Step::Schema(prepared) => schema_edit::author(&mut edit, prepared),
            };
            if let Err(refusal) = result {
                refused = Some((index, refusal));
                break;
            }
            origins.resize(
                edit.transaction().len(),
                u32::try_from(index).unwrap_or(u32::MAX),
            );
        }
        match refused {
            None => Ok(edit.finish()),
            Some(refused) => Err(refused),
        }
    };
    match collected {
        Ok(transaction) => Ok(Lowered {
            transaction,
            origins,
        }),
        Err((index, refusal)) => Err(fail(index, refusal.into_error(store))),
    }
}

/// The prim path `path` names, interned.
fn prim_path(store: &InMemoryStore, path: &str) -> Result<PathId, Error> {
    if path.contains('.') {
        return Err(Error::new(
            "path.invalid",
            "not a prim path, such as /World/Rock",
        ));
    }
    store
        .try_path(path)
        .map_err(|error| Error::new("path.invalid", format!("not a prim path: {error}")))
}

/// What earlier edits of a batch leave that later ones, lowered against
/// the stage as it was before the batch, must know.
#[derive(Debug, Default)]
struct Pending {
    /// Variant specs the batch creates, so a later `addVariant` does not
    /// create them again.
    variants: Vec<SpecPath>,
    /// The number of sublayers the target's layer has once the batch's
    /// sublayer edits so far apply; `None` before the first.
    sublayers: Option<usize>,
}

impl Pending {
    /// The number of sublayers the target's layer has, as the batch's
    /// sublayer edits so far leave it.
    fn sublayers(&mut self, scope: &Scope<'_>) -> usize {
        *self.sublayers.get_or_insert_with(|| {
            scope
                .store
                .layer(scope.target.layer())
                .map_or(0, |layer| layer.sublayers().len())
        })
    }
}

/// Lowers the generic edit `edit` into `txn`, returning what it needs
/// collected through the schema edit first.
fn lower_one(
    edit: &EditInput,
    batch: &EditBatch,
    scope: &Scope<'_>,
    txn: &mut Transaction,
    pending: &mut Pending,
) -> Result<Option<Need>, Error> {
    let mut need = None;
    let store = scope.store;
    let at = scope.target;
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
        // Collected through the schema edit ([`lower`]).
        EditInput::DefinePrim { .. } => {}
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
            // `UsdPrim::CreateAttribute`: the spec is stamped as
            // `_CreateAttributeSpecForEditing` stamps one, else created
            // with the type and qualifiers given.
            let declaration =
                stamped_declaration(scope, path).unwrap_or_else(|| PropertyDeclaration {
                    kind: PropertyKind::Attribute,
                    type_name: Some(ty),
                    variability: if *uniform {
                        Variability::Uniform
                    } else {
                        Variability::Varying
                    },
                    custom: *custom,
                });
            if let Some(value) = value {
                txn.set_default(at.property(property), value);
            }
            return Ok(Some(Need::Spec(property, declaration)));
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
                list.push(target_path(store, target)?);
            }
            txn.set_targets(at, ListOp::explicit(list));
        }
        EditInput::AddTarget {
            connection,
            path,
            target,
            position,
        } => {
            let property = property(path)?;
            let position = list_position(position)?;
            let target = target_path(store, target)?;
            if *connection {
                need = check_connectable(scope, path, property, true)?;
            }
            txn.add_target(at.property(property), target, position);
        }
        EditInput::RemoveTarget {
            connection,
            path,
            target,
        } => {
            let property = property(path)?;
            let target = target_path(store, target)?;
            if *connection {
                need = check_connectable(scope, path, property, true)?;
            }
            txn.remove_target(at.property(property), target);
        }
        EditInput::ClearTargets { connection, path } => {
            let property = property(path)?;
            if *connection {
                need = check_connectable(scope, path, property, false)?;
            }
            txn.clear_targets(at.property(property));
        }
        EditInput::CopySpec { path, to } => {
            let address = |path: &str| -> Result<_, Error> {
                Ok(if path.contains('.') {
                    at.property(property(path)?)
                } else {
                    at.prim(prim(path)?)
                })
            };
            txn.copy_spec(address(path)?, address(to)?);
        }
        EditInput::InsertSublayer {
            asset,
            index,
            offset,
            scale,
        } => {
            let offset = layer_offset(*offset, *scale)?;
            let count = pending.sublayers(scope);
            let index = index.map_or(count, |index| index as usize);
            txn.insert_sublayer(
                at.layer(),
                index,
                SublayerEntry::unresolved(asset.as_str(), offset),
            );
            // An index past the end is rejected when the batch applies.
            pending.sublayers = Some(count + usize::from(index <= count));
        }
        EditInput::RemoveSublayer { index } => {
            let count = pending.sublayers(scope);
            let index = *index as usize;
            txn.remove_sublayer(at.layer(), index);
            pending.sublayers = Some(count - usize::from(index < count));
        }
        EditInput::SetSublayerOffset {
            index,
            offset,
            scale,
        } => {
            let offset = layer_offset(*offset, *scale)?;
            txn.set_sublayer_offset(at.layer(), *index as usize, offset);
        }
        EditInput::SetSublayers {
            assets,
            offsets,
            scales,
        } => {
            if offsets.len() != assets.len() || scales.len() != assets.len() {
                return Err(Error::new(
                    "argument.invalid",
                    "a sublayer list needs one offset and one scale per asset path",
                ));
            }
            // An entry the layer lists already keeps the layer it resolved
            // to; another is authored by its asset path, which the stage
            // then asks for, as `insertSublayer` authors one.
            let current = store
                .layer(at.layer())
                .map(|layer| layer.sublayers().to_vec())
                .unwrap_or_default();
            let mut entries = Vec::with_capacity(assets.len());
            for ((asset, offset), scale) in assets.iter().zip(offsets).zip(scales) {
                let offset = layer_offset(*offset, *scale)?;
                entries.push(
                    match current
                        .iter()
                        .find(|entry| entry.asset.as_deref() == Some(asset.as_str()))
                    {
                        Some(entry) => {
                            SublayerEntry::with_asset(entry.layer, asset.as_str(), offset)
                        }
                        None => SublayerEntry::unresolved(asset.as_str(), offset),
                    },
                );
            }
            pending.sublayers = Some(entries.len());
            txn.set_sublayers(at.layer(), entries);
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
            let position = list_position(position)?;
            let arc = arc(
                scope,
                asset.as_deref(),
                prim_path.as_deref(),
                layer_offset(*offset, *scale)?,
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
            // An internal arc names a stage path, which the transaction maps;
            // the authored item, in the layer's namespace, stands in for it
            // only where the target maps the prim to itself.
            let internal = asset.as_deref().is_none_or(str::is_empty);
            let arc = if internal && !scope.target.maps_to_itself(path_id, store.paths()) {
                built
            } else {
                authored_arc(scope, path_id, *payload, &built).unwrap_or(built)
            };
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
        EditInput::AddClassArc {
            specialize,
            path,
            prim_path,
            position,
        } => {
            let at = at.prim(prim(path)?);
            let position = list_position(position)?;
            let base = class_path(store, prim_path)?;
            if *specialize {
                txn.add_specialize(at, base, position);
            } else {
                txn.add_inherit(at, base, position);
            }
        }
        EditInput::RemoveClassArc {
            specialize,
            path,
            prim_path,
        } => {
            let at = at.prim(prim(path)?);
            let base = class_path(store, prim_path)?;
            if *specialize {
                txn.remove_specialize(at, base);
            } else {
                txn.remove_inherit(at, base);
            }
        }
        EditInput::AddVariantSet {
            path,
            set,
            position,
        } => {
            let at = at.prim(prim(path)?);
            let position = list_position(position)?;
            txn.add_variant_set(at, store.tokens().intern(set), position);
        }
        EditInput::RemoveVariantSet { path, set } => {
            txn.remove_variant_set(at.prim(prim(path)?), store.tokens().intern(set));
        }
        EditInput::AddVariant {
            path,
            set,
            variant,
            position,
        } => {
            let host = prim(path)?;
            let position = list_position(position)?;
            if !is_variant_name(variant) {
                return Err(Error::new(
                    "path.invalid",
                    format!(
                        "`{variant}` is not a variant name: letters, digits, `_`, `|` and `-`, after an optional `.`"
                    ),
                ));
            }
            txn.add_variant_set(at.prim(host), store.tokens().intern(set), position);
            // The set's own checks (an identifier, not on the pseudo-root,
            // a path the target maps) reject the edit when it applies.
            let paths = store.paths();
            let mapped = at
                .map_to_spec_path(host, paths)
                .filter(|_| paths.parent(host).is_some() && layerstack::ident::is_identifier(set));
            if let Some(mapped) = mapped {
                let named = format!(
                    "{}{{{set}={variant}}}",
                    mapped.display(paths, store.tokens())
                );
                let spec = SpecPath::parse(&named, store.tokens(), paths).map_err(|error| {
                    Error::new(
                        "path.invalid",
                        format!("`{named}` is not a variant: {error}"),
                    )
                })?;
                let authored = store
                    .layer(at.layer())
                    .is_some_and(|layer| layer.variant_spec_at(&spec, paths).is_some());
                if !authored && !pending.variants.contains(&spec) {
                    txn.create_prim(
                        Address::spec(at.layer(), spec.clone()),
                        Specifier::Over,
                        None,
                    );
                    pending.variants.push(spec);
                }
            }
        }
        EditInput::SetSpecifier { path, specifier } => {
            txn.set_specifier(at.prim(prim(path)?), define_specifier(specifier)?);
        }
        EditInput::SetTypeName { path, type_name } => {
            let at = at.prim(prim(path)?);
            match type_name {
                Some(name) => txn.set_type_name(at, store.tokens().intern(name)),
                None => txn.clear_type_name(at),
            };
        }
        EditInput::SetChildrenReorder { path, order } => {
            let at = at.prim(prim(path)?);
            match order {
                Some(order) => txn.set_prim_order(at, names(store, order)),
                None => txn.clear_prim_order(at),
            };
        }
        EditInput::SetPropertyOrder { path, order } => {
            let at = at.prim(prim(path)?);
            match order {
                Some(order) => txn.set_property_order(at, names(store, order)),
                None => txn.clear_property_order(at),
            };
        }
        EditInput::SetRelocates { sources, targets } => {
            if sources.len() != targets.len() {
                return Err(Error::new(
                    "argument.invalid",
                    "a relocate list needs one target per source",
                ));
            }
            let mut list = Vec::with_capacity(sources.len());
            for (source, target) in sources.iter().zip(targets) {
                list.push(Relocate {
                    source: prim(source)?,
                    target: (!target.is_empty()).then(|| prim(target)).transpose()?,
                });
            }
            txn.set_layer_relocates(at.layer(), list);
        }
        EditInput::ClearRelocates => {
            txn.clear_layer_relocates(at.layer());
        }
        EditInput::ClearActive { path } => {
            txn.clear_active(at.prim(prim(path)?));
        }
        EditInput::ClearInstanceable { path } => {
            txn.clear_instanceable(at.prim(prim(path)?));
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
                need = stamped_declaration(scope, path)
                    .map(|declaration| Need::Spec(property, declaration));
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
        EditInput::SetLayerMetadata { key, value: slot } => {
            let value = value(*slot)?;
            let key = store.tokens().intern(key);
            txn.set_layer_metadata(at.layer(), key, FieldValue::Value(value));
        }
        EditInput::ClearLayerMetadata { key } => {
            let key = store.tokens().intern(key);
            txn.clear_layer_metadata(at.layer(), key);
        }
        // Prepared and collected by `lower` itself.
        EditInput::Schema(_) => {}
    }
    Ok(need)
}

/// The declaration a spec for the property at `path` is stamped with, as
/// OpenUSD's `UsdStage::_CreatePropertySpecForEditing` stamps one before
/// it authors one of the property's fields where the target layer has no
/// spec: the prim's schema definition of the property (not `custom`),
/// else the stage's composed declaration of it, read in stage namespace.
/// `None` when nothing defines or declares the property, or declares it
/// an attribute without a type. A spec the target layer (or an earlier
/// edit of the batch) already has stays as it is
/// ([`layerstack_schemas::SchemaEdit::declare_property`]).
///
/// Spec: AOUSD Core §12.2.2–§12.2.4 (property declarations), §13.3.2.3
/// (the prim definition).
fn stamped_declaration(scope: &Scope<'_>, path: &str) -> Option<PropertyDeclaration> {
    let composed = scope.view.property(path)?;
    let declaration = match composed.definition() {
        Some(definition) => PropertyDeclaration {
            kind: definition.kind,
            type_name: definition.type_name.clone(),
            variability: definition.variability,
            custom: false,
        },
        None => composed.declaration()?,
    };
    (declaration.kind == PropertyKind::Relationship || declaration.type_name.is_some())
        .then_some(declaration)
}

/// The relationship target or attribute connection `path` names.
fn target_path(store: &InMemoryStore, path: &str) -> Result<TargetPath, Error> {
    store.try_target_path(path).map_err(|error| {
        Error::new(
            "path.invalid",
            format!("`{path}` is not a target path: {error}"),
        )
    })
}

/// The prim an inherit or specialize names, a stage path the transaction
/// maps through the target (a root prim path is authored as given).
fn class_path(store: &InMemoryStore, path: &str) -> Result<PathId, Error> {
    if path.contains('.') {
        return Err(Error::new(
            "path.invalid",
            format!("`{path}` is not a prim path to inherit or specialize"),
        ));
    }
    store.try_path(path).map_err(|error| {
        Error::new(
            "path.invalid",
            format!("`{path}` is not a prim path to inherit or specialize: {error}"),
        )
    })
}

/// The names of a child or property order, interned.
fn names(store: &InMemoryStore, order: &[String]) -> Vec<TokenId> {
    order
        .iter()
        .map(|name| store.tokens().intern(name))
        .collect()
}

/// Whether `name` is a variant name, as OpenUSD's
/// `SdfSchema::IsValidVariantIdentifier` reads one: letters, digits, `_`,
/// `|` and `-`, after an optional leading `.`.
///
/// Spec: AOUSD Core §7.3.6 (variant specs), §8 (paths).
fn is_variant_name(name: &str) -> bool {
    let rest = name.strip_prefix('.').unwrap_or(name);
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '|' | '-'))
}

/// The list position `name` names (OpenUSD's `UsdListPosition`).
fn list_position(name: &str) -> Result<ListPosition, Error> {
    Ok(match name {
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
    })
}

/// A layer offset of an arc or sublayer: a finite offset and a finite,
/// positive scale.
///
/// Spec: AOUSD Core §12.3.2.1 (layer offsets).
fn layer_offset(offset: f64, scale: f64) -> Result<LayerOffset, Error> {
    if !(offset.is_finite() && scale.is_finite() && scale > 0.0) {
        return Err(Error::new(
            "argument.invalid",
            "a layer offset needs a finite offset and a finite, positive scale",
        ));
    }
    Ok(LayerOffset { offset, scale })
}

/// Checks that the property at `path` is an attribute, which connection
/// edits author: as the target layer authors it, else as the stage
/// defines or declares it. A relationship is refused
/// (`edit.rejected.not_an_attribute`). A property nothing declares
/// before the batch, when `declared`, must be created as an attribute by
/// an earlier edit of the batch ([`Need::Attribute`], checked as the
/// edits are collected), else it is refused
/// (`edit.rejected.undeclared_type`): the transaction would create a
/// `custom` relationship for it, where OpenUSD's `AddConnection` and
/// `RemoveConnection` fail for want of a type. Clearing connections
/// nothing authors does nothing, as the transaction does.
///
/// Spec: AOUSD Core §7.6.4.2.3 (`connectionPaths`).
fn check_connectable(
    scope: &Scope<'_>,
    path: &str,
    property: PropertyPath,
    declared: bool,
) -> Result<Option<Need>, Error> {
    let target = scope.target;
    let authored = target
        .map_property_to_spec_path(property, scope.store.paths())
        .and_then(|spec| {
            scope
                .store
                .layer(target.layer())?
                .property_at(&spec)
                .map(|spec| spec.kind)
        });
    let kind = authored.or_else(|| {
        let composed = scope.view.property(path)?;
        composed
            .definition()
            .map(|definition| definition.kind)
            .or_else(|| composed.declaration().map(|declared| declared.kind))
    });
    match kind {
        Some(PropertyKind::Attribute) => Ok(None),
        Some(PropertyKind::Relationship) => Err(not_an_attribute(path)),
        None if !declared => Ok(None),
        None => Ok(Some(Need::Attribute(property))),
    }
}

/// The refusal of a connection edit of the relationship at `path`.
pub(crate) fn not_an_attribute(path: impl core::fmt::Display) -> Error {
    Error::new(
        "edit.rejected.not_an_attribute",
        format!("{path} is a relationship: edit its targets instead"),
    )
}

/// The refusal of a connection edit of the property at `path`, which
/// nothing declares.
pub(crate) fn undeclared_type(path: impl core::fmt::Display) -> Error {
    Error::new(
        "edit.rejected.undeclared_type",
        format!("nothing declares the attribute {path}: create it with its type first"),
    )
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

/// The specifier a `definePrim` edit names: `def`, `over` or `class`.
fn define_specifier(specifier: &str) -> Result<Specifier, Error> {
    match specifier {
        "def" => Ok(Specifier::Def),
        "over" => Ok(Specifier::Over),
        "class" => Ok(Specifier::Class),
        other => Err(Error::new(
            "argument.invalid",
            format!("`{other}` is not a specifier: def, over or class"),
        )),
    }
}

/// Collects a `definePrim` edit through the schema edit, as OpenUSD's
/// stage calls author:
///
/// - `def` ([`SchemaEdit::define_prim`], `UsdStage::DefinePrim`).
/// - `over` ([`SchemaEdit::override_prim`], `UsdStage::OverridePrim`):
///   nothing when the prim exists, so a `def` stays a `def`; otherwise an
///   `over` spec.
/// - `class` ([`SchemaEdit::create_class_prim`],
///   `UsdStage::CreateClassPrim`): nothing when the prim is a class or in
///   one; refused for a defined prim that is not a class.
///
/// A type name given with `over` or `class` is then authored on the
/// target's spec, as `UsdPrim::SetTypeName` authors it.
///
/// [`SchemaEdit::define_prim`]: layerstack_schemas::SchemaEdit::define_prim
/// [`SchemaEdit::override_prim`]: layerstack_schemas::SchemaEdit::override_prim
/// [`SchemaEdit::create_class_prim`]: layerstack_schemas::SchemaEdit::create_class_prim
///
/// Spec: AOUSD Core §7.6 (specifier), §12.2.1 (specifier resolution).
fn define(
    edit: &mut layerstack_schemas::SchemaEdit<'_>,
    path: PathId,
    specifier: Specifier,
    type_name: Option<&str>,
) -> Result<(), Refusal> {
    match specifier {
        Specifier::Def => {
            edit.define_prim(path, type_name.unwrap_or(""))?;
            return Ok(());
        }
        Specifier::Over => {
            edit.override_prim(path)?;
        }
        Specifier::Class => {
            edit.create_class_prim(path)?;
        }
    }
    if let Some(name) = type_name {
        let name = edit.tokens().intern(name);
        let mut txn = Transaction::new();
        txn.set_type_name(edit.target().prim(path), name);
        edit.append(txn);
    }
    Ok(())
}

/// The reference or payload an add or remove names: an external arc to
/// `asset` resolved against the target layer, or an internal arc
/// without one; to `prim_path`, or to the default prim without one. An
/// internal arc's path is a stage path, which the transaction maps through
/// the target (a root prim path is kept as given, as OpenUSD's
/// `_TranslatePath` keeps it); an external arc's names a prim in its asset.
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
        Some(asset) => Reference::deferred(scope.target.layer(), asset, target, offset),
        None => Reference::from_source(
            ReferenceSource::Layer {
                layer: scope.target.layer(),
                asset: None,
            },
            target,
        ),
    };
    arc.layer_offset = offset;
    Ok(arc)
}

/// The arc a layer of the target's layer stack authors on `path` that
/// names what `wanted` names (its asset path as authored and its target),
/// as it is authored: a removal must name the authored item, which records
/// how the arc was resolved, not only what was written.
///
/// The target layer is searched first, then the other layers of its
/// stack, at the spec path the target maps `path` to (`/Robot` through a
/// reference to it, `/World/Robot{finish=painted}` inside a variant); a
/// target that maps paths outside its prim to themselves searches the
/// stage path last.
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
    let paths = scope.store.paths();
    let layer = scope.target.layer();
    let order: Vec<LayerId> = core::iter::once(layer)
        .chain(scope.stack.iter().copied().filter(|id| *id != layer))
        .collect();
    let mapped = scope.target.map_to_spec_path(path, paths);
    let stage_path = SpecPath::from_prim_path(path);
    let sites = mapped.iter().chain(
        (scope.target.has_root_identity() && mapped.as_ref() != Some(&stage_path))
            .then_some(&stage_path),
    );
    for site in sites {
        for id in &order {
            let Some(list) = scope
                .store
                .layer(*id)
                .and_then(|layer| authored_arcs(layer, site, paths, payload))
            else {
                continue;
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
    }
    None
}

/// The references (or payloads) the prim or variant spec at `site` in
/// `layer` authors.
fn authored_arcs<'l>(
    layer: &'l Layer,
    site: &SpecPath,
    paths: &PathInterner,
    payload: bool,
) -> Option<&'l ListOp<Reference>> {
    let (references, payloads) = match layer.prim_at(site) {
        Some(prim) => (&prim.references, &prim.payloads),
        None => {
            let variant = layer.variant_spec_at(site, paths)?;
            (&variant.references, &variant.payloads)
        }
    };
    Some(if payload { payloads } else { references })
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
        layerstack_usda::save_usda(store.layer(id).unwrap(), store.tokens(), store.paths()).unwrap()
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

    /// Receipts combined with `copy` and `absorb` undo as one atomic
    /// transaction: a stale older receipt stops the whole undo, and the
    /// receipts it combined stay as they were.
    #[test]
    fn combined_receipts_undo_atomically() {
        let mut stage = open();
        let target = root(&stage);
        let before = text(&stage, target);
        let set = |stage: &mut Stage, path: &str, value: f64| {
            let mut batch = EditBatch::new();
            let value = numbers(&mut batch, "double", &[value]);
            batch.set_value(path.to_string(), None, value);
            stage.apply(target.0, &mut batch).unwrap()
        };
        let size = set(&mut stage, "/World.size", 3.0);
        let spin = set(&mut stage, "/World.spin", 4.0);
        let mut combined = spin.copy().unwrap();
        combined.absorb(&size).unwrap();
        assert!(size.undoable() && spin.undoable(), "the parts stay");

        // An edit outside the receipts makes the older one stale.
        let _outside = set(&mut stage, "/World.size", 5.0);
        let after = text(&stage, target);
        let error = stage.undo(&mut combined).unwrap_err();
        assert_eq!(error.code, "edit.stale");
        assert_eq!(text(&stage, target), after, "nothing was undone");
        assert!(combined.undoable());

        let _back = set(&mut stage, "/World.size", 3.0);
        let mut redo = stage.undo(&mut combined).unwrap();
        assert_eq!(text(&stage, target), before, "both undone");
        stage.undo(&mut redo).unwrap();

        let mut other = open();
        let foreign = set(&mut other, "/World.size", 6.0);
        let mut copy = size.copy().unwrap();
        assert_eq!(copy.absorb(&foreign).unwrap_err().code, "receipt.affinity");
        let mut used = spin.copy().unwrap();
        stage.undo(&mut used).unwrap();
        assert!(!combined.undoable() && spin.undoable());
        assert_eq!(used.copy().unwrap_err().code, "receipt.released");
        assert_eq!(copy.absorb(&used).unwrap_err().code, "receipt.released");
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
            // `OverridePrim` then `SetTypeName`: the `def` stays.
            case(
                "/World/Rock",
                "over",
                Some("Cube"),
                true,
                &[("/World/Rock", "def:Cube")],
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
    fn add_remove_and_clear_targets_and_connections() {
        let mut batch = EditBatch::new();
        batch.add_target(
            false,
            "/World.lookAt".into(),
            "/Spare".into(),
            "backOfAppend".into(),
        );
        round_trip(batch, |stage| {
            assert_eq!(
                stage
                    .relationship("/World.lookAt")
                    .unwrap()
                    .as_list()
                    .unwrap()[1],
                Wire::List(vec![Wire::text("/World/Rock"), Wire::text("/Spare")]),
                "an explicit list takes the target"
            );
        });
        let mut batch = EditBatch::new();
        batch.remove_target(false, "/World.lookAt".into(), "/World/Rock".into());
        round_trip(batch, |stage| {
            assert_eq!(
                stage
                    .relationship("/World.lookAt")
                    .unwrap()
                    .as_list()
                    .unwrap()[1],
                Wire::List(Vec::new())
            );
        });
        let mut batch = EditBatch::new();
        batch.clear_targets(false, "/World.lookAt".into());
        let stage = round_trip(batch, |stage| {
            assert_eq!(
                stage
                    .relationship("/World.lookAt")
                    .unwrap()
                    .as_list()
                    .unwrap()[1],
                Wire::List(Vec::new())
            );
        });
        assert!(text(&stage, root(&stage)).contains("rel lookAt\n"));
        let mut batch = EditBatch::new();
        batch.add_target(
            true,
            "/World.size".into(),
            "/Spare.count".into(),
            "backOfPrepend".into(),
        );
        let stage = round_trip(batch, |_| {});
        assert!(
            text(&stage, root(&stage)).contains("prepend double size.connect = </Spare.count>"),
            "{}",
            text(&stage, root(&stage))
        );
        let mut batch = EditBatch::new();
        batch.remove_target(true, "/World.spin".into(), "/Spare.count".into());
        let stage = round_trip(batch, |_| {});
        assert!(text(&stage, root(&stage)).contains("delete double spin.connect = </Spare.count>"));

        let mut stage = open();
        let target = root(&stage);
        let refused = |stage: &mut Stage, batch: &mut EditBatch| {
            let error = stage.apply(target.0, batch).unwrap_err();
            (error.code, error.edit)
        };
        let mut batch = EditBatch::new();
        batch.add_target(
            true,
            "/World.lookAt".into(),
            "/Spare.count".into(),
            "backOfPrepend".into(),
        );
        assert_eq!(
            refused(&mut stage, &mut batch),
            ("edit.rejected.not_an_attribute", Some(0))
        );
        let mut batch = EditBatch::new();
        batch.add_target(
            true,
            "/World.nothing".into(),
            "/Spare.count".into(),
            "backOfPrepend".into(),
        );
        assert_eq!(
            refused(&mut stage, &mut batch),
            ("edit.rejected.undeclared_type", Some(0)),
            "OpenUSD's AddConnection fails without a type"
        );
        let mut batch = EditBatch::new();
        batch.create_attribute("/World.nothing".into(), "double".into(), false, true, None);
        batch.add_target(
            true,
            "/World.nothing".into(),
            "/Spare.count".into(),
            "backOfPrepend".into(),
        );
        stage.apply(target.0, &mut batch).unwrap();
        let mut batch = EditBatch::new();
        batch.add_target(
            false,
            "/World.lookAt".into(),
            "/Spare".into(),
            "middle".into(),
        );
        assert_eq!(
            refused(&mut stage, &mut batch),
            ("argument.invalid", Some(0))
        );
    }

    #[test]
    fn apply_and_remove_api_schemas() {
        let mut batch = EditBatch::new();
        batch.apply_api("/World/Rock".into(), "MaterialBindingAPI".into(), None);
        batch.apply_api(
            "/World/Rock".into(),
            "CollectionAPI".into(),
            Some("shiny".into()),
        );
        let stage = round_trip(batch, |_| {});
        assert!(
            text(&stage, root(&stage))
                .contains(r#"prepend apiSchemas = ["MaterialBindingAPI", "CollectionAPI:shiny"]"#),
            "{}",
            text(&stage, root(&stage))
        );
        let mut batch = EditBatch::new();
        batch.remove_api("/World".into(), "PluginOnlyAPI".into(), None);
        let stage = round_trip(batch, |_| {});
        assert!(
            text(&stage, root(&stage)).contains(r#"delete apiSchemas = ["PluginOnlyAPI"]"#),
            "any name is removed, registered or not"
        );

        let mut stage = open();
        let target = root(&stage);
        let mut batch = EditBatch::new();
        batch.apply_api("/World".into(), "NoSuchAPI".into(), None);
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "schema.cannot_apply"
        );
        let mut batch = EditBatch::new();
        batch.apply_api("/World".into(), "CollectionAPI".into(), None);
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "schema.cannot_apply",
            "a multiple-apply schema needs an instance"
        );
        let mut batch = EditBatch::new();
        batch.remove_api("/Nowhere".into(), "MaterialBindingAPI".into(), None);
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "prim.not_found"
        );
    }

    #[test]
    fn add_and_remove_inherits_and_specializes() {
        let mut batch = EditBatch::new();
        batch.add_class_arc(
            false,
            "/Spare".into(),
            "/World/Rock".into(),
            "backOfPrepend".into(),
        );
        let stage = round_trip(batch, |stage| {
            assert_eq!(prim_field(stage, "/Spare", 0), Wire::text("Sphere"));
        });
        assert!(text(&stage, root(&stage)).contains("prepend inherits = </World/Rock>"));
        let mut batch = EditBatch::new();
        batch.add_class_arc(
            true,
            "/Spare".into(),
            "/World/Rock".into(),
            "frontOfAppend".into(),
        );
        let stage = round_trip(batch, |stage| {
            assert_eq!(prim_field(stage, "/Spare", 0), Wire::text("Sphere"));
        });
        assert!(text(&stage, root(&stage)).contains("append specializes = </World/Rock>"));
        let mut batch = EditBatch::new();
        batch.remove_class_arc(true, "/Spare".into(), "/World/Rock".into());
        let stage = round_trip(batch, |_| {});
        assert!(text(&stage, root(&stage)).contains("delete specializes = </World/Rock>"));
        let mut stage = open();
        let target = root(&stage);
        let mut batch = EditBatch::new();
        batch.add_class_arc(
            false,
            "/Spare".into(),
            "/World.size".into(),
            "backOfPrepend".into(),
        );
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "path.invalid"
        );
    }

    #[test]
    fn variant_sets_and_variants_are_added_and_removed() {
        let mut batch = EditBatch::new();
        batch.add_variant(
            "/World/Rock".into(),
            "look".into(),
            "rough".into(),
            "backOfPrepend".into(),
        );
        batch.add_variant(
            "/World/Rock".into(),
            "look".into(),
            "smooth".into(),
            "backOfPrepend".into(),
        );
        // Adding what the batch added already adds nothing more.
        batch.add_variant(
            "/World/Rock".into(),
            "look".into(),
            "rough".into(),
            "backOfPrepend".into(),
        );
        batch.add_variant_set("/World/Rock".into(), "size".into(), "backOfAppend".into());
        let stage = round_trip(batch, |stage| {
            assert_eq!(
                stage.variant_sets("/World/Rock").unwrap(),
                Wire::List(vec![
                    Wire::List(vec![
                        Wire::text("look"),
                        Wire::Null,
                        Wire::List(vec![Wire::text("rough"), Wire::text("smooth")]),
                    ]),
                    Wire::List(vec![Wire::text("size"), Wire::Null, Wire::List(Vec::new())]),
                ])
            );
        });
        let usda = text(&stage, root(&stage));
        assert!(usda.contains(r#"prepend variantSets = "look""#), "{usda}");
        assert!(usda.contains(r#"append variantSets = "size""#), "{usda}");

        // A variant the layer has already is left as it is.
        let mut stage = open();
        let target = root(&stage);
        let before = text(&stage, target);
        let mut batch = EditBatch::new();
        batch.add_variant(
            "/World".into(),
            "shade".into(),
            "light".into(),
            "backOfPrepend".into(),
        );
        stage.apply(target.0, &mut batch).unwrap();
        assert_eq!(text(&stage, target), before);

        let mut batch = EditBatch::new();
        batch.remove_variant_set("/World".into(), "shade".into());
        let stage = round_trip(batch, |stage| {
            assert_eq!(stage.get("/World.tint", None).unwrap(), Wire::Null);
        });
        let usda = text(&stage, root(&stage));
        assert!(!usda.contains("variantSet \"shade\""));
        assert!(
            usda.contains(r#"prepend variantSets = "shade""#),
            "the name stays listed, as SdfPrimSpec::RemoveVariantSet leaves it"
        );

        let mut stage = open();
        let refused = |batch: &mut EditBatch, stage: &mut Stage| {
            stage.apply(target.0, batch).unwrap_err().code
        };
        let mut batch = EditBatch::new();
        batch.add_variant(
            "/World".into(),
            "shade".into(),
            "a b".into(),
            "backOfPrepend".into(),
        );
        assert_eq!(refused(&mut batch, &mut stage), "path.invalid");
        let mut batch = EditBatch::new();
        batch.add_variant(
            "/World".into(),
            "two words".into(),
            "a".into(),
            "backOfPrepend".into(),
        );
        assert_eq!(
            refused(&mut batch, &mut stage),
            "edit.rejected.invalid_variant_set_name"
        );
    }

    #[test]
    fn specifier_type_name_and_cleared_prim_fields() {
        let mut batch = EditBatch::new();
        batch.set_specifier("/World/Rock".into(), "class".into());
        round_trip(batch, |stage| {
            assert_eq!(prim_field(stage, "/World/Rock", 1), Wire::text("class"));
        });
        let mut batch = EditBatch::new();
        batch.set_type_name("/World/Rock".into(), Some("Cube".into()));
        round_trip(batch, |stage| {
            assert_eq!(prim_field(stage, "/World/Rock", 0), Wire::text("Cube"));
        });
        let mut batch = EditBatch::new();
        batch.set_type_name("/World/Rock".into(), None);
        round_trip(batch, |stage| {
            assert_eq!(prim_field(stage, "/World/Rock", 0), Wire::Null);
        });

        let mut stage = open();
        let target = root(&stage);
        let before = text(&stage, target);
        let mut batch = EditBatch::new();
        batch.set_active("/World/Rock".into(), false);
        batch.set_instanceable("/Spare".into(), true);
        stage.apply(target.0, &mut batch).unwrap();
        let mut batch = EditBatch::new();
        batch.clear_active("/World/Rock".into());
        batch.clear_instanceable("/Spare".into());
        let mut receipt = stage.apply(target.0, &mut batch).unwrap();
        assert_eq!(text(&stage, target), before, "both opinions cleared");
        assert_eq!(prim_field(&stage, "/World/Rock", 2), Wire::Bool(true));
        stage.undo(&mut receipt).unwrap();
        assert_eq!(prim_field(&stage, "/World/Rock", 2), Wire::Bool(false));

        let mut batch = EditBatch::new();
        batch.set_specifier("/World".into(), "maybe".into());
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "argument.invalid"
        );
    }

    #[test]
    fn child_and_property_orders() {
        let mut batch = EditBatch::new();
        batch.set_children_reorder("/".into(), Some(vec!["Spare".into(), "World".into()]));
        let stage = round_trip(batch, |stage| {
            assert_eq!(
                stage.children("/").unwrap(),
                vec![String::from("/Spare"), String::from("/World")]
            );
        });
        assert!(text(&stage, root(&stage)).contains(r#"reorder rootPrims = ["Spare", "World"]"#));
        let mut batch = EditBatch::new();
        batch.set_property_order("/World".into(), Some(vec!["spin".into(), "size".into()]));
        let stage = round_trip(batch, |_| {});
        assert!(
            text(&stage, root(&stage)).contains(r#"reorder properties = ["spin", "size"]"#),
            "{}",
            text(&stage, root(&stage))
        );

        let mut stage = open();
        let target = root(&stage);
        let before = text(&stage, target);
        let mut batch = EditBatch::new();
        batch.set_children_reorder("/World".into(), Some(vec!["Rock".into()]));
        stage.apply(target.0, &mut batch).unwrap();
        let mut batch = EditBatch::new();
        batch.set_children_reorder("/World".into(), None);
        batch.set_property_order("/World".into(), None);
        stage.apply(target.0, &mut batch).unwrap();
        assert_eq!(text(&stage, target), before, "cleared");
    }

    #[test]
    fn copy_spec_duplicates_a_prim_or_property() {
        let mut batch = EditBatch::new();
        batch.copy_spec("/World/Rock".into(), "/World/Pebble".into());
        batch.copy_spec("/World.size".into(), "/World.width".into());
        round_trip(batch, |stage| {
            assert_eq!(prim_field(stage, "/World/Pebble", 0), Wire::text("Sphere"));
            assert_eq!(
                stage.get("/World.width", None).unwrap(),
                scalar("double", Wire::Number(2.0))
            );
        });
        let mut stage = open();
        let target = root(&stage);
        let mut batch = EditBatch::new();
        batch.copy_spec("/World/Rock".into(), "/World.rock".into());
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "edit.rejected.copy_between_kinds"
        );
        let mut batch = EditBatch::new();
        batch.copy_spec("/World".into(), "/World/Inner".into());
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "edit.rejected.copy_into_source"
        );
    }

    #[test]
    fn sublayers_are_inserted_reordered_offset_and_removed() {
        const EXTRA: &str = "#usda 1.0\ndef \"Extra\" {\n    int count = 5\n}\n";
        const MORE: &str = "#usda 1.0\ndef \"Extra\" {\n    int count = 6\n}\n";
        let mut stage = open();
        let target = root(&stage);
        let before = (text(&stage, target), snapshot(&stage));
        let mut batch = EditBatch::new();
        batch.insert_sublayer("./extra.usda".into(), None, 0.0, 1.0);
        batch.insert_sublayer("./more.usda".into(), None, 10.0, 2.0);
        let mut receipt = stage.apply(target.0, &mut batch).unwrap();
        let record = receipt.record.as_list().unwrap().to_vec();
        let wanted: Vec<_> = record[4]
            .as_list()
            .unwrap()
            .iter()
            .map(|row| row.as_list().unwrap()[0].as_text().unwrap().to_string())
            .collect();
        assert_eq!(
            wanted,
            ["extra.usda", "more.usda"],
            "asked for, as an arc's asset is"
        );
        assert_eq!(
            stage.layer_sublayers(target.0).unwrap(),
            Wire::List(vec![
                Wire::List(vec![
                    Wire::text("./extra.usda"),
                    Wire::Number(0.0),
                    Wire::Number(1.0)
                ]),
                Wire::List(vec![
                    Wire::text("./more.usda"),
                    Wire::Number(10.0),
                    Wire::Number(2.0)
                ]),
            ])
        );
        let storage = stage.document_mut().storage_mut();
        storage.supply("extra.usda", EXTRA.as_bytes().to_vec());
        storage.supply("more.usda", MORE.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert_eq!(
            stage.get("/Extra.count", None).unwrap(),
            scalar("int", Wire::Number(5.0)),
            "the stronger sublayer wins"
        );

        let mut reorder = EditBatch::new();
        reorder.set_sublayers(
            vec!["./more.usda".into(), "./extra.usda".into()],
            vec![0.0, 0.0],
            vec![1.0, 1.0],
        );
        let mut reordered = stage.apply(target.0, &mut reorder).unwrap();
        assert_eq!(
            stage.get("/Extra.count", None).unwrap(),
            scalar("int", Wire::Number(6.0))
        );
        let mut offset = EditBatch::new();
        offset.set_sublayer_offset(1, 5.0, 1.0);
        let mut offsetted = stage.apply(target.0, &mut offset).unwrap();
        assert!(text(&stage, target).contains("@./extra.usda@ (offset = 5)"));
        let mut remove = EditBatch::new();
        remove.remove_sublayer(0);
        let mut removed = stage.apply(target.0, &mut remove).unwrap();
        assert_eq!(
            stage.get("/Extra.count", None).unwrap(),
            scalar("int", Wire::Number(5.0))
        );
        for receipt in [&mut removed, &mut offsetted, &mut reordered, &mut receipt] {
            stage.undo(receipt).unwrap();
        }
        assert_eq!((text(&stage, target), snapshot(&stage)), before);

        let mut batch = EditBatch::new();
        batch.remove_sublayer(0);
        let error = stage.apply(target.0, &mut batch).unwrap_err();
        assert_eq!(
            (error.code, error.edit),
            ("edit.rejected.no_such_sublayer", Some(0))
        );
        let mut batch = EditBatch::new();
        batch.insert_sublayer("./extra.usda".into(), None, 0.0, 1.0);
        batch.insert_sublayer("./extra.usda".into(), None, 0.0, 1.0);
        let error = stage.apply(target.0, &mut batch).unwrap_err();
        assert_eq!(
            (error.code, error.edit),
            ("edit.rejected.duplicate_sublayer", Some(1))
        );
        let mut batch = EditBatch::new();
        batch.insert_sublayer("./extra.usda".into(), None, 0.0, 0.0);
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "argument.invalid"
        );
        let mut batch = EditBatch::new();
        batch.set_sublayers(vec!["./extra.usda".into()], vec![], vec![]);
        assert_eq!(
            stage.apply(target.0, &mut batch).unwrap_err().code,
            "argument.invalid"
        );
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
        let nothing = || Wire::List(vec![Wire::List(Vec::new()); 3]);
        let batches = |record: Wire| record.as_list().unwrap()[0].clone();
        let loaded = batches(stage.take_changes(true));
        assert_eq!(
            loaded.as_list().unwrap()[0].as_list().unwrap()[2],
            Wire::List(vec![Wire::text("/Spare")]),
            "loading the supplied reference resynced its prim"
        );
        assert_eq!(stage.take_changes(true), nothing());
        let target = root(&stage);
        let mut batch = EditBatch::new();
        batch.define_prim("/Garden".into(), "def".into(), None);
        let three = numbers(&mut batch, "double", &[3.0]);
        batch.set_value("/World.size".into(), None, three);
        stage.apply(target.0, &mut batch).unwrap();
        let changes = batches(stage.take_changes(true));
        let batches = changes.as_list().unwrap();
        assert_eq!(batches.len(), 1);
        let fields = batches[0].as_list().unwrap();
        assert_eq!(
            fields[0],
            Wire::List(vec![Wire::text("/Garden")]),
            "created"
        );
        // The value edit is a property change: `/World.size`'s default.
        let size = Wire::List(vec![
            Wire::text("/World.size"),
            Wire::text("default"),
            Wire::Null,
        ]);
        assert!(
            fields[6].as_list().unwrap().contains(&size)
                || fields[2].as_list().unwrap().contains(&Wire::text("/World")),
            "{fields:?}"
        );
        assert_eq!(stage.take_changes(true), nothing());
        let mut batch = EditBatch::new();
        batch.set_active("/Garden".into(), false);
        stage.apply(target.0, &mut batch).unwrap();
        assert_eq!(stage.take_changes(false), nothing());
        assert_eq!(stage.take_changes(true), nothing(), "consumed");
    }
}

/// Edits through arcs: a robot referenced with `(offset = 10; scale = 2)`,
/// a second plain reference to it, and a sublayer authored with
/// `(offset = 10)`, lowered through node and local variant targets.
#[cfg(test)]
mod through_arcs {
    use alloc::{string::String, vec, vec::Vec};

    use layerstack::{ArcKind, EditTarget, LayerId, SpecPath};

    use super::EditBatch;
    use crate::{
        error::Error,
        stage::{BROWSER_LIMITS, Resolved, Stage},
        wire::Wire,
    };

    const SCENE: &str = r#"#usda 1.0
(
    subLayers = [
        @./shot.usda@ (offset = 10)
    ]
)
def Xform "World" {
    def Xform "Robot" (
        prepend references = @./robot.usda@ (offset = 10; scale = 2)
        variants = {
            string finish = "painted"
        }
        prepend variantSets = "finish"
    ) {
        variantSet "finish" = {
            "painted" (
                prepend references = @./paint.usda@
            ) {
                color3f color = (0.8, 0.2, 0.1)
            }
        }
    }
    def Xform "Spare" (
        prepend references = @./robot.usda@
    ) {
    }
    def "Lamp" {
    }
}
"#;
    const ROBOT: &str = r#"#usda 1.0
(
    defaultPrim = "Robot"
)
def Xform "Robot" {
    double height = 1.2
    double spin.timeSamples = {
        0: 0,
    }
    rel aim = </Robot/Head>
    def Sphere "Head" {
    }
    def "Part" (
        prepend references = </Robot/Head>
    ) {
    }
    def "Badge" (
        prepend references = @./paint.usda@
    ) {
    }
}
"#;
    const SHOT: &str = "#usda 1.0\nover \"World\" {\n    over \"Lamp\" {\n        double glow.timeSamples = {\n            0: 1,\n        }\n    }\n}\n";
    const PAINT: &str = "#usda 1.0\n(\n    defaultPrim = \"Paint\"\n)\ndef \"Paint\" {\n    double gloss = 0.5\n}\n";

    fn open() -> Stage {
        let mut stage = Stage::open(
            "scene.usda",
            SCENE.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        let storage = stage.document_mut().storage_mut();
        storage.supply("robot.usda", ROBOT.as_bytes().to_vec());
        storage.supply("shot.usda", SHOT.as_bytes().to_vec());
        storage.supply("paint.usda", PAINT.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        stage
    }

    fn layer(stage: &Stage, identifier: &str) -> LayerId {
        let document = stage.document();
        document
            .store()
            .layer_ids()
            .into_iter()
            .find(|id| document.identifier(*id) == Some(identifier))
            .unwrap_or_else(|| panic!("no layer {identifier}"))
    }

    fn text(stage: &Stage, id: LayerId) -> String {
        let store = stage.document().store();
        layerstack_usda::save_usda(store.layer(id).unwrap(), store.tokens(), store.paths()).unwrap()
    }

    /// The target of the `kind` node of `prim`'s graph that reads
    /// `robot.usda`, in that layer.
    fn through(stage: &Stage, prim: &str, kind: ArcKind) -> Resolved {
        let robot = layer(stage, "robot.usda");
        let document = stage.document();
        let store = document.store();
        let path = store.path(prim);
        let composed = document.view().prim(prim).expect("a composed prim");
        let graph = composed.composition_graph();
        let (node, layers) = graph
            .nodes()
            .find(|(_, node)| node.arc_kind() == kind && node.layer_stack() == robot)
            .map(|(id, node)| (id, node.layers().collect::<Vec<_>>()))
            .expect("a node of that arc");
        let target =
            EditTarget::for_node_layer(document.stage().stage(), store, path, node, layers[0])
                .expect("an editable node");
        Resolved {
            target,
            stack: layers,
        }
    }

    fn number(stage: &Stage, path: &str, time: Option<f64>) -> Wire {
        stage.get(path, time).unwrap().as_list().unwrap()[2].clone()
    }

    #[test]
    fn values_samples_and_specs_land_in_the_referenced_layer() {
        let mut stage = open();
        let robot = layer(&stage, "robot.usda");
        let before = text(&stage, robot);
        let target = through(&stage, "/World/Robot", ArcKind::References);
        let mut batch = EditBatch::new();
        let height = batch.value_numbers("double".into(), vec![1.5]);
        let spin = batch.value_numbers("double".into(), vec![70.0]);
        batch.set_value("/World/Robot.height".into(), None, height);
        batch.set_value("/World/Robot.spin".into(), Some(14.0), spin);
        batch.create_attribute(
            "/World/Robot.width".into(),
            "double".into(),
            false,
            true,
            None,
        );
        let hidden = batch.value_numbers("bool".into(), vec![1.0]);
        batch.set_metadata("/World/Robot/Head.radius".into(), "hidden".into(), hidden);
        batch.define_prim("/World/Robot/Arm".into(), "def".into(), Some("Cube".into()));
        let mut receipt = stage.apply_through(&target, &mut batch).unwrap();
        assert_eq!(receipt.target(), robot);
        let after = text(&stage, robot);
        assert!(after.contains("double height = 1.5"), "{after}");
        assert!(
            after.contains("2: 70"),
            "stage time 14 is layer time (14 - 10) / 2: {after}"
        );
        assert!(after.contains("custom double width"), "{after}");
        assert!(
            after.contains("double radius (\n            hidden = true"),
            "the schema's radius stamped in the asset: {after}"
        );
        assert!(after.contains("def Cube \"Arm\""), "{after}");
        assert_eq!(
            number(&stage, "/World/Spare.height", None),
            Wire::Number(1.5),
            "every prim referencing the asset changes"
        );
        assert!(stage.prim("/World/Spare/Arm").unwrap() != Wire::Null);
        assert_eq!(
            text(&stage, layer(&stage, "scene.usda")),
            text(&open(), layer(&open(), "scene.usda")),
            "the scene is untouched"
        );
        let mut redo = stage.undo(&mut receipt).unwrap();
        assert_eq!(text(&stage, robot), before, "undo restores the asset");
        stage.undo(&mut redo).unwrap();
        assert_eq!(text(&stage, robot), after, "redo writes it again");
    }

    #[test]
    fn targets_and_internal_arcs_map_into_the_referenced_namespace() {
        let mut stage = open();
        assert_eq!(
            number(&stage, "/World/Spare/Badge.gloss", None),
            Wire::Number(0.5)
        );
        let robot = layer(&stage, "robot.usda");
        let target = through(&stage, "/World/Robot", ArcKind::References);
        let mut batch = EditBatch::new();
        batch.set_targets(
            "/World/Robot.aim".into(),
            vec!["/World/Robot/Head".into(), "/World/Robot.height".into()],
        );
        batch.add_arc(
            false,
            "/World/Robot/Arm".into(),
            None,
            Some("/World/Robot/Head".into()),
            "backOfPrepend".into(),
            0.0,
            1.0,
        );
        // A root prim path is authored as given (OpenUSD's `_TranslatePath`).
        batch.add_arc(
            false,
            "/World/Robot/Arm".into(),
            None,
            Some("/Library".into()),
            "backOfAppend".into(),
            0.0,
            1.0,
        );
        batch.remove_arc(
            false,
            "/World/Robot/Part".into(),
            None,
            Some("/World/Robot/Head".into()),
        );
        // An external arc is removed as the asset authors it.
        batch.remove_arc(
            false,
            "/World/Robot/Badge".into(),
            Some("./paint.usda".into()),
            None,
        );
        stage.apply_through(&target, &mut batch).unwrap();
        let after = text(&stage, robot);
        assert!(
            after.contains("def \"Badge\" (\n        delete references = @./paint.usda@\n    )"),
            "{after}"
        );
        assert_eq!(
            stage.get("/World/Spare/Badge.gloss", None).unwrap(),
            Wire::Null,
            "the paint no longer reaches the badge"
        );
        assert!(
            after.contains("rel aim = [\n        </Robot/Head>,\n        </Robot.height>,\n    ]"),
            "{after}"
        );
        assert!(
            after.contains("prepend references = </Robot/Head>"),
            "{after}"
        );
        assert!(after.contains("append references = </Library>"), "{after}");
        assert!(
            after.contains("delete references = </Robot/Head>"),
            "{after}"
        );
        assert_eq!(
            stage.relationship("/World/Spare.aim").unwrap(),
            Wire::List(vec![
                Wire::Bool(false),
                Wire::List(vec![
                    Wire::text("/World/Spare/Head"),
                    Wire::text("/World/Spare.height")
                ]),
            ]),
            "read back through the other reference"
        );
    }

    #[test]
    fn paths_the_reference_does_not_map_are_rejected_with_their_edit() {
        let mut stage = open();
        let robot = layer(&stage, "robot.usda");
        let before = text(&stage, robot);
        let target = through(&stage, "/World/Robot", ArcKind::References);
        let mut batch = EditBatch::new();
        let one = batch.value_numbers("double".into(), vec![1.0]);
        batch.set_value("/World/Robot.height".into(), None, one);
        batch.set_value("/World/Lamp.glow".into(), None, one);
        let error = stage.apply_through(&target, &mut batch).unwrap_err();
        assert_eq!(
            (error.code, error.edit, error.path.as_deref()),
            (
                "edit.rejected.unmappable",
                Some(1),
                Some("/World/Lamp.glow")
            )
        );
        let mut batch = EditBatch::new();
        batch.set_targets("/World/Robot.aim".into(), vec!["/World/Lamp".into()]);
        let error = stage.apply_through(&target, &mut batch).unwrap_err();
        assert_eq!(
            (error.code, error.edit),
            ("edit.rejected.unmappable_target", Some(0))
        );
        // OpenUSD's `_DefinePrim` cannot create the spec of an undefined
        // ancestor its edit target does not map.
        let mut batch = EditBatch::new();
        batch.define_prim("/World/Ghost/Shade".into(), "def".into(), None);
        let error = stage.apply_through(&target, &mut batch).unwrap_err();
        assert_eq!(
            (error.code, error.edit),
            ("edit.rejected.unmappable", Some(0))
        );
        assert_eq!(text(&stage, robot), before, "nothing applied");
        let mut defined = EditBatch::new();
        defined.define_prim("/World/Robot/Deep/Leaf".into(), "def".into(), None);
        assert_eq!(
            stage.apply_through(&target, &mut defined).map(|_| ()),
            Ok(()),
            "an undefined ancestor inside the reference is defined in the asset"
        );
        assert!(text(&stage, robot).contains("def \"Deep\""));
        assert!(!text(&stage, layer(&stage, "scene.usda")).contains("Deep"));
    }

    #[test]
    fn a_muted_layer_is_not_written_through_an_arc() {
        let mut stage = open();
        let robot = layer(&stage, "robot.usda");
        let target = through(&stage, "/World/Robot", ArcKind::References);
        stage.set_layer_muted(robot.0, true).unwrap();
        let mut batch = EditBatch::new();
        let one = batch.value_numbers("double".into(), vec![1.0]);
        batch.set_value("/World/Robot.height".into(), None, one);
        assert_eq!(
            stage.apply_through(&target, &mut batch).unwrap_err().code,
            "layer.muted"
        );
    }

    #[test]
    fn a_local_variant_target_writes_inside_the_branch() {
        let mut stage = open();
        let scene = layer(&stage, "scene.usda");
        let variant = {
            let store = stage.document().store();
            SpecPath::parse(
                "/World/Robot{finish=painted}",
                store.tokens(),
                store.paths(),
            )
            .unwrap()
        };
        let target = Resolved {
            target: EditTarget::for_local_variant(scene, &variant),
            stack: stage.document().stage().stage().layer_stack().to_vec(),
        };
        let mut batch = EditBatch::new();
        let gloss = batch.value_numbers("double".into(), vec![0.9]);
        batch.set_value("/World/Robot.gloss".into(), None, gloss);
        batch.remove_arc(
            false,
            "/World/Robot".into(),
            Some("./paint.usda".into()),
            None,
        );
        stage.apply_through(&target, &mut batch).unwrap();
        let after = text(&stage, scene);
        assert!(
            after.contains("\"painted\" (\n                delete references = @./paint.usda@\n"),
            "the removal names the arc authored in the branch: {after}"
        );
        assert!(after.contains("double gloss = 0.9"), "{after}");
        assert_eq!(
            number(&stage, "/World/Robot.gloss", None),
            Wire::Number(0.9)
        );
    }

    #[test]
    fn a_local_sublayer_is_written_at_its_offset() {
        let mut stage = open();
        let shot = layer(&stage, "shot.usda");
        let mut batch = EditBatch::new();
        let two = batch.value_numbers("double".into(), vec![2.0]);
        batch.set_value("/World/Lamp.glow".into(), Some(15.0), two);
        stage.apply(shot.0, &mut batch).unwrap();
        let after = text(&stage, shot);
        assert!(
            after.contains("5: 2"),
            "stage time 15 is the sublayer's time 15 - 10: {after}"
        );
        assert_eq!(
            stage.time_samples("/World/Lamp.glow").unwrap(),
            [10.0, 15.0],
            "read back at the stage time it was set at"
        );
    }

    /// Relocates move what arcs bring: `robot.usda`'s `Head`, reached
    /// through the reference, moves to `Face`; a locally authored prim
    /// would not move.
    #[test]
    fn relocates_are_set_and_cleared() {
        let mut stage = open();
        let root = stage.document().root_layer();
        let before = text(&stage, root);
        let relocate = |stage: &mut Stage, sources: Vec<String>, targets: Vec<String>| {
            let mut batch = EditBatch::new();
            batch.set_relocates(sources, targets);
            stage.apply(root.0, &mut batch)
        };
        let children = |stage: &Stage| stage.children("/World/Robot").unwrap();
        let mut moved = relocate(
            &mut stage,
            vec!["/World/Robot/Head".into()],
            vec!["/World/Robot/Face".into()],
        )
        .unwrap();
        assert!(
            text(&stage, root).contains("relocates = {"),
            "{}",
            text(&stage, root)
        );
        assert!(children(&stage).contains(&String::from("/World/Robot/Face")));
        assert!(!children(&stage).contains(&String::from("/World/Robot/Head")));
        let mut clear = EditBatch::new();
        clear.clear_relocates();
        let mut cleared = stage.apply(root.0, &mut clear).unwrap();
        assert_eq!(text(&stage, root), before);
        assert!(children(&stage).contains(&String::from("/World/Robot/Head")));
        stage.undo(&mut cleared).unwrap();
        assert!(children(&stage).contains(&String::from("/World/Robot/Face")));
        stage.undo(&mut moved).unwrap();
        assert_eq!(text(&stage, root), before);

        let mut removed = relocate(
            &mut stage,
            vec!["/World/Robot/Head".into()],
            vec![String::new()],
        )
        .unwrap();
        assert!(
            !children(&stage)
                .iter()
                .any(|child| child.ends_with("/Head")),
            "an empty target removes the source"
        );
        stage.undo(&mut removed).unwrap();

        let code = |result: Result<_, Error>| result.unwrap_err().code;
        assert_eq!(
            code(relocate(
                &mut stage,
                vec!["/World".into()],
                vec!["/Elsewhere".into()]
            )),
            "edit.rejected.invalid_relocate",
            "a root prim is not relocated"
        );
        assert_eq!(
            code(relocate(
                &mut stage,
                vec!["/World/Robot/Head".into()],
                vec![]
            )),
            "argument.invalid"
        );
        assert_eq!(
            code(relocate(
                &mut stage,
                vec!["World".into()],
                vec!["/World/Face".into()]
            )),
            "path.invalid"
        );
    }
}
