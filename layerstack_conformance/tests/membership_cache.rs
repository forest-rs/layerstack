// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained collection and light-link captures through the consumer surface.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    EditTarget, FieldValue, LayerId, ListOp, PropertyPath, TargetPath, Transaction, Value,
};
use layerstack_schemas::{
    MembershipCache, MembershipCacheError, MembershipQuery, Scene, light::LightCaptureError,
    usd_lux::LightFilter,
};
fn property(
    store: &mut layerstack::InMemoryStore,
    owner: layerstack::PathId,
    name: &str,
) -> layerstack::Address {
    EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(owner, store.tokens.intern(name)))
}
fn assert_fresh(sample: layerstack_schemas::MembershipSample<'_>, scene: &Scene<'_>) {
    let fresh = MembershipQuery::compute(scene, sample.collection.owner, &sample.collection.name);
    assert_eq!(
        sample.query, &fresh,
        "retained compilation matches a fresh query"
    );
    assert_eq!(
        sample.membership,
        sample
            .targets
            .iter()
            .map(|p| fresh.is_included(scene, *p))
            .collect::<Vec<_>>(),
        "retained decisions match fresh membership in candidate order"
    );
}
#[test]
fn light_defaults_order_and_duplicates_retain_queries_across_parameter_edits() {
    let (mut store, mut live) = support::scene(
        r#"#usda 1.0
def SphereLight "Light" { float inputs:intensity = 1 }
def Scope "Geo" {}
def LightFilter "Filter" {}
"#,
    );
    let light = store.path("/Light");
    let geo = TargetPath::Prim(store.path("/Geo"));
    let filter = store.path("/Filter");
    let mut cache = MembershipCache::new();
    let first = cache
        .capture_light_links(&Scene::new(live.stage(), &store), light, &[geo, geo])
        .unwrap();
    assert!(
        first
            .illumination
            .membership
            .iter()
            .all(|m| m.is_included())
    );
    let revisions = (first.illumination.revisions, first.shadows.revisions);
    assert_fresh(first.illumination, &Scene::new(live.stage(), &store));
    let mut edit = Transaction::new();
    edit.set_default(
        property(&mut store, light, "inputs:intensity"),
        Value::Float(2.),
    );
    let applied = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    let next = cache
        .capture_light_links(&scene, light, &[geo, geo])
        .unwrap();
    assert!(!next.illumination.evaluated && !next.shadows.evaluated);
    assert_eq!(
        (next.illumination.revisions, next.shadows.revisions),
        revisions
    );
    let reordered = cache.capture_light_links(&scene, light, &[geo]).unwrap();
    assert_eq!(reordered.illumination.revisions.query, revisions.0.query);
    assert_ne!(
        reordered.illumination.revisions.decisions,
        revisions.0.decisions
    );
    let view = LightFilter::new(&scene, filter).unwrap();
    assert!(
        cache
            .capture_filter_links(&view, &[geo])
            .unwrap()
            .membership[0]
            .is_included()
    );
    assert_eq!(cache.stats().query_builds, 3);
    assert_eq!(cache.memory().collections, 3);
    assert_eq!(cache.memory().targets, 3);
    cache.reset_stats();
    assert_eq!(cache.stats().requests, 0);
}
#[test]
fn nested_external_collection_edits_update_only_illumination_and_preserve_stable_results() {
    let (mut store, mut live) = support::scene(
        r#"#usda 1.0
def SphereLight "Light" {
 bool collection:lightLink:includeRoot = false
 rel collection:lightLink:includes = </Sets.collection:red>
}
def Scope "Sets" (prepend apiSchemas = ["CollectionAPI:red", "CollectionAPI:other"]) {
 rel collection:red:includes = </A>
 rel collection:other:includes = </A>
}
def Scope "A" {}
def Scope "B" {}
"#,
    );
    let light = store.path("/Light");
    let sets = store.path("/Sets");
    let a = TargetPath::Prim(store.path("/A"));
    let b = TargetPath::Prim(store.path("/B"));
    let mut cache = MembershipCache::new();
    let first = cache
        .capture_light_links(&Scene::new(live.stage(), &store), light, &[a, b, a])
        .unwrap();
    let before = (first.illumination.revisions, first.shadows.revisions);
    assert_eq!(
        first
            .illumination
            .membership
            .iter()
            .map(|m| m.is_included())
            .collect::<Vec<_>>(),
        [true, false, true]
    );
    assert!(
        first
            .illumination
            .dependencies
            .collections
            .iter()
            .any(|c| c.owner == sets && c.name == "red")
    );
    for (name, expected_evaluated) in [("other", false), ("red", true)] {
        let mut edit = Transaction::new();
        edit.set_targets(
            property(&mut store, sets, &format!("collection:{name}:includes")),
            ListOp::explicit(vec![b]),
        );
        let applied = live.apply(&mut store, &edit).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &applied.changes);
        let next = cache
            .capture_light_links(&scene, light, &[a, b, a])
            .unwrap();
        assert_eq!(next.illumination.evaluated, expected_evaluated);
        assert!(!next.shadows.evaluated);
        assert_eq!(next.shadows.revisions, before.1);
        assert_fresh(next.illumination, &scene);
    }
    let updated = cache
        .capture_light_links(&Scene::new(live.stage(), &store), light, &[a, b, a])
        .unwrap()
        .illumination
        .revisions;
    assert_ne!(updated, before.0);
    // An authored redundant exclusion changes the query, but no candidate decision.
    let mut edit = Transaction::new();
    edit.set_targets(
        property(&mut store, sets, "collection:red:excludes"),
        ListOp::explicit(vec![a]),
    );
    let applied = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    let next = cache
        .capture_light_links(&scene, light, &[a, b, a])
        .unwrap();
    assert_ne!(next.illumination.revisions.query, updated.query);
    assert_eq!(next.illumination.revisions.decisions, updated.decisions);
}
#[test]
fn expression_predicates_refresh_after_geometry_metadata_without_changing_compiled_query() {
    let (mut store, mut live) = support::scene(
        r#"#usda 1.0
def Scope "Sets" (prepend apiSchemas = ["CollectionAPI:models"]) {
 pathExpression collection:models:membershipExpression = "//{kind:component}"
}
def Scope "Geo" (kind = "component") { custom float value = 1 }
"#,
    );
    let sets = store.path("/Sets");
    let geo = store.path("/Geo");
    let targets = [TargetPath::Prim(geo)];
    let mut cache = MembershipCache::new();
    let first = cache
        .capture(&Scene::new(live.stage(), &store), sets, "models", &targets)
        .unwrap();
    assert!(first.membership[0].is_included());
    assert!(first.dependencies.scene_objects);
    let revisions = first.revisions;
    let mut edit = Transaction::new();
    edit.set_default(property(&mut store, geo, "value"), Value::Float(2.));
    let applied = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    assert!(
        !cache
            .capture(&scene, sets, "models", &targets)
            .unwrap()
            .evaluated
    );
    let mut edit = Transaction::new();
    let kind = store.tokens.intern("kind");
    let group = store.tokens.intern("group");
    edit.set_metadata(
        EditTarget::for_layer(LayerId(1)).prim(geo),
        kind,
        FieldValue::Value(Value::Token(group)),
    );
    let applied = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    let next = cache.capture(&scene, sets, "models", &targets).unwrap();
    assert!(next.evaluated);
    assert!(!next.membership[0].is_included());
    assert_eq!(next.revisions.query, revisions.query);
    assert_ne!(next.revisions.decisions, revisions.decisions);
    assert_fresh(next, &scene);
}
#[test]
fn missing_expression_reference_repairs_and_clear_does_not_recycle_stamps() {
    let (mut store, mut live) = support::scene(
        r#"#usda 1.0
def Scope "Sets" (prepend apiSchemas = ["CollectionAPI:outer"]) {
 pathExpression collection:outer:membershipExpression = "%/Other:inner"
}
def Scope "Other" { pathExpression collection:inner:membershipExpression = "/Geo" }
def Scope "Geo" {}
"#,
    );
    let sets = store.path("/Sets");
    let other = store.path("/Other");
    let targets = [TargetPath::Prim(store.path("/Geo"))];
    let mut cache = MembershipCache::new();
    let first = cache
        .capture(&Scene::new(live.stage(), &store), sets, "outer", &targets)
        .unwrap();
    assert!(first.dependencies.all_collections);
    assert!(!first.query.problems().is_empty());
    assert!(!first.membership[0].is_included());
    let mut edit = Transaction::new();
    let api = store.tokens.intern("CollectionAPI:inner");
    edit.add_applied_schema(EditTarget::for_layer(LayerId(1)).prim(other), api);
    let applied = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    let repaired = cache.capture(&scene, sets, "outer", &targets).unwrap();
    assert!(repaired.query.problems().is_empty());
    assert!(repaired.membership[0].is_included());
    assert!(!repaired.dependencies.all_collections);
    assert_fresh(repaired, &scene);
    let revisions = repaired.revisions;
    cache.clear();
    assert_eq!(cache.memory().collections, 0);
    let next = cache.capture(&scene, sets, "outer", &targets).unwrap();
    assert!(
        next.revisions.query > revisions.query && next.revisions.decisions > revisions.decisions
    );
}
#[test]
fn deletion_recreation_and_invalid_requests_remain_explicit() {
    let (mut store, mut live) =
        support::scene("#usda 1.0\ndef SphereLight \"Light\" {}\ndef Scope \"Geo\" {}\n");
    let light = store.path("/Light");
    let geo = store.path("/Geo");
    let targets = [TargetPath::Prim(geo)];
    let mut cache = MembershipCache::new();
    let original = cache
        .capture_light_links(&Scene::new(live.stage(), &store), light, &targets)
        .unwrap()
        .illumination
        .revisions;
    let mut edit = Transaction::new();
    edit.remove_spec(EditTarget::for_layer(LayerId(1)).prim(light));
    let applied = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    assert_eq!(cache.memory().collections, 0);
    assert_eq!(
        cache
            .capture_light_links(&scene, light, &targets)
            .unwrap_err(),
        LightCaptureError::MissingPrim(light)
    );
    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undone.changes);
    let recreated = cache
        .capture_light_links(&scene, light, &targets)
        .unwrap()
        .illumination;
    assert!(recreated.revisions.query > original.query);
    assert!(recreated.revisions.decisions > original.decisions);
    assert_fresh(recreated, &scene);
    assert_eq!(
        cache
            .capture_light_links(&scene, geo, &targets)
            .unwrap_err(),
        LightCaptureError::NotLight(geo)
    );
    for name in ["", "a:", "bad name", "includes", "a:mode"] {
        assert_eq!(
            cache.capture(&scene, light, name, &targets).unwrap_err(),
            MembershipCacheError::InvalidName(name.into())
        );
    }
}

#[test]
fn expression_property_existence_tracks_implicit_creation_and_undo() {
    let (mut store, mut live) = support::scene(
        r#"#usda 1.0
def Scope "Sets" (prepend apiSchemas = ["CollectionAPI:props"]) {
 pathExpression collection:props:membershipExpression = "/Geo.link"
}
def Scope "Geo" {}
"#,
    );
    let sets = store.path("/Sets");
    let geo = store.path("/Geo");
    let token = store.tokens.intern("link");
    let targets = [TargetPath::Property(PropertyPath::new(geo, token))];
    let mut cache = MembershipCache::new();
    let first = cache
        .capture(&Scene::new(live.stage(), &store), sets, "props", &targets)
        .unwrap();
    assert!(!first.membership[0].is_included());
    let revisions = first.revisions;
    let mut edit = Transaction::new();
    edit.set_targets(
        property(&mut store, geo, "link"),
        ListOp::explicit(vec![TargetPath::Prim(sets)]),
    );
    let applied = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    let created = cache.capture(&scene, sets, "props", &targets).unwrap();
    assert!(created.membership[0].is_included());
    assert_fresh(created, &scene);
    assert_eq!(created.revisions.query, revisions.query);
    assert_ne!(created.revisions.decisions, revisions.decisions);
    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undone.changes);
    let restored = cache.capture(&scene, sets, "props", &targets).unwrap();
    assert!(!restored.membership[0].is_included());
    assert_fresh(restored, &scene);
}
