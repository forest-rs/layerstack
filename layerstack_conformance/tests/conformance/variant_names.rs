// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A prim's composed variant names per variant set, against OpenUSD 26.11
//! `UsdVariantSet::GetVariantNames`.
use crate::support::schema_scene as support;
use serde::Deserialize;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    prim: String,
    set: String,
    names: Vec<String>,
}

#[test]
fn variant_names_match_cpp() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../../fixtures/variant_names/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(include_str!("../../fixtures/variant_names/scene.usda"));
    for row in &oracle.rows {
        let prim = live.stage().prim(store.path(&row.prim), &store).unwrap();
        assert_eq!(
            prim.variant_names(row.set.as_str()),
            row.names,
            "{} {}",
            row.prim,
            row.set
        );
    }
}
