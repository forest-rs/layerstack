// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transactions: batches of spec edits applied atomically.

use alloc::{boxed::Box, vec::Vec};

use super::{
    apply::{self, Guarded},
    error::{EditError, Slot},
    same::Same,
    spec::{Loc, spec_at},
    target::Address,
};
use crate::{
    doc::{FieldValue, LayerId, LayerStore, Specifier, Value},
    interner::TokenId,
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
/// OpenUSD: an `SdfChangeBlock` of `SdfLayer` / `SdfPrimSpec` /
/// `SdfAttributeSpec` edits, except that OpenUSD neither rolls back a
/// failed block nor records inverses.
///
/// Spec: AOUSD Core §7 (scene description: layers, prim, variant and
/// property specs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transaction {
    pub(crate) preconditions: Vec<Precondition>,
    pub(crate) ops: Vec<Op>,
}

/// One edit of a [`Transaction`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op {
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
    /// A storage-level step of an inverse, guarded by the step it undoes.
    Raw(Box<Guarded>),
}

/// A condition a [`Transaction`] checks before applying anything.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Precondition {
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
    Field(FieldValue),
    Token(TokenId),
    Property(Box<PropertySpec>),
    Spec,
}

impl Same for Authored {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Value(a), Self::Value(b)) => a.same(b),
            (Self::Field(a), Self::Field(b)) => a.same(b),
            (Self::Property(a), Self::Property(b)) => a.same(b),
            (Self::Token(a), Self::Token(b)) => a == b,
            (Self::Spec, Self::Spec) => true,
            _ => false,
        }
    }
}

impl Transaction {
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
    /// needed. A default or time samples in `spec` must conform to its
    /// declared type.
    ///
    /// Rejected if the property spec exists.
    ///
    /// OpenUSD: `SdfAttributeSpec::New`, `SdfRelationshipSpec::New`.
    pub fn create_property(&mut self, at: Address, spec: PropertySpec) -> &mut Self {
        self.push(Op::CreateProperty {
            at,
            spec: Box::new(spec),
        })
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
    /// Fields that the layer model keeps in dedicated members
    /// (`specifier`, `typeName`, `active`, the composition arcs, variant
    /// selections and the like) are rejected: they have their own edits or
    /// none.
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
        apply::apply(store, self, None).map(|applied| applied.inverse)
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
            Self::CreateProperty { at, .. } => (at, Slot::Spec),
            Self::SetDefault { at, .. } | Self::ClearDefault { at } => (at, Slot::Default),
            Self::SetTimeSample { at, time, .. } | Self::RemoveTimeSample { at, time } => {
                (at, Slot::TimeSample(*time))
            }
            Self::SetMetadata { at, key, .. } | Self::ClearMetadata { at, key } => {
                (at, Slot::Metadata(*key))
            }
            Self::SetVariantSelection { at, set, .. } => (at, Slot::VariantSelection(*set)),
            Self::Raw(_) => return None,
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
    let loc = Loc::of(&path, store.paths_mut());
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
    })
}
