// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Why a transaction was not applied.

use alloc::sync::Arc;
use core::fmt;

use crate::{doc::LayerId, interner::TokenId, path::TargetPath, spec_path::SpecPath};

/// Why a [`Transaction`](super::Transaction) was not applied. When one is
/// returned, no layer was changed.
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
}

/// Why one edit of a transaction cannot be applied.
#[derive(Clone, Debug, PartialEq)]
pub enum Rejection {
    /// No layer with this id is in the store.
    NoSuchLayer(LayerId),
    /// The edit target does not map the stage path.
    Unmappable,
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
    /// The value does not conform to the attribute's declared type.
    TypeMismatch {
        /// The attribute.
        path: SpecPath,
        /// Its declared type name.
        declared: Arc<str>,
    },
    /// The metadata field is kept in a dedicated member of the layer model
    /// and has its own edit, or none: the prim fields of
    /// [`PrimField`](super::PrimField) have the `set_`, `clear_`, `add_` and
    /// `remove_` edits of [`Transaction`](super::Transaction).
    ReservedField(TokenId),
    /// The edit addresses the pseudo-root, which holds no prim fields.
    ///
    /// OpenUSD: `UsdStage::_SetMetadata` rejects metadata on the
    /// pseudo-root.
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
    /// A step of an inverse no longer matches the layer: the layer was
    /// changed in a way the inverse cannot undo.
    Diverged(SpecPath),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration {
                layer,
                expected,
                found,
            } => write!(
                f,
                "layer {} changed since the edit was prepared (generation {found}, expected {expected})",
                layer.0
            ),
            Self::StaleValue { layer, slot, .. } => write!(
                f,
                "the authored {slot:?} of a spec in layer {} changed since the edit was prepared",
                layer.0
            ),
            Self::UnresolvedPrecondition { index } => {
                write!(
                    f,
                    "precondition {index} names a path its target does not map"
                )
            }
            Self::Rejected { op, reason } => write!(f, "edit {op} rejected: {reason}"),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchLayer(layer) => write!(f, "no layer {}", layer.0),
            Self::Unmappable => f.write_str("the edit target does not map the path"),
            Self::UnmappableTarget(_) => f.write_str("the edit target does not map a target path"),
            Self::NotAProperty(_) => f.write_str("expected a property path"),
            Self::NotAPrim(_) => f.write_str("expected a prim or variant path"),
            Self::NoSuchSpec(_) => f.write_str("no spec at the path"),
            Self::SpecExists(_) => f.write_str("a spec exists at the path"),
            Self::NotAnAttribute(_) => f.write_str("the property is not an attribute"),
            Self::UndeclaredType(_) => f.write_str("the attribute declares no type"),
            Self::TypeMismatch { declared, .. } => {
                write!(f, "the value is not a `{declared}`")
            }
            Self::ReservedField(_) => f.write_str("the field has a dedicated member"),
            Self::PseudoRoot => f.write_str("the pseudo-root holds no prim fields"),
            Self::PseudoRootArc => f.write_str("an arc cannot target the pseudo-root"),
            Self::NotOnVariant(field) => write!(f, "a variant spec holds no {field:?} field"),
            Self::Diverged(_) => f.write_str("the layer no longer matches the inverse"),
        }
    }
}

impl core::error::Error for EditError {}
