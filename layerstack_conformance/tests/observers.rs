// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Independent notification and computed-query observers over host-owned scenes.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    ChangeHistoryError, EditTarget, InMemoryStore, LayerId, LiveStage, PropertyPath, StageOptions,
    Transaction, Value,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{
    Time,
    bounds::BoundsOptions,
    retained::{Query, QueryAnswer, QueryCause, RetainedQueries},
};
use std::sync::{Arc, Mutex};

fn fixture() -> (InMemoryStore, LayerId, LiveStage, PropertyPath) {
    let loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/retained_queries.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let mut store = loaded.store;
    let size = store.property_path("/World/Shape.size");
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        with_provenance: true,
        ..StageOptions::default()
    };
    let live = LiveStage::compose(&mut store, loaded.root_layer, options);
    (store, loaded.root_layer, live, size)
}
fn set(layer: LayerId, path: PropertyPath, value: f64) -> Transaction {
    let mut txn = Transaction::new();
    txn.set_default(
        EditTarget::for_layer(layer).property(path),
        Value::Double(value),
    );
    txn
}
fn queries() -> RetainedQueries {
    RetainedQueries::new(Time::Default, BoundsOptions::default())
}

#[test]
fn callbacks_and_independent_queries_observe_host_edits() {
    let (mut store, layer, mut live, size) = fixture();
    let mut a = queries();
    let mut b = queries();
    let qa = a.observe(Query::WorldBound(size.prim_path()));
    let qb = b.observe(Query::WorldBound(size.prim_path()));
    let initial = a
        .view(&mut live, &mut store)
        .unwrap()
        .poll(qa)
        .unwrap()
        .answer
        .clone();
    b.view(&mut live, &mut store).unwrap().poll(qb).unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    let subscription = live.subscribe_changes(move |notice| {
        assert!(notice.stage.has_prim(size.prim_path()));
        sink.lock()
            .unwrap()
            .push((notice.revision, notice.changes.clone()));
    });
    let applied = live.apply(&mut store, &set(layer, size, 4.0)).unwrap();
    assert_eq!(received.lock().unwrap()[0].1, applied.changes);
    let after = a
        .view(&mut live, &mut store)
        .unwrap()
        .poll(qa)
        .unwrap()
        .answer
        .clone();
    assert_ne!(initial, after);
    assert!(
        !a.view(&mut live, &mut store)
            .unwrap()
            .poll(qa)
            .unwrap()
            .evaluated
    );
    assert_eq!(
        *b.view(&mut live, &mut store)
            .unwrap()
            .poll(qb)
            .unwrap()
            .answer,
        after
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(
        *a.view(&mut live, &mut store)
            .unwrap()
            .poll(qa)
            .unwrap()
            .answer,
        initial
    );
    assert_eq!(
        *b.view(&mut live, &mut store)
            .unwrap()
            .poll(qb)
            .unwrap()
            .answer,
        initial
    );
    assert!(live.unsubscribe_changes(&subscription));
    assert!(!live.unsubscribe_changes(&subscription));
    live.apply(&mut store, &set(layer, size, 6.0)).unwrap();
    assert_eq!(received.lock().unwrap().len(), 2);
}

#[test]
fn direct_source_edits_and_other_stages_need_no_query_session() {
    let (mut store, layer, mut live, size) = fixture();
    let mut other = LiveStage::compose(&mut store, layer, StageOptions::default());
    let mut a = queries();
    let q = a.observe(Query::WorldBound(size.prim_path()));
    let initial = a
        .view(&mut live, &mut store)
        .unwrap()
        .poll(q)
        .unwrap()
        .answer
        .clone();
    other.apply(&mut store, &set(layer, size, 8.0)).unwrap();
    let after = a
        .view(&mut live, &mut store)
        .unwrap()
        .poll(q)
        .unwrap()
        .answer
        .clone();
    assert_ne!(initial, after);
    set(layer, size, 12.0).apply(&mut store).unwrap();
    assert_ne!(
        *a.view(&mut live, &mut store)
            .unwrap()
            .poll(q)
            .unwrap()
            .answer,
        after
    );
    let spec = layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
        "double",
        false,
        Value::Double(0.0),
    ))
    .with_default(Value::Double(2.0));
    store
        .layers
        .get_mut(&layer)
        .unwrap()
        .set_property(size, spec);
    assert_eq!(
        *a.view(&mut live, &mut store)
            .unwrap()
            .poll(q)
            .unwrap()
            .answer,
        initial
    );
    assert!(matches!(
        a.view(&mut other, &mut store),
        Err(ChangeHistoryError::DifferentStage)
    ));
}

#[test]
fn history_loss_is_explicit_and_refreshes_slow_queries() {
    let (mut store, layer, mut live, size) = fixture();
    let mut fast = live.change_cursor();
    let mut slow = fast.clone();
    let mut a = queries();
    let q = a.observe(Query::WorldBound(size.prim_path()));
    let initial = a
        .view(&mut live, &mut store)
        .unwrap()
        .poll(q)
        .unwrap()
        .answer
        .clone();
    for n in 0..70 {
        live.apply(&mut store, &set(layer, size, f64::from(n + 3)))
            .unwrap();
        assert_eq!(live.changes_since(&mut fast).unwrap().count(), 1);
    }
    assert!(matches!(
        live.changes_since(&mut slow),
        Err(ChangeHistoryError::Expired)
    ));
    assert_eq!(live.changes_since(&mut slow).unwrap().count(), 0);
    let mut view = a.view(&mut live, &mut store).unwrap();
    let update = view.poll(q).unwrap();
    assert!(update.causes.contains(&QueryCause::HistoryLost));
    assert_ne!(*update.answer, initial);
    assert_eq!(update.revision, fast.revision());
}

#[test]
fn replacement_and_recompose_paths_publish_without_manual_delivery() {
    let (mut store, layer, mut live, size) = fixture();
    let mut cursor = live.change_cursor();
    let mut a = queries();
    let q = a.observe(Query::WorldBound(size.prim_path()));
    a.view(&mut live, &mut store).unwrap().poll(q).unwrap();
    let mut replacement = store.layers[&layer].clone();
    replacement.prims.clear();
    // Replacement has exactly the old counters; insert_layer must detect reload.
    store.insert_layer(replacement);
    live.synchronize(&mut store);
    assert_eq!(live.changes_since(&mut cursor).unwrap().count(), 1);
    assert!(matches!(
        a.view(&mut live, &mut store)
            .unwrap()
            .poll(q)
            .unwrap()
            .answer,
        QueryAnswer::WorldBound(Err(_))
    ));
    store.layers.get_mut(&layer).unwrap().touch();
    live.notify_changed_layers(&store);
    live.recompose(&mut store);
    // Full rebuild reports the pseudo-root even for an empty stage.
    assert_eq!(live.changes_since(&mut cursor).unwrap().count(), 1);
}

#[test]
fn failed_edits_and_foreign_handles_do_not_deliver_notices() {
    let (mut store, layer, mut live, size) = fixture();
    let mut other = LiveStage::compose(&mut store, layer, StageOptions::default());
    let calls = Arc::new(Mutex::new(0));
    let sink = calls.clone();
    let subscription = live.subscribe_changes(move |_| *sink.lock().unwrap() += 1);
    assert!(!other.unsubscribe_changes(&subscription));
    let mut foreign = other.change_cursor();
    assert!(matches!(
        live.changes_since(&mut foreign),
        Err(ChangeHistoryError::DifferentStage)
    ));
    let mut cursor = live.change_cursor();
    let missing = store.property_path("/Missing.size");
    assert!(live.apply(&mut store, &set(layer, missing, 2.0)).is_err());
    live.synchronize(&mut store);
    assert_eq!(*calls.lock().unwrap(), 0);
    assert_eq!(live.changes_since(&mut cursor).unwrap().count(), 0);
    live.apply(&mut store, &set(layer, size, 3.0)).unwrap();
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
#[cfg(panic = "unwind")]
fn callback_panic_keeps_committed_state_and_query_evidence() {
    let (mut store, layer, mut live, size) = fixture();
    let mut a = queries();
    let q = a.observe(Query::WorldBound(size.prim_path()));
    let before = a
        .view(&mut live, &mut store)
        .unwrap()
        .poll(q)
        .unwrap()
        .answer
        .clone();
    let subscription = live.subscribe_changes(|_| panic!("observer failure"));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        live.apply(&mut store, &set(layer, size, 9.0)).unwrap();
    }));
    assert!(result.is_err());
    live.unsubscribe_changes(&subscription);
    let mut view = a.view(&mut live, &mut store).unwrap();
    let update = view.poll(q).unwrap();
    assert!(update.evaluated);
    assert_ne!(*update.answer, before);
    assert!(
        update
            .causes
            .iter()
            .any(|cause| matches!(cause, QueryCause::Property { .. }))
    );
}
