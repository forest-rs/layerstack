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
