// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The family-generic chain kernel. User-facing docs live on
//! [`OpinionFamily`] and [`resolve_family_chain`], since this module is private.

use alloc::vec::Vec;
use core::convert::Infallible;

use crate::{
    IgnoreReason, ListOp, OpinionKind, OpinionOp, ShallowOverlay, combine_dictionary_chain,
};

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
    /// An operation this family does not handle. It is skipped and reported
    /// with the given reason; it does not stop the walk.
    ///
    /// A host family usually returns [`IgnoreReason::OtherFamily`].
    Foreign(IgnoreReason),
}

/// Describes how one kind of value resolves, for operation types the crate
/// doesn't define.
///
/// Implement this when [`OpinionOp`] can't express your values, for example
/// numbers adjusted by relative offsets or arrays patched with [`ArrayEdit`].
/// `Op` is your own authored operation type. You say which operations are
/// complete values, which are edits and which are blocks, and how an edit
/// applies to a value. [`resolve_family_chain`] does the walk, stops early,
/// and tracks provenance.
///
/// Each chain resolves with one family. If a chain can mix kinds of value,
/// pick the family from the strongest opinion and let the family mark the
/// others [`FamilyMember::Foreign`].
///
/// ```
/// use opinionated::{FamilyMember, IgnoreReason, OpinionFamily, resolve_family_chain};
///
/// // A stat in a game: a base value, with buffs and debuffs layered on top.
/// enum StatOp {
///     Base(i32),
///     Modifier(i32),
///     Disabled,
///     Label(&'static str),
/// }
///
/// struct Stat;
///
/// impl OpinionFamily<StatOp> for Stat {
///     type Value = i32;
///     type Edit<'op> = i32;
///
///     fn classify<'op>(&self, op: &'op StatOp) -> FamilyMember<i32, i32> {
///         match op {
///             StatOp::Base(value) => FamilyMember::Dense(*value),
///             StatOp::Modifier(delta) => FamilyMember::Sparse(*delta),
///             StatOp::Disabled => FamilyMember::Block,
///             StatOp::Label(_) => FamilyMember::Foreign(IgnoreReason::OtherFamily),
///         }
///     }
///
///     fn apply<'op>(&self, delta: i32, base: i32) -> i32
///     where
///         StatOp: 'op,
///     {
///         base + delta
///     }
///
///     // What modifiers apply to when no layer sets a base.
///     fn seed(&self) -> i32 {
///         10
///     }
/// }
///
/// let chain = [
///     (StatOp::Modifier(5), "potion"),
///     (StatOp::Label("hero"), "profile"),
///     (StatOp::Base(100), "class"),
///     (StatOp::Base(50), "defaults"), // hidden by the class base; never read
/// ];
/// let (value, from) = resolve_family_chain(&Stat, chain.iter().map(|(op, p)| (op, p)))
///     .resolved()
///     .unwrap();
/// assert_eq!(value, 105);
/// assert_eq!(from, "potion");
/// ```
///
/// [`ScalarFamily`], [`ListFamily`] and [`DictionaryFamily`] implement this
/// trait for [`OpinionOp`], giving the same results as
/// [`resolve_ordered_chain`].
///
/// [`OpinionOp`]: crate::OpinionOp
/// [`ArrayEdit`]: crate::ArrayEdit
/// [`resolve_ordered_chain`]: crate::resolve_ordered_chain
pub trait OpinionFamily<Op> {
    /// The dense resolved value this family produces.
    type Value;
    /// The sparse edit representation this family folds.
    ///
    /// Most families use an owned type and ignore `'op`. To avoid copying a
    /// large edit out of the operation, borrow it instead:
    /// `type Edit<'op> = &'op ArrayEdit<T>;`.
    type Edit<'op>
    where
        Op: 'op;

    /// Classifies one authored operation.
    ///
    /// Dense values are owned so edits can modify the result in place. Sparse
    /// edits may borrow authored data or own data synthesized at query time;
    /// the kernel retains them only until this fold completes.
    fn classify<'op>(&self, op: &'op Op) -> FamilyMember<Self::Value, Self::Edit<'op>>;

    /// Applies one sparse edit over a weaker `base` value.
    ///
    /// `edit` is stronger than `base`; the kernel applies accumulated edits
    /// weakest-first so stronger edits have the last word.
    fn apply<'op>(&self, edit: Self::Edit<'op>, base: Self::Value) -> Self::Value
    where
        Op: 'op;

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

/// The base selected by a family fold, before applying stronger sparse edits.
///
/// This records a kernel fact. A host may return its own fallback after an
/// absent or blocked fold; that does not mean the kernel called
/// [`OpinionFamily::seed`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FamilyBase {
    /// A dense opinion supplied the base and terminated the fold.
    Dense,
    /// Sparse edits were applied to [`OpinionFamily::seed`], at chain end or
    /// after a weaker block. The seed was actually requested.
    Seed,
    /// No member contributed; the seed was not requested.
    Absent,
    /// A block appeared before any contribution; the seed was not requested.
    Blocked,
}

/// A [`FamilyResolution`] paired with the events that explain it.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct FamilyReport<T, P> {
    /// The outcome of the fold.
    pub resolution: FamilyResolution<T, P>,
    /// How the fold obtained its base, independently of diagnostic events.
    pub base: FamilyBase,
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
/// The chain is consumed lazily: once a dense member or block ends the fold,
/// no weaker opinion is pulled from the iterator or classified. Callers whose
/// [`OpinionFamily::classify`] samples or clones can therefore pass their
/// whole opinion stack without paying for hidden opinions.
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
    fold_family_chain(family, opinions_strong_to_weak, None).0
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
    let (resolution, base) = fold_family_chain(family, opinions_strong_to_weak, Some(&mut events));
    FamilyReport {
        resolution,
        base,
        events,
    }
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
) -> (FamilyResolution<F::Value, P>, FamilyBase)
where
    Op: 'a,
    F: OpinionFamily<Op>,
    P: Clone + 'a,
{
    let mut provenance: Option<P> = None;
    let mut edits: Vec<F::Edit<'a>> = Vec::new();

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
                return (
                    FamilyResolution::Resolved { value, provenance },
                    FamilyBase::Dense,
                );
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
                        (
                            FamilyResolution::Resolved { value, provenance },
                            FamilyBase::Seed,
                        )
                    }
                    None => (
                        FamilyResolution::Blocked {
                            provenance: prov.clone(),
                        },
                        FamilyBase::Blocked,
                    ),
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
            (
                FamilyResolution::Resolved { value, provenance },
                FamilyBase::Seed,
            )
        }
        None => (FamilyResolution::Absent, FamilyBase::Absent),
    }
}

/// Applies accumulated edits weakest-first over `base`.
///
/// `edits` are accumulated strongest-to-weakest, so iterating in reverse
/// applies the weakest edit first and lets stronger edits have the last word.
fn materialize<'a, Op: 'a, F>(family: &F, base: F::Value, edits: Vec<F::Edit<'a>>) -> F::Value
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
    type Edit<'op>
        = Infallible
    where
        OpinionOp<V, I, K>: 'op;

    fn classify<'op>(
        &self,
        op: &'op OpinionOp<V, I, K>,
    ) -> FamilyMember<Self::Value, Self::Edit<'op>> {
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

    fn apply<'op>(&self, edit: Self::Edit<'op>, base: Self::Value) -> Self::Value
    where
        OpinionOp<V, I, K>: 'op,
    {
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
/// An explicit [`OpinionOp::List`] is a dense value that hides weaker lists;
/// other list operations are borrowed sparse edits composed over weaker lists;
/// [`OpinionOp::Block`] blocks; every other operation is foreign. The seed is
/// the empty list.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ListFamily;

impl<V, I, K> OpinionFamily<OpinionOp<V, I, K>> for ListFamily
where
    I: Clone + Eq,
{
    type Value = Vec<I>;
    type Edit<'op>
        = &'op ListOp<I>
    where
        OpinionOp<V, I, K>: 'op;

    fn classify<'op>(
        &self,
        op: &'op OpinionOp<V, I, K>,
    ) -> FamilyMember<Self::Value, Self::Edit<'op>> {
        match op {
            OpinionOp::List(edit) => match &edit.explicit {
                Some(items) => FamilyMember::Dense(items.clone()),
                None => FamilyMember::Sparse(edit),
            },
            OpinionOp::Block => FamilyMember::Block,
            OpinionOp::Set(_) | OpinionOp::Dictionary(_) => {
                FamilyMember::Foreign(IgnoreReason::IncompatibleOperation {
                    resolved: OpinionKind::List,
                    ignored: op.kind(),
                })
            }
        }
    }

    fn apply<'op>(&self, edit: Self::Edit<'op>, mut base: Self::Value) -> Self::Value
    where
        OpinionOp<V, I, K>: 'op,
    {
        edit.apply_in_place(&mut base);
        base
    }

    fn seed(&self) -> Self::Value {
        Vec::new()
    }
}

/// The dictionary family adapter for [`OpinionOp`].
///
/// [`OpinionOp::Dictionary`] is a sparse edit combined over weaker
/// dictionaries under the [`ShallowOverlay`] policy; [`OpinionOp::Block`]
/// blocks; every other operation is foreign. The seed is the empty dictionary.
///
/// Shallow overlay is associative, so the kernel's weakest-first application
/// matches a strongest-first fold. Recursive combination is not associative
/// (see [`combine_dictionary_chain`]) and is therefore not offered as a family
/// over this kernel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DictionaryFamily;

impl<V, I, K> OpinionFamily<OpinionOp<V, I, K>> for DictionaryFamily
where
    V: Clone,
    K: Clone + Ord,
{
    type Value = Vec<(K, V)>;
    type Edit<'op>
        = &'op [(K, V)]
    where
        OpinionOp<V, I, K>: 'op;

    fn classify<'op>(
        &self,
        op: &'op OpinionOp<V, I, K>,
    ) -> FamilyMember<Self::Value, Self::Edit<'op>> {
        match op {
            OpinionOp::Dictionary(entries) => FamilyMember::Sparse(entries),
            OpinionOp::Block => FamilyMember::Block,
            OpinionOp::Set(_) | OpinionOp::List(_) => {
                FamilyMember::Foreign(IgnoreReason::IncompatibleOperation {
                    resolved: OpinionKind::Dictionary,
                    ignored: op.kind(),
                })
            }
        }
    }

    fn apply<'op>(&self, edit: Self::Edit<'op>, base: Self::Value) -> Self::Value
    where
        OpinionOp<V, I, K>: 'op,
    {
        // `edit` is stronger than `base`; combining strongest-first lets the
        // stronger entries win by key.
        combine_dictionary_chain(&ShallowOverlay, [edit, base.as_slice()])
    }

    fn seed(&self) -> Self::Value {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex};
    use alloc::{rc::Rc, string::String, vec};
    use core::cell::Cell;

    // Deliberately not Clone, and borrowing caller-owned data. Neither the
    // program nor its backing data needs ownership transfer into the fold.
    struct TextEdit<'data>(&'data str);
    enum TextOp<'data> {
        Edit(TextEdit<'data>),
        Block,
    }
    struct TextFamily;
    impl<'data> OpinionFamily<TextOp<'data>> for TextFamily {
        type Value = String;
        type Edit<'op>
            = &'op TextEdit<'data>
        where
            TextOp<'data>: 'op;
        fn classify<'op>(
            &self,
            op: &'op TextOp<'data>,
        ) -> FamilyMember<Self::Value, Self::Edit<'op>> {
            match op {
                TextOp::Edit(edit) => FamilyMember::Sparse(edit),
                TextOp::Block => FamilyMember::Block,
            }
        }
        fn apply<'op>(&self, edit: Self::Edit<'op>, mut base: Self::Value) -> Self::Value
        where
            TextOp<'data>: 'op,
        {
            base.push_str(edit.0);
            base
        }
        fn seed(&self) -> String {
            String::from("seed")
        }
    }

    #[test]
    fn non_clone_edits_borrow_non_static_data_and_preserve_reports() {
        let strong = String::from("/strong");
        let weak = String::from("/weak");
        let ops = [
            TextOp::Edit(TextEdit(&strong)),
            TextOp::Edit(TextEdit(&weak)),
            TextOp::Block,
        ];
        let sources = ["strong", "weak", "block"];
        let chain = || {
            ops.iter()
                .zip(sources.iter())
                .chain(core::iter::once_with(|| panic!("hidden source read")))
        };
        let lean = resolve_family_chain(&TextFamily, chain());
        let report = resolve_family_chain_report(&TextFamily, chain());
        assert_eq!(lean, report.resolution);
        assert_eq!(
            lean.resolved(),
            Some((String::from("seed/weak/strong"), "strong"))
        );
        assert_eq!(
            report.events,
            [
                FamilyEvent::ContributedSparse {
                    provenance: "strong"
                },
                FamilyEvent::ContributedSparse { provenance: "weak" },
                FamilyEvent::StoppedByBlock {
                    provenance: "block"
                },
            ]
        );
    }

    #[derive(Debug)]
    struct Element {
        value: i32,
        clones: Rc<Cell<usize>>,
    }
    impl Clone for Element {
        fn clone(&self) -> Self {
            self.clones.set(self.clones.get() + 1);
            Self {
                value: self.value,
                clones: self.clones.clone(),
            }
        }
    }
    enum ArrayOp {
        Dense(Vec<Element>),
        Edit(ArrayEdit<Element>),
    }
    struct Arrays;
    impl OpinionFamily<ArrayOp> for Arrays {
        type Value = Vec<Element>;
        type Edit<'op> = &'op ArrayEdit<Element>;
        fn classify<'op>(&self, op: &'op ArrayOp) -> FamilyMember<Self::Value, Self::Edit<'op>> {
            match op {
                ArrayOp::Dense(items) => FamilyMember::Dense(items.clone()),
                ArrayOp::Edit(edit) => FamilyMember::Sparse(edit),
            }
        }
        fn apply<'op>(&self, edit: Self::Edit<'op>, mut base: Self::Value) -> Self::Value
        where
            ArrayOp: 'op,
        {
            edit.apply_in_place(&mut base, None);
            base
        }
        fn seed(&self) -> Self::Value {
            Vec::new()
        }
    }

    #[test]
    fn only_dense_elements_and_executed_literals_are_cloned() {
        let clones = Rc::new(Cell::new(0));
        let element = |value| Element {
            value,
            clones: clones.clone(),
        };
        let write = |index, value| ArrayEditOp::Write {
            index: ArrayIndex::Position(index),
            src: ArrayEditOperand::Literal(element(value)),
        };
        let ops = [
            ArrayOp::Edit(ArrayEdit {
                ops: vec![write(0, 99), write(999, 123)],
            }),
            ArrayOp::Edit(ArrayEdit {
                ops: vec![write(0, 7)],
            }),
            ArrayOp::Dense(vec![element(1), element(2)]),
        ];
        let sources = [0, 1, 2];
        let chain = || {
            ops.iter()
                .zip(sources.iter())
                .chain(core::iter::once_with(|| panic!("hidden source read")))
        };
        let result = resolve_family_chain(&Arrays, chain()).resolved().unwrap();
        assert_eq!(
            result.0.iter().map(|v| v.value).collect::<Vec<_>>(),
            [99, 2]
        );
        assert_eq!(result.1, 0);
        assert_eq!(
            clones.get(),
            4,
            "two dense elements and two executed writes; no program copies"
        );
        clones.set(0);
        let report = resolve_family_chain_report(&Arrays, chain());
        assert_eq!(report.events.len(), 3);
        assert_eq!(
            report
                .resolution
                .resolved()
                .unwrap()
                .0
                .iter()
                .map(|v| v.value)
                .collect::<Vec<_>>(),
            [99, 2]
        );
        assert_eq!(
            clones.get(),
            4,
            "reporting must not clone the edit programs either"
        );
        let ArrayOp::Dense(original) = &ops[2] else {
            unreachable!()
        };
        assert_eq!(
            original.iter().map(|v| v.value).collect::<Vec<_>>(),
            [1, 2],
            "authored inputs stay unchanged"
        );
    }
}
