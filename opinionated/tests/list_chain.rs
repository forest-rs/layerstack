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
