//! List operation semantics (`ListOps`).
//!
//! A [`ListOp`] edits an ordered, unique-element list. `ListOps` can be chained in
//! strength order to implement deterministic list composition.
//!
//! Spec: AOUSD Core §12.4 (`ListOps`).

use alloc::vec::Vec;

/// A list operation over unique elements.
///
/// ```
/// use layerstack::ListOp;
///
/// let op = ListOp {
///     prepend: vec![1, 2],
///     append: vec![5],
///     delete: vec![3],
///     ..ListOp::default()
/// };
/// assert_eq!(op.apply_to(&[3, 4]), vec![1, 2, 4, 5]);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListOp<T> {
    /// If present, replaces the current list before other edits apply.
    pub explicit: Option<Vec<T>>,
    /// Prepends elements (preserving uniqueness).
    pub prepend: Vec<T>,
    /// Appends elements (preserving uniqueness).
    pub append: Vec<T>,
    /// Deletes elements (by equality).
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
    /// Applies this operation to a base list.
    #[must_use]
    pub fn apply_to(&self, base: &[T]) -> Vec<T> {
        // Spec: AOUSD Core §12.4 (`ListOps`) defines list op properties as
        // ordered unique sequences. If an explicit list is authored, other
        // operations are considered spurious (see supplemental reference
        // implementation behavior).
        if let Some(explicit) = &self.explicit {
            return explicit.clone();
        }

        let mut out = base.to_vec();

        if !self.delete.is_empty() {
            out.retain(|item| !self.delete.contains(item));
        }

        // Prepend moves items to the front, preserving the specified order.
        for item in self.prepend.iter().rev() {
            out.retain(|x| x != item);
            out.insert(0, item.clone());
        }

        // Append moves items to the end, preserving the specified order.
        for item in &self.append {
            out.retain(|x| x != item);
            out.push(item.clone());
        }

        out
    }
}

/// Resolves a chain of list operations in strength order.
///
/// `ops_strong_to_weak` must yield the strongest op first.
///
/// Spec: AOUSD Core §12.4 (`ListOps`) composes lists via strength ordering, with
/// stronger opinions taking precedence over weaker ones. This function applies
/// operations from weakest → strongest so that stronger ops have the last word.
///
/// ```
/// use layerstack::listop::{ListOp, resolve_list_chain};
///
/// let weak = ListOp { append: vec![1_u32, 2], ..ListOp::default() };
/// let strong = ListOp { prepend: vec![0_u32], ..ListOp::default() };
///
/// // Strong op runs last, so 0 ends up at the front.
/// assert_eq!(resolve_list_chain::<u32>(&[], [strong, weak]), vec![0, 1, 2]);
/// ```
#[must_use]
pub fn resolve_list_chain<T: Clone + Eq>(
    base: &[T],
    ops_strong_to_weak: impl IntoIterator<Item = ListOp<T>>,
) -> Vec<T> {
    let mut out = base.to_vec();
    let mut ops: Vec<ListOp<T>> = ops_strong_to_weak.into_iter().collect();
    ops.reverse();
    for op in ops {
        out = op.apply_to(&out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn stronger_explicit_overrides_weaker_edits() {
        // Weak appends should not override a stronger explicit opinion.
        let weak = ListOp {
            append: vec![2_u32],
            ..ListOp::default()
        };
        let strong = ListOp {
            explicit: Some(vec![1_u32]),
            ..ListOp::default()
        };

        assert_eq!(resolve_list_chain::<u32>(&[], [strong, weak]), vec![1_u32]);
    }

    #[test]
    fn chain_applies_weak_to_strong() {
        let weak = ListOp {
            append: vec![2_u32],
            ..ListOp::default()
        };
        let strong = ListOp {
            append: vec![1_u32],
            ..ListOp::default()
        };

        assert_eq!(
            resolve_list_chain::<u32>(&[], [strong, weak]),
            vec![2_u32, 1_u32]
        );
    }

    #[test]
    fn append_moves_existing_item_to_end() {
        // Supplemental combine_chain/append_over_explicit.json:
        // appending 100 over explicit [100, 150] yields [150, 100].
        let weak = ListOp {
            explicit: Some(vec![100_u32, 150_u32]),
            ..ListOp::default()
        };
        let strong = ListOp {
            append: vec![100_u32],
            ..ListOp::default()
        };
        assert_eq!(
            resolve_list_chain::<u32>(&[], [strong, weak]),
            vec![150_u32, 100_u32]
        );
    }

    #[test]
    fn prepend_moves_existing_item_to_front() {
        // Supplemental combine_chain/prepend_over_composable.json:
        // prepending [75, 150] over appended [100, 150] yields ordered elements [75, 150, 100].
        let weak = ListOp {
            append: vec![100_u32, 150_u32],
            ..ListOp::default()
        };
        let strong = ListOp {
            prepend: vec![75_u32, 150_u32],
            ..ListOp::default()
        };
        assert_eq!(
            resolve_list_chain::<u32>(&[], [strong, weak]),
            vec![75_u32, 150_u32, 100_u32]
        );
    }
}
