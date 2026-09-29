// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage notification works independently of schemas and computed queries.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    ChangeHistoryError, EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec,
    PropertyPath, PropertySpec, PropertyType, ResolvedValue, StageOptions, Transaction, Value,
};
use std::sync::{Arc, Mutex};

fn fixture() -> (InMemoryStore, LiveStage, PropertyPath) {
    let mut store = InMemoryStore::default();
    let path = store.property_path("/Rock.weight");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        path.prim_path(),
        PrimSpec::def().with_property(path.property(), spec(1.0)),
    );
    store.insert_layer(layer);
    let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    (store, live, path)
}
fn spec(value: f64) -> PropertySpec {
    PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)))
        .with_default(Value::Double(value))
}
fn set(path: PropertyPath, value: f64) -> Transaction {
    let mut txn = Transaction::new();
    txn.set_default(
        EditTarget::for_layer(LayerId(1)).property(path),
        Value::Double(value),
    );
    txn
}

#[test]
fn callbacks_and_independent_cursors_observe_the_same_completed_changes() {
    let (mut store, mut live, path) = fixture();
    let mut a = live.change_cursor();
    let mut b = a.clone();
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    let subscription = live.subscribe_changes(move |notice| {
        let value = notice.stage.resolve_property_path(path).unwrap().value;
        sink.lock()
            .unwrap()
            .push((notice.revision, notice.changes.clone(), value));
    });
    let applied = live.apply(&mut store, &set(path, 2.0)).unwrap();
    assert_eq!(received.lock().unwrap()[0].1, applied.changes);
    assert_eq!(
        received.lock().unwrap()[0].2,
        ResolvedValue::Scalar(Value::Double(2.0))
    );
    assert_eq!(
        live.changes_since(&mut a).unwrap().collect::<Vec<_>>(),
        [&applied.changes]
    );
    assert_eq!(live.changes_since(&mut a).unwrap().count(), 0);
    assert_eq!(live.changes_since(&mut b).unwrap().count(), 1);
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(
        received.lock().unwrap()[1].2,
        ResolvedValue::Scalar(Value::Double(1.0))
    );
    assert!(live.unsubscribe_changes(&subscription));
    assert!(!live.unsubscribe_changes(&subscription));
    live.apply(&mut store, &set(path, 3.0)).unwrap();
    assert_eq!(received.lock().unwrap().len(), 2);
}

#[test]
fn external_authoring_reload_and_recompose_publish_reports() {
    let (mut store, mut live, path) = fixture();
    let mut cursor = live.change_cursor();
    let mut other = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    other.apply(&mut store, &set(path, 2.0)).unwrap();
    live.synchronize(&mut store);
    assert_eq!(live.changes_since(&mut cursor).unwrap().count(), 1);
    set(path, 3.0).apply(&mut store).unwrap();
    live.synchronize(&mut store);
    assert_eq!(live.changes_since(&mut cursor).unwrap().count(), 1);
    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .set_property(path, spec(4.0));
    live.notify_changed_layers(&store);
    live.recompose(&mut store);
    assert_eq!(live.changes_since(&mut cursor).unwrap().count(), 1);
    let mut replacement = store.layers[&LayerId(1)].clone();
    replacement.prims.clear(); // Identical counters must not hide this reload.
    store.insert_layer(replacement);
    live.synchronize(&mut store);
    let changes = live.changes_since(&mut cursor).unwrap().next().unwrap();
    assert!(changes.removed.contains(&path.prim_path()));
    assert!(!live.stage().has_prim(path.prim_path()));
}

#[test]
fn history_loss_foreign_handles_and_failed_edits_are_explicit() {
    let (mut store, mut live, path) = fixture();
    let mut fast = live.change_cursor();
    let mut slow = fast.clone();
    let mut other = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let subscription = other.subscribe_changes(|_| {});
    assert!(!live.unsubscribe_changes(&subscription));
    let mut foreign = other.change_cursor();
    assert!(matches!(
        live.changes_since(&mut foreign),
        Err(ChangeHistoryError::DifferentStage)
    ));
    let missing = store.property_path("/Missing.weight");
    assert!(live.apply(&mut store, &set(missing, 1.0)).is_err());
    assert_eq!(live.changes_since(&mut fast).unwrap().count(), 0);
    for n in 0..70 {
        live.apply(&mut store, &set(path, f64::from(n))).unwrap();
        assert_eq!(live.changes_since(&mut fast).unwrap().count(), 1);
    }
    assert!(matches!(
        live.changes_since(&mut slow),
        Err(ChangeHistoryError::Expired)
    ));
    assert_eq!(live.changes_since(&mut slow).unwrap().count(), 0);
    assert_eq!(slow.revision(), fast.revision());
}

#[test]
#[cfg(panic = "unwind")]
fn callback_panic_preserves_committed_state_and_history() {
    let (mut store, mut live, path) = fixture();
    let mut cursor = live.change_cursor();
    live.subscribe_changes(|_| panic!("observer failure"));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        live.apply(&mut store, &set(path, 9.0)).unwrap();
    }));
    assert!(result.is_err());
    assert_eq!(
        live.stage().resolve_property_path(path).unwrap().value,
        ResolvedValue::Scalar(Value::Double(9.0))
    );
    assert_eq!(live.changes_since(&mut cursor).unwrap().count(), 1);
}
