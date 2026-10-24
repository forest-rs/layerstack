// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition reports its errors in the same order every time.
//!
//! Each map composition builds hashes with seeds of its own, so composing
//! one stage repeatedly in one process visits hashed collections in many
//! orders. [`Stage::composition_errors`] must not follow any of them.
//!
//! The entries are every supplemental composition fixture and every layer
//! of the local error, expression variable and relocation fixtures.
//!
//! Spec: AOUSD Core §10.6 (composition errors).

use std::path::{Path, PathBuf};

use layerstack::{Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};

const ROUNDS: usize = 12;

fn entries() -> Vec<PathBuf> {
    let mut entries = Vec::new();
    let supplemental = workspace_root()
        .join("core-spec-supplemental-release_dec2025")
        .join("composition")
        .join("tests")
        .join("assets");
    for case in std::fs::read_dir(&supplemental).expect("supplemental assets") {
        let dir = case.expect("case").path();
        let Ok(pcp) = std::fs::read_to_string(dir.join("pcp.json")) else {
            continue;
        };
        let pcp: serde_json::Value = serde_json::from_str(&pcp).expect("pcp.json");
        if let Some(entry) = pcp["entry"].as_str() {
            entries.push(dir.join("usda").join(entry));
        }
    }
    let fixtures = workspace_root()
        .join("layerstack_conformance")
        .join("fixtures");
    for dir in ["composition_errors", "expression_variables", "relocates"] {
        for file in std::fs::read_dir(fixtures.join(dir)).expect("fixtures") {
            let path = file.expect("fixture").path();
            if path.extension().is_some_and(|ext| ext == "usda") {
                entries.push(path);
            }
        }
    }
    entries.sort();
    entries
}

fn errors(entry: &Path) -> Vec<String> {
    let mut loaded = load_entry_usda(entry);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    stage
        .composition_errors()
        .iter()
        .map(|error| format!("{error:?}"))
        .collect()
}

#[test]
fn errors_are_reported_in_one_order() {
    let mut varying = Vec::new();
    for entry in entries() {
        let first = errors(&entry);
        if first.len() < 2 {
            continue;
        }
        if (1..ROUNDS).any(|_| errors(&entry) != first) {
            varying.push(entry.display().to_string());
        }
    }
    assert!(varying.is_empty(), "error order varies: {varying:#?}");
}
