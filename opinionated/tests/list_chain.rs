// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! List-chain semantics vectors.
//!
//! These cases mirror the AOUSD Core §12.4 `ListOps` supplemental reference
//! vectors that `layerstack` relies on; `layerstack` re-exports this kernel.

use opinionated::{ListOp, resolve_list_chain};

#[test]
fn stronger_explicit_overrides_weaker_edits() {
    // Weak appends should not override a stronger explicit opinion.
    let weak = ListOp::appended(vec![2_u32]);
    let strong = ListOp::explicit(vec![1_u32]);

    assert_eq!(resolve_list_chain::<u32>(&[], [strong, weak]), vec![1_u32]);
}

#[test]
fn chain_applies_weak_to_strong() {
    let weak = ListOp::appended(vec![2_u32]);
    let strong = ListOp::appended(vec![1_u32]);

    assert_eq!(
        resolve_list_chain::<u32>(&[], [strong, weak]),
        vec![2_u32, 1_u32]
    );
}

#[test]
fn append_moves_existing_item_to_end() {
    // Supplemental combine_chain/append_over_explicit.json:
    // appending 100 over explicit [100, 150] yields [150, 100].
    let weak = ListOp::explicit(vec![100_u32, 150_u32]);
    let strong = ListOp::appended(vec![100_u32]);
    assert_eq!(
        resolve_list_chain::<u32>(&[], [strong, weak]),
        vec![150_u32, 100_u32]
    );
}

#[test]
fn prepend_moves_existing_item_to_front() {
    // Supplemental combine_chain/prepend_over_composable.json:
    // prepending [75, 150] over appended [100, 150] yields ordered elements [75, 150, 100].
    let weak = ListOp::appended(vec![100_u32, 150_u32]);
    let strong = ListOp::prepended(vec![75_u32, 150_u32]);
    assert_eq!(
        resolve_list_chain::<u32>(&[], [strong, weak]),
        vec![75_u32, 150_u32, 100_u32]
    );
}

#[test]
fn constructors_and_builders_author_each_list() {
    let op = ListOp::prepended(vec![1_u32])
        .with_appended(vec![2])
        .with_deleted(vec![3]);
    assert_eq!(op.explicit, None);
    assert_eq!(
        (
            op.prepend.as_slice(),
            op.append.as_slice(),
            op.delete.as_slice()
        ),
        ([1].as_slice(), [2].as_slice(), [3].as_slice())
    );
    assert_eq!(ListOp::<u32>::new(), ListOp::default());
    assert_eq!(ListOp::new().apply_to(&[4_u32, 5]), vec![4, 5]);
    assert_eq!(ListOp::deleted(vec![4_u32]).apply_to(&[4, 5]), vec![5]);
}

#[test]
fn items_and_lists_cover_every_list() {
    let mut op = ListOp::explicit(vec![1_u32])
        .with_deleted(vec![2])
        .with_prepended(vec![3])
        .with_appended(vec![4]);
    assert_eq!(op.items().copied().collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    for list in op.lists_mut() {
        list.iter_mut().for_each(|item| *item *= 10);
    }
    assert_eq!(
        op.items().copied().collect::<Vec<_>>(),
        vec![10, 20, 30, 40]
    );
    let names = op.map_lists(|items| items.iter().map(u32::to_string).collect());
    assert_eq!(names.explicit, Some(vec!["10".to_string()]));
    assert_eq!(names.append, vec!["40".to_string()]);
}

// Reorder semantics follow OpenUSD's `SdfListOp::_ReorderKeysHelper`
// (`pxr/usd/sdf/listOp.cpp`); AOUSD Core §12.4.

#[test]
fn reorder_moves_named_items_with_their_followers() {
    let op = ListOp::reordered(vec!["c", "a"]);
    assert_eq!(op.apply_to(&["a", "b", "c", "d"]), vec!["c", "d", "a", "b"]);
}

#[test]
fn reorder_keeps_leading_unnamed_items_in_front() {
    let op = ListOp::reordered(vec!["b", "a"]);
    assert_eq!(op.apply_to(&["x", "a", "b"]), vec!["x", "b", "a"]);
}

#[test]
fn reorder_ignores_items_the_list_does_not_hold_and_repeats() {
    let op = ListOp::reordered(vec!["z", "b", "b", "a"]);
    assert_eq!(op.apply_to(&["a", "b"]), vec!["b", "a"]);
    assert_eq!(
        ListOp::reordered(vec!["z"]).apply_to(&["a", "b"]),
        vec!["a", "b"]
    );
}

#[test]
fn reorder_applies_after_the_other_edits_of_its_operation() {
    let op = ListOp::appended(vec!["c"])
        .with_deleted(vec!["b"])
        .with_reordered(vec!["c", "a"]);
    assert_eq!(op.apply_to(&["a", "b"]), vec!["c", "a"]);
    // An explicit list makes the reorder spurious.
    let op = ListOp::explicit(vec!["a", "b"]).with_reordered(vec!["b", "a"]);
    assert_eq!(op.apply_to(&[]), vec!["a", "b"]);
}

#[test]
fn stronger_reorder_orders_weaker_items() {
    let weak = ListOp::prepended(vec!["clay", "board"]);
    let strong = ListOp::reordered(vec!["board", "clay"]);
    assert_eq!(
        resolve_list_chain::<&str>(&[], [strong, weak]),
        vec!["board", "clay"]
    );
}

#[test]
fn merge_combines_two_statements_of_one_spec() {
    let mut op = ListOp::prepended(vec![1_u32]).with_reordered(vec![2]);
    op.merge(ListOp::appended(vec![3]).with_reordered(vec![1]));
    assert_eq!(op.prepend, vec![1]);
    assert_eq!(op.append, vec![3]);
    assert_eq!(op.reorder, vec![2, 1]);
    op.merge(ListOp::explicit(vec![4]));
    assert_eq!(op.explicit, Some(vec![4]));
}

#[test]
fn an_empty_explicit_list_is_authored() {
    assert!(ListOp::<u32>::new().is_empty());
    assert!(!ListOp::<u32>::explicit(Vec::new()).is_empty());
    assert!(!ListOp::reordered(vec![1_u32]).is_empty());
}

// The legacy `add` follows OpenUSD's `SdfListOp::_AddKeys`: an item the
// list lacks goes to the back, one it holds stays in place.

#[test]
fn add_inserts_missing_items_before_the_reorder() {
    let op = ListOp::added(vec!["d"]).with_reordered(vec!["c", "a"]);
    assert_eq!(op.apply_to(&["a", "b", "c"]), vec!["c", "d", "a", "b"]);
}

#[test]
fn add_leaves_a_held_item_where_it_is() {
    assert_eq!(
        ListOp::added(vec!["a"]).apply_to(&["a", "b"]),
        vec!["a", "b"]
    );
    assert_eq!(
        ListOp::appended(vec!["a"]).apply_to(&["a", "b"]),
        vec!["b", "a"]
    );
    let op = ListOp::added(vec!["a"]).with_reordered(vec!["b"]);
    assert_eq!(op.apply_to(&["a", "b"]), vec!["a", "b"]);
}

#[test]
fn add_applies_after_delete_and_before_prepend() {
    let op = ListOp::added(vec!["a", "c"])
        .with_deleted(vec!["a"])
        .with_prepended(vec!["c"]);
    assert_eq!(op.apply_to(&["a", "b"]), vec!["c", "b", "a"]);
}
