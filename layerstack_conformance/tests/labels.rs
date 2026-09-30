// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Snapshot-owned semantic labels compared with OpenUSD.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::{Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{LabelInterval, LabelQueryError, LabelsQuery, Scene, Time};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    queries: Vec<Query>,
    taxonomies: Vec<Taxonomies>,
}
#[derive(Deserialize)]
struct Query {
    path: String,
    taxonomy: String,
    mode: String,
    start: Option<f64>,
    end: Option<f64>,
    closed_start: bool,
    closed_end: bool,
    direct: Vec<String>,
    inherited: Vec<String>,
    has_direct: bool,
    has_inherited: bool,
}
#[derive(Deserialize)]
struct Taxonomies {
    path: String,
    direct: Vec<String>,
    inherited: Vec<String>,
}

#[test]
fn labels_match_openusd() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/labels/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/labels/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let schemas = Arc::new(layerstack_schemas::openusd(&mut loaded.store.tokens));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    for expected in oracle.taxonomies {
        let path = loaded.store.path(&expected.path);
        let scene = Scene::new(&stage, &loaded.store);
        assert_eq!(
            scene.direct_taxonomies(path),
            expected.direct,
            "direct taxonomy {}",
            expected.path
        );
        assert_eq!(
            scene.inherited_taxonomies(path),
            expected.inherited,
            "inherited taxonomy {}",
            expected.path
        );
    }
    let mut sampled_block_divergences = 0;
    for mut expected in oracle.queries {
        // AOUSD Core §12.3.6/§16.2.16.3: a sampled block yields the
        // fallback empty set, so labels from other times remain in the
        // interval union. OpenUSD26.8 discards that union after Get fails.
        // Classify only these exact `sampled-block-drops-fallback` cases.
        if expected.path == "/SampleBlocks"
            && expected.taxonomy == "kind"
            && expected.mode == "interval"
        {
            let retained = match (expected.start, expected.end) {
                (Some(0.0), Some(6.0)) => Some(vec!["after", "before"]),
                (Some(2.0), Some(4.0)) | (None, Some(4.0)) => Some(vec!["before"]),
                (Some(4.0), None) => Some(vec!["after"]),
                _ => None,
            };
            if let Some(labels) = retained {
                assert!(expected.direct.is_empty());
                assert!(expected.inherited.is_empty());
                expected.direct = labels.iter().map(|v| (*v).into()).collect();
                expected.inherited = expected.direct.clone();
                sampled_block_divergences += 1;
            }
        }
        let path = loaded.store.path(&expected.path);
        let scene = Scene::new(&stage, &loaded.store);
        let mut query = if expected.mode == "time" {
            LabelsQuery::new(
                scene,
                &expected.taxonomy,
                expected.start.map_or(Time::Default, Time::at),
            )
            .unwrap()
        } else {
            let interval = LabelInterval::new(
                expected.start.unwrap_or(f64::NEG_INFINITY),
                expected.end.unwrap_or(f64::INFINITY),
                expected.closed_start,
                expected.closed_end,
            )
            .unwrap();
            LabelsQuery::in_interval(scene, &expected.taxonomy, interval).unwrap()
        };
        assert_eq!(
            query.direct_labels(path),
            expected.direct,
            "direct {} {} {} {:?}",
            expected.path,
            expected.taxonomy,
            expected.mode,
            expected.start
        );
        assert_eq!(
            query.inherited_labels(path),
            expected.inherited,
            "inherited {} {} {} {:?}",
            expected.path,
            expected.taxonomy,
            expected.mode,
            expected.start
        );
        assert_eq!(query.has_direct_label(path, "object"), expected.has_direct);
        assert_eq!(
            query.has_inherited_label(path, "object"),
            expected.has_inherited
        );
        // Repeated reads use the same snapshot-owned direct cache.
        assert_eq!(query.direct_labels(path), expected.direct);
        assert!(matches!(
            LabelsQuery::new(scene, "", Time::Default),
            Err(LabelQueryError::EmptyTaxonomy)
        ));
    }
    assert_eq!(sampled_block_divergences, 4);
}

#[test]
fn label_queries_belong_to_one_snapshot() {
    use layerstack::{LiveStage, edit::EditTarget};
    use layerstack_schemas::{SchemaEdit, usd_semantics::SemanticsLabelsApi};
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/labels/scene.usda"),
    );
    let schemas = Arc::new(layerstack_schemas::openusd(&mut loaded.store.tokens));
    let mut live = LiveStage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let root = loaded.store.path("/Root");
    let child = loaded.store.path("/Root/Child/Unapplied");
    let absent = loaded.store.path("/Root/Absent");
    {
        let mut query = LabelsQuery::new(
            Scene::new(live.stage(), &loaded.store),
            "kind",
            Time::Default,
        )
        .unwrap();
        assert_eq!(query.inherited_labels(child), ["object", "scene"]);
        assert!(query.direct_labels(absent).is_empty());
        assert!(query.inherited_labels(absent).is_empty());
        assert!(!query.has_inherited_label(absent, "scene"));
        assert!(
            Scene::new(live.stage(), &loaded.store)
                .inherited_taxonomies(absent)
                .is_empty()
        );
    }
    let handle = SemanticsLabelsApi::get(&Scene::new(live.stage(), &loaded.store), root, "kind")
        .unwrap()
        .edit();
    let mut edit = SchemaEdit::new(
        live.stage(),
        &mut loaded.store,
        EditTarget::for_layer(loaded.root_layer),
    );
    handle.set_labels(&mut edit, &["changed"]);
    let transaction = edit.finish();
    let inverse = live.apply(&mut loaded.store, &transaction).unwrap().inverse;
    {
        let mut query = LabelsQuery::new(
            Scene::new(live.stage(), &loaded.store),
            "kind",
            Time::Default,
        )
        .unwrap();
        assert_eq!(query.inherited_labels(child), ["changed"]);
    }
    live.apply(&mut loaded.store, &inverse).unwrap();
    let mut query = LabelsQuery::new(
        Scene::new(live.stage(), &loaded.store),
        "kind",
        Time::Default,
    )
    .unwrap();
    assert_eq!(query.inherited_labels(child), ["object", "scene"]);
}
