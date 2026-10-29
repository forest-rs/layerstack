// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Relational attribute and mapper targets composed through arcs as
//! OpenUSD 26.11 composes them.
//!
//! `scripts/relational_arcs_oracle.py` writes `fixtures/relational_arcs`
//! (the layers and `oracle.json`) and documents every case: an asset's
//! targets of relational attributes (`</A.b[/C].d>`) and mappers
//! (`</A.b.mapper[/C]>`), and connections to relational attributes,
//! through each arc kind, nested brackets, relative paths and paths that
//! do not map. Here, layerstack must compose the same targets and
//! connections for every property, instance proxies included, and its
//! flattened layer must author the same explicit lists as OpenUSD's.
//!
//! Spec: AOUSD Core §8 (paths), §10.2 (arcs map namespace), §10.3.2
//! (target paths across arcs). OpenUSD: `Pcp_TranslatePath`
//! (`pxr/usd/pcp/pathTranslation.cpp`).

use std::collections::BTreeMap;

use layerstack::stage::flatten::FlattenRequirements;
use layerstack::{LayerId, PropertyPath, Stage, StageOptions, TargetPath};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/relational_arcs/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    targets: BTreeMap<String, Vec<String>>,
    flattened: BTreeMap<String, Vec<String>>,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert!(
        oracle.openusd_version.starts_with("26."),
        "oracle from OpenUSD {}",
        oracle.openusd_version
    );
    oracle
}

/// Every property's composed targets and every flattened property spec's
/// explicit list match OpenUSD's; the failure lists every one that
/// differs.
#[test]
fn relational_targets_map_through_every_arc_as_openusd_maps_them() {
    let oracle = oracle();
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/relational_arcs/root.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let store = &loaded.store;
    let show = |items: &[TargetPath]| -> Vec<String> {
        items
            .iter()
            .map(|t| t.display(store.paths(), store.tokens()))
            .collect()
    };
    let mut failures = Vec::new();
    {
        let view = stage.view(store);
        for (path, want) in &oracle.targets {
            let Some(property) = view.property(path) else {
                failures.push(format!("{path}: not composed"));
                continue;
            };
            let got = match property.as_relationship() {
                Some(relationship) => show(&relationship.targets()),
                None => show(&property.as_attribute().expect("attribute").connections()),
            };
            if &got != want {
                failures.push(format!("{path}: got {got:?}, OpenUSD {want:?}"));
            }
        }
    }
    let id = LayerId(
        loaded
            .store
            .layer_ids()
            .iter()
            .map(|id| id.0)
            .max()
            .unwrap_or(0)
            + 1,
    );
    let flat = stage
        .flatten(
            &mut loaded.store,
            loaded.root_layer,
            id,
            &FlattenRequirements::default(),
        )
        .expect("flattens");
    let store = &loaded.store;
    let mut flattened = BTreeMap::new();
    for (&prim, spec) in flat.layer.prims().iter() {
        for entry in &spec.properties {
            let (name, property) = (entry.name, &entry.spec);
            let Some(explicit) = property.targets.as_ref().and_then(|t| t.explicit.as_ref()) else {
                continue;
            };
            let path = PropertyPath::new(prim, name).display(store.paths(), store.tokens());
            let items: Vec<String> = explicit
                .iter()
                .map(|t| t.display(store.paths(), store.tokens()))
                .collect();
            flattened.insert(path, items);
        }
    }
    for (path, want) in &oracle.flattened {
        match flattened.get(path) {
            Some(got) if got == want => {}
            got => failures.push(format!("flattened {path}: got {got:?}, OpenUSD {want:?}")),
        }
    }
    for path in flattened.keys() {
        if !oracle.flattened.contains_key(path) {
            failures.push(format!("flattened {path}: OpenUSD authors no list"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
