# Observing a live stage

LiveStage publishes composed change reports independently of the application's
authoring interface. Callbacks receive completed updates immediately; cursors let
consumers process evidence later. Neither requires a query session or schema cache.

```rust
let subscription = live.subscribe_changes(|notice| {
    println!("revision {}: {:?}", notice.revision, notice.changes);
    // notice.stage and notice.store expose the completed composed result.
});
let mut cursor = live.change_cursor();

live.apply(&mut store, &transaction)?; // Callback runs before this returns.
for changes in live.changes_since(&mut cursor)? {
    // Update the consumer's own state from this report.
}
live.unsubscribe_changes(&subscription);
```

Run `cargo run -p layerstack_examples --bin stage_notices` for a complete example,
including an edit authored outside LiveStage.

## What generates a notification

`apply`, `recompose` and `recompose_changes` publish completed reports. The reports
distinguish created/removed paths, resync roots and info-only changes. Existing-
property transactions also retain complete field inventories when known; absence
of an inventory means unknown, not unchanged.

`synchronize` scans participating layer generations and recomposes changes authored
through Layer methods, direct transactions or another stage using the store. These
external edits produce conservative resyncs. Unlike OpenUSD's synchronous source-
layer dispatch, their notifications arrive at this explicit synchronization point.
A scan costs O(participating layers). Raw writes to public importer fields must call
Layer::touch. InMemoryStore::insert_layer advances generations on replacement;
custom stores must preserve generation monotonicity or explicitly notify the stage.
External asset-resolution changes require explicit stage notification too.

Callbacks run in registration order and borrow the completed stage, source store,
revision and Changes. They can inspect the scene but must queue further edits until
dispatch returns. A panic propagates and skips later callbacks; the committed state
and cursor history remain available. Dropping a subscription token does not remove
the callback: unsubscribe explicitly, or drop the stage. Callbacks require Send +
Sync to preserve LiveStage's existing thread-transfer traits; dispatch is synchronous.

## Deferred readers and memory

Each cursor is independent and belongs to one stage. Reading advances that cursor;
the returned iterator must be processed in full. Another stage's cursor is rejected.
After the first cursor subscribes, the stage retains at most 64 reports. This bounds
report count, not bytes: a structural report can contain many paths. A slow cursor
gets ChangeHistoryError::Expired and advances to the current revision; rebuild the
consumer's derived state before continuing. Callback-only use retains no report
history. Callbacks and readers share the same reports and revision sequence.

Callbacks perform no computation on behalf of the application. The stage does not
have a frame clock; animation evaluation alone generates no authored-change notice.
No scheduler, background thread, new dependency or unsafe code is introduced.
