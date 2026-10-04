// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{ChangeHistoryBudget, InMemoryStore, Layer, LayerId, StageOptions};
use alloc::vec;
use core::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn budgets_account_for_reports_and_make_gaps_explicit_without_losing_callbacks() {
    let mut store = InMemoryStore::default();
    let root = LayerId(1);
    store.insert_layer(Layer::new(root));
    let stage = Stage::compose(&mut store, root, StageOptions::default());
    let path = store.path("/Changed");
    let changes = Changes {
        changed_info_only: vec![path],
        ..Default::default()
    };
    let mut journal = Journal::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    journal.subscribe(move |_| {
        counter.fetch_add(1, Ordering::Relaxed);
    });
    let mut cursor = journal.cursor();
    journal.publish(&changes, &stage, &store);
    let bytes = journal.stats().retained_bytes;
    assert!(bytes > size_of::<Changes>());
    journal.set_budget(ChangeHistoryBudget {
        max_batches: 2,
        max_retained_bytes: bytes,
    });
    journal.publish(&changes, &stage, &store);
    assert_eq!(journal.stats().retained_batches, 1);
    assert_eq!(journal.stats().retained_bytes, bytes);
    assert!(matches!(
        journal.read(&mut cursor),
        Err(ChangeHistoryError::Expired)
    ));
    assert_eq!(cursor.revision(), 2);
    assert_eq!(journal.read(&mut cursor).unwrap().count(), 0);
    let oversized = Changes {
        changed_info_only: vec![path; 100],
        ..Default::default()
    };
    journal.publish(&oversized, &stage, &store);
    let stats = journal.stats();
    assert_eq!(stats.retained_batches, 0);
    assert_eq!(stats.retained_bytes, 0);
    assert_eq!(stats.allocated_bytes, 0);
    assert_eq!(stats.recorded_batches, 2);
    assert_eq!(stats.recorded_items, 2);
    assert_eq!(stats.discarded_batches, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert!(matches!(
        journal.read(&mut cursor),
        Err(ChangeHistoryError::Expired)
    ));
    journal.publish(&changes, &stage, &store);
    assert_eq!(journal.read(&mut cursor).unwrap().count(), 1);
    journal.set_budget(ChangeHistoryBudget {
        max_batches: 0,
        max_retained_bytes: 0,
    });
    journal.publish(&changes, &stage, &store);
    assert!(matches!(
        journal.read(&mut cursor),
        Err(ChangeHistoryError::Expired)
    ));
    assert_eq!(journal.read(&mut cursor).unwrap().count(), 0);
    assert_eq!(calls.load(Ordering::Relaxed), 5);
}

#[test]
fn batch_budget_shrink_invalidates_only_observers_that_lost_evidence() {
    let mut store = InMemoryStore::default();
    let root = LayerId(1);
    store.insert_layer(Layer::new(root));
    let stage = Stage::compose(&mut store, root, StageOptions::default());
    let path = store.path("/Changed");
    let changes = Changes {
        resynced: vec![path],
        ..Default::default()
    };
    let mut journal = Journal::default();
    let mut slow = journal.cursor();
    journal.publish(&changes, &stage, &store);
    let mut fast = journal.cursor();
    journal.publish(&changes, &stage, &store);
    journal.set_budget(ChangeHistoryBudget {
        max_batches: 1,
        max_retained_bytes: usize::MAX,
    });
    assert!(matches!(
        journal.read(&mut slow),
        Err(ChangeHistoryError::Expired)
    ));
    assert_eq!(journal.read(&mut fast).unwrap().count(), 1);
    let other = Journal::default();
    let before = fast.revision();
    assert!(matches!(
        other.read(&mut fast),
        Err(ChangeHistoryError::DifferentStage)
    ));
    assert_eq!(fast.revision(), before);
}
