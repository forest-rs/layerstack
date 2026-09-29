// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Observe authored changes without owning the host's editing workflow.
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec, PropertyType,
    StageOptions, Transaction, Value,
};

fn main() {
    let mut store = InMemoryStore::default();
    let path = store.property_path("/Rock.weight");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        path.prim_path(),
        PrimSpec::def().with_property(
            path.property(),
            PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)))
                .with_default(Value::Double(1.0)),
        ),
    );
    store.insert_layer(layer);
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let subscription = live.subscribe_changes(|notice| {
        println!("revision {}: {:?}", notice.revision, notice.changes);
    });
    let mut cursor = live.change_cursor();
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(path),
        Value::Double(2.0),
    );
    let applied = live.apply(&mut store, &edit).unwrap(); // Delivers the callback.
    for changes in live.changes_since(&mut cursor).unwrap() {
        println!("deferred observer: {changes:?}");
    }
    applied.inverse.apply(&mut store).unwrap(); // Author outside the live stage.
    live.synchronize(&mut store); // Discovers and notifies the undo.
    live.unsubscribe_changes(&subscription);
}
