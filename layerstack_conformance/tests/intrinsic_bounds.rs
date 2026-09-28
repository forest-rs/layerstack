// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Geometry-driven extents compared with OpenUSD's boundable API.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{
    Scene, Time,
    bounds::{BoundsCache, BoundsError, BoundsOptions},
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    records: Vec<Record>,
}
#[derive(Deserialize)]
struct Record {
    path: String,
    time: Option<f64>,
    bounds: Option<Extent>,
    divergence: Option<String>,
}
#[derive(Deserialize)]
struct Extent {
    empty: bool,
    min: Option<[f64; 3]>,
    max: Option<[f64; 3]>,
}

#[test]
fn intrinsic_extents_match_cpp_across_time_changes() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/intrinsic_bounds/oracle.json")).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/intrinsic_bounds/scene.usda"),
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
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    for (record, path) in oracle.records.iter().zip(paths) {
        let time = record.time.map_or(Time::Default, Time::at);
        cache.set_time(time);
        let actual = cache.untransformed_bound(&scene, path);
        let mut fresh = BoundsCache::new(time, BoundsOptions::default());
        assert_eq!(
            actual,
            fresh.untransformed_bound(&scene, path),
            "{} {:?}",
            record.path,
            record.time
        );
        if let Some(divergence) = &record.divergence {
            match divergence.as_str() {
                "default-time-block-hides-fallback" => {
                    assert_eq!(record.path, "/BlockedRadius");
                    assert!(record.time.is_none() && record.bounds.is_none());
                }
                "sampled-block-drops-fallback" => {
                    assert_eq!(record.path, "/CubeChangingExtent");
                    assert!(matches!(record.time, Some(0.0 | 1.0)));
                    assert_eq!(record.bounds.as_ref().unwrap().max, Some([6.0; 3]));
                }
                _ => panic!("unknown divergence {divergence}"),
            }
            // AOUSD Core §12.3.6 resolves the schema fallback in these cases.
            // Preserve both documented differences and the exact C++ result.
            let range = actual.unwrap().range;
            assert_eq!(range.min, [-1.0; 3]);
            assert_eq!(range.max, [1.0; 3]);
        } else if let Some(expected) = &record.bounds {
            let actual = actual.unwrap().range;
            assert_eq!(
                actual.is_empty(),
                expected.empty,
                "{} {:?}",
                record.path,
                record.time
            );
            if !expected.empty {
                assert_eq!(
                    actual.min,
                    expected.min.unwrap(),
                    "{} {:?}",
                    record.path,
                    record.time
                );
                assert_eq!(
                    actual.max,
                    expected.max.unwrap(),
                    "{} {:?}",
                    record.path,
                    record.time
                );
            }
        } else {
            assert_eq!(
                actual,
                Err(BoundsError::ExtentUnavailable(path)),
                "{} {:?}",
                record.path,
                record.time
            );
        }
    }
}

#[test]
fn live_geometry_edits_switch_between_intrinsic_and_authored_extents() {
    use layerstack::{
        InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertyPath, Value,
        edit::{EditTarget, Transaction},
    };
    let mut store = InMemoryStore::default();
    let root = store.path("/Root");
    let cube = store.path("/Root/Cube");
    let cube_type = store.tokens.intern("Cube");
    let xform_type = store.tokens.intern("Xform");
    let size = store.tokens.intern("size");
    let extent = store.tokens.intern("extent");
    let layer_id = LayerId(1);
    let mut layer = Layer::new(layer_id);
    layer.insert_prim(root, PrimSpec::def().with_type_name(xform_type));
    layer.insert_prim(cube, PrimSpec::def().with_type_name(cube_type));
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, layer_id, options);
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(
        cache
            .world_bound(&Scene::new(live.stage(), &store), root)
            .unwrap()
            .range
            .max,
        [1.0; 3]
    );
    let target = EditTarget::for_layer(layer_id);
    let authored = Value::Array(vec![Value::Vec3f([-3.0; 3]), Value::Vec3f([3.0; 3])]);
    for (field, value, maximum) in [
        (size, Value::Double(8.0), 4.0),
        (extent, authored, 3.0),
        (size, Value::Double(12.0), 3.0),
        (extent, Value::Blocked, 6.0),
    ] {
        let mut edit = Transaction::new();
        edit.set_default(target.property(PropertyPath::new(cube, field)), value);
        let applied = live.apply(&mut store, &edit).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &applied.changes);
        let actual = cache.world_bound(&scene, root).unwrap();
        let mut fresh = BoundsCache::new(Time::Default, BoundsOptions::default());
        assert_eq!(actual, fresh.world_bound(&scene, root).unwrap());
        assert_eq!(actual.range.max, [maximum; 3]);
    }
}
