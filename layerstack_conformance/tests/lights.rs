// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Light discovery, cache lifetime and transactional cache authoring.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{LayerId, TargetPath, edit::EditTarget};
use layerstack_schemas::{
    Scene, SchemaEdit,
    light::{LightListMode as Mode, LightListQuery},
    usd_lux::{LightListApiEdit, LightListApiLightListCacheBehavior as Behavior},
};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
    stored: Vec<String>,
    behavior: String,
    invalidated: String,
    retained: Vec<String>,
}
#[derive(Deserialize)]
struct Row {
    root: String,
    mode: String,
    targets: Vec<String>,
}
fn oracle() -> Oracle {
    serde_json::from_str(include_str!("../fixtures/lights/oracle.json")).unwrap()
}
#[test]
fn discovery_matches_cpp_including_forwarding_halt_filters_and_instances() {
    let oracle = oracle();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, live) = support::scene(include_str!("../fixtures/lights/scene.usda"));
    for row in oracle.rows {
        let root = store.path(&row.root);
        let scene = Scene::new(live.stage(), &store);
        let mode = if row.mode == "ignore" {
            Mode::IgnoreCache
        } else {
            Mode::ConsultModelHierarchyCache
        };
        let targets = scene.compute_light_list(root, mode).unwrap();
        assert_eq!(
            targets
                .iter()
                .map(|p| p.display(&store.paths, &store.tokens))
                .collect::<Vec<_>>(),
            row.targets,
            "{} {}",
            row.root,
            row.mode
        );
    }
}
#[test]
fn runtime_cache_has_explicit_snapshot_lifetime_and_work_counters() {
    let (mut store, live) = support::scene(include_str!("../fixtures/lights/scene.usda"));
    let root = store.path("/World");
    let missing = store.path("/Absent");
    let mut query = LightListQuery::new(Scene::new(live.stage(), &store));
    let first = query.compute(root, Mode::IgnoreCache).unwrap().to_vec();
    let visited = query.stats().visited_prims;
    assert_eq!(query.compute(root, Mode::IgnoreCache).unwrap(), first);
    assert_eq!(query.stats().visited_prims, visited);
    assert_eq!(query.stats().cache_hits, 1);
    query
        .compute(root, Mode::ConsultModelHierarchyCache)
        .unwrap();
    assert_eq!(query.stats().entries, 2);
    assert!(query.stats().authored_cache_reads > 0);
    assert!(query.compute(missing, Mode::IgnoreCache).is_err());
    assert_eq!(query.stats().entries, 2);
    query.clear();
    assert_eq!(
        query.stats(),
        layerstack_schemas::light::LightListStats::default()
    );
}
#[test]
fn storing_invalidating_and_undoing_cache_matches_cpp() {
    let oracle = oracle();
    let (mut store, mut live) = support::scene(include_str!("../fixtures/lights/scene.usda"));
    let root = store.path("/World/Ignored");
    let child = TargetPath::Prim(store.path("/World/Ignored/Child"));
    let outside = TargetPath::Prim(store.path("/Outside"));
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let api = LightListApiEdit::new(&edit, root).unwrap();
    api.store_light_list(&mut edit, &[outside, child, child])
        .unwrap();
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let api = layerstack_schemas::usd_lux::LightListApi::get(&scene, root).unwrap();
    assert_eq!(
        api.light_list()
            .iter()
            .map(|p| p.display(&store.paths, &store.tokens))
            .collect::<Vec<_>>(),
        oracle.stored
    );
    assert_eq!(
        api.light_list_cache_behavior().unwrap().as_str(),
        oracle.behavior
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = LightListApiEdit::new(&edit, root).unwrap();
    handle.invalidate_light_list(&mut edit).unwrap();
    let transaction = edit.finish();
    let invalidation = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let api = layerstack_schemas::usd_lux::LightListApi::get(&scene, root).unwrap();
    assert_eq!(api.light_list_cache_behavior(), Some(Behavior::Ignore));
    assert_eq!(
        api.light_list_cache_behavior().unwrap().as_str(),
        oracle.invalidated
    );
    assert_eq!(
        api.light_list()
            .iter()
            .map(|p| p.display(&store.paths, &store.tokens))
            .collect::<Vec<_>>(),
        oracle.retained
    );
    live.apply(&mut store, &invalidation.inverse).unwrap();
    live.apply(&mut store, &applied.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        layerstack_schemas::usd_lux::LightListApi::get(&scene, root)
            .unwrap()
            .light_list()[0]
            .display(&store.paths, &store.tokens),
        "/IgnoredGhost"
    );
}

#[test]
fn cache_edits_declare_unapplied_properties_and_reject_type_conflicts() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef Xform \"Root\" {}\ndef Xform \"Bad\" {\n float lightList:cacheBehavior = 1\n}\n",
    );
    let root = store.path("/Root");
    let bad = store.path("/Bad");
    let target = TargetPath::Prim(root);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let api = LightListApiEdit::new(&edit, bad).unwrap();
    assert_eq!(
        api.store_light_list(&mut edit, &[target]),
        Err(layerstack_schemas::light::LightListError::WrongPropertyType)
    );
    let api = LightListApiEdit::new(&edit, root).unwrap();
    api.store_light_list(&mut edit, &[target]).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    assert!(!scene.has_api(root, "LightListAPI", None));
    assert_eq!(
        scene
            .compute_light_list(root, Mode::ConsultModelHierarchyCache)
            .unwrap(),
        [target]
    );
    assert!(
        scene
            .compute_light_list(root, Mode::IgnoreCache)
            .unwrap()
            .is_empty()
    );
    let names: Vec<_> = scene
        .stage()
        .property_names(bad, &store)
        .into_iter()
        .map(|n| store.tokens.resolve(n))
        .collect();
    assert!(names.contains(&"lightList:cacheBehavior"));
    assert!(!names.contains(&"lightList"));
}
