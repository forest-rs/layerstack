// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Intrinsic light bounds compared with C++, including time and invalidation.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{EditTarget, InterpolationType, LayerId, PropertyPath, Transaction, Value};
use layerstack_schemas::{
    Scene, Time,
    bounds::{BoundsCache, BoundsError, BoundsOptions},
};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    schema: String,
    time: Option<f64>,
    extent: Option<[[f64; 3]; 2]>,
    blocked_dimension: Option<String>,
}
#[test]
fn light_extents_match_cpp_defaults_and_interpolated_float_parameters() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/lux_extents.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    for row in oracle.rows {
        let text = format!(
            "#usda 1.0\ndef {} \"Light\" {{\n float inputs:radius.timeSamples = {{1: 1.234567, 3: 3.456789}}\n float inputs:length.timeSamples = {{1: 1.234567, 3: 3.456789}}\n float inputs:width.timeSamples = {{1: 1.234567, 3: 3.456789}}\n float inputs:height.timeSamples = {{1: 1.234567, 3: 3.456789}}\n}}",
            row.schema
        );
        let text = if let Some(dimension) = &row.blocked_dimension {
            format!(
                "#usda 1.0\ndef {} \"Light\" {{\n float inputs:{dimension} = None\n}}\n",
                row.schema
            )
        } else {
            text
        };
        let (mut store, live) = support::scene(&text);
        let path = store.path("/Light");
        let time = row.time.map_or(Time::Default, |code| Time::At {
            code,
            interpolation: InterpolationType::Linear,
        });
        let mut cache = BoundsCache::new(time, BoundsOptions::default());
        let actual = cache.world_bound(&Scene::new(live.stage(), &store), path);
        if let Some(expected) = row.extent {
            let actual = actual.unwrap();
            assert_eq!(
                [actual.range.min, actual.range.max],
                expected,
                "{} {:?}",
                row.schema,
                time
            );
        } else {
            assert_eq!(
                actual,
                Err(BoundsError::ExtentUnavailable(path)),
                "{} {:?}",
                row.schema,
                time
            );
        }
    }
}
#[test]
fn authored_extents_win_and_dimension_edits_invalidate_retained_bounds() {
    let (mut store, mut live) =
        support::scene("#usda 1.0\ndef SphereLight \"Light\" {\n float inputs:radius = 2\n}\n");
    let path = store.path("/Light");
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(
        cache
            .world_bound(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .range
            .max,
        [2.; 3]
    );
    let radius = store.tokens.intern("inputs:radius");
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(path, radius)),
        Value::Float(4.),
    );
    let applied = live.apply(&mut store, &edit).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    assert_eq!(
        cache
            .world_bound(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .range
            .max,
        [4.; 3]
    );
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n float3[] extent = [(-7,-8,-9), (7,8,9)]\n bool inputs:treatAsPoint = true\n}\n",
    );
    let path = store.path("/Light");
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(
        cache
            .world_bound(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .range
            .max,
        [7., 8., 9.]
    );
}

#[test]
fn blocked_light_dimensions_fail_at_default_and_retire_numeric_cached_bounds() {
    for (schema, dimension) in [
        ("SphereLight", "radius"),
        ("DiskLight", "radius"),
        ("CylinderLight", "radius"),
        ("CylinderLight", "length"),
        ("RectLight", "width"),
        ("RectLight", "height"),
        ("PortalLight", "width"),
        ("PortalLight", "height"),
    ] {
        let (mut store, live) = support::scene(&format!(
            "#usda 1.0\ndef {schema} \"Light\" {{\n float inputs:{dimension} = None\n}}\n"
        ));
        let path = store.path("/Light");
        let scene = Scene::new(live.stage(), &store);
        let mut cache = BoundsCache::new(Time::at(1.), BoundsOptions::default());
        assert!(
            cache.world_bound(&scene, path).is_ok(),
            "{schema} {dimension}"
        );
        cache.set_time(Time::Default);
        assert_eq!(
            cache.world_bound(&scene, path),
            Err(BoundsError::ExtentUnavailable(path)),
            "{schema} {dimension}"
        );
        let mut fresh = BoundsCache::new(Time::Default, BoundsOptions::default());
        assert_eq!(
            fresh.world_bound(&scene, path),
            Err(BoundsError::ExtentUnavailable(path))
        );
        cache.set_time(Time::at(2.));
        assert!(cache.world_bound(&scene, path).is_ok());
    }
}
