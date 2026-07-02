// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A family-generic chain kernel for folding ordered opinions.
//!
//! [`OpinionOp`]'s closed enum covers scalar, list, and dictionary families,
//! but domains such as `layerstack` carry value families the enum cannot
//! express (sparse array edits, time-sampled values, schema fallback seeds).
//! This module extracts the one fold shape every family already follows into a
//! reusable kernel, so a domain can add its own family without forking the
//! crate.
//!
//! A family is exactly three operations ([`OpinionFamily`]): classify one
//! authored operation into a [`FamilyMember`], apply one sparse edit over a
//! weaker base value, and provide the weakest [`OpinionFamily::seed`] value.
//! [`resolve_family_chain`] folds a strongest-to-weakest chain over one family;
//! [`resolve_family_chain_report`] additionally records family-agnostic
//! [`FamilyEvent`]s for diagnostics.
//!
//! The in-crate adapters [`ScalarFamily`], [`ListFamily`], and
//! [`DictionaryFamily`] express the existing [`OpinionOp`] semantics over this
//! kernel and are proven identical to [`resolve_ordered_chain`] by parity
//! tests.
//!
//! [`resolve_ordered_chain`]: crate::resolve_ordered_chain

use alloc::vec::Vec;
use core::convert::Infallible;

use crate::{IgnoreReason, ListOp, OpinionKind, OpinionOp, combine_dictionary_chain};

/// How one authored operation participates in a family's fold.
///
/// `D` is the family's dense resolved value; `S` is its sparse edit
/// representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FamilyMember<D, S> {
    /// A dense, self-sufficient value; terminates the fold.
    Dense(D),
    /// A sparse edit that composes over weaker opinions.
    Sparse(S),
    /// Blocks all weaker opinions.
    Block,
    /// Another family's operation; skipped, with the reason reported.
    Foreign(IgnoreReason),
}

/// A value family that folds over an ordered opinion chain.
///
/// Implementors describe one family's behavior over an arbitrary authored
/// operation type `Op`. Family selection is the caller's responsibility; the
/// kernel folds exactly one family per chain.
pub trait OpinionFamily<Op> {
    /// The dense resolved value this family produces.
    type Value;
    /// The sparse edit representation this family folds.
    type Edit;

    /// Classifies one authored operation.
    ///
    /// Members are returned owned because sampled or interpolated values are
    /// synthesized at query time, not borrowed from storage.
    fn classify(&self, op: &Op) -> FamilyMember<Self::Value, Self::Edit>;

    /// Applies one sparse edit over a weaker `base` value.
    ///
    /// `edit` is stronger than `base`; the kernel applies accumulated edits
    /// weakest-first so stronger edits have the last word.
    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value;

    /// The weakest base value when no dense opinion terminates the chain.
    fn seed(&self) -> Self::Value;
}

/// The outcome of folding one family over an ordered opinion chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FamilyResolution<T, P> {
    /// No opinion contributed to the fold.
    Absent,
    /// A block cut the chain before any opinion contributed.
    Blocked {
        /// Provenance for the blocking opinion.
        provenance: P,
    },
    /// The chain folded to a value.
    Resolved {
        /// The folded value.
        value: T,
        /// Provenance from the strongest contributing opinion.
        provenance: P,
    },
}

impl<T, P> FamilyResolution<T, P> {
    /// Returns the resolved value and provenance, if the chain produced one.
    #[must_use]
    pub fn resolved(self) -> Option<(T, P)> {
        match self {
            Self::Resolved { value, provenance } => Some((value, provenance)),
            Self::Absent | Self::Blocked { .. } => None,
        }
    }

    /// Returns `true` when no opinion contributed to the fold.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Returns `true` when a block cut the chain before any contribution.
    #[must_use]
    pub const fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }
}

/// One event in a [`FamilyReport`].
///
/// Kernel events are deliberately family-agnostic: they name how a member
/// participated without knowing the family's [`OpinionKind`] altitude.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FamilyEvent<P> {
    /// A dense member absorbed accumulated edits and terminated the fold.
    ContributedDense {
        /// Provenance for the dense opinion.
        provenance: P,
    },
    /// A sparse edit was accumulated into the fold.
    ContributedSparse {
        /// Provenance for the sparse opinion.
        provenance: P,
    },
    /// A block stopped weaker opinions from contributing.
    StoppedByBlock {
        /// Provenance for the block.
        provenance: P,
    },
    /// A foreign opinion was skipped and did not contribute.
    Ignored {
        /// Provenance for the ignored opinion.
        provenance: P,
        /// Reason the opinion did not contribute.
        reason: IgnoreReason,
    },
}

/// A [`FamilyResolution`] paired with the events that explain it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FamilyReport<T, P> {
    /// The outcome of the fold.
    pub resolution: FamilyResolution<T, P>,
    /// Ordered events explaining how the chain was folded.
    pub events: Vec<FamilyEvent<P>>,
}

/// Folds one family over a strongest-to-weakest opinion chain.
///
/// Walks the chain strongest-to-weakest, accumulating sparse edits until a
/// dense member absorbs them, a block cuts the chain, or the chain ends; then
/// applies the edits weakest-first over the dense value or the family
/// [`OpinionFamily::seed`], respectively. Foreign opinions are skipped and
/// never terminate the fold. Provenance is the strongest contributing
/// (non-foreign, non-block) opinion.
///
/// Use [`resolve_family_chain_report`] when diagnostic events are needed.
#[must_use]
pub fn resolve_family_chain<'a, Op, F, P>(
    family: &F,
    opinions_strong_to_weak: impl IntoIterator<Item = (&'a Op, &'a P)>,
) -> FamilyResolution<F::Value, P>
where
    Op: 'a,
    F: OpinionFamily<Op>,
    P: Clone + 'a,
{
    fold_family_chain(family, opinions_strong_to_weak, None)
}

/// Folds one family over a chain and records diagnostic events.
///
/// Behaves exactly like [`resolve_family_chain`] and additionally records a
/// family-agnostic [`FamilyEvent`] for each member the fold visits.
#[must_use]
pub fn resolve_family_chain_report<'a, Op, F, P>(
    family: &F,
    opinions_strong_to_weak: impl IntoIterator<Item = (&'a Op, &'a P)>,
) -> FamilyReport<F::Value, P>
where
    Op: 'a,
    F: OpinionFamily<Op>,
    P: Clone + 'a,
{
    let mut events = Vec::new();
    let resolution = fold_family_chain(family, opinions_strong_to_weak, Some(&mut events));
    FamilyReport { resolution, events }
}

/// The shared fold used by both the lean and reporting entry points.
///
/// When `events` is `Some`, one [`FamilyEvent`] is recorded per visited member;
/// when `None`, no events are allocated, keeping the lean path allocation-free
/// apart from the sparse-edit accumulator.
fn fold_family_chain<'a, Op, F, P>(
    family: &F,
    opinions_strong_to_weak: impl IntoIterator<Item = (&'a Op, &'a P)>,
    mut events: Option<&mut Vec<FamilyEvent<P>>>,
) -> FamilyResolution<F::Value, P>
where
    Op: 'a,
    F: OpinionFamily<Op>,
    P: Clone + 'a,
{
    let mut provenance: Option<P> = None;
    let mut edits: Vec<F::Edit> = Vec::new();

    for (op, prov) in opinions_strong_to_weak {
        match family.classify(op) {
            FamilyMember::Dense(value) => {
                if let Some(sink) = events.as_mut() {
                    sink.push(FamilyEvent::ContributedDense {
                        provenance: prov.clone(),
                    });
                }
                let provenance = provenance.unwrap_or_else(|| prov.clone());
                let value = materialize(family, value, edits);
                return FamilyResolution::Resolved { value, provenance };
            }
            FamilyMember::Sparse(edit) => {
                if let Some(sink) = events.as_mut() {
                    sink.push(FamilyEvent::ContributedSparse {
                        provenance: prov.clone(),
                    });
                }
                if provenance.is_none() {
                    provenance = Some(prov.clone());
                }
                edits.push(edit);
            }
            FamilyMember::Block => {
                if let Some(sink) = events.as_mut() {
                    sink.push(FamilyEvent::StoppedByBlock {
                        provenance: prov.clone(),
                    });
                }
                return match provenance {
                    // A block cuts the chain, but edits accumulated from
                    // stronger opinions still materialize over the seed.
                    Some(provenance) => {
                        let value = materialize(family, family.seed(), edits);
                        FamilyResolution::Resolved { value, provenance }
                    }
                    None => FamilyResolution::Blocked {
                        provenance: prov.clone(),
                    },
                };
            }
            FamilyMember::Foreign(reason) => {
                if let Some(sink) = events.as_mut() {
                    sink.push(FamilyEvent::Ignored {
                        provenance: prov.clone(),
                        reason,
                    });
                }
            }
        }
    }

    match provenance {
        Some(provenance) => {
            let value = materialize(family, family.seed(), edits);
            FamilyResolution::Resolved { value, provenance }
        }
        None => FamilyResolution::Absent,
    }
}

/// Applies accumulated edits weakest-first over `base`.
///
/// `edits` are accumulated strongest-to-weakest, so iterating in reverse
/// applies the weakest edit first and lets stronger edits have the last word.
fn materialize<Op, F>(family: &F, base: F::Value, edits: Vec<F::Edit>) -> F::Value
where
    F: OpinionFamily<Op>,
{
    let mut value = base;
    for edit in edits.into_iter().rev() {
        value = family.apply(edit, value);
    }
    value
}

/// The scalar family adapter for [`OpinionOp`].
///
/// [`OpinionOp::Set`] is the dense value; [`OpinionOp::Block`] blocks; every
/// other operation is foreign. Scalars never accumulate sparse edits, so the
/// family has no seed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScalarFamily;

impl<V, I, K> OpinionFamily<OpinionOp<V, I, K>> for ScalarFamily
where
    V: Clone,
{
    type Value = V;
    type Edit = Infallible;

    fn classify(&self, op: &OpinionOp<V, I, K>) -> FamilyMember<Self::Value, Self::Edit> {
        match op {
            OpinionOp::Set(value) => FamilyMember::Dense(value.clone()),
            OpinionOp::Block => FamilyMember::Block,
            OpinionOp::List(_) | OpinionOp::Dictionary(_) => {
                FamilyMember::Foreign(IgnoreReason::IncompatibleOperation {
                    resolved: OpinionKind::Set,
                    ignored: op.kind(),
                })
            }
        }
    }

    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value {
        // `Edit` is uninhabited: the scalar family never produces a sparse
        // edit, so this arm is unreachable at the type level.
        let _ = base;
        match edit {}
    }

    fn seed(&self) -> Self::Value {
        // Unreachable: a scalar chain never accumulates sparse edits, so the
        // fold never materializes over a seed.
        unreachable!("scalar family never accumulates sparse edits")
    }
}

/// The list family adapter for [`OpinionOp`].
///
/// [`OpinionOp::List`] is a sparse edit composed over weaker lists;
/// [`OpinionOp::Block`] blocks; every other operation is foreign. The seed is
/// the empty list.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ListFamily;

impl<V, I, K> OpinionFamily<OpinionOp<V, I, K>> for ListFamily
where
    I: Clone + Eq,
{
    type Value = Vec<I>;
    type Edit = ListOp<I>;

    fn classify(&self, op: &OpinionOp<V, I, K>) -> FamilyMember<Self::Value, Self::Edit> {
        match op {
            OpinionOp::List(edit) => FamilyMember::Sparse(edit.clone()),
            OpinionOp::Block => FamilyMember::Block,
            OpinionOp::Set(_) | OpinionOp::Dictionary(_) => {
                FamilyMember::Foreign(IgnoreReason::IncompatibleOperation {
                    resolved: OpinionKind::List,
                    ignored: op.kind(),
                })
            }
        }
    }

    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value {
        edit.apply_to(&base)
    }

    fn seed(&self) -> Self::Value {
        Vec::new()
    }
}

/// The dictionary family adapter for [`OpinionOp`].
///
/// [`OpinionOp::Dictionary`] is a sparse edit combined over weaker
/// dictionaries; [`OpinionOp::Block`] blocks; every other operation is foreign.
/// The seed is the empty dictionary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DictionaryFamily;

impl<V, I, K> OpinionFamily<OpinionOp<V, I, K>> for DictionaryFamily
where
    V: Clone,
    K: Clone + Ord,
{
    type Value = Vec<(K, V)>;
    type Edit = Vec<(K, V)>;

    fn classify(&self, op: &OpinionOp<V, I, K>) -> FamilyMember<Self::Value, Self::Edit> {
        match op {
            OpinionOp::Dictionary(entries) => FamilyMember::Sparse(entries.clone()),
            OpinionOp::Block => FamilyMember::Block,
            OpinionOp::Set(_) | OpinionOp::List(_) => {
                FamilyMember::Foreign(IgnoreReason::IncompatibleOperation {
                    resolved: OpinionKind::Dictionary,
                    ignored: op.kind(),
                })
            }
        }
    }

    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value {
        // `edit` is stronger than `base`; combining strongest-first lets the
        // stronger entries win by key.
        combine_dictionary_chain([edit, base])
    }

    fn seed(&self) -> Self::Value {
        Vec::new()
    }
}
