// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`UndoHistory`]: grouping, staleness, the bound and unwinding, over
//! the rock scene of the transaction tests.

use alloc::{vec, vec::Vec};

use super::tests::{ROCK, Rng, SCENE, assert_live_matches_fresh, layers, random_edit, rocks, spec};
use super::*;
use crate::{InMemoryStore, Layer, LiveStage, StageOptions, Value};

fn live(store: &mut InMemoryStore) -> LiveStage {
    LiveStage::compose(store, SCENE, StageOptions::default())
}

/// A transaction setting the default of the attribute spec at `path` in
/// the rock asset.
fn set(store: &mut InMemoryStore, path: &str, value: f64) -> Transaction {
    let at = Address::spec(ROCK, spec(store, path));
    let mut txn = Transaction::new();
    txn.set_default(at, Value::Double(value));
    txn
}

/// Applies `txn` through `live` and records its inverse as `label`.
fn apply(
    history: &mut UndoHistory,
    live: &mut LiveStage,
    store: &mut InMemoryStore,
    label: &str,
    txn: &Transaction,
) -> Recorded {
    let applied = live.apply(store, txn).expect("applies");
    history.record(label, applied.inverse)
}

/// Sets the attribute spec at `path` without recording it: an edit the
/// history does not know about.
fn outside(live: &mut LiveStage, store: &mut InMemoryStore, path: &str, value: f64) {
    let txn = set(store, path, value);
    live.apply(store, &txn).expect("applies");
}

fn labels<'a>(steps: impl Iterator<Item = &'a HistoryStep>) -> Vec<&'a str> {
    steps.map(HistoryStep::label).collect()
}

/// For random edits, recorded alone or in random (nested) groups, with
/// undos and redos interleaved: each undo restores the layers exactly as
/// they were before the step, each redo exactly as they were after it,
/// and the stage stays what a fresh composition gives. Undoing every step
/// gets back to the start; redoing every step gets back to the end.
#[test]
fn undo_and_redo_restore_identical_layers() {
    let (mut steps, mut grouped, mut undos) = (0, 0, 0);
    for seed in 1..=96_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut store = rocks();
        let mut live = live(&mut store);
        let mut history = UndoHistory::new();
        // The layers after each undo step, the current state last, and
        // the states the redo steps get back to, the next last.
        let mut done: Vec<Vec<Layer>> = vec![layers(&store)];
        let mut undone: Vec<Vec<Layer>> = Vec::new();
        let mut group_recorded = false;
        for _ in 0..60 {
            match rng.below(12) {
                0..=6 => {
                    let mut txn = Transaction::new();
                    for _ in 0..=rng.below(3) {
                        random_edit(&mut rng, &mut store, &mut txn);
                    }
                    let Ok(applied) = live.apply(&mut store, &txn) else {
                        continue;
                    };
                    let recorded = history.record("edit", applied.inverse);
                    match recorded {
                        Recorded::Step { dropped: None } => {
                            done.push(layers(&store));
                            undone.clear();
                        }
                        Recorded::Grouped => {
                            group_recorded = true;
                            undone.clear();
                        }
                        // An inverse with no edits: the transaction changed
                        // nothing.
                        Recorded::Nothing if !group_recorded => {
                            assert_eq!(&layers(&store), done.last().expect("state"));
                        }
                        Recorded::Nothing => {}
                        other => panic!("seed {seed}: {other:?}"),
                    }
                }
                7 => history.begin_group("group"),
                8 => {
                    if history.group_depth() == 1 && group_recorded {
                        assert!(matches!(
                            history.end_group(),
                            Ok(Recorded::Step { dropped: None })
                        ));
                        done.push(layers(&store));
                        undone.clear();
                        group_recorded = false;
                        grouped += 1;
                    } else if history.group_depth() > 0 {
                        assert_eq!(history.end_group(), Ok(Recorded::Nothing));
                    }
                }
                9 | 10 if history.group_depth() == 0 && done.len() > 1 => {
                    history
                        .undo(&mut live, &mut store)
                        .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
                    undone.push(done.pop().expect("state"));
                    let step = history.next_redo().expect("undone");
                    assert_eq!(
                        step.layers(),
                        step.transaction().layers(),
                        "the redo writes the layers the step's edits wrote"
                    );
                    assert_eq!(
                        &layers(&store),
                        done.last().expect("state"),
                        "seed {seed}: undo restores the layers before the step"
                    );
                    undos += 1;
                }
                11 if history.group_depth() == 0 && !undone.is_empty() => {
                    history
                        .redo(&mut live, &mut store)
                        .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
                    done.push(undone.pop().expect("state"));
                    assert_eq!(
                        &layers(&store),
                        done.last().expect("state"),
                        "seed {seed}: redo restores the layers after the step"
                    );
                }
                _ => {}
            }
            assert_eq!(history.undo_steps().len(), done.len() - 1);
            assert_eq!(history.redo_steps().len(), undone.len());
        }
        while history.group_depth() > 0 {
            history.end_group().expect("open");
        }
        if group_recorded {
            done.push(layers(&store));
            undone.clear();
        }
        while let Some(state) = undone.pop() {
            history.redo(&mut live, &mut store).expect("redoes");
            assert_eq!(layers(&store), state, "seed {seed}");
            done.push(state);
        }
        steps += done.len() - 1;
        let end = layers(&store);
        while history.can_undo() {
            history.undo(&mut live, &mut store).expect("undoes");
        }
        assert_eq!(layers(&store), done[0], "seed {seed}: back to the start");
        assert_live_matches_fresh(&live, &mut store, "after undoing everything");
        while history.can_redo() {
            history.redo(&mut live, &mut store).expect("redoes");
        }
        assert_eq!(layers(&store), end, "seed {seed}: forward to the end");
        assert_live_matches_fresh(&live, &mut store, "after redoing everything");
    }
    assert!(
        steps > 400 && grouped > 40 && undos > 250,
        "{steps} steps, {grouped} groups, {undos} undos"
    );
}

/// Transactions recorded in a group undo as one step, atomically: when an
/// edit the history did not record makes the oldest of them stale,
/// undoing the group changes nothing, even the slots the newer ones
/// restore, and the step stays.
#[test]
fn a_group_undoes_atomically() {
    let mut store = rocks();
    let mut live = live(&mut store);
    let start = layers(&store);
    let mut history = UndoHistory::new();
    history.begin_group("resize and roughen");
    let size = set(&mut store, "/Rock.size", 2.0);
    let roughness = set(&mut store, "/Rock{shape=round}.roughness", 0.5);
    let spin = set(&mut store, "/Rock.spin", 9.0);
    for (label, txn) in [("size", &size), ("roughness", &roughness), ("spin", &spin)] {
        let recorded = apply(&mut history, &mut live, &mut store, label, txn);
        assert_eq!(recorded, Recorded::Grouped);
    }
    assert!(!history.can_undo(), "a group is open");
    assert!(matches!(
        history.undo(&mut live, &mut store),
        Err(HistoryError::GroupOpen { label }) if label == "resize and roughen"
    ));
    assert_eq!(history.end_group(), Ok(Recorded::Step { dropped: None }));
    let step = history.next_undo().expect("one step");
    assert_eq!(
        (step.label(), step.receipts(), step.layers()),
        ("resize and roughen", 3, &[ROCK][..])
    );

    // Not recorded: the slot the oldest edit wrote changes.
    outside(&mut live, &mut store, "/Rock.size", 3.0);
    let before = layers(&store);
    let error = history.undo(&mut live, &mut store).unwrap_err();
    assert_eq!(error.code(), "history.stale");
    assert!(
        matches!(&error, HistoryError::Stale { label, error: EditError::StaleValue { .. } } if label == "resize and roughen"),
        "{error:?}"
    );
    assert_eq!(layers(&store), before, "nothing was undone, not even spin");
    assert_eq!(labels(history.undo_steps()), ["resize and roughen"]);
    assert!(history.can_undo() && !history.can_redo());

    // Put the slot back as the group left it, and the step undoes.
    outside(&mut live, &mut store, "/Rock.size", 2.0);
    history.undo(&mut live, &mut store).expect("undoes now");
    assert_eq!(layers(&store), start);
    assert_eq!(labels(history.redo_steps()), ["resize and roughen"]);
    assert_eq!(history.next_redo().map(HistoryStep::receipts), Some(3));
}

/// An edit the history did not record makes the step that restores the
/// slot it changed stale: undo reports it and changes nothing; an
/// outside edit of another slot of the same spec does not stop the undo.
#[test]
fn a_stale_step_fails_cleanly() {
    let mut store = rocks();
    let mut live = live(&mut store);
    let mut history = UndoHistory::new();
    let size = set(&mut store, "/Rock.size", 2.0);
    apply(&mut history, &mut live, &mut store, "size 2", &size);
    let overwrite = set(&mut store, "/Rock.size", 3.0);
    let outside_undo = live.apply(&mut store, &overwrite).unwrap().inverse;
    let before = layers(&store);
    let error = history.undo(&mut live, &mut store).unwrap_err();
    assert!(
        matches!(&error, HistoryError::Stale { label, .. } if label == "size 2"),
        "{error:?}"
    );
    let shown = alloc::format!("{}", error.display(&store));
    assert!(
        shown.starts_with("\"size 2\" is stale: ") && shown.contains("/Rock.size"),
        "{shown}"
    );
    assert_eq!(layers(&store), before);
    assert_eq!(labels(history.undo_steps()), ["size 2"]);
    assert_eq!(history.redo_steps().len(), 0);

    // The outside edit undone, the step is current again; an outside
    // edit of another slot leaves it so.
    live.apply(&mut store, &outside_undo).unwrap();
    outside(&mut live, &mut store, "/Rock{shape=round}.roughness", 0.7);
    let mut fresh = rocks();
    set(&mut fresh, "/Rock{shape=round}.roughness", 0.7)
        .apply(&mut fresh)
        .unwrap();
    let expected = layers(&fresh);
    history.undo(&mut live, &mut store).expect("undoes");
    assert_eq!(layers(&store), expected);
    assert_eq!(
        history.undo(&mut live, &mut store),
        Err(HistoryError::Empty)
    );
}

/// The bound keeps at most `limit` steps, undo and redo together, and
/// reports each step it drops.
#[test]
fn the_bound_drops_the_oldest_step() {
    let mut store = rocks();
    let mut live = live(&mut store);
    let mut history = UndoHistory::with_limit(2);
    assert_eq!(history.limit(), Some(2));
    for (index, value) in [2.0, 3.0].into_iter().enumerate() {
        let txn = set(&mut store, "/Rock.size", value);
        let label = alloc::format!("size {index}");
        assert_eq!(
            apply(&mut history, &mut live, &mut store, &label, &txn),
            Recorded::Step { dropped: None }
        );
    }
    let txn = set(&mut store, "/Rock.size", 4.0);
    let Recorded::Step {
        dropped: Some(dropped),
    } = apply(&mut history, &mut live, &mut store, "size 2", &txn)
    else {
        panic!("the oldest step is dropped");
    };
    assert_eq!(dropped.label(), "size 0");
    assert_eq!(history.dropped(), 1);
    assert_eq!(labels(history.undo_steps()), ["size 1", "size 2"]);
    history.undo(&mut live, &mut store).unwrap();
    history.undo(&mut live, &mut store).unwrap();
    assert_eq!(
        history.undo(&mut live, &mut store),
        Err(HistoryError::Empty)
    );
    assert_eq!(labels(history.redo_steps()), ["size 2", "size 1"]);
    assert_eq!(history.next_redo().map(HistoryStep::label), Some("size 1"));
}

/// A group that records nothing, and an inverse with no edits, add no
/// step and keep the redo stack; recording anything else clears it.
#[test]
fn empty_records_add_no_step() {
    let mut store = rocks();
    let mut live = live(&mut store);
    let mut history = UndoHistory::new();
    let txn = set(&mut store, "/Rock.size", 2.0);
    apply(&mut history, &mut live, &mut store, "size", &txn);
    history.undo(&mut live, &mut store).unwrap();
    history.begin_group("nothing");
    history.begin_group("nested nothing");
    assert_eq!(history.group_depth(), 2);
    assert_eq!(history.open_group(), Some("nothing"));
    assert_eq!(history.end_group(), Ok(Recorded::Nothing));
    assert_eq!(history.end_group(), Ok(Recorded::Nothing));
    assert_eq!(history.end_group(), Err(HistoryError::NoGroup));
    assert_eq!(
        history.record("no edits", Transaction::new()),
        Recorded::Nothing
    );
    assert_eq!(labels(history.redo_steps()), ["size"]);
    let txn = set(&mut store, "/Rock.spin", 1.0);
    apply(&mut history, &mut live, &mut store, "spin", &txn);
    assert!(!history.can_redo(), "a new step forks history");
}

/// Abandoning a group undoes what its innermost level recorded and
/// forgets it; an outer level keeps what it recorded before.
#[test]
fn abandoning_a_group_undoes_what_it_recorded() {
    let mut store = rocks();
    let mut live = live(&mut store);
    let start = layers(&store);
    let mut history = UndoHistory::new();
    history.begin_group("outer");
    let size = set(&mut store, "/Rock.size", 2.0);
    apply(&mut history, &mut live, &mut store, "size", &size);
    let after_size = layers(&store);
    history.begin_group("inner");
    let spin = set(&mut store, "/Rock.spin", 5.0);
    apply(&mut history, &mut live, &mut store, "spin", &spin);
    let roughness = set(&mut store, "/Rock{shape=round}.roughness", 0.5);
    apply(&mut history, &mut live, &mut store, "roughness", &roughness);
    let undone = history
        .abandon_group(&mut live, &mut store)
        .unwrap()
        .expect("the inner level recorded");
    assert!(!undone.inverse.is_empty());
    assert_eq!(layers(&store), after_size);
    assert_eq!(history.group_depth(), 1);
    assert_eq!(history.end_group(), Ok(Recorded::Step { dropped: None }));
    assert_eq!(history.next_undo().map(HistoryStep::receipts), Some(1));

    history.begin_group("abandoned");
    apply(&mut history, &mut live, &mut store, "spin", &spin);
    history.abandon_group(&mut live, &mut store).unwrap();
    assert_eq!(history.open_group(), None);
    assert_eq!(layers(&store), after_size);
    assert_eq!(labels(history.undo_steps()), ["outer"]);
    history.undo(&mut live, &mut store).unwrap();
    assert_eq!(layers(&store), start);
    assert_eq!(
        history.abandon_group(&mut live, &mut store),
        Err(HistoryError::NoGroup)
    );
}

/// Unwinding to a mark undoes every step recorded since, as one
/// transaction, and forgets them; a mark whose step is gone is refused.
#[test]
fn unwinding_returns_to_a_mark() {
    let mut store = rocks();
    let mut live = live(&mut store);
    let mut history = UndoHistory::new();
    let start = layers(&store);
    let empty = history.mark();
    let size = set(&mut store, "/Rock.size", 2.0);
    apply(&mut history, &mut live, &mut store, "size", &size);
    let after_size = layers(&store);
    let mark = history.mark();
    assert_eq!(history.unwind_to(mark, &mut live, &mut store), Ok(None));
    for (path, value) in [
        ("/Rock.spin", 1.0),
        ("/Rock{shape=round}.roughness", 0.3),
        ("/Rock.spin", 2.0),
    ] {
        let txn = set(&mut store, path, value);
        apply(&mut history, &mut live, &mut store, path, &txn);
    }
    let undone = history
        .unwind_to(mark, &mut live, &mut store)
        .unwrap()
        .expect("three steps since");
    assert_eq!(undone.inverse.layers(), [ROCK]);
    assert_eq!(layers(&store), after_size);
    assert_eq!(labels(history.undo_steps()), ["size"]);
    assert!(!history.can_redo(), "unwound steps are forgotten");

    history.undo(&mut live, &mut store).unwrap();
    assert_eq!(
        history.unwind_to(mark, &mut live, &mut store),
        Err(HistoryError::MarkGone),
        "its step is undone"
    );
    history.redo(&mut live, &mut store).unwrap();
    history.begin_group("open");
    assert!(matches!(
        history.unwind_to(mark, &mut live, &mut store),
        Err(HistoryError::GroupOpen { .. })
    ));
    history.end_group().unwrap();
    history
        .unwind_to(empty, &mut live, &mut store)
        .unwrap()
        .expect("one step since");
    assert_eq!(layers(&store), start);

    let txn = set(&mut store, "/Rock.size", 5.0);
    apply(&mut history, &mut live, &mut store, "size", &txn);
    let mark = history.mark();
    history.reset();
    assert_eq!(history.undo_steps().len(), 0);
    assert_eq!(
        history.unwind_to(mark, &mut live, &mut store),
        Err(HistoryError::MarkGone)
    );
    let mut bounded = UndoHistory::with_limit(1);
    let empty = bounded.mark();
    let txn = set(&mut store, "/Rock.spin", 3.0);
    apply(&mut bounded, &mut live, &mut store, "spin", &txn);
    let txn = set(&mut store, "/Rock.spin", 4.0);
    apply(&mut bounded, &mut live, &mut store, "spin", &txn);
    assert_eq!(
        bounded.unwind_to(empty, &mut live, &mut store),
        Err(HistoryError::MarkGone),
        "a step since the mark was dropped"
    );
}

/// The codes are stable.
#[test]
fn history_error_codes() {
    assert_eq!(HistoryError::Empty.code(), "history.empty");
    assert_eq!(HistoryError::NoGroup.code(), "history.no_group");
    assert_eq!(HistoryError::MarkGone.code(), "history.mark_gone");
    let label = alloc::string::String::from("x");
    assert_eq!(
        HistoryError::GroupOpen {
            label: label.clone()
        }
        .code(),
        "history.group_open"
    );
    let stale = HistoryError::from_edit(
        "x",
        EditError::StaleGeneration {
            layer: ROCK,
            expected: 1,
            found: 2,
        },
    );
    assert_eq!(stale.code(), "history.stale");
    let rejected = HistoryError::from_edit("x", EditError::UnresolvedPrecondition { index: 0 });
    assert_eq!(rejected.code(), "history.rejected");
    assert!(rejected.edit_error().is_some());
}
