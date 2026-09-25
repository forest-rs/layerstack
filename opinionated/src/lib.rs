// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Layered sparse opinion resolution over typed addresses.
//!
//! `opinionated` resolves authored opinions keyed by `(address, field)` across
//! an explicit layer-strength order. It intentionally does not model
//! namespaces, schemas, references, variants, or storage backends.
//!
//! Each layer holds at most one opinion per `(address, field)` key; setting an
//! opinion again in the same layer replaces the previous one. Resolution
//! distinguishes a key with no opinions ([`Resolution::Absent`]) from a key
//! whose strongest opinion suppresses the value ([`Resolution::Blocked`]).
//!
//! # Example
//!
//! ```
//! use opinionated::{OpinionOp, SparseComposer};
//!
//! #[derive(Clone, Copy, Debug, Eq, PartialEq)]
//! enum Layer {
//!     Project,
//!     Defaults,
//! }
//!
//! #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
//! enum Address {
//!     Workspace,
//! }
//!
//! #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
//! enum Field {
//!     Theme,
//! }
//!
//! type Composer =
//!     SparseComposer<Layer, Address, Field, &'static str, &'static str, &'static str, &'static str>;
//!
//! let mut composer = Composer::try_new([Layer::Project, Layer::Defaults]).unwrap();
//!
//! composer
//!     .set_opinion(
//!         Layer::Defaults,
//!         Address::Workspace,
//!         Field::Theme,
//!         OpinionOp::Set("light"),
//!         "defaults",
//!     )
//!     .unwrap();
//! composer
//!     .set_opinion(
//!         Layer::Project,
//!         Address::Workspace,
//!         Field::Theme,
//!         OpinionOp::Set("dark"),
//!         "project",
//!     )
//!     .unwrap();
//!
//! let resolved = composer
//!     .resolve(Address::Workspace, Field::Theme)
//!     .resolved()
//!     .unwrap();
//! assert_eq!(resolved.value.as_scalar(), Some(&"dark"));
//! assert_eq!(resolved.provenance, "project");
//! ```

#![no_std]

extern crate alloc;

use alloc::{collections::BTreeMap, string::String, vec::Vec};

mod array_edit;
mod dictionary;
mod family;

pub use array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayFill, ArrayIndex, FillWith};
pub use dictionary::{
    DictionaryAdapter, ShallowOverlay, combine_dictionaries, combine_dictionary_chain,
};
pub use family::{
    DictionaryFamily, FamilyEvent, FamilyMember, FamilyReport, FamilyResolution, ListFamily,
    OpinionFamily, ScalarFamily, resolve_family_chain, resolve_family_chain_report,
};

/// A stable key for resolving one field on one address.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OpinionKey<A, F> {
    /// The composed object, row, entity, or other domain address.
    pub address: A,
    /// The field being resolved on the address.
    pub field: F,
}

impl<A, F> OpinionKey<A, F> {
    /// Creates a new opinion key.
    #[must_use]
    pub const fn new(address: A, field: F) -> Self {
        Self { address, field }
    }
}

/// An ordered unique-list edit.
///
/// `ListOp` is intentionally small: explicit replaces the whole list, deletes
/// remove matching items, prepend inserts items at the front, and append
/// inserts items at the back. Re-inserting an existing item moves it to the
/// requested position.
///
/// An authored `explicit` list makes the other edits spurious: applying the
/// operation yields the explicit list unchanged. This matches the `ListOps`
/// semantics of AOUSD Core §12.4 as implemented by `layerstack`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListOp<T> {
    /// Optional explicit list replacement. When present, the other edits in
    /// this operation are spurious.
    pub explicit: Option<Vec<T>>,
    /// Items to move or insert at the front, in authored order.
    pub prepend: Vec<T>,
    /// Items to move or insert at the back, in authored order.
    pub append: Vec<T>,
    /// Items to delete before prepends/appends are applied.
    pub delete: Vec<T>,
}

impl<T> Default for ListOp<T> {
    fn default() -> Self {
        Self {
            explicit: None,
            prepend: Vec::new(),
            append: Vec::new(),
            delete: Vec::new(),
        }
    }
}

impl<T: Clone + Eq> ListOp<T> {
    /// Applies this list operation to `base`.
    #[must_use]
    pub fn apply_to(&self, base: &[T]) -> Vec<T> {
        if let Some(explicit) = &self.explicit {
            return explicit.clone();
        }

        let mut out = base.to_vec();

        out.retain(|item| !self.delete.contains(item));

        for item in self.prepend.iter().rev() {
            out.retain(|existing| existing != item);
            out.insert(0, item.clone());
        }

        for item in &self.append {
            out.retain(|existing| existing != item);
            out.push(item.clone());
        }

        out
    }
}

/// Resolves a strong-to-weak chain of list operations.
#[must_use]
pub fn resolve_list_chain<T: Clone + Eq>(
    fallback: &[T],
    ops_strong_to_weak: impl IntoIterator<Item = ListOp<T>>,
) -> Vec<T> {
    let mut ops: Vec<ListOp<T>> = ops_strong_to_weak.into_iter().collect();
    let mut out = fallback.to_vec();
    while let Some(op) = ops.pop() {
        out = op.apply_to(&out);
    }
    out
}

/// An authored operation for one `(address, field)` key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpinionOp<V, I = V, K = String> {
    /// Strongest-wins scalar value.
    Set(V),
    /// Blocks weaker opinions for this key.
    Block,
    /// Composes an ordered unique list.
    List(ListOp<I>),
    /// Combines dictionary entries by key, with stronger keys winning.
    ///
    /// `V` is opaque to the enum API, so entries combine under the
    /// [`ShallowOverlay`] policy: nested dictionaries are not merged. Hosts
    /// whose values nest dictionaries combine them recursively with
    /// [`combine_dictionary_chain`] and their own [`DictionaryAdapter`].
    Dictionary(Vec<(K, V)>),
}

/// The operation family authored by an [`OpinionOp`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpinionKind {
    /// A strongest-wins scalar set.
    Set,
    /// A block that suppresses weaker opinions.
    Block,
    /// An ordered unique-list edit.
    List,
    /// A dictionary edit.
    Dictionary,
}

/// A resolved value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedValue<V, I = V, K = String> {
    /// A strongest-wins scalar value.
    Scalar(V),
    /// A composed ordered unique list.
    List(Vec<I>),
    /// A combined dictionary.
    Dictionary(Vec<(K, V)>),
}

impl<V, I, K> ResolvedValue<V, I, K> {
    /// Returns the scalar value, if this is scalar.
    #[must_use]
    pub const fn as_scalar(&self) -> Option<&V> {
        match self {
            Self::Scalar(value) => Some(value),
            Self::List(_) | Self::Dictionary(_) => None,
        }
    }

    /// Returns the list value, if this is a list.
    #[must_use]
    pub fn as_list(&self) -> Option<&[I]> {
        match self {
            Self::List(value) => Some(value),
            Self::Scalar(_) | Self::Dictionary(_) => None,
        }
    }

    /// Returns the dictionary entries, if this is a dictionary.
    #[must_use]
    pub fn as_dictionary(&self) -> Option<&[(K, V)]> {
        match self {
            Self::Dictionary(value) => Some(value),
            Self::Scalar(_) | Self::List(_) => None,
        }
    }
}

/// A resolved value with provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved<V, I = V, K = String, P = ()> {
    /// The composed value.
    pub value: ResolvedValue<V, I, K>,
    /// Provenance from the strongest contributing opinion.
    pub provenance: P,
}

/// The outcome of resolving one `(address, field)` key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution<V, I = V, K = String, P = ()> {
    /// No opinions are authored for the key.
    Absent,
    /// The strongest opinion blocks the key from resolving to a value.
    Blocked {
        /// Provenance for the blocking opinion.
        provenance: P,
    },
    /// The opinion chain resolved to a value.
    Resolved(Resolved<V, I, K, P>),
}

impl<V, I, K, P> Resolution<V, I, K, P> {
    /// Returns the resolved value, if the chain produced one.
    #[must_use]
    pub fn resolved(self) -> Option<Resolved<V, I, K, P>> {
        match self {
            Self::Resolved(resolved) => Some(resolved),
            Self::Absent | Self::Blocked { .. } => None,
        }
    }

    /// Returns a reference to the resolved value, if the chain produced one.
    #[must_use]
    pub const fn as_resolved(&self) -> Option<&Resolved<V, I, K, P>> {
        match self {
            Self::Resolved(resolved) => Some(resolved),
            Self::Absent | Self::Blocked { .. } => None,
        }
    }

    /// Returns `true` when no opinions are authored for the key.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Returns `true` when the strongest opinion blocks the key.
    #[must_use]
    pub const fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }
}

/// A storage-independent report for one resolution attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionReport<V, I = V, K = String, P = ()> {
    /// The outcome of the resolution.
    pub resolution: Resolution<V, I, K, P>,
    /// Ordered events explaining how the chain was interpreted.
    pub events: Vec<ResolutionEvent<P>>,
}

/// One event in a [`ResolutionReport`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolutionEvent<P = ()> {
    /// An opinion contributed to the resolved value.
    Contributed {
        /// Provenance for the contributing opinion.
        provenance: P,
        /// Operation kind that contributed.
        kind: OpinionKind,
    },
    /// A block stopped weaker opinions from contributing.
    StoppedByBlock {
        /// Provenance for the block.
        provenance: P,
    },
    /// An opinion was present but did not contribute.
    Ignored {
        /// Provenance for the ignored opinion.
        provenance: P,
        /// Reason the opinion did not contribute.
        reason: IgnoreReason,
    },
}

/// Why an opinion in a chain did not contribute to the resolved value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IgnoreReason {
    /// A stronger scalar set already resolved the value.
    WeakerThanSet,
    /// A stronger block cut off this weaker opinion.
    WeakerThanBlock,
    /// The opinion's operation kind is incompatible with the resolved kind.
    IncompatibleOperation {
        /// The operation family selected by the strongest non-block opinion.
        resolved: OpinionKind,
        /// The incompatible operation family on the ignored opinion.
        ignored: OpinionKind,
    },
}

/// Error returned when addressing a layer outside the composer's layer order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownLayer<L> {
    /// The layer that was not present in the composer layer order.
    pub layer: L,
}

/// Error returned when constructing a composer with duplicate layers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateLayer<L> {
    /// The duplicated layer.
    pub layer: L,
}

/// A borrowed opinion in strength order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpinionRef<'a, L, V, I = V, K = String, P = ()> {
    /// The layer that authored the opinion.
    pub layer: &'a L,
    /// The authored operation.
    pub op: &'a OpinionOp<V, I, K>,
    /// Caller-supplied provenance for the opinion.
    pub provenance: &'a P,
}

impl<'a, L, V, I, K, P> OpinionRef<'a, L, V, I, K, P> {
    /// Drops layer metadata and returns the chain-resolution view.
    #[must_use]
    pub const fn as_chain_opinion(self) -> ChainOpinion<'a, V, I, K, P> {
        ChainOpinion {
            op: self.op,
            provenance: self.provenance,
        }
    }
}

/// A borrowed opinion in already-sorted chain order.
///
/// This is the lower-level input type for [`resolve_ordered_chain`]. It does
/// not carry layer metadata so callers that already own ordered opinion stacks
/// can use the resolver without adopting [`SparseComposer`]'s storage model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChainOpinion<'a, V, I = V, K = String, P = ()> {
    /// The authored operation.
    pub op: &'a OpinionOp<V, I, K>,
    /// Caller-supplied provenance for the opinion.
    pub provenance: &'a P,
}

#[derive(Clone, Debug)]
struct StoredOpinion<L, V, I, K, P> {
    layer: L,
    rank: usize,
    op: OpinionOp<V, I, K>,
    provenance: P,
}

type OpinionBucket<L, V, I, K, P> = Vec<StoredOpinion<L, V, I, K, P>>;
type OpinionMap<L, A, F, V, I, K, P> = BTreeMap<OpinionKey<A, F>, OpinionBucket<L, V, I, K, P>>;

/// A sparse opinion composer over typed addresses and fields.
///
/// Layers are supplied strongest-to-weakest and are fixed for the lifetime of
/// the composer. Each layer holds at most one opinion per `(address, field)`
/// key; [`SparseComposer::set_opinion`] replaces any previous opinion the same
/// layer authored for that key.
#[derive(Clone, Debug)]
pub struct SparseComposer<L, A, F, V, I = V, K = String, P = ()> {
    layers: Vec<L>,
    opinions: OpinionMap<L, A, F, V, I, K, P>,
}

impl<L, A, F, V, I, K, P> SparseComposer<L, A, F, V, I, K, P>
where
    L: PartialEq,
    A: Ord,
    F: Ord,
{
    /// Creates an empty composer with layers ordered strongest-to-weakest.
    ///
    /// Returns [`DuplicateLayer`] if a layer appears more than once. Duplicate
    /// layers would make strength ordering ambiguous.
    pub fn try_new(
        layers_strong_to_weak: impl IntoIterator<Item = L>,
    ) -> Result<Self, DuplicateLayer<L>> {
        let mut layers = Vec::new();
        for layer in layers_strong_to_weak {
            if layers.contains(&layer) {
                return Err(DuplicateLayer { layer });
            }
            layers.push(layer);
        }

        Ok(Self {
            layers,
            opinions: BTreeMap::new(),
        })
    }

    /// Returns the layer order, strongest-to-weakest.
    #[must_use]
    pub fn layers(&self) -> &[L] {
        &self.layers
    }

    /// Sets the opinion `layer` authors for one `(address, field)` key.
    ///
    /// Replaces any opinion the same layer previously authored for the key.
    /// Returns [`UnknownLayer`] if `layer` was not supplied in the composer's
    /// layer order.
    pub fn set_opinion(
        &mut self,
        layer: L,
        address: A,
        field: F,
        op: OpinionOp<V, I, K>,
        provenance: P,
    ) -> Result<(), UnknownLayer<L>> {
        let Some(rank) = self.layer_rank(&layer) else {
            return Err(UnknownLayer { layer });
        };

        let stored = StoredOpinion {
            layer,
            rank,
            op,
            provenance,
        };
        let bucket = self
            .opinions
            .entry(OpinionKey::new(address, field))
            .or_default();
        match bucket.binary_search_by(|opinion| opinion.rank.cmp(&rank)) {
            Ok(index) => bucket[index] = stored,
            Err(index) => bucket.insert(index, stored),
        }
        Ok(())
    }

    /// Removes the opinion `layer` authored for one `(address, field)` key.
    ///
    /// Returns `Ok(true)` when an opinion was removed, `Ok(false)` when the
    /// layer had no opinion for the key, and [`UnknownLayer`] if `layer` was
    /// not supplied in the composer's layer order.
    pub fn remove_opinion(
        &mut self,
        layer: L,
        address: A,
        field: F,
    ) -> Result<bool, UnknownLayer<L>> {
        let Some(rank) = self.layer_rank(&layer) else {
            return Err(UnknownLayer { layer });
        };

        let key = OpinionKey::new(address, field);
        let Some(bucket) = self.opinions.get_mut(&key) else {
            return Ok(false);
        };
        let Ok(index) = bucket.binary_search_by(|opinion| opinion.rank.cmp(&rank)) else {
            return Ok(false);
        };
        bucket.remove(index);
        if bucket.is_empty() {
            self.opinions.remove(&key);
        }
        Ok(true)
    }

    /// Removes every opinion authored by `layer` and returns how many were
    /// removed.
    ///
    /// Returns [`UnknownLayer`] if `layer` was not supplied in the composer's
    /// layer order.
    pub fn clear_layer(&mut self, layer: L) -> Result<usize, UnknownLayer<L>> {
        let Some(rank) = self.layer_rank(&layer) else {
            return Err(UnknownLayer { layer });
        };

        let mut removed = 0;
        self.opinions.retain(|_, bucket| {
            let before = bucket.len();
            bucket.retain(|opinion| opinion.rank != rank);
            removed += before - bucket.len();
            !bucket.is_empty()
        });
        Ok(removed)
    }

    /// Returns the keys that currently have authored opinions, in key order.
    pub fn keys(&self) -> impl Iterator<Item = &OpinionKey<A, F>> {
        self.opinions.keys()
    }

    /// Returns the authored opinion stack for one key, strongest-to-weakest.
    ///
    /// This is an inspection API for diagnostics and explain views. Resolution
    /// still happens through [`SparseComposer::resolve`].
    #[must_use]
    pub fn opinion_stack(&self, address: A, field: F) -> Vec<OpinionRef<'_, L, V, I, K, P>> {
        self.opinions
            .get(&OpinionKey::new(address, field))
            .map(|bucket| {
                bucket
                    .iter()
                    .map(|opinion| OpinionRef {
                        layer: &opinion.layer,
                        op: &opinion.op,
                        provenance: &opinion.provenance,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn layer_rank(&self, layer: &L) -> Option<usize> {
        self.layers.iter().position(|candidate| candidate == layer)
    }
}

impl<L, A, F, V, I, K, P> SparseComposer<L, A, F, V, I, K, P>
where
    L: PartialEq,
    A: Ord,
    F: Ord,
    V: Clone,
    I: Clone + Eq,
    K: Clone + Ord,
    P: Clone,
{
    /// Resolves one field on one address.
    #[must_use]
    pub fn resolve(&self, address: A, field: F) -> Resolution<V, I, K, P> {
        match self.opinions.get(&OpinionKey::new(address, field)) {
            Some(bucket) => resolve_ordered_chain(bucket.iter().map(|opinion| ChainOpinion {
                op: &opinion.op,
                provenance: &opinion.provenance,
            })),
            None => Resolution::Absent,
        }
    }

    /// Resolves one field on one address over a fallback seed.
    ///
    /// The fallback participates as the weakest dense seed without being
    /// modeled as an extra layer; see [`resolve_ordered_chain_with_fallback`]
    /// for the exact semantics.
    #[must_use]
    pub fn resolve_with_fallback(
        &self,
        address: A,
        field: F,
        fallback: &ResolvedValue<V, I, K>,
    ) -> Resolution<V, I, K, P> {
        match self.opinions.get(&OpinionKey::new(address, field)) {
            Some(bucket) => resolve_ordered_chain_with_fallback(
                bucket.iter().map(|opinion| ChainOpinion {
                    op: &opinion.op,
                    provenance: &opinion.provenance,
                }),
                fallback,
            ),
            None => Resolution::Absent,
        }
    }

    /// Resolves one field and returns an explanation report.
    ///
    /// The report includes contributing opinions, incompatible mixed operation
    /// kinds, and block cutoffs. It is intended for diagnostics and authoring
    /// tools; [`SparseComposer::resolve`] remains the compact value API.
    #[must_use]
    pub fn explain(&self, address: A, field: F) -> ResolutionReport<V, I, K, P> {
        match self.opinions.get(&OpinionKey::new(address, field)) {
            Some(bucket) => {
                resolve_ordered_chain_report(bucket.iter().map(|opinion| ChainOpinion {
                    op: &opinion.op,
                    provenance: &opinion.provenance,
                }))
            }
            None => ResolutionReport {
                resolution: Resolution::Absent,
                events: Vec::new(),
            },
        }
    }
}

impl<V, I, K> OpinionOp<V, I, K> {
    /// Returns this operation's family.
    #[must_use]
    pub const fn kind(&self) -> OpinionKind {
        match self {
            Self::Set(_) => OpinionKind::Set,
            Self::Block => OpinionKind::Block,
            Self::List(_) => OpinionKind::List,
            Self::Dictionary(_) => OpinionKind::Dictionary,
        }
    }

    /// Returns `true` when this operation blocks weaker opinions.
    #[must_use]
    pub const fn is_block(&self) -> bool {
        matches!(self, Self::Block)
    }
}

/// Resolves an already ordered chain of opinions.
///
/// Opinions must be supplied strongest-to-weakest. This function is the
/// storage-agnostic resolver used by [`SparseComposer::resolve`]. It is useful
/// when a caller already has an ordered stack and does not need
/// [`SparseComposer`]'s sparse storage. Unlike
/// [`resolve_ordered_chain_report`], it does not record diagnostic events.
#[must_use]
pub fn resolve_ordered_chain<'a, V, I, K, P>(
    opinions_strong_to_weak: impl IntoIterator<Item = ChainOpinion<'a, V, I, K, P>>,
) -> Resolution<V, I, K, P>
where
    V: Clone + 'a,
    I: Clone + Eq + 'a,
    K: Clone + Ord + 'a,
    P: Clone + 'a,
{
    fold_ordered_chain(opinions_strong_to_weak, None)
}

/// Resolves an already ordered chain of opinions over a fallback seed.
///
/// Behaves exactly like [`resolve_ordered_chain`], with `fallback`
/// participating as the weakest dense seed for the resolved family: list
/// chains fold over the fallback list and dictionary chains combine over the
/// fallback entries. Scalar chains are strongest-wins and never consult the
/// seed, and a fallback whose shape does not match the resolved family is
/// ignored.
///
/// The fallback is a seed, not an opinion — it carries no provenance. An
/// empty chain therefore still resolves [`Resolution::Absent`], and a
/// strongest block still resolves [`Resolution::Blocked`]. A weaker block
/// cuts off weaker opinions, but edits accumulated from stronger opinions
/// still fold over the seed, matching [`resolve_family_chain`]'s block
/// semantics.
#[must_use]
pub fn resolve_ordered_chain_with_fallback<'a, V, I, K, P>(
    opinions_strong_to_weak: impl IntoIterator<Item = ChainOpinion<'a, V, I, K, P>>,
    fallback: &ResolvedValue<V, I, K>,
) -> Resolution<V, I, K, P>
where
    V: Clone + 'a,
    I: Clone + Eq + 'a,
    K: Clone + Ord + 'a,
    P: Clone + 'a,
{
    fold_ordered_chain(opinions_strong_to_weak, Some(fallback))
}

/// The shared fold used by both ordered-chain entry points.
///
/// When `fallback` is `Some`, it seeds the resolved family's fold as the
/// weakest dense value; when `None`, families fold over their empty seeds.
fn fold_ordered_chain<'a, V, I, K, P>(
    opinions_strong_to_weak: impl IntoIterator<Item = ChainOpinion<'a, V, I, K, P>>,
    fallback: Option<&ResolvedValue<V, I, K>>,
) -> Resolution<V, I, K, P>
where
    V: Clone + 'a,
    I: Clone + Eq + 'a,
    K: Clone + Ord + 'a,
    P: Clone + 'a,
{
    let mut opinions = opinions_strong_to_weak.into_iter();
    let Some(strongest) = opinions.next() else {
        return Resolution::Absent;
    };

    match strongest.op {
        OpinionOp::Block => Resolution::Blocked {
            provenance: strongest.provenance.clone(),
        },
        OpinionOp::Set(value) => Resolution::Resolved(Resolved {
            value: ResolvedValue::Scalar(value.clone()),
            provenance: strongest.provenance.clone(),
        }),
        OpinionOp::List(op) => {
            let mut ops = Vec::new();
            ops.push(op.clone());
            for opinion in opinions {
                match opinion.op {
                    OpinionOp::List(op) => ops.push(op.clone()),
                    OpinionOp::Block => break,
                    OpinionOp::Set(_) | OpinionOp::Dictionary(_) => {}
                }
            }
            let seed = match fallback {
                Some(ResolvedValue::List(items)) => items.as_slice(),
                _ => &[],
            };
            Resolution::Resolved(Resolved {
                value: ResolvedValue::List(resolve_list_chain(seed, ops)),
                provenance: strongest.provenance.clone(),
            })
        }
        OpinionOp::Dictionary(entries) => {
            let mut dicts = Vec::new();
            dicts.push(entries.as_slice());
            for opinion in opinions {
                match opinion.op {
                    OpinionOp::Dictionary(entries) => dicts.push(entries.as_slice()),
                    OpinionOp::Block => break,
                    OpinionOp::Set(_) | OpinionOp::List(_) => {}
                }
            }
            if let Some(ResolvedValue::Dictionary(entries)) = fallback {
                dicts.push(entries.as_slice());
            }
            Resolution::Resolved(Resolved {
                value: ResolvedValue::Dictionary(combine_dictionary_chain(&ShallowOverlay, dicts)),
                provenance: strongest.provenance.clone(),
            })
        }
    }
}

/// Resolves an already ordered chain and returns an explanation report.
///
/// Mixed operation families are intentionally not coerced. The strongest
/// non-block operation selects the resolved value family; weaker incompatible
/// operations are reported as ignored. A block stops all weaker opinions.
#[must_use]
pub fn resolve_ordered_chain_report<'a, V, I, K, P>(
    opinions_strong_to_weak: impl IntoIterator<Item = ChainOpinion<'a, V, I, K, P>>,
) -> ResolutionReport<V, I, K, P>
where
    V: Clone + 'a,
    I: Clone + Eq + 'a,
    K: Clone + Ord + 'a,
    P: Clone + 'a,
{
    let opinions: Vec<_> = opinions_strong_to_weak.into_iter().collect();
    let Some(strongest) = opinions.first() else {
        return ResolutionReport {
            resolution: Resolution::Absent,
            events: Vec::new(),
        };
    };

    let mut events = Vec::new();

    match strongest.op {
        OpinionOp::Block => {
            events.push(ResolutionEvent::StoppedByBlock {
                provenance: strongest.provenance.clone(),
            });
            events.extend(
                opinions
                    .iter()
                    .skip(1)
                    .map(|opinion| ResolutionEvent::Ignored {
                        provenance: opinion.provenance.clone(),
                        reason: IgnoreReason::WeakerThanBlock,
                    }),
            );
            ResolutionReport {
                resolution: Resolution::Blocked {
                    provenance: strongest.provenance.clone(),
                },
                events,
            }
        }
        OpinionOp::Set(value) => {
            events.push(ResolutionEvent::Contributed {
                provenance: strongest.provenance.clone(),
                kind: OpinionKind::Set,
            });
            events.extend(
                opinions
                    .iter()
                    .skip(1)
                    .map(|opinion| ResolutionEvent::Ignored {
                        provenance: opinion.provenance.clone(),
                        reason: IgnoreReason::WeakerThanSet,
                    }),
            );
            ResolutionReport {
                resolution: Resolution::Resolved(Resolved {
                    value: ResolvedValue::Scalar(value.clone()),
                    provenance: strongest.provenance.clone(),
                }),
                events,
            }
        }
        OpinionOp::List(_) => {
            let mut ops = Vec::new();
            record_list_events(&opinions, &mut ops, &mut events);
            ResolutionReport {
                resolution: Resolution::Resolved(Resolved {
                    value: ResolvedValue::List(resolve_list_chain(&[], ops)),
                    provenance: strongest.provenance.clone(),
                }),
                events,
            }
        }
        OpinionOp::Dictionary(_) => {
            let mut dicts = Vec::new();
            record_dictionary_events(&opinions, &mut dicts, &mut events);
            ResolutionReport {
                resolution: Resolution::Resolved(Resolved {
                    value: ResolvedValue::Dictionary(combine_dictionary_chain(
                        &ShallowOverlay,
                        dicts,
                    )),
                    provenance: strongest.provenance.clone(),
                }),
                events,
            }
        }
    }
}

fn record_list_events<'a, V, I, K, P>(
    opinions: &[ChainOpinion<'a, V, I, K, P>],
    ops: &mut Vec<ListOp<I>>,
    events: &mut Vec<ResolutionEvent<P>>,
) where
    I: Clone,
    P: Clone,
{
    for (index, opinion) in opinions.iter().enumerate() {
        match opinion.op {
            OpinionOp::List(op) => {
                ops.push(op.clone());
                events.push(ResolutionEvent::Contributed {
                    provenance: opinion.provenance.clone(),
                    kind: OpinionKind::List,
                });
            }
            OpinionOp::Block => {
                record_block_cutoff(opinions, index, events);
                break;
            }
            OpinionOp::Set(_) | OpinionOp::Dictionary(_) => {
                events.push(ResolutionEvent::Ignored {
                    provenance: opinion.provenance.clone(),
                    reason: IgnoreReason::IncompatibleOperation {
                        resolved: OpinionKind::List,
                        ignored: opinion.op.kind(),
                    },
                });
            }
        }
    }
}

fn record_dictionary_events<'a, V, I, K, P>(
    opinions: &[ChainOpinion<'a, V, I, K, P>],
    dicts: &mut Vec<&'a [(K, V)]>,
    events: &mut Vec<ResolutionEvent<P>>,
) where
    P: Clone,
{
    for (index, opinion) in opinions.iter().enumerate() {
        match opinion.op {
            OpinionOp::Dictionary(entries) => {
                dicts.push(entries.as_slice());
                events.push(ResolutionEvent::Contributed {
                    provenance: opinion.provenance.clone(),
                    kind: OpinionKind::Dictionary,
                });
            }
            OpinionOp::Block => {
                record_block_cutoff(opinions, index, events);
                break;
            }
            OpinionOp::Set(_) | OpinionOp::List(_) => {
                events.push(ResolutionEvent::Ignored {
                    provenance: opinion.provenance.clone(),
                    reason: IgnoreReason::IncompatibleOperation {
                        resolved: OpinionKind::Dictionary,
                        ignored: opinion.op.kind(),
                    },
                });
            }
        }
    }
}

fn record_block_cutoff<'a, V, I, K, P>(
    opinions: &[ChainOpinion<'a, V, I, K, P>],
    block_index: usize,
    events: &mut Vec<ResolutionEvent<P>>,
) where
    P: Clone,
{
    let block = &opinions[block_index];
    events.push(ResolutionEvent::StoppedByBlock {
        provenance: block.provenance.clone(),
    });
    events.extend(
        opinions
            .iter()
            .skip(block_index + 1)
            .map(|opinion| ResolutionEvent::Ignored {
                provenance: opinion.provenance.clone(),
                reason: IgnoreReason::WeakerThanBlock,
            }),
    );
}
