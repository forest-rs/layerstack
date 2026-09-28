// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored-extent bounds compared with OpenUSD, including oriented matrices.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{
    Scene, Time,
    bounds::{BoundingBox, BoundsCache, BoundsOptions},
    usd_geom::ImageablePurpose,
};
use serde::Deserialize;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    records: Vec<Record>,
}
#[derive(Deserialize)]
struct Record {
    time: Option<f64>,
    path: String,
    hints: bool,
    ignore: bool,
    purposes: Vec<String>,
    bounds: BTreeMap<String, BoxRecord>,
}
#[derive(Deserialize)]
struct BoxRecord {
    min: [f64; 3],
    max: [f64; 3],
    matrix: [[f64; 4]; 4],
}
fn compare(actual: BoundingBox, expected: &BoxRecord, context: &str) {
    assert_eq!(
        actual.range.is_empty(),
        (0..3).any(|i| expected.min[i] > expected.max[i]),
        "{context}"
    );
    if actual.range.is_empty() {
        return;
    }
    for (a, b) in actual
        .range
        .min
        .into_iter()
        .chain(actual.range.max)
        .chain(actual.matrix.into_iter().flatten())
        .zip(
            expected
                .min
                .into_iter()
                .chain(expected.max)
                .chain(expected.matrix.into_iter().flatten()),
        )
    {
        assert!(
            (a - b).abs() <= 1e-9 * b.abs().max(1.0),
            "{context}: {a} != {b}; actual {actual:?}"
        );
    }
}
#[test]
fn authored_bounds_match_cpp() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/bounds/oracle.json")).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/bounds/scene.usda"),
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
    let mut previous = None;
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    for (record, path) in oracle.records.iter().zip(paths) {
        let options = BoundsOptions {
            included_purposes: record
                .purposes
                .iter()
                .map(|p| ImageablePurpose::from_token(p))
                .collect(),
            use_extents_hint: record.hints,
            ignore_visibility: record.ignore,
        };
        if previous.as_ref() != Some(&options) {
            cache = BoundsCache::new(Time::Default, options.clone());
            previous = Some(options);
        }
        cache.set_time(record.time.map_or(Time::Default, Time::at));
        for (name, expected) in &record.bounds {
            let actual = match name.as_str() {
                "world" => cache.world_bound(&scene, path),
                "local" => cache.local_bound(&scene, path),
                _ => cache.untransformed_bound(&scene, path),
            }
            .unwrap();
            compare(
                actual,
                expected,
                &format!("{} {name} {:?}", record.path, previous),
            );
        }
    }
}

#[test]
fn edits_invalidate_ancestors_and_descendants_but_keep_other_branches() {
    use layerstack::{
        LiveStage, Value,
        edit::{EditTarget, Transaction},
    };
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/bounds/scene.usda"),
    );
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options);
    let root = loaded.store.path("/World");
    let leaf = loaded.store.path("/World/Model/Tilt/A");
    let other = loaded.store.path("/World/Unmodelled");
    let changed_parent = loaded.store.path("/World/Model/Tilt");
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    let scene = Scene::new(live.stage(), &loaded.store);
    cache.world_bound(&scene, root).unwrap();
    let original = cache.world_bound(&scene, leaf).unwrap();
    let other_bound = cache.world_bound(&scene, other).unwrap();
    let transform = loaded.store.tokens.intern("xformOp:translate");
    let mut transaction = Transaction::new();
    transaction.set_default(
        EditTarget::for_layer(loaded.root_layer)
            .property(layerstack::PropertyPath::new(changed_parent, transform)),
        Value::Vec3d([100.0, 200.0, 300.0]),
    );
    let applied = live.apply(&mut loaded.store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &loaded.store);
    cache.apply_changes(&scene, &applied.changes);
    let computed = cache.stats().computed;
    assert_eq!(cache.world_bound(&scene, other).unwrap(), other_bound);
    assert_eq!(cache.stats().computed, computed, "unrelated branch reused");
    assert_ne!(cache.world_bound(&scene, leaf).unwrap(), original);
    let actual = cache.world_bound(&scene, root).unwrap();
    let mut clean = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(actual, clean.world_bound(&scene, root).unwrap());
    let undone = live.apply(&mut loaded.store, &applied.inverse).unwrap();
    let scene = Scene::new(live.stage(), &loaded.store);
    cache.apply_changes(&scene, &undone.changes);
    assert_eq!(cache.world_bound(&scene, leaf).unwrap(), original);
    let mut removal = Transaction::new();
    removal.remove_spec(EditTarget::for_layer(loaded.root_layer).prim(changed_parent));
    let removed = live.apply(&mut loaded.store, &removal).unwrap();
    let scene = Scene::new(live.stage(), &loaded.store);
    cache.apply_changes(&scene, &removed.changes);
    assert!(cache.world_bound(&scene, leaf).is_err());
    let mut clean = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(
        cache.world_bound(&scene, root).unwrap(),
        clean.world_bound(&scene, root).unwrap()
    );
    let restored = live.apply(&mut loaded.store, &removed.inverse).unwrap();
    let scene = Scene::new(live.stage(), &loaded.store);
    cache.apply_changes(&scene, &restored.changes);
    assert_eq!(cache.world_bound(&scene, leaf).unwrap(), original);
}

#[test]
fn unsupported_geometry_reports_an_error_instead_of_an_incomplete_bound() {
    use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, Value};
    use layerstack_schemas::bounds::BoundsError;
    let mut store = InMemoryStore::default();
    let root = store.path("/Root");
    let mesh = store.path("/Root/Mesh");
    let instancer = store.path("/Instances");
    let xform_type = store.tokens.intern("Xform");
    let mesh_type = store.tokens.intern("Mesh");
    let instancer_type = store.tokens.intern("PointInstancer");
    let extent = store.tokens.intern("extent");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(root, PrimSpec::def().with_type_name(xform_type));
    layer.insert_prim(
        mesh,
        PrimSpec::def().with_type_name(mesh_type).with_property(
            extent,
            PropertySpec::attribute().with_default(Value::Array(vec![Value::Vec3f([1.0; 3])])),
        ),
    );
    layer.insert_prim(instancer, PrimSpec::def().with_type_name(instancer_type));
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut store, LayerId(1), options);
    let scene = Scene::new(&stage, &store);
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(
        cache.world_bound(&scene, root),
        Err(BoundsError::ExtentUnavailable(mesh))
    );
    assert_eq!(
        cache.world_bound(&scene, instancer),
        Err(BoundsError::PointInstancerUnsupported(instancer))
    );
}

#[test]
fn invalidation_reaches_independently_queried_descendants() {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/bounds/scene.usda"),
    );
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let ancestor = loaded.store.path("/World/Model");
    let leaf = loaded.store.path("/World/Model/Tilt/A");
    let other = loaded.store.path("/World/Unmodelled/E");
    let scene = Scene::new(&stage, &loaded.store);
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    cache.world_bound(&scene, leaf).unwrap();
    cache.world_bound(&scene, other).unwrap();
    assert_eq!(cache.len(), 2);
    cache.invalidate(&scene, ancestor);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.stats().invalidated, 1);
    cache.world_bound(&scene, other).unwrap();
    assert_eq!(cache.stats().computed, 2);
    cache.world_bound(&scene, leaf).unwrap();
    assert_eq!(cache.stats().computed, 3);
    cache.set_time(Time::at(1.0));
    assert!(cache.is_empty());
    assert_eq!(cache.stats().computed, 0);
}

#[test]
fn excluded_children_track_visibility_type_and_deletion_changes() {
    use layerstack::{InMemoryStore, Layer, LayerId, LayerStore, PrimSpec, PropertySpec, Value};
    let mut store = InMemoryStore::default();
    let root = store.path("/Root");
    let branch = store.path("/Root/Branch");
    let leaf = store.path("/Root/Branch/Leaf");
    let xform = store.tokens.intern("Xform");
    let mesh = store.tokens.intern("Mesh");
    let material = store.tokens.intern("Material");
    let visibility = store.tokens.intern("visibility");
    let invisible = store.tokens.intern("invisible");
    let inherited = store.tokens.intern("inherited");
    let extent = store.tokens.intern("extent");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(root, PrimSpec::def().with_type_name(xform));
    layer.insert_prim(
        branch,
        PrimSpec::def().with_type_name(xform).with_property(
            visibility,
            PropertySpec::attribute().with_default(Value::Token(invisible)),
        ),
    );
    layer.insert_prim(
        leaf,
        PrimSpec::def().with_type_name(mesh).with_property(
            extent,
            PropertySpec::attribute().with_default(Value::Array(vec![
                Value::Vec3f([10.0; 3]),
                Value::Vec3f([20.0; 3]),
            ])),
        ),
    );
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    let check =
        |store: &mut InMemoryStore, cache: &mut BoundsCache, empty: bool, invalidate: bool| {
            let stage = Stage::compose(store, LayerId(1), options.clone());
            let scene = Scene::new(&stage, store);
            if invalidate {
                cache.invalidate(&scene, branch);
            }
            let actual = cache.world_bound(&scene, root).unwrap();
            let mut clean = BoundsCache::new(Time::Default, BoundsOptions::default());
            assert_eq!(actual, clean.world_bound(&scene, root).unwrap());
            assert_eq!(actual.range.is_empty(), empty);
            if !empty {
                assert_eq!(actual.range.min, [10.0; 3]);
            }
        };
    check(&mut store, &mut cache, true, false);
    // An excluded branch was never evaluated for bounds, but its inclusion
    // decision must still be evicted when its visibility changes.
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prims
        .get_mut(&branch)
        .unwrap()
        .property_mut(visibility)
        .unwrap()
        .default = Some(Value::Token(inherited));
    let stage = Stage::compose(&mut store, LayerId(1), options.clone());
    cache.invalidate(&Scene::new(&stage, &store), root);
    check(&mut store, &mut cache, false, false);
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prims
        .get_mut(&branch)
        .unwrap()
        .type_name = Some(material);
    check(&mut store, &mut cache, true, true);
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prims
        .get_mut(&branch)
        .unwrap()
        .type_name = Some(xform);
    check(&mut store, &mut cache, false, true);
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prims
        .get_mut(&branch)
        .unwrap()
        .property_mut(visibility)
        .unwrap()
        .default = Some(Value::Token(invisible));
    check(&mut store, &mut cache, true, true);
    let removed = store
        .layer_mut(LayerId(1))
        .unwrap()
        .prims
        .remove(&branch)
        .unwrap();
    check(&mut store, &mut cache, true, true);
    let mut restored = removed;
    restored.property_mut(visibility).unwrap().default = Some(Value::Token(inherited));
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .insert_prim(branch, restored);
    check(&mut store, &mut cache, false, true);
    // Invalidate above a formerly excluded child: dependency indexing must
    // include skipped nodes, not only successfully computed bounds.
    let stage = Stage::compose(&mut store, LayerId(1), options.clone());
    let scene = Scene::new(&stage, &store);
    cache.invalidate(&scene, root);
    assert!(cache.is_empty());
    assert!(!cache.world_bound(&scene, root).unwrap().range.is_empty());
    for specifier in [
        layerstack::Specifier::Class,
        layerstack::Specifier::Over,
        layerstack::Specifier::Def,
    ] {
        store
            .layer_mut(LayerId(1))
            .unwrap()
            .prims
            .get_mut(&root)
            .unwrap()
            .specifier = Some(specifier);
        let stage = Stage::compose(&mut store, LayerId(1), options.clone());
        let scene = Scene::new(&stage, &store);
        cache.invalidate(&scene, root);
        assert_eq!(
            cache.world_bound(&scene, root).unwrap().range.is_empty(),
            specifier != layerstack::Specifier::Def
        );
        // An explicitly queried boundable still owns its authored extent.
        assert!(!cache.world_bound(&scene, leaf).unwrap().range.is_empty());
    }
}
