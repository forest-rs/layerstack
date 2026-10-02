// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Intrinsic widths and retained invalidation match the built-in C++ providers.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{LayerId, Time, edit::EditTarget};
use layerstack_schemas::{
    Scene, SchemaEdit,
    bounds::{BoundsCache, BoundsError, BoundsOptions},
    usd_geom::Points,
};

#[test]
fn point_and_curve_extents_match_cpp_at_default_and_sampled_times() {
    let (mut store, live) = support::scene(include_str!("../fixtures/point_curve_bounds.usda"));
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/point_curve_bounds.json")).unwrap();
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    for row in expected.as_array().unwrap() {
        let path = store.path(row["path"].as_str().unwrap());
        cache.set_time(row["time"].as_f64().map_or(Time::Default, Time::at));
        let result = cache.world_bound(&Scene::new(live.stage(), &store), path);
        if row["extent"].is_null() {
            assert_eq!(result, Err(BoundsError::ExtentUnavailable(path)));
        } else {
            let range = result.unwrap().aligned_range();
            assert_eq!(
                serde_json::json!([range.min, range.max]),
                row["extent"],
                "{row}"
            );
        }
    }
}

#[test]
fn width_edits_invalidate_cached_bounds_and_undo_restores_them() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/point_curve_bounds.usda"));
    let path = store.path("/Points");
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    let original = cache
        .world_bound(&Scene::new(live.stage(), &store), path)
        .unwrap();
    let points = Points::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    points.set_widths(&mut edit, &[8., 12.]);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    let changed = cache.world_bound(&scene, path).unwrap();
    assert_ne!(changed, original);
    assert_eq!(
        changed,
        BoundsCache::new(Time::Default, BoundsOptions::default())
            .world_bound(&scene, path)
            .unwrap()
    );
    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undone.changes);
    assert_eq!(cache.world_bound(&scene, path).unwrap(), original);
}
