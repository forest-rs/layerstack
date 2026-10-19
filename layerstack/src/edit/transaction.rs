// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transactions: batches of spec edits applied atomically.

use alloc::{boxed::Box, vec::Vec};

use super::{
    apply::{self, AnimationState, Guarded},
    error::{EditError, Slot},
    layer_fields::SublayerEdit,
    prim_fields::{ArcEdit, ArcItem, ListPosition, PrimField, PrimValue},
    same::Same,
    spec::{Loc, spec_at},
    target::Address,
};
use crate::{
    doc::{
        FieldValue, LayerId, LayerOffset, LayerStore, Reference, Relocate, Specifier,
        SublayerEntry, Value,
    },
    interner::TokenId,
    listop::ListOp,
    path::{PathId, TargetPath},
    property::{PropertySpec, get_property},
};

/// A batch of spec edits, applied atomically across layers.
///
/// A transaction lists edits in order, and preconditions that must hold
/// before any of them is applied. Applying it
/// ([`Transaction::apply`], or [`LiveStage::apply`](crate::LiveStage::apply)
/// to recompose too) either applies every edit, moving the
/// [`Layer::generation`](crate::Layer::generation) of each edited layer
/// forward once (and its
/// [`Layer::structural_generation`](crate::Layer::structural_generation)
/// when specs, children, variants or selections changed), or applies none
/// and leaves every layer exactly as it was.
///
/// Applying a transaction returns its inverse: a transaction that undoes
/// exactly what it did. The inverse removes the specs the transaction
/// created instead of writing back old values, restores what it changed
/// or removed, and touches only the slots the transaction touched, down to
/// single time samples. Applying the inverses of a sequence of
/// transactions in reverse order restores every layer exactly. An
/// inverse's own inverse redoes the transaction.
///
/// An inverse is guarded by what its transaction wrote: each of its steps
/// restores a slot only while that slot still holds what the transaction
/// left there, and otherwise the inverse fails with
/// [`EditError::StaleValue`] and changes nothing. So undoing never
/// overwrites a later edit of the same default, sample, field, selection
/// or spec, while later edits of other slots, even of the same spec or
/// layer, do not stop it. Guards and value preconditions compare authored
/// state, not numbers: floats compare by bit pattern at any depth, so an
/// unchanged NaN matches itself and `-0.0` differs from `+0.0`. [`Transaction::expect_unchanged`] also guards
/// an inverse by its layers' generations, for hosts that want any later
/// edit of those layers to stop it.
///
/// Edits that need a spec that does not exist yet create it, as OpenUSD's
/// authoring API does: a prim spec as an `over`, with any missing
/// ancestors and variant branches (`SdfCreatePrimInLayer`), and an
/// attribute spec with its declared type.
///
/// An edit addressed to the pseudo-root (`/`) or to a property path on it
/// is rejected ([`Rejection::PseudoRoot`]), except for the root prim order
/// ([`Transaction::set_prim_order`]): the pseudo-root spec holds a layer's
/// metadata and root prims, and no prim fields or properties. Layer
/// metadata has its own edits, [`Transaction::set_layer_metadata`] and
/// its siblings; OpenUSD's `UsdPrim::SetMetadata` on the pseudo-root
/// writes a field registered on layers as layer metadata, which this
/// keeps to one edit. AOUSD Core §5.1 (the pseudo-root), §7.6.1 (layer
/// metadata).
///
/// OpenUSD: an `SdfChangeBlock` of `SdfLayer` / `SdfPrimSpec` /
/// `SdfAttributeSpec` edits, except that OpenUSD neither rolls back a
/// failed block nor records inverses.
///
/// Spec: AOUSD Core §7 (scene description: layers, prim, variant and
/// property specs).
///
/// [`Rejection::PseudoRoot`]: super::Rejection::PseudoRoot
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transaction {
    pub(crate) preconditions: Vec<Precondition>,
    pub(crate) ops: Vec<Op>,
}

/// One edit of a [`Transaction`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op {
    LayerMetadata {
        layer: LayerId,
        key: TokenId,
        value: Option<FieldValue>,
    },
    CreatePrim {
        at: Address,
        specifier: Specifier,
        type_name: Option<TokenId>,
    },
    CreateProperty {
        at: Address,
        spec: Box<PropertySpec>,
    },
    RemoveSpec {
        at: Address,
    },
    SetDefault {
        at: Address,
        value: Value,
    },
    ClearDefault {
        at: Address,
    },
    BlockAttribute {
        at: Address,
    },
    SetTimeSample {
        at: Address,
        time: f64,
        value: Value,
    },
    RemoveTimeSample {
        at: Address,
        time: f64,
    },
    SetMetadata {
        at: Address,
        key: TokenId,
        value: FieldValue,
    },
    ClearMetadata {
        at: Address,
        key: TokenId,
    },
    SetVariantSelection {
        at: Address,
        set: TokenId,
        variant: Option<TokenId>,
    },
    SetTargets {
        at: Address,
        targets: Option<ListOp<TargetPath>>,
    },
    /// Adds a target at a position, or removes it (`None`).
    EditTargetList {
        at: Address,
        target: TargetPath,
        position: Option<ListPosition>,
    },
    AddAppliedSchema {
        at: Address,
        name: TokenId,
    },
    RemoveAppliedSchema {
        at: Address,
        name: TokenId,
    },
    SetPrimField {
        at: Address,
        value: PrimValue,
    },
    EditArc {
        at: Address,
        edit: Box<ArcEdit>,
    },
    EditSublayers {
        layer: LayerId,
        edit: Box<SublayerEdit>,
    },
    SetRelocates {
        layer: LayerId,
        relocates: Vec<Relocate>,
    },
    CopySpec {
        from: Address,
        to: Address,
    },
    AddVariantSet {
        at: Address,
        name: TokenId,
        position: ListPosition,
    },
    RemoveVariantSet {
        at: Address,
        name: TokenId,
    },
    /// A storage-level step of an inverse, guarded by the step it undoes.
    Raw(Box<Guarded>),
}

/// A condition a [`Transaction`] checks before applying anything.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Precondition {
    LayerMetadata {
        layer: LayerId,
        key: TokenId,
        expected: Box<Option<FieldValue>>,
    },
    Generation {
        layer: LayerId,
        generation: u64,
    },
    Authored {
        at: Address,
        slot: Slot,
        /// Boxed: an authored value, a list op among them, is large.
        expected: Box<Option<Authored>>,
    },
}

/// An authored value a precondition expects, compared with [`Same`], so a
/// NaN expectation holds against the same NaN and `-0.0` differs from
/// `+0.0`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Authored {
    Value(Value),
    Animation(Box<AnimationState>),
    Field(FieldValue),
    Token(TokenId),
    Property(Box<PropertySpec>),
    Prim(Box<PrimValue>),
    Spec,
}

impl Same for Authored {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Value(a), Self::Value(b)) => a.same(b),
            (Self::Animation(a), Self::Animation(b)) => a.same(b),
            (Self::Field(a), Self::Field(b)) => a.same(b),
            (Self::Property(a), Self::Property(b)) => a.same(b),
            (Self::Token(a), Self::Token(b)) => a == b,
            (Self::Prim(a), Self::Prim(b)) => a == b,
            (Self::Spec, Self::Spec) => true,
            _ => false,
        }
    }
}

impl Transaction {
    /// Sets authored layer metadata, independently of prim metadata.
    /// `defaultPrim` accepts a token; fields stored in other dedicated layer
    /// members are rejected: sublayers have [`Transaction::set_sublayers`]
    /// and its siblings, relocates [`Transaction::set_layer_relocates`]. Inverses guard this field
    /// alone. AOUSD Core §7.6.1; OpenUSD `SdfLayer::SetField`.
    pub fn set_layer_metadata(
        &mut self,
        layer: LayerId,
        key: TokenId,
        value: FieldValue,
    ) -> &mut Self {
        self.push(Op::LayerMetadata {
            layer,
            key,
            value: Some(value),
        })
    }

    /// Replaces the sublayers of `layer`, with their offsets.
    ///
    /// The list is checked first: a layer named twice (by layer or by
    /// authored asset path) or an empty asset path is rejected, as OpenUSD
    /// rejects it, and so is a layer the store does not hold and a cycle (a
    /// sublayer that is `layer`, or whose own sublayers reach it), which
    /// OpenUSD accepts and reports only when composing. An entry whose
    /// asset did not resolve ([`SublayerEntry::unresolved`]) keeps its
    /// place.
    ///
    /// A [`LiveStage`](crate::LiveStage) recomposes from its new layer
    /// stacks.
    ///
    /// OpenUSD: `SdfLayer::SetSubLayerPaths` and `subLayerOffsets`.
    ///
    /// Spec: AOUSD Core §7.6.1 (`subLayers`), §10.3.1 (layer stacks),
    /// §12.3.2.1 (layer offsets).
    pub fn set_sublayers(&mut self, layer: LayerId, sublayers: Vec<SublayerEntry>) -> &mut Self {
        self.edit_sublayers(layer, SublayerEdit::Set(sublayers))
    }

    /// Inserts `sublayer` at `index` of the sublayers of `layer` (`index`
    /// equal to their number appends it), checking the list as
    /// [`Transaction::set_sublayers`] does. Earlier entries are stronger.
    ///
    /// OpenUSD: `SdfLayer::InsertSubLayerPath`.
    pub fn insert_sublayer(
        &mut self,
        layer: LayerId,
        index: usize,
        sublayer: SublayerEntry,
    ) -> &mut Self {
        self.edit_sublayers(layer, SublayerEdit::Insert(index, sublayer))
    }

    /// Removes the sublayer at `index` of `layer`'s sublayers, with its
    /// offset.
    ///
    /// OpenUSD: `SdfLayer::RemoveSubLayerPath`.
    pub fn remove_sublayer(&mut self, layer: LayerId, index: usize) -> &mut Self {
        self.edit_sublayers(layer, SublayerEdit::Remove(index))
    }

    /// Sets the time offset of the sublayer at `index` of `layer`'s
    /// sublayers.
    ///
    /// OpenUSD: `SdfLayer::SetSubLayerOffset`.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (layer offsets).
    pub fn set_sublayer_offset(
        &mut self,
        layer: LayerId,
        index: usize,
        offset: LayerOffset,
    ) -> &mut Self {
        self.edit_sublayers(layer, SublayerEdit::Offset(index, offset))
    }

    fn edit_sublayers(&mut self, layer: LayerId, edit: SublayerEdit) -> &mut Self {
        self.push(Op::EditSublayers {
            layer,
            edit: Box::new(edit),
        })
    }

    /// Replaces the `layerRelocates` of `layer`.
    ///
    /// An entry composition would ignore is rejected, where OpenUSD accepts
    /// it and reports it when composing: one whose source is a root prim,
    /// or whose target is the source, an ancestor or a descendant of it
    /// ([`Rejection::InvalidRelocate`]), and a second entry for one source
    /// ([`Rejection::DuplicateRelocateSource`]). Whether an entry is valid
    /// against the rest of its layer stack is left to composition, which
    /// reports it.
    ///
    /// OpenUSD: `SdfLayer::SetRelocates`.
    ///
    /// Spec: AOUSD Core §10.3.2.6 (relocates).
    ///
    /// [`Rejection::InvalidRelocate`]: super::Rejection::InvalidRelocate
    /// [`Rejection::DuplicateRelocateSource`]: super::Rejection::DuplicateRelocateSource
    pub fn set_layer_relocates(&mut self, layer: LayerId, relocates: Vec<Relocate>) -> &mut Self {
        self.push(Op::SetRelocates { layer, relocates })
    }

    /// Clears the `layerRelocates` of `layer`.
    ///
    /// OpenUSD: `SdfLayer::ClearRelocates`.
    pub fn clear_layer_relocates(&mut self, layer: LayerId) -> &mut Self {
        self.set_layer_relocates(layer, Vec::new())
    }

    /// Clears one authored layer field without erasing weaker stage opinions.
    pub fn clear_layer_metadata(&mut self, layer: LayerId, key: TokenId) -> &mut Self {
        self.push(Op::LayerMetadata {
            layer,
            key,
            value: None,
        })
    }

    /// Requires the authored layer field to match before any edit is applied.
    /// Values compare by authored representation, including float bit patterns.
    pub fn expect_layer_metadata(
        &mut self,
        layer: LayerId,
        key: TokenId,
        expected: Option<FieldValue>,
    ) -> &mut Self {
        self.preconditions.push(Precondition::LayerMetadata {
            layer,
            key,
            expected: Box::new(expected),
        });
        self
    }

    /// Creates an empty transaction.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the transaction has no edits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Returns the number of edits.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Consumes `other`, appending its edits and preconditions without applying
    /// either transaction. Edits retain their order; all preconditions are checked
    /// against the initial store before any combined edit runs.
    ///
    /// Applying the result is one atomic operation with one inverse. Conflicting
    /// guards or rejected edits reject the whole batch. Both transactions must
    /// address the same store and use that store's interned paths and tokens.
    pub fn append(&mut self, mut other: Self) -> &mut Self {
        self.preconditions.append(&mut other.preconditions);
        self.ops.append(&mut other.ops);
        self
    }

    /// Creates the prim spec at `at` with `specifier` and `type_name`,
    /// creating missing ancestors as `over`s and missing variant branches
    /// on the way. `at` may also name a variant (`/Rock{shape=smooth}`),
    /// which is created in its variant set; `specifier` and `type_name` do
    /// not apply to it.
    ///
    /// Rejected if the spec exists.
    ///
    /// OpenUSD: `SdfPrimSpec::New`, `SdfCreatePrimInLayer`,
    /// `SdfVariantSpec::New`.
    pub fn create_prim(
        &mut self,
        at: Address,
        specifier: Specifier,
        type_name: Option<TokenId>,
    ) -> &mut Self {
        self.push(Op::CreatePrim {
            at,
            specifier,
            type_name,
        })
    }

    /// Creates the property spec `spec` at `at`, creating its prim spec if
    /// needed. An attribute's declared type must be a registered value type
    /// ([`PropertyType::parse`](crate::PropertyType::parse)), and a default
    /// or time samples in `spec` must conform to it.
    ///
    /// Rejected if the property spec exists, and for an unregistered type
    /// ([`Rejection::UnknownValueType`](super::Rejection::UnknownValueType)).
    ///
    /// OpenUSD: `SdfAttributeSpec::New`, `SdfRelationshipSpec::New`.
    ///
    /// Spec: AOUSD Core §6.2 (value types), §7.6.4.1.1 (`typeName`).
    pub fn create_property(&mut self, at: Address, spec: PropertySpec) -> &mut Self {
        self.push(Op::CreateProperty {
            at,
            spec: Box::new(spec),
        })
    }

    /// Copies the prim spec at `from`, with every spec beneath it, or the
    /// property spec at `from`, to `to`, in the same or another layer. The
    /// copy replaces whatever `to` holds, and missing ancestors of `to` are
    /// created as `over`s, as the other edits create them.
    ///
    /// Paths the copied specs author that point into the copied namespace
    /// follow it to `to`: relationship targets, attribute connections,
    /// inherits, specializes, internal references and payloads, and path
    /// expressions (OpenUSD leaves those pointing at the source). For a
    /// property, that namespace is its prim's. External arcs and paths
    /// elsewhere stay as they are; internal arcs copied to another layer
    /// are anchored there. Specs inside an ancestor's variant branches are
    /// not part of the copy.
    ///
    /// A prim and a property copy only to their own kind
    /// ([`Rejection::CopyBetweenKinds`]); variant-qualified paths are
    /// rejected ([`Rejection::VariantCopy`]), and so is a copy within one
    /// layer onto, into or around its source ([`Rejection::CopyIntoSource`]).
    ///
    /// OpenUSD: `SdfCopySpec`.
    ///
    /// Spec: AOUSD Core §7 (scene description), §8 (spec paths).
    ///
    /// [`Rejection::CopyBetweenKinds`]: super::Rejection::CopyBetweenKinds
    /// [`Rejection::VariantCopy`]: super::Rejection::VariantCopy
    /// [`Rejection::CopyIntoSource`]: super::Rejection::CopyIntoSource
    pub fn copy_spec(&mut self, from: Address, to: Address) -> &mut Self {
        self.push(Op::CopySpec { from, to })
    }

    /// Removes the prim, variant or property spec at `at`, with everything
    /// authored beneath it. Removing a spec that does not exist does
    /// nothing.
    ///
    /// OpenUSD: `SdfPrimSpec::RemoveNameChild`,
    /// `SdfVariantSetSpec::RemoveVariant`, `SdfPrimSpec::RemoveProperty`.
    pub fn remove_spec(&mut self, at: Address) -> &mut Self {
        self.push(Op::RemoveSpec { at })
    }

    /// Sets the targets of the relationship, or the connections of the
    /// attribute, at `at` to the list op `targets`, replacing any authored
    /// on that spec. A missing property spec is created: with the prim's
    /// schema definition of the property when it has one, else with the
    /// stage's composed declaration (for a stage address applied through
    /// [`LiveStage::apply`](crate::LiveStage::apply)), whose `custom`
    /// qualifier it keeps, else as a `custom` relationship, as OpenUSD
    /// stamps one for a property nothing declares. An attribute created
    /// this way needs a declared type.
    ///
    /// Target paths are named as the address is: through an edit target
    /// ([`EditTarget::property`](super::EditTarget::property)) they are
    /// stage paths, each mapped to the layer's namespace as the property's
    /// own path is, and one the target does not map rejects the edit
    /// ([`Rejection::UnmappableTarget`](super::Rejection::UnmappableTarget)); with a spec address
    /// ([`Address::spec`](super::Address::spec)) they are the layer's own
    /// paths, authored as given. Mapping at the address keeps every
    /// caller's targets in the same namespace as its other paths.
    ///
    /// OpenUSD: `UsdRelationship::SetTargets`,
    /// `UsdAttribute::SetConnections`, `SdfPropertySpec`'s
    /// `targetPaths` / `connectionPaths` list ops.
    ///
    /// Spec: AOUSD Core §7.6.4.2.3 (`connectionPaths`), §7.6.5.1.1
    /// (`targetPaths`), §12.4.
    pub fn set_targets(&mut self, at: Address, targets: ListOp<TargetPath>) -> &mut Self {
        self.push(Op::SetTargets {
            at,
            targets: Some(targets),
        })
    }

    /// Adds `target` to the targets of the relationship, or the
    /// connections of the attribute, at `at`, at `position` (see
    /// [`ListPosition`]): an explicit list takes it whatever the position,
    /// and a target already in the chosen list moves. The target is not
    /// taken out of the deleted targets or the other list, so adding a
    /// deleted target leaves it deleted at that strength and listed after.
    /// A missing property spec is created as [`Transaction::set_targets`]
    /// creates it, and `target` is mapped as it maps targets; so a property
    /// nothing declares becomes a `custom` relationship, and an attribute
    /// nothing declares takes connections once it is created with its type
    /// ([`Transaction::create_property`]), as `AddConnection` fails for it.
    ///
    /// Each call edits the list op of the one spec the address names, so a
    /// layer can append targets to, or delete them from, what weaker layers
    /// author, where [`Transaction::set_targets`] with an explicit list
    /// replaces them.
    ///
    /// OpenUSD: `UsdRelationship::AddTarget`, `UsdAttribute::AddConnection`,
    /// with `Usd_InsertListItem` (`pxr/usd/usd/relationship.cpp`,
    /// `pxr/usd/usd/valueUtils.h`).
    ///
    /// Spec: AOUSD Core §7.6.4.2.3 (`connectionPaths`), §7.6.5.1.1
    /// (`targetPaths`), §12.4 (list ops).
    pub fn add_target(
        &mut self,
        at: Address,
        target: TargetPath,
        position: ListPosition,
    ) -> &mut Self {
        self.push(Op::EditTargetList {
            at,
            target,
            position: Some(position),
        })
    }

    /// Removes `target` from the targets of the relationship, or the
    /// connections of the attribute, at `at`: from the explicit list when
    /// the spec's list op is explicit, and otherwise from its prepended,
    /// appended and added targets, adding it to its deleted targets (once),
    /// which removes it from weaker opinions too. A missing property spec
    /// is created as [`Transaction::set_targets`] creates it, holding the
    /// deletion, and `target` is mapped as it maps targets.
    ///
    /// OpenUSD: `UsdRelationship::RemoveTarget`,
    /// `UsdAttribute::RemoveConnection`, with `SdfListEditorProxy::Remove`
    /// (`pxr/usd/usd/relationship.cpp`, `pxr/usd/sdf/listEditorProxy.h`).
    ///
    /// Spec: AOUSD Core §7.6.4.2.3, §7.6.5.1.1, §12.4.
    pub fn remove_target(&mut self, at: Address, target: TargetPath) -> &mut Self {
        self.push(Op::EditTargetList {
            at,
            target,
            position: None,
        })
    }

    /// Removes the targets or connections authored on the property spec at
    /// `at`, keeping the spec. Clearing nothing does nothing.
    ///
    /// OpenUSD: `UsdRelationship::ClearTargets`,
    /// `UsdAttribute::ClearConnections`.
    pub fn clear_targets(&mut self, at: Address) -> &mut Self {
        self.push(Op::SetTargets { at, targets: None })
    }

    /// Applies the API schema `name` (`LabelAPI`, or `CollectionAPI:inst`
    /// for a multiple-apply instance) to the prim spec at `at`, creating
    /// the spec as an `over` if needed: `name` is added to the end of the
    /// spec's `apiSchemas` prepends, or of its explicit list when the list
    /// op is explicit, unless the op already prepends, appends or lists
    /// it. Deleted items are left as they are.
    ///
    /// This authors the spec only; whether the schema may be applied is
    /// the caller's question ([`crate::SchemaRegistry::can_apply`]).
    ///
    /// OpenUSD: `UsdPrim::AddAppliedSchema`, which `UsdPrim::ApplyAPI`
    /// calls.
    ///
    /// Spec: AOUSD Core §13.2.1.2 (`apiSchemas`), §13.3.2.
    pub fn add_applied_schema(&mut self, at: Address, name: TokenId) -> &mut Self {
        self.push(Op::AddAppliedSchema { at, name })
    }

    /// Removes the API schema `name` from the prim spec at `at`, creating
    /// the spec as an `over` if needed: `name` is removed from the explicit
    /// list when the spec's `apiSchemas` list op is explicit, and otherwise
    /// from its prepended and appended items and added to its deleted
    /// items, which removes weaker opinions' applications too.
    ///
    /// OpenUSD: `UsdPrim::RemoveAppliedSchema`, which `UsdPrim::RemoveAPI`
    /// calls.
    ///
    /// Spec: AOUSD Core §13.2.1.2 (`apiSchemas`), §12.4 (list ops).
    pub fn remove_applied_schema(&mut self, at: Address, name: TokenId) -> &mut Self {
        self.push(Op::RemoveAppliedSchema { at, name })
    }

    /// Sets the specifier of the prim spec at `at`, creating the spec if
    /// needed. A variant spec has none ([`Rejection::NotOnVariant`]).
    ///
    /// OpenUSD: `UsdPrim::SetSpecifier`.
    ///
    /// Spec: AOUSD Core §7.6 (specifier), §12.2.1 (specifier resolution).
    ///
    /// [`Rejection::NotOnVariant`]: super::Rejection::NotOnVariant
    pub fn set_specifier(&mut self, at: Address, specifier: Specifier) -> &mut Self {
        self.set_field(at, PrimValue::Specifier(Some(specifier)))
    }

    /// Sets the type name of the prim spec at `at`, creating the spec as an
    /// `over` if needed. A variant spec has none.
    ///
    /// OpenUSD: `UsdPrim::SetTypeName`.
    ///
    /// Spec: AOUSD Core §7.6 (`typeName`), §12.2.3 (type name resolution).
    pub fn set_type_name(&mut self, at: Address, type_name: TokenId) -> &mut Self {
        self.set_field(at, PrimValue::TypeName(Some(type_name)))
    }

    /// Clears the type name of the prim spec at `at`. Clearing a field of a
    /// spec that does not exist does nothing, here and for every `clear_`
    /// edit of a prim field.
    ///
    /// OpenUSD: `UsdPrim::ClearTypeName`.
    pub fn clear_type_name(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::TypeName(None))
    }

    /// Sets the `active` opinion of the prim or variant spec at `at`,
    /// creating the spec as an `over` if needed. An inactive prim's
    /// descendants are not composed.
    ///
    /// OpenUSD: `UsdPrim::SetActive`.
    ///
    /// Spec: AOUSD Core §7.6 (`active`), §11 (stage population).
    pub fn set_active(&mut self, at: Address, active: bool) -> &mut Self {
        self.set_field(at, PrimValue::Active(Some(active)))
    }

    /// Clears the `active` opinion of the spec at `at`.
    ///
    /// OpenUSD: `UsdPrim::ClearActive`.
    pub fn clear_active(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::Active(None))
    }

    /// Sets the `instanceable` opinion of the prim or variant spec at `at`,
    /// creating the spec as an `over` if needed.
    ///
    /// OpenUSD: `UsdPrim::SetInstanceable`.
    ///
    /// Spec: AOUSD Core §11.3.3 (instanceable).
    pub fn set_instanceable(&mut self, at: Address, instanceable: bool) -> &mut Self {
        self.set_field(at, PrimValue::Instanceable(Some(instanceable)))
    }

    /// Clears the `instanceable` opinion of the spec at `at`.
    ///
    /// OpenUSD: `UsdPrim::ClearInstanceable`.
    pub fn clear_instanceable(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::Instanceable(None))
    }

    /// Sets the child order (`reorder nameChildren`) of the prim spec at
    /// `at`, creating the spec as an `over` if needed. Names are authored
    /// as given; an empty order is authored too. A variant spec has none.
    /// On the pseudo-root this is the layer's root prim order (`reorder
    /// rootPrims`), the one prim field the pseudo-root holds.
    ///
    /// OpenUSD: `UsdPrim::SetChildrenReorder`; on the pseudo-root,
    /// `SdfLayer::SetRootPrimOrder`.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields), §11 (child order).
    pub fn set_prim_order(&mut self, at: Address, order: Vec<TokenId>) -> &mut Self {
        self.set_field(at, PrimValue::PrimOrder(Some(order)))
    }

    /// Clears the child order of the prim spec at `at`.
    ///
    /// OpenUSD: `UsdPrim::ClearChildrenReorder`.
    pub fn clear_prim_order(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::PrimOrder(None))
    }

    /// Sets the property order (`reorder properties`) of the prim or variant
    /// spec at `at`, creating the spec as an `over` if needed. Names are
    /// authored as given.
    ///
    /// OpenUSD: `UsdPrim::SetPropertyOrder`.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    pub fn set_property_order(&mut self, at: Address, order: Vec<TokenId>) -> &mut Self {
        self.set_field(at, PrimValue::PropertyOrder(Some(order)))
    }

    /// Clears the property order of the spec at `at`.
    ///
    /// OpenUSD: `UsdPrim::ClearPropertyOrder`.
    pub fn clear_property_order(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::PropertyOrder(None))
    }

    /// Sets the references list op of the prim or variant spec at `at`,
    /// replacing the one authored there, and creating the spec as an `over`
    /// if needed.
    ///
    /// Arc paths are named as the address is: through an edit target the
    /// paths of internal arcs (no asset, anchored at the target's layer;
    /// see [`ReferenceSource::Layer`](crate::ReferenceSource::Layer)) are stage paths, mapped to the layer's
    /// namespace as relationship targets are
    /// ([`Transaction::set_targets`]), and one the target does not map
    /// rejects the edit. An external arc names a path in its asset and is
    /// authored as given, with its layer offset. An arc to the pseudo-root
    /// is rejected.
    ///
    /// OpenUSD: `UsdReferences::SetReferences`.
    ///
    /// Spec: AOUSD Core §10.3.2 (references), §12.4 (list ops).
    pub fn set_references(&mut self, at: Address, references: ListOp<Reference>) -> &mut Self {
        self.set_field(at, PrimValue::References(references))
    }

    /// Clears the references authored on the spec at `at`.
    ///
    /// OpenUSD: `UsdReferences::ClearReferences`.
    pub fn clear_references(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::References(ListOp::default()))
    }

    /// Adds `reference` to the references of the prim or variant spec at
    /// `at` at `position` (see [`ListPosition`]), creating the spec as an
    /// `over` if needed. Its path is mapped as
    /// [`Transaction::set_references`] maps arc paths.
    ///
    /// OpenUSD: `UsdReferences::AddReference`.
    ///
    /// Spec: AOUSD Core §10.3.2, §12.4.
    pub fn add_reference(
        &mut self,
        at: Address,
        reference: Reference,
        position: ListPosition,
    ) -> &mut Self {
        self.edit_arc(at, ArcItem::Reference(reference), Some(position))
    }

    /// Removes `reference` from the references of the spec at `at`: from its
    /// explicit list when the list op is explicit, and otherwise from its
    /// prepended and appended items, adding it to its deleted items, which
    /// removes weaker opinions' arcs too. The spec is created as an `over`
    /// if needed.
    ///
    /// OpenUSD: `UsdReferences::RemoveReference`.
    ///
    /// Spec: AOUSD Core §10.3.2, §12.4.
    pub fn remove_reference(&mut self, at: Address, reference: Reference) -> &mut Self {
        self.edit_arc(at, ArcItem::Reference(reference), None)
    }

    /// Sets the payloads list op of the spec at `at`, as
    /// [`Transaction::set_references`] sets references.
    ///
    /// OpenUSD: `UsdPayloads::SetPayloads`.
    ///
    /// Spec: AOUSD Core §10 (payloads), §12.4.
    pub fn set_payloads(&mut self, at: Address, payloads: ListOp<Reference>) -> &mut Self {
        self.set_field(at, PrimValue::Payloads(payloads))
    }

    /// Clears the payloads authored on the spec at `at`.
    ///
    /// OpenUSD: `UsdPayloads::ClearPayloads`.
    pub fn clear_payloads(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::Payloads(ListOp::default()))
    }

    /// Adds `payload` to the payloads of the spec at `at`, as
    /// [`Transaction::add_reference`] adds a reference.
    ///
    /// OpenUSD: `UsdPayloads::AddPayload`.
    pub fn add_payload(
        &mut self,
        at: Address,
        payload: Reference,
        position: ListPosition,
    ) -> &mut Self {
        self.edit_arc(at, ArcItem::Payload(payload), Some(position))
    }

    /// Removes `payload` from the payloads of the spec at `at`, as
    /// [`Transaction::remove_reference`] removes a reference.
    ///
    /// OpenUSD: `UsdPayloads::RemovePayload`.
    pub fn remove_payload(&mut self, at: Address, payload: Reference) -> &mut Self {
        self.edit_arc(at, ArcItem::Payload(payload), None)
    }

    /// Sets the inherits list op of the prim or variant spec at `at`,
    /// replacing the one authored there, and creating the spec as an `over`
    /// if needed. Through an edit target the paths are stage paths, each
    /// mapped to the layer's namespace; one the target does not map
    /// rejects the edit, where OpenUSD drops it and reports success.
    ///
    /// OpenUSD: `UsdInherits::SetInherits`.
    ///
    /// Spec: AOUSD Core §10 (inherits), §12.4.
    pub fn set_inherits(&mut self, at: Address, inherits: ListOp<PathId>) -> &mut Self {
        self.set_field(at, PrimValue::Inherits(inherits))
    }

    /// Clears the inherits authored on the spec at `at`.
    ///
    /// OpenUSD: `UsdInherits::ClearInherits`.
    pub fn clear_inherits(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::Inherits(ListOp::default()))
    }

    /// Adds the inherit `path` to the spec at `at` at `position`, mapping
    /// the path as [`Transaction::set_inherits`] does.
    ///
    /// OpenUSD: `UsdInherits::AddInherit`.
    pub fn add_inherit(&mut self, at: Address, path: PathId, position: ListPosition) -> &mut Self {
        self.edit_arc(at, ArcItem::Inherit(path), Some(position))
    }

    /// Removes the inherit `path` from the spec at `at`, as
    /// [`Transaction::remove_reference`] removes a reference.
    ///
    /// OpenUSD: `UsdInherits::RemoveInherit`.
    pub fn remove_inherit(&mut self, at: Address, path: PathId) -> &mut Self {
        self.edit_arc(at, ArcItem::Inherit(path), None)
    }

    /// Sets the specializes list op of the spec at `at`, as
    /// [`Transaction::set_inherits`] sets inherits.
    ///
    /// OpenUSD: `UsdSpecializes::SetSpecializes`.
    ///
    /// Spec: AOUSD Core §10 (specializes), §12.4.
    pub fn set_specializes(&mut self, at: Address, specializes: ListOp<PathId>) -> &mut Self {
        self.set_field(at, PrimValue::Specializes(specializes))
    }

    /// Clears the specializes authored on the spec at `at`.
    ///
    /// OpenUSD: `UsdSpecializes::ClearSpecializes`.
    pub fn clear_specializes(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::Specializes(ListOp::default()))
    }

    /// Adds the specialize `path` to the spec at `at`, as
    /// [`Transaction::add_inherit`] adds an inherit.
    ///
    /// OpenUSD: `UsdSpecializes::AddSpecialize`.
    pub fn add_specialize(
        &mut self,
        at: Address,
        path: PathId,
        position: ListPosition,
    ) -> &mut Self {
        self.edit_arc(at, ArcItem::Specialize(path), Some(position))
    }

    /// Removes the specialize `path` from the spec at `at`, as
    /// [`Transaction::remove_reference`] removes a reference.
    ///
    /// OpenUSD: `UsdSpecializes::RemoveSpecialize`.
    pub fn remove_specialize(&mut self, at: Address, path: PathId) -> &mut Self {
        self.edit_arc(at, ArcItem::Specialize(path), None)
    }

    /// Sets the `variantSetNames` list op of the prim or variant spec at
    /// `at` (for a variant spec, the sets nested in it), replacing the one
    /// authored there, and creating the spec as an `over` if needed. The
    /// names need no variant set spec: a listed set without one
    /// contributes no opinions. Repeated names are dropped as `SdfListOp`
    /// drops them: the first of each explicit, prepended or deleted name
    /// is kept, and the last of each appended one.
    ///
    /// Only listed sets compose, in the order the list ops of a site's
    /// layers give, weakest first, applied as list ops.
    ///
    /// OpenUSD: `UsdPrim::SetMetadata` of `variantSetNames`,
    /// `SdfPrimSpec::GetVariantSetNameList`.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops),
    /// §10.5 (variant selection).
    pub fn set_variant_set_names(&mut self, at: Address, names: ListOp<TokenId>) -> &mut Self {
        self.set_field(at, PrimValue::VariantSetNames(names))
    }

    /// Clears the `variantSetNames` authored on the spec at `at`, leaving
    /// its variant set specs, which then compose only where another
    /// opinion lists them.
    ///
    /// OpenUSD: `UsdPrim::ClearMetadata` of `variantSetNames`.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`).
    pub fn clear_variant_set_names(&mut self, at: Address) -> &mut Self {
        self.set_field(at, PrimValue::VariantSetNames(ListOp::default()))
    }

    /// Adds the variant set `name` to the prim or variant spec at `at`:
    /// creates the spec as an `over` if needed, creates the variant set
    /// spec unless the spec holds one, and puts `name` in the spec's
    /// `variantSetNames` at `position`, as [`Transaction::add_inherit`]
    /// puts an inherit: an explicit list takes it whatever the position,
    /// and a name already in the chosen list moves. The name is not taken
    /// out of the deleted names or the other list, so adding a deleted set
    /// leaves it deleted at that strength and listed after.
    ///
    /// Rejected if `name` is not an identifier
    /// ([`Rejection::InvalidVariantSetName`]), and for the pseudo-root
    /// ([`Rejection::PseudoRoot`]) and a property path.
    ///
    /// OpenUSD: `UsdVariantSets::AddVariantSet`, with
    /// `Usd_InsertListItem` (`pxr/usd/usd/variantSets.cpp`,
    /// `pxr/usd/usd/valueUtils.h`).
    ///
    /// Spec: AOUSD Core §7.3.6 (variant set specs), §7.6.2.3.5
    /// (`variantSetNames`), §12.4 (list ops).
    ///
    /// [`Rejection::InvalidVariantSetName`]: super::Rejection::InvalidVariantSetName
    /// [`Rejection::PseudoRoot`]: super::Rejection::PseudoRoot
    pub fn add_variant_set(
        &mut self,
        at: Address,
        name: TokenId,
        position: ListPosition,
    ) -> &mut Self {
        self.push(Op::AddVariantSet { at, name, position })
    }

    /// Removes the variant set spec `name` of the prim or variant spec at
    /// `at` with everything in it: its variant specs and the prim and
    /// property specs authored in them, nested variant sets included. The
    /// spec's `variantSetNames` is left as it is, so a set it lists stays
    /// listed, without opinions. Removing a set the spec does not hold,
    /// or from a spec that does not exist, does nothing.
    ///
    /// Rejected for a property path.
    ///
    /// OpenUSD: `SdfPrimSpec::RemoveVariantSet`, which erases the set from
    /// `variantSetChildren` only (`pxr/usd/sdf/primSpec.cpp`).
    ///
    /// Spec: AOUSD Core §7.3.6 (variant set specs), §7.6.2.3.5
    /// (`variantSetNames`).
    pub fn remove_variant_set(&mut self, at: Address, name: TokenId) -> &mut Self {
        self.push(Op::RemoveVariantSet { at, name })
    }

    fn set_field(&mut self, at: Address, value: PrimValue) -> &mut Self {
        self.push(Op::SetPrimField { at, value })
    }

    fn edit_arc(
        &mut self,
        at: Address,
        item: ArcItem,
        position: Option<ListPosition>,
    ) -> &mut Self {
        self.push(Op::EditArc {
            at,
            edit: Box::new(ArcEdit { item, position }),
        })
    }

    /// Sets the default value of the attribute at `at`.
    ///
    /// The value must conform to the attribute's declared type: the type
    /// authored on the edited spec, or else, for a stage address applied
    /// through [`LiveStage::apply`](crate::LiveStage::apply), the composed
    /// declaration of the stage property. A missing attribute spec is
    /// created with that type. [`Value::Blocked`] blocks the value.
    ///
    /// OpenUSD: `UsdAttribute::Set` at the default time,
    /// `SdfAttributeSpec::SetDefaultValue`.
    ///
    /// Spec: AOUSD Core §7.6.4.2.1 (`default`), §12.3 (value blocks).
    pub fn set_default(&mut self, at: Address, value: Value) -> &mut Self {
        self.push(Op::SetDefault { at, value })
    }

    /// Blocks an attribute's value in the target layer: clears its time samples
    /// and spline, then authors a blocked default. Declaration, connections and
    /// metadata are preserved. A missing local spec uses the composed declaration.
    /// Undo guards the default and animation slots, retaining unrelated later edits.
    ///
    /// OpenUSD: `UsdAttribute::Block`; AOUSD Core §12.3.6 (value blocking).
    pub fn block_attribute(&mut self, at: Address) -> &mut Self {
        self.push(Op::BlockAttribute { at })
    }

    /// Clears the default value of the attribute at `at`; the attribute
    /// spec stays.
    ///
    /// OpenUSD: `SdfAttributeSpec::ClearDefaultValue`.
    pub fn clear_default(&mut self, at: Address) -> &mut Self {
        self.push(Op::ClearDefault { at })
    }

    /// Sets the one time sample at `time` of the attribute at `at`, leaving
    /// every other sample as it is. For a stage address, `time` is a stage
    /// time, mapped through the target's layer offset; for a spec address
    /// it is a layer time. Types are checked as for
    /// [`Transaction::set_default`].
    ///
    /// OpenUSD: `UsdAttribute::Set` at a time, `SdfLayer::SetTimeSample`.
    ///
    /// Spec: AOUSD Core §7.6.4.2.2 (`timeSamples`), §12.3.2.1 (layer
    /// offsets).
    pub fn set_time_sample(&mut self, at: Address, time: f64, value: Value) -> &mut Self {
        self.push(Op::SetTimeSample { at, time, value })
    }

    /// Removes the one time sample at `time` of the attribute at `at`,
    /// mapping `time` as [`Transaction::set_time_sample`] does. Removing
    /// the last sample removes the `timeSamples` field.
    ///
    /// OpenUSD: `SdfLayer::EraseTimeSample`.
    pub fn remove_time_sample(&mut self, at: Address, time: f64) -> &mut Self {
        self.push(Op::RemoveTimeSample { at, time })
    }

    /// Sets the metadata field `key` of the prim, variant or property spec
    /// at `at`, creating a missing prim or variant spec. A property spec
    /// must exist.
    ///
    /// Fields that the layer model keeps in dedicated members are rejected:
    /// they have their own edits ([`Transaction::set_active`],
    /// [`Transaction::add_reference`] and the others for the
    /// [`PrimField`]s, [`Transaction::set_default`],
    /// [`Transaction::set_targets`], [`Transaction::set_variant_selection`])
    /// or none.
    ///
    /// OpenUSD: `SdfSpec::SetInfo`.
    ///
    /// Spec: AOUSD Core §7.4 (metadata fields).
    pub fn set_metadata(&mut self, at: Address, key: TokenId, value: FieldValue) -> &mut Self {
        self.push(Op::SetMetadata { at, key, value })
    }

    /// Blocks the metadata field `key` at `at`: authors a value block, which
    /// discards weaker opinions of the field.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution), §12.3 (value blocks).
    pub fn block_metadata(&mut self, at: Address, key: TokenId) -> &mut Self {
        self.set_metadata(at, key, FieldValue::Value(Value::Blocked))
    }

    /// Clears the metadata field `key` of the spec at `at`.
    ///
    /// OpenUSD: `SdfSpec::ClearInfo`.
    pub fn clear_metadata(&mut self, at: Address, key: TokenId) -> &mut Self {
        self.push(Op::ClearMetadata { at, key })
    }

    /// Sets (`Some`) or clears (`None`) the selection for the variant set
    /// `set` authored on the prim or variant spec at `at`, creating a
    /// missing spec when setting.
    ///
    /// OpenUSD: `UsdVariantSet::SetVariantSelection`,
    /// `SdfPrimSpec::SetVariantSelection`.
    ///
    /// Spec: AOUSD Core §10.5 (variant selection).
    pub fn set_variant_selection(
        &mut self,
        at: Address,
        set: TokenId,
        variant: Option<TokenId>,
    ) -> &mut Self {
        self.push(Op::SetVariantSelection { at, set, variant })
    }

    /// Requires `layer` to be at `generation` ([`Layer::generation`]) when
    /// the transaction is applied.
    ///
    /// [`Layer::generation`]: crate::Layer::generation
    pub fn expect_generation(&mut self, layer: LayerId, generation: u64) -> &mut Self {
        self.preconditions
            .push(Precondition::Generation { layer, generation });
        self
    }

    /// Requires the default value of the attribute at `at` to be `expected`
    /// (`None`: no default, or no spec) when the transaction is applied.
    pub fn expect_default(&mut self, at: Address, expected: Option<Value>) -> &mut Self {
        self.expect(at, Slot::Default, expected.map(Authored::Value))
    }

    /// Requires the time sample at `time` of the attribute at `at` to be
    /// `expected` when the transaction is applied; `time` maps as for
    /// [`Transaction::set_time_sample`].
    pub fn expect_time_sample(
        &mut self,
        at: Address,
        time: f64,
        expected: Option<Value>,
    ) -> &mut Self {
        self.expect(at, Slot::TimeSample(time), expected.map(Authored::Value))
    }

    /// Requires the metadata field `key` of the spec at `at` to be
    /// `expected` when the transaction is applied.
    pub fn expect_metadata(
        &mut self,
        at: Address,
        key: TokenId,
        expected: Option<FieldValue>,
    ) -> &mut Self {
        self.expect(at, Slot::Metadata(key), expected.map(Authored::Field))
    }

    /// Requires the selection for `set` authored on the spec at `at` to be
    /// `expected` when the transaction is applied.
    pub fn expect_variant_selection(
        &mut self,
        at: Address,
        set: TokenId,
        expected: Option<TokenId>,
    ) -> &mut Self {
        self.expect(
            at,
            Slot::VariantSelection(set),
            expected.map(Authored::Token),
        )
    }

    /// Prepares the transaction against the layers as they are now:
    /// requires every layer it edits to keep its current generation, and
    /// every slot it edits to keep its current authored value, until it is
    /// applied.
    ///
    /// An edit prepared this way fails with [`EditError::StaleGeneration`]
    /// or [`EditError::StaleValue`] once anything changed its layers in the
    /// meantime, even when the changed value is back to what it was. For
    /// an inverse this adds the generations of the layers it restores; its
    /// steps check their slots already.
    /// Addresses the store cannot resolve yet are left for
    /// [`Transaction::apply`] to reject.
    pub fn expect_unchanged(&mut self, store: &mut dyn LayerStore) -> &mut Self {
        let mut layers: Vec<LayerId> = Vec::new();
        let mut expectations = Vec::new();
        for op in &self.ops {
            let layer = match op {
                Op::Raw(guarded) => guarded.step.layer(),
                Op::LayerMetadata { layer, .. } => *layer,
                _ => op
                    .slot()
                    .map(|(at, _)| at.layer())
                    .expect("a slot per edit"),
            };
            if !layers.contains(&layer) {
                layers.push(layer);
            }
            // Inverse steps check their slots themselves.
            let Some((at, slot)) = op.slot() else {
                continue;
            };
            if let Some(found) = authored(store, at, &slot) {
                expectations.push(Precondition::Authored {
                    at: at.clone(),
                    slot,
                    expected: Box::new(found),
                });
            }
        }
        for layer in layers {
            if let Some(found) = store.layer(layer) {
                let generation = found.generation();
                self.expect_generation(layer, generation);
            }
        }
        self.preconditions.extend(expectations);
        self
    }

    /// Applies the transaction to the layers of `store` and returns its
    /// inverse; see the [type docs](Self).
    ///
    /// Preconditions are checked first, against the layers as they are.
    /// Then the edits are applied in order, each seeing the effects of the
    /// ones before it. If a precondition fails or an edit is rejected, the
    /// edits applied so far are undone and every layer, generation
    /// included, is left as it was.
    ///
    /// No stage is told about the edits: use
    /// [`LiveStage::apply`](crate::LiveStage::apply) to recompose one.
    pub fn apply(&self, store: &mut dyn LayerStore) -> Result<Self, EditError> {
        apply::apply(store, self, None, None).map(|applied| applied.inverse)
    }

    fn push(&mut self, op: Op) -> &mut Self {
        self.ops.push(op);
        self
    }

    fn expect(&mut self, at: Address, slot: Slot, expected: Option<Authored>) -> &mut Self {
        self.preconditions.push(Precondition::Authored {
            at,
            slot,
            expected: Box::new(expected),
        });
        self
    }
}

impl Op {
    /// The slot this edit writes, for [`Transaction::expect_unchanged`].
    fn slot(&self) -> Option<(&Address, Slot)> {
        Some(match self {
            Self::CreatePrim { at, .. } | Self::RemoveSpec { at } => (at, Slot::Spec),
            Self::CreateProperty { at, .. } | Self::BlockAttribute { at } => (at, Slot::Spec),
            Self::SetDefault { at, .. } | Self::ClearDefault { at } => (at, Slot::Default),
            Self::SetTimeSample { at, time, .. } | Self::RemoveTimeSample { at, time } => {
                (at, Slot::TimeSample(*time))
            }
            Self::SetMetadata { at, key, .. } | Self::ClearMetadata { at, key } => {
                (at, Slot::Metadata(*key))
            }
            Self::SetVariantSelection { at, set, .. } => (at, Slot::VariantSelection(*set)),
            Self::SetTargets { at, .. } | Self::EditTargetList { at, .. } => (at, Slot::Targets),
            Self::AddAppliedSchema { at, .. } | Self::RemoveAppliedSchema { at, .. } => {
                (at, Slot::AppliedSchemas)
            }
            Self::SetPrimField { at, value } => (at, Slot::Prim(value.field())),
            Self::EditArc { at, edit } => (at, Slot::Prim(edit.item.field())),
            Self::Raw(_)
            | Self::LayerMetadata { .. }
            | Self::EditSublayers { .. }
            | Self::SetRelocates { .. } => return None,
            Self::CopySpec { to, .. } => (to, Slot::Spec),
            Self::AddVariantSet { at, .. } => (at, Slot::Prim(PrimField::VariantSetNames)),
            Self::RemoveVariantSet { at, .. } => (at, Slot::Spec),
        })
    }
}

/// The value authored in `slot` at `at`: `Some(None)` when nothing is
/// authored there, `None` when `at` does not resolve.
pub(crate) fn authored(
    store: &mut dyn LayerStore,
    at: &Address,
    slot: &Slot,
) -> Option<Option<Authored>> {
    let path = at.resolve(store.paths_mut())?;
    let loc = Loc::of(&path);
    let layer = store.layer(at.layer())?;
    let Some(spec) = spec_at(layer, &loc) else {
        return Some(None);
    };
    let property = path
        .property()
        .and_then(|name| get_property(spec.properties(), name));
    Some(match slot {
        Slot::Spec => match path.property() {
            Some(_) => property.cloned().map(|p| Authored::Property(Box::new(p))),
            None => Some(Authored::Spec),
        },
        Slot::Default => property
            .and_then(|p| p.default.clone())
            .map(Authored::Value),
        Slot::Animation => property.map(|p| {
            Authored::Animation(Box::new(AnimationState {
                samples: p.time_samples.clone(),
                spline: p.spline.clone(),
            }))
        }),
        Slot::TimeSample(time) => {
            let time = at.layer_time(*time);
            property
                .and_then(|p| p.time_samples.as_deref())
                .and_then(|samples| samples.iter().find(|(t, _)| t.total_cmp(&time).is_eq()))
                .map(|(_, value)| Authored::Value(value.clone()))
        }
        Slot::Metadata(key) => match path.property() {
            Some(_) => property.and_then(|p| p.metadata(*key)).cloned(),
            None => crate::doc::get_field(spec.fields(), key).cloned(),
        }
        .map(Authored::Field),
        Slot::VariantSelection(set) => spec.variant_selection(*set).map(Authored::Token),
        Slot::AppliedSchemas => store
            .tokens()
            .lookup("apiSchemas")
            .and_then(|key| crate::doc::get_field(spec.fields(), &key).cloned())
            .map(Authored::Field),
        Slot::Targets => property
            .and_then(|p| p.targets.clone())
            .map(|targets| Authored::Field(FieldValue::PathListOp(targets))),
        // Layer slots name no spec; preconditions guard them by generation.
        Slot::Sublayers | Slot::Relocates => return None,
        Slot::Prim(field) => super::prim_fields::read(&*store, spec, *field)
            .filter(|value| !value.is_clear())
            .map(|value| Authored::Prim(Box::new(value))),
    })
}
