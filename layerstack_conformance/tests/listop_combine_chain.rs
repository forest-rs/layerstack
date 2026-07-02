// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden tests for `ListOp` behavior.
//!
//! Inputs are sourced from the AOUSD supplemental release:
//! `core-spec-supplemental-release_dec2025/data_types/tests/combine_chain/*.json`.

use std::path::PathBuf;

use layerstack::listop::resolve_list_chain;

use layerstack_conformance::{listop_vectors::load_cases, workspace_root};

fn vectors_dir() -> PathBuf {
    workspace_root()
        .join("core-spec-supplemental-release_dec2025")
        .join("data_types")
        .join("tests")
        .join("combine_chain")
}

#[test]
fn combine_chain_vectors_match_ordered_elements() {
    let dir = vectors_dir();
    let files = [
        "inert_only.json",
        "explicit_only.json",
        "composable_only.json",
        "append_over_explicit.json",
        "prepend_over_explicit.json",
        "delete_over_explicit.json",
        "prepend_over_composable.json",
        "append_over_composable.json",
    ];

    for filename in files {
        let path = dir.join(filename);
        let cases = load_cases(&path);

        for (index, case) in cases.into_iter().enumerate() {
            let _ = index;
            let ops_strong_to_weak = case.chain.into_iter().map(|op| op.to_listop());
            let resolved = resolve_list_chain::<u32>(&[], ops_strong_to_weak);
            let expected = case.combined_reduced.expected_ordered_elements();
            assert_eq!(
                resolved, expected,
                "vector mismatch: file={filename} desc={}",
                case.description
            );
        }
    }
}
