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

/// Duplicate sequences are outside AOUSD Core §6.6.3.2's valid input domain.
/// Check OpenUSD's handling separately from the normative supplemental vectors.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn duplicate_list_inputs_match_openusd() {
    use layerstack::ListOp;
    use std::process::Command;

    let explicit_python = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit_python.as_deref().unwrap_or("python3");
    let available = Command::new(python)
        .args(["-c", "from pxr import Sdf, Usd"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit_python.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr"
    );
    if !available {
        eprintln!("skipped duplicate-list oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }

    // Exhaust every sequence up to six items over three distinct values,
    // including empty, unique and duplicate inputs (1,093 sequences).
    let mut inputs = Vec::new();
    for len in 0..=6 {
        for mut encoded in 0..3_usize.pow(len) {
            let input: Vec<u32> = (0..len)
                .map(|_| {
                    let item = u32::try_from(encoded % 3).unwrap();
                    encoded /= 3;
                    item
                })
                .collect();
            inputs.push(input);
        }
    }
    let output = Command::new(python)
        .args([
            "-c",
            r#"
import json, sys
from pxr import Sdf, Usd
assert Usd.GetVersion() == (0, 26, 11), Usd.GetVersion()
rows = []
for items in json.loads(sys.argv[1]):
    rows.append([
        Sdf.IntListOp.CreateExplicit(items).ApplyOperations([0, 4]),
        Sdf.IntListOp.Create(prependedItems=items).ApplyOperations([0, 4]),
        Sdf.IntListOp.Create(appendedItems=items).ApplyOperations([0, 4]),
    ])
print(json.dumps(rows))
"#,
        ])
        .arg(serde_json::to_string(&inputs).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenUSD oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<[Vec<u32>; 3]> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(expected.len(), inputs.len());
    for (items, results) in inputs.into_iter().zip(expected) {
        let operations = [
            ListOp::explicit(items.clone()),
            ListOp::prepended(items.clone()),
            ListOp::appended(items),
        ];
        for (operation, expected) in operations.into_iter().zip(results) {
            assert_eq!(operation.apply_to(&[0, 4]), expected, "{operation:?}");
        }
    }
}
