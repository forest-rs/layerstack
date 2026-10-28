// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Why a transaction was not applied.

use alloc::sync::Arc;
use core::fmt;

use crate::{
    diagnostic::{Describe, Described, Names, with_store},
    doc::{LayerId, LayerStore},
    interner::TokenId,
    path::TargetPath,
    spec_path::SpecPath,
};

/// Why a [`Transaction`](super::Transaction) was not applied. When one is
/// returned, no layer was changed.
///
/// [`EditError::display`] writes it in words, its paths, names and slots
/// as text; its `Display` writes the same with ids, having no store.
#[derive(Clone, Debug, PartialEq)]
pub enum EditError {
    /// `layer` is not at the generation the transaction expects: it was
    /// edited after the transaction was prepared.
    StaleGeneration {
        /// The edited layer.
        layer: LayerId,
        /// The generation the transaction expects.
        expected: u64,
        /// The layer's generation.
        found: u64,
    },
    /// The authored value the transaction expects in `slot` of the spec at
    /// `path` in `layer` is not there: a value precondition failed, or, for
    /// an inverse, the slot no longer holds what the undone edit wrote.
    StaleValue {
        /// The layer of the spec.
        layer: LayerId,
        /// The spec path.
        path: SpecPath,
        /// The slot whose value changed.
        slot: Slot,
    },
    /// A precondition names a stage path its target does not map.
    UnresolvedPrecondition {
        /// The position of the precondition among the transaction's
        /// preconditions.
        index: usize,
    },
    /// An edit cannot be applied.
    Rejected {
        /// The position of the edit in the transaction.
        op: usize,
        /// Why.
        reason: Rejection,
    },
    /// The store passed to [`LiveStage::apply`](crate::LiveStage::apply) is
    /// not the one the stage was composed from: its token or path interner
    /// is another, so the transaction's ids would name other names and paths
    /// in it. Nothing was applied and the stage is unchanged.
    ///
    /// A store published in place of the stage's own, such as an
    /// [`InMemoryStore::snapshot`](crate::InMemoryStore::snapshot), is
    /// another store too: synchronize the stage against it first
    /// ([`LiveStage::synchronize`](crate::LiveStage::synchronize)), which
    /// rebuilds the stage on it.
    ForeignStore {
        /// The token and path domains the stage was composed from
        /// ([`Stage::store_identity`](crate::Stage::store_identity)).
        stage: crate::StoreIdentity,
        /// The domains of the store passed in
        /// ([`LayerStore::identity`]).
        store: crate::StoreIdentity,
    },
}

/// A slot of a spec that a precondition checks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    /// Whether the spec exists, and for a property spec its whole content.
    Spec,
    /// An attribute's default value.
    Default,
    /// The attribute's authored time-sample and spline slots together.
    Animation,
    /// An attribute's time sample at a time.
    TimeSample(f64),
    /// A relationship's targets or an attribute's connections.
    Targets,
    /// A prim spec's `apiSchemas` list op.
    AppliedSchemas,
    /// A metadata field.
    Metadata(TokenId),
    /// The selection for a variant set.
    VariantSelection(TokenId),
    /// A prim spec field kept in a dedicated member.
    Prim(super::PrimField),
    /// A layer's sublayers and their offsets.
    Sublayers,
    /// A layer's `layerRelocates`.
    Relocates,
}

/// Why one edit of a transaction cannot be applied.
#[derive(Clone, Debug, PartialEq)]
pub enum Rejection {
    /// No layer with this id is in the store.
    NoSuchLayer(LayerId),
    /// The edit target does not map this stage path, the edit's address.
    Unmappable(TargetPath),
    /// The edit target does not map this relationship or connection
    /// target (a stage path).
    UnmappableTarget(TargetPath),
    /// The edit needs a property path and was given a prim or variant
    /// path.
    NotAProperty(SpecPath),
    /// The edit needs a prim or variant path and was given a property path.
    NotAPrim(SpecPath),
    /// The edit needs a spec that does not exist.
    NoSuchSpec(SpecPath),
    /// The spec to create exists.
    SpecExists(SpecPath),
    /// The property is a relationship; the edit needs an attribute.
    NotAnAttribute(SpecPath),
    /// No type is declared for the attribute, so a value cannot be checked
    /// or a spec created: author one with
    /// [`Transaction::create_property`](super::Transaction::create_property).
    UndeclaredType(SpecPath),
    /// The attribute's type is not a registered value type: a type a
    /// layer authors that no registered type has
    /// ([`PropertyType::unregistered`](crate::PropertyType::unregistered)).
    /// An attribute with it cannot be created or given a value.
    ///
    /// OpenUSD: `SdfAttributeSpec::New` rejects an invalid
    /// `SdfValueTypeName`, and `UsdAttribute::Set` an attribute whose
    /// type name the registry does not know.
    ///
    /// Spec: AOUSD Core §6.2 (value types), §7.6.4.1.1 (`typeName`).
    UnknownValueType {
        /// The attribute.
        path: SpecPath,
        /// Its type name.
        type_name: Arc<str>,
    },
    /// The value does not conform to the attribute's declared type.
    TypeMismatch {
        /// The attribute.
        path: SpecPath,
        /// Its declared type name.
        declared: Arc<str>,
        /// The role-less type of the value offered (`double`, `float3[]`).
        offered: &'static str,
    },
    /// The metadata field is kept in a dedicated member of the layer model
    /// and has its own edit, or none: the prim fields of
    /// [`PrimField`](super::PrimField) have the `set_`, `clear_`, `add_` and
    /// `remove_` edits of [`Transaction`](super::Transaction).
    ReservedField(TokenId),
    /// The edit addresses the pseudo-root (`/`) or a property path on it.
    /// The pseudo-root spec holds a layer's metadata and root prims, and
    /// no prim fields (other than the root prim order) or properties:
    /// author layer metadata with
    /// [`Transaction::set_layer_metadata`](super::Transaction::set_layer_metadata).
    ///
    /// OpenUSD rejects these edits too (`UsdStage::_SetMetadata`,
    /// `_CanEditInfoOnSpec`, `SdfPrimSpec::_ValidateEdit`,
    /// `SdfAttributeSpec::New`), except that `UsdPrim::SetMetadata` and
    /// `ClearMetadata` on the pseudo-root write a field registered on
    /// layers as layer metadata.
    ///
    /// Spec: AOUSD Core §5.1 (the pseudo-root), §7.6.1 (layer metadata).
    PseudoRoot,
    /// An arc targets the pseudo-root; arcs target prims.
    ///
    /// Spec: AOUSD Core §10 (composition arcs).
    PseudoRootArc,
    /// A variant spec does not hold this prim field: its specifier, type
    /// name and child order belong to the prim spec hosting the variant set.
    /// OpenUSD accepts such an edit inside a variant edit context and
    /// authors nothing.
    ///
    /// Spec: AOUSD Core §7.6.7 (variant specs).
    NotOnVariant(super::PrimField),
    /// No sublayer is at this index of the layer's sublayers (for an
    /// insertion, the index is past the end).
    NoSuchSublayer(usize),
    /// The sublayer at this index has an empty asset path.
    ///
    /// OpenUSD: `SdfSubLayerTypePolicy` rejects it.
    EmptySublayerAsset(usize),
    /// The sublayer at this index repeats an earlier one, by layer or by
    /// authored asset path.
    ///
    /// OpenUSD: `SdfSubLayerTypePolicy` rejects it.
    DuplicateSublayer(usize),
    /// Sublayering this layer makes a cycle: it is the edited layer, or its
    /// own sublayers reach it. OpenUSD accepts the edit and composes it as
    /// `PcpErrorSublayerCycle`.
    ///
    /// Spec: AOUSD Core §10.3.1 (layer stacks).
    SublayerCycle(LayerId),
    /// A relocate composition would ignore on its own. OpenUSD accepts it
    /// and composes it as `PcpErrorInvalidAuthoredRelocation`.
    ///
    /// Spec: AOUSD Core §10.3.2.6 (relocates restrictions).
    InvalidRelocate {
        /// The relocate's source.
        source: crate::PathId,
        /// Why composition would ignore it.
        reason: crate::InvalidRelocationReason,
    },
    /// Two relocates of the list move this source. OpenUSD accepts them and
    /// composes them as `PcpErrorInvalidConflictingRelocation`, ignoring
    /// both.
    ///
    /// Spec: AOUSD Core §10.3.2.6 (each source has one target).
    DuplicateRelocateSource(crate::PathId),
    /// A copy names a prim and a property, which copy only to their own
    /// kind.
    CopyBetweenKinds,
    /// A copy names a variant-qualified path; copies are of prim and
    /// property specs outside variant branches.
    VariantCopy,
    /// A copy within one layer whose destination is the source, or an
    /// ancestor or descendant of it.
    CopyIntoSource,
    /// The copied specs author paths the copy cannot remap (for example
    /// an invalid path expression, or an arc whose asset is an expression).
    UnsupportedCopy,
    /// A variant set name that is not an identifier.
    ///
    /// OpenUSD: `SdfVariantSetSpec::New` and the `variantSetNames` list
    /// editor reject it.
    ///
    /// Spec: AOUSD Core §7.3.3 (identifiers), §7.3.6 (variant set specs).
    InvalidVariantSetName(TokenId),
    /// A step of an inverse no longer matches the layer: the layer was
    /// changed in a way the inverse cannot undo.
    Diverged(SpecPath),
}

impl EditError {
    /// The stable code naming this error's kind, for hosts and bindings
    /// that branch on it without matching the enum.
    ///
    /// | Variant | Code |
    /// |---|---|
    /// | [`Self::StaleGeneration`], [`Self::StaleValue`] | `edit.stale` |
    /// | [`Self::UnresolvedPrecondition`] | `edit.unresolved_precondition` |
    /// | [`Self::Rejected`] | the reason's [`Rejection::code`], `edit.rejected.` and the reason |
    /// | [`Self::ForeignStore`] | `edit.foreign_store` |
    ///
    /// Both stale guards share `edit.stale`: either way the layer changed
    /// after the transaction (or the inverse) was made, nothing was
    /// applied, and the recovery is to read the stage again.
    ///
    /// Codes are part of the public contract: a released code keeps its
    /// spelling and meaning, and a new variant brings a new code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::StaleGeneration { .. } | Self::StaleValue { .. } => "edit.stale",
            Self::UnresolvedPrecondition { .. } => "edit.unresolved_precondition",
            Self::Rejected { reason, .. } => reason.code(),
            Self::ForeignStore { .. } => "edit.foreign_store",
        }
    }

    /// The error in words, its paths, names and slots written as text
    /// with `store`, the store the transaction was applied to ("edit 0
    /// rejected: the value is a `double`, but /World/Sun.inputs:intensity
    /// is a `float`"). `Display` writes the same without a store, with ids.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl fmt::Display + 'a {
        with_store(self, store)
    }
}

impl Describe for EditError {
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration {
                layer,
                expected,
                found,
            } => write!(
                f,
                "{} changed since the edit was prepared (generation {found}, expected {expected})",
                names.layer(*layer)
            ),
            Self::StaleValue { layer, path, slot } => write!(
                f,
                "{} of {} in {} changed since the edit was prepared",
                Described(slot, names),
                names.spec(path),
                names.layer(*layer)
            ),
            Self::UnresolvedPrecondition { index } => {
                write!(
                    f,
                    "precondition {index} names a path its target does not map"
                )
            }
            Self::Rejected { op, reason } => {
                write!(f, "edit {op} rejected: {}", Described(reason, names))
            }
            Self::ForeignStore { stage, store } => write!(
                f,
                "the store is not the one the stage was composed from: its {}; \
                 synchronize the stage against it first if it replaces that store",
                stage.differing_domains(store),
            ),
        }
    }
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.describe(f, Names(None))
    }
}

impl Slot {
    /// The slot in words ("the time sample at 5", "the `kind` metadata"),
    /// its names written with `store`.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl fmt::Display + 'a {
        with_store(self, store)
    }
}

impl Describe for Slot {
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result {
        match self {
            Self::Spec => f.write_str("the spec"),
            Self::Default => f.write_str("the default"),
            Self::Animation => f.write_str("the time samples and spline"),
            Self::TimeSample(time) => write!(f, "the time sample at {time}"),
            Self::Targets => f.write_str("the targets"),
            Self::AppliedSchemas => f.write_str("the `apiSchemas`"),
            Self::Metadata(key) => write!(f, "the `{}` metadata", names.token(*key)),
            Self::VariantSelection(set) => {
                write!(f, "the selection of variant set `{}`", names.token(*set))
            }
            Self::Prim(field) => write!(f, "the `{}`", field_name(*field)),
            Self::Sublayers => f.write_str("the sublayers"),
            Self::Relocates => f.write_str("the `layerRelocates`"),
        }
    }
}

impl Rejection {
    /// The stable code naming this rejection: `edit.rejected.` and the
    /// variant's name in `snake_case`, the code
    /// [`EditError::code`] gives a rejected edit. Every rejection's code
    /// starts with `edit.rejected.`.
    ///
    /// | Variant | Code |
    /// |---|---|
    /// | [`Self::NoSuchLayer`] | `edit.rejected.no_such_layer` |
    /// | [`Self::Unmappable`] | `edit.rejected.unmappable` |
    /// | [`Self::UnmappableTarget`] | `edit.rejected.unmappable_target` |
    /// | [`Self::NotAProperty`] | `edit.rejected.not_a_property` |
    /// | [`Self::NotAPrim`] | `edit.rejected.not_a_prim` |
    /// | [`Self::NoSuchSpec`] | `edit.rejected.no_such_spec` |
    /// | [`Self::SpecExists`] | `edit.rejected.spec_exists` |
    /// | [`Self::NotAnAttribute`] | `edit.rejected.not_an_attribute` |
    /// | [`Self::UndeclaredType`] | `edit.rejected.undeclared_type` |
    /// | [`Self::UnknownValueType`] | `edit.rejected.unknown_value_type` |
    /// | [`Self::TypeMismatch`] | `edit.rejected.type_mismatch` |
    /// | [`Self::ReservedField`] | `edit.rejected.reserved_field` |
    /// | [`Self::PseudoRoot`] | `edit.rejected.pseudo_root` |
    /// | [`Self::PseudoRootArc`] | `edit.rejected.pseudo_root_arc` |
    /// | [`Self::NotOnVariant`] | `edit.rejected.not_on_variant` |
    /// | [`Self::NoSuchSublayer`] | `edit.rejected.no_such_sublayer` |
    /// | [`Self::EmptySublayerAsset`] | `edit.rejected.empty_sublayer_asset` |
    /// | [`Self::DuplicateSublayer`] | `edit.rejected.duplicate_sublayer` |
    /// | [`Self::SublayerCycle`] | `edit.rejected.sublayer_cycle` |
    /// | [`Self::InvalidRelocate`] | `edit.rejected.invalid_relocate` |
    /// | [`Self::DuplicateRelocateSource`] | `edit.rejected.duplicate_relocate_source` |
    /// | [`Self::CopyBetweenKinds`] | `edit.rejected.copy_between_kinds` |
    /// | [`Self::VariantCopy`] | `edit.rejected.variant_copy` |
    /// | [`Self::CopyIntoSource`] | `edit.rejected.copy_into_source` |
    /// | [`Self::UnsupportedCopy`] | `edit.rejected.unsupported_copy` |
    /// | [`Self::InvalidVariantSetName`] | `edit.rejected.invalid_variant_set_name` |
    /// | [`Self::Diverged`] | `edit.rejected.diverged` |
    ///
    /// Codes are part of the public contract: a released code keeps its
    /// spelling and meaning, and a new variant brings a new code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoSuchLayer(_) => "edit.rejected.no_such_layer",
            Self::Unmappable(_) => "edit.rejected.unmappable",
            Self::UnmappableTarget(_) => "edit.rejected.unmappable_target",
            Self::NotAProperty(_) => "edit.rejected.not_a_property",
            Self::NotAPrim(_) => "edit.rejected.not_a_prim",
            Self::NoSuchSpec(_) => "edit.rejected.no_such_spec",
            Self::SpecExists(_) => "edit.rejected.spec_exists",
            Self::NotAnAttribute(_) => "edit.rejected.not_an_attribute",
            Self::UndeclaredType(_) => "edit.rejected.undeclared_type",
            Self::UnknownValueType { .. } => "edit.rejected.unknown_value_type",
            Self::TypeMismatch { .. } => "edit.rejected.type_mismatch",
            Self::ReservedField(_) => "edit.rejected.reserved_field",
            Self::PseudoRoot => "edit.rejected.pseudo_root",
            Self::PseudoRootArc => "edit.rejected.pseudo_root_arc",
            Self::NotOnVariant(_) => "edit.rejected.not_on_variant",
            Self::NoSuchSublayer(_) => "edit.rejected.no_such_sublayer",
            Self::EmptySublayerAsset(_) => "edit.rejected.empty_sublayer_asset",
            Self::DuplicateSublayer(_) => "edit.rejected.duplicate_sublayer",
            Self::SublayerCycle(_) => "edit.rejected.sublayer_cycle",
            Self::InvalidRelocate { .. } => "edit.rejected.invalid_relocate",
            Self::DuplicateRelocateSource(_) => "edit.rejected.duplicate_relocate_source",
            Self::CopyBetweenKinds => "edit.rejected.copy_between_kinds",
            Self::VariantCopy => "edit.rejected.variant_copy",
            Self::CopyIntoSource => "edit.rejected.copy_into_source",
            Self::UnsupportedCopy => "edit.rejected.unsupported_copy",
            Self::InvalidVariantSetName(_) => "edit.rejected.invalid_variant_set_name",
            Self::Diverged(_) => "edit.rejected.diverged",
        }
    }

    /// Why the edit was rejected, its paths and names written as text with
    /// `store`.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl fmt::Display + 'a {
        with_store(self, store)
    }
}

/// The field's name as layers spell it (`typeName`).
fn field_name(field: super::PrimField) -> &'static str {
    use super::PrimField;
    match field {
        PrimField::Specifier => "specifier",
        PrimField::TypeName => "typeName",
        PrimField::Active => "active",
        PrimField::Instanceable => "instanceable",
        PrimField::PrimOrder => "primOrder",
        PrimField::PropertyOrder => "propertyOrder",
        PrimField::References => "references",
        PrimField::Payloads => "payload",
        PrimField::Inherits => "inheritPaths",
        PrimField::Specializes => "specializes",
        PrimField::VariantSetNames => "variantSetNames",
    }
}

impl Describe for Rejection {
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result {
        match self {
            Self::NoSuchLayer(layer) => write!(f, "no {} is in the store", names.layer(*layer)),
            Self::Unmappable(path) => {
                write!(f, "the edit target does not map {}", names.target(*path))
            }
            Self::UnmappableTarget(path) => write!(
                f,
                "the edit target does not map the target {}",
                names.target(*path)
            ),
            Self::NotAProperty(path) => write!(f, "{} is not a property path", names.spec(path)),
            Self::NotAPrim(path) => {
                write!(f, "{} is not a prim or variant path", names.spec(path))
            }
            Self::NoSuchSpec(path) => write!(f, "no spec is at {}", names.spec(path)),
            Self::SpecExists(path) => write!(f, "a spec is already at {}", names.spec(path)),
            Self::NotAnAttribute(path) => write!(f, "{} is not an attribute", names.spec(path)),
            Self::UndeclaredType(path) => write!(
                f,
                "{} declares no type; create it with a type first",
                names.spec(path)
            ),
            Self::UnknownValueType { path, type_name } => write!(
                f,
                "`{type_name}`, the type of {}, is not a registered value type",
                names.spec(path)
            ),
            Self::TypeMismatch {
                path,
                declared,
                offered,
            } => write!(
                f,
                "the value is a `{offered}`, but {} is a `{declared}`",
                names.spec(path)
            ),
            Self::ReservedField(field) => write!(
                f,
                "`{}` has a dedicated member and edit, not a metadata edit",
                names.token(*field)
            ),
            Self::PseudoRoot => f.write_str(
                "the pseudo-root holds no prim fields or properties; layer metadata has its own edits",
            ),
            Self::PseudoRootArc => f.write_str("an arc cannot target the pseudo-root"),
            Self::NotOnVariant(field) => {
                write!(f, "a variant spec holds no `{}`", field_name(*field))
            }
            Self::NoSuchSublayer(index) => write!(f, "no sublayer is at index {index}"),
            Self::EmptySublayerAsset(index) => {
                write!(f, "the sublayer at index {index} has an empty asset path")
            }
            Self::DuplicateSublayer(index) => {
                write!(f, "the sublayer at index {index} repeats an earlier one")
            }
            Self::SublayerCycle(layer) => {
                write!(f, "sublayering {} makes a cycle", names.layer(*layer))
            }
            Self::InvalidRelocate { source, reason } => write!(
                f,
                "composition would ignore the relocate of {}: {reason:?}",
                names.path(*source)
            ),
            Self::DuplicateRelocateSource(source) => {
                write!(f, "two relocates move {}", names.path(*source))
            }
            Self::CopyBetweenKinds => {
                f.write_str("a prim and a property copy only to their own kind")
            }
            Self::VariantCopy => f.write_str("variant specs are not copied"),
            Self::CopyIntoSource => {
                f.write_str("the copy's destination is its source, or inside or around it")
            }
            Self::UnsupportedCopy => {
                f.write_str("the copied specs author paths that cannot be remapped")
            }
            Self::InvalidVariantSetName(name) => write!(
                f,
                "`{}` is not an identifier, so not a variant set name",
                names.token(*name)
            ),
            Self::Diverged(path) => write!(
                f,
                "the layer no longer matches the inverse at {}",
                names.spec(path)
            ),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.describe(f, Names(None))
    }
}

impl core::error::Error for EditError {}
