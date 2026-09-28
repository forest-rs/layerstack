// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Precise reports map source edits and remain conservative for unknown edits.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertyField, PropertyPath,
    PropertySpec, Reference, StageOptions, Transaction, Value,
};

#[test]
fn referenced_values_and_undo_report_composed_properties() {
    let mut store = InMemoryStore::default();
    let source = store.path("/Source");
    let a = store.path("/A");
    let b = store.path("/B");
    let value = store.tokens.intern("weight");
    let mut asset = Layer::new(LayerId(2));
    asset.insert_prim(
        source,
        PrimSpec::def().with_property(
            value,
            PropertySpec::typed_attribute(layerstack::PropertyType::new(
                "double",
                false,
                Value::Double(0.0),
            ))
            .with_default(Value::Double(1.0)),
        ),
    );
    store.insert_layer(asset);
    let mut root = Layer::new(LayerId(1));
    for path in [a, b] {
        root.insert_prim(
            path,
            PrimSpec::def().with_reference(Reference::new(LayerId(2), source)),
        );
    }
    store.insert_layer(root);
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let mut txn = Transaction::new();
    txn.set_default(
        EditTarget::for_layer(LayerId(2)).property(PropertyPath::new(source, value)),
        Value::Double(2.0),
    );
    let applied = live.apply(&mut store, &txn).unwrap();
    for path in [a, b] {
        let fields = applied
            .changes
            .properties_for(path)
            .expect("complete field inventory");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, value);
        assert_eq!(fields[0].field, PropertyField::Default);
    }
    let undo = live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(
        undo.changes.property_changes,
        applied.changes.property_changes
    );
    store.layers.get_mut(&LayerId(2)).unwrap().set_property(
        PropertyPath::new(source, value),
        PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "double",
            false,
            Value::Double(0.0),
        ))
        .with_default(Value::Double(3.0)),
    );
    live.notify_changed_layers(&store);
    let external = live.recompose_changes(&mut store);
    assert!(
        external.property_changes.is_empty(),
        "unknown edits have no complete field inventory"
    );
    assert_eq!(external.resynced, vec![a, b]);
    assert!(
        live.recompose_changes(&mut store).resynced.is_empty(),
        "reports are drained once"
    );
}

#[test]
fn precise_unrelated_edits_preserve_geometry_caches() {
    use layerstack_schemas::{
        Scene, Time, XformCache,
        bounds::{BoundsCache, BoundsOptions},
    };
    use std::sync::Arc;
    let mut store = InMemoryStore::default();
    let root = store.path("/World");
    let child = store.path("/World/Shape");
    let cube = store.tokens.intern("Cube");
    let size = store.tokens.intern("size");
    let color = store.tokens.intern("inputs:roughness");
    let attr = |v| {
        PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "double",
            false,
            Value::Double(0.0),
        ))
        .with_default(Value::Double(v))
    };
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(root, PrimSpec::def().with_property(color, attr(0.5)));
    layer.insert_prim(
        child,
        PrimSpec::def()
            .with_type_name(cube)
            .with_property(size, attr(2.0)),
    );
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let mut xforms = XformCache::new(Time::Default);
    let mut bounds = BoundsCache::new(Time::Default, BoundsOptions::default());
    let scene = Scene::new(live.stage(), &store);
    xforms.local_to_world(&scene, child);
    let before = bounds.world_bound(&scene, root).unwrap();
    let xcomputed = xforms.stats().local_computed;
    let bcomputed = bounds.stats().computed;
    let mut txn = Transaction::new();
    txn.set_default(
        EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(root, color)),
        Value::Double(0.7),
    );
    let applied = live.apply(&mut store, &txn).unwrap();
    let scene = Scene::new(live.stage(), &store);
    xforms.apply_changes(&scene, &applied.changes);
    bounds.apply_changes(&scene, &applied.changes);
    xforms.local_to_world(&scene, child);
    assert_eq!(bounds.world_bound(&scene, root).unwrap(), before);
    assert_eq!(
        xforms.stats().local_computed,
        xcomputed,
        "unrelated fields retain transforms"
    );
    assert_eq!(
        bounds.stats().computed,
        bcomputed,
        "unrelated fields retain bounds"
    );
    let mut txn = Transaction::new();
    txn.set_default(
        EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(child, size)),
        Value::Double(4.0),
    );
    let applied = live.apply(&mut store, &txn).unwrap();
    let scene = Scene::new(live.stage(), &store);
    bounds.apply_changes(&scene, &applied.changes);
    let after = bounds.world_bound(&scene, root).unwrap();
    assert_ne!(before, after, "geometry edits still update bounds");
    assert_eq!(
        after,
        BoundsCache::new(Time::Default, BoundsOptions::default())
            .world_bound(&scene, root)
            .unwrap()
    );
}
