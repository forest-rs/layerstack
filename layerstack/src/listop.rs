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
