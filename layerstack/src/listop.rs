// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! List operation semantics (`ListOps`).
//!
//! A [`ListOp`] edits an ordered, unique-element list. `ListOps` can be chained
//! in strength order to implement deterministic list composition.
//!
//! The implementation is the domain-neutral list-edit kernel from the
//! [`opinionated`] crate; `layerstack` re-exports it so both crates share one
//! set of semantics.
//!
//! Spec: AOUSD Core §12.4 (`ListOps`). List op properties are ordered unique
//! sequences; an authored explicit list makes the other edits spurious (see
//! supplemental reference implementation behavior); a reorder moves the
//! items it names into its order after the other edits, as OpenUSD's
//! `SdfListOp::ApplyOperations` does. Chains compose via strength ordering:
//! operations apply from weakest → strongest so that stronger ops have the
//! last word.
//!
//! ```
//! use layerstack::listop::{ListOp, resolve_list_chain};
//!
//! let op = ListOp::prepended(vec![1, 2])
//!     .with_appended(vec![5])
//!     .with_deleted(vec![3]);
//! assert_eq!(op.apply_to(&[3, 4]), vec![1, 2, 4, 5]);
//!
//! let weak = ListOp::appended(vec![1_u32, 2]);
//! let strong = ListOp::prepended(vec![0_u32]);
//!
//! // Strong op runs last, so 0 ends up at the front.
//! assert_eq!(resolve_list_chain::<u32>(&[], [strong, weak]), vec![0, 1, 2]);
//!
//! // A stronger reorder orders the weaker items.
//! let order = ListOp::reordered(vec![2_u32, 1]);
//! let weak = ListOp::appended(vec![1_u32, 2]);
//! assert_eq!(resolve_list_chain::<u32>(&[], [order, weak]), vec![2, 1]);
//! ```

pub use opinionated::{ListOp, resolve_list_chain};

use alloc::boxed::Box;
use core::{fmt, ops};

/// A [`ListOp`] kept out of line, allocated only once it is authored: a
/// spec's list ops are mostly empty, and an empty one is a null pointer
/// here rather than six empty vectors.
///
/// It reads as the [`ListOp`] it holds, an empty one when none is
/// authored. [`Self::make_mut`] allocates the op to author into;
/// [`Self::get_mut`] edits an authored one and allocates nothing. An op
/// emptied in place stays allocated, and still reads, and compares, as
/// empty.
pub struct SparseListOp<T>(Option<Box<ListOp<T>>>);

impl<T: 'static> SparseListOp<T> {
    const EMPTY: &'static ListOp<T> = &ListOp::new();

    /// An empty list op, which allocates nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self(None)
    }

    /// The list op to edit in place, if one is authored: `None`, and no
    /// allocation, for an empty one.
    pub fn get_mut(&mut self) -> Option<&mut ListOp<T>> {
        self.0.as_deref_mut()
    }

    /// The list op to author into, allocated first if none is authored.
    pub fn make_mut(&mut self) -> &mut ListOp<T> {
        self.0.get_or_insert_with(|| Box::new(ListOp::new()))
    }

    /// The list op, moved out.
    #[must_use]
    pub fn into_inner(self) -> ListOp<T> {
        self.0.map_or_else(ListOp::new, |op| *op)
    }
}

impl<T> Default for SparseListOp<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T: Clone> Clone for SparseListOp<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: 'static> ops::Deref for SparseListOp<T> {
    type Target = ListOp<T>;

    fn deref(&self) -> &ListOp<T> {
        self.0.as_deref().unwrap_or(Self::EMPTY)
    }
}

/// An op that authors nothing ([`ListOp::is_empty`]) allocates nothing.
impl<T> From<ListOp<T>> for SparseListOp<T> {
    fn from(op: ListOp<T>) -> Self {
        Self((!op.is_empty()).then(|| Box::new(op)))
    }
}

impl<T: PartialEq + 'static> PartialEq for SparseListOp<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Eq + 'static> Eq for SparseListOp<T> {}

impl<T: PartialEq + 'static> PartialEq<ListOp<T>> for SparseListOp<T> {
    fn eq(&self, other: &ListOp<T>) -> bool {
        **self == *other
    }
}

impl<T: fmt::Debug + 'static> fmt::Debug for SparseListOp<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}
