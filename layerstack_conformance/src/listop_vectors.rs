// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ListOp` conformance tests derived from AOUSD supplemental materials.
//!
//! Source: `core-spec-supplemental-release_dec2025/data_types/tests/combine_chain/*.json`.
//!
//! Spec: AOUSD Core §12.4 (`ListOps`).

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CombineChainCase {
    pub description: String,
    pub chain: Vec<ListOpData>,
    #[serde(rename = "combined_reduced")]
    pub combined_reduced: ListOpData,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListOpData {
    #[serde(default)]
    pub explicit_items: Option<Vec<u32>>,
    #[serde(default)]
    pub prepended_items: Vec<u32>,
    #[serde(default)]
    pub appended_items: Vec<u32>,
    #[serde(default)]
    pub deleted_items: Vec<u32>,
}

impl ListOpData {
    pub fn to_listop(&self) -> layerstack::ListOp<u32> {
        layerstack::ListOp {
            explicit: self.explicit_items.clone(),
            prepend: self.prepended_items.clone(),
            append: self.appended_items.clone(),
            delete: self.deleted_items.clone(),
        }
    }

    pub fn expected_ordered_elements(&self) -> Vec<u32> {
        if let Some(explicit) = &self.explicit_items {
            return explicit.clone();
        }

        // Mirrors the supplemental reference implementation's `ordered_elements`:
        // chain(prepended if not in appended, appended).
        let mut out = Vec::new();
        out.extend(
            self.prepended_items
                .iter()
                .copied()
                .filter(|x| !self.appended_items.contains(x)),
        );
        out.extend(self.appended_items.iter().copied());
        out
    }
}

pub fn load_cases(path: &Path) -> Vec<CombineChainCase> {
    let text = std::fs::read_to_string(path).expect("read json file");
    serde_json::from_str::<Vec<CombineChainCase>>(&text).expect("parse json")
}
