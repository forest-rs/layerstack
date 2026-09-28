// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transform variability and sample times are composed, not authored inventories.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{Scene, usd_geom::Xformable};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    records: Vec<Record>,
}
#[derive(Deserialize)]
struct Record {
    path: String,
    varying: bool,
    times: Vec<f64>,
    interval: Vec<f64>,
}
#[test]
fn composed_transform_queries_match_cpp() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/transform_queries/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/transform_queries/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let paths: Vec<_> = oracle
        .records
        .iter()
        .map(|r| loaded.store.path(&r.path))
        .collect();
    let scene = Scene::new(&stage, &loaded.store);
    for (record, path) in oracle.records.iter().zip(paths) {
        let view = Xformable::new(&scene, path).unwrap();
        assert_eq!(
            view.transform_might_be_time_varying(),
            record.varying,
            "{}",
            record.path
        );
        assert_eq!(
            view.transform_time_samples(),
            record.times,
            "{}",
            record.path
        );
        assert_eq!(
            view.transform_time_samples_in_interval(1.0, 5.0),
            record.interval,
            "{}",
            record.path
        );
        assert!(view.transform_time_samples_in_interval(5.0, 1.0).is_empty());
        assert!(
            view.transform_time_samples_in_interval(f64::NAN, 1.0)
                .is_empty()
        );
    }
}
