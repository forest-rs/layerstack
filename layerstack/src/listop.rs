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

use crate::HashSet;
use alloc::{boxed::Box, vec::Vec};
use core::{fmt, hash::Hash, ops};

/// Appends, in order, each item of `weak` that `strong` does not hold yet:
/// the union that child names take across layers (native
/// `PcpComposeSiteChildNames`) and that stitching takes of two orders.
///
/// Short lists compare each pair; longer ones hash, so a prim with many
/// children unions them in linear time.
pub(crate) fn append_unique<T: Clone + Eq + Hash>(strong: &mut Vec<T>, weak: &[T]) {
    const PAIRWISE: usize = 256;
    if strong.len().saturating_mul(weak.len()) <= PAIRWISE {
        for value in weak {
            if !strong.contains(value) {
                strong.push(value.clone());
            }
        }
        return;
    }
    let added: Vec<T> = {
        let mut seen: HashSet<&T> = strong.iter().collect();
        weak.iter()
            .filter(|value| seen.insert(value))
            .cloned()
            .collect()
    };
    strong.extend(added);
}

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

    /// Whether a list op is authored: one was authored into
    /// ([`Self::make_mut`]), even if edits in place have since emptied it.
    /// OpenUSD keeps a list op field whose items an edit removes
    /// (`SdfListOp::ModifyOperations`) and writes it with no statements,
    /// as `UsdNamespaceEditor` leaves an arc to a deleted prim.
    #[must_use]
    pub fn is_authored(&self) -> bool {
        self.0.is_some()
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

#[cfg(test)]
mod tests {
    use super::append_unique;
    use alloc::vec::Vec;

    #[test]
    fn append_unique_keeps_strong_order_then_new_weak_items() {
        let mut strong = alloc::vec![3, 1];
        append_unique(&mut strong, &[1, 4, 3, 2, 4]);
        assert_eq!(strong, [3, 1, 4, 2]);
    }

    #[test]
    fn append_unique_hashes_long_lists_to_the_same_union() {
        let strong: Vec<u32> = (0..400).step_by(2).collect();
        let weak: Vec<u32> = (0..400).rev().chain(0..400).collect();
        let mut pairwise = strong.clone();
        for value in &weak {
            if !pairwise.contains(value) {
                pairwise.push(*value);
            }
        }
        let mut hashed = strong;
        append_unique(&mut hashed, &weak);
        assert_eq!(hashed, pairwise);
    }
}
