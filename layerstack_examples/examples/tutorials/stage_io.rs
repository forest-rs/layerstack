// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Traverse with pruning, retain an attribute query, export a binary layer,
//! reload sources, and protect unsaved edits through an explicit policy.
//! <https://openusd.org/release/tut_traversing_stage.html>
//! <https://openusd.org/release/tut_converting_between_layer_formats.html>
//! OpenUSD `UsdStage::Reload` excludes session layers and their sublayers.
//! <https://openusd.org/release/api/class_usd_stage.html>
mod support;
use layerstack::{EditTarget, Layer, LayerId, PrimPredicate, Time, Transaction, Value};
use layerstack_io::ReloadPolicy;
fn main() {
    let directory = support::directory();
    let mut document = support::hello(&directory);
    let (store, live) = document.parts_mut();
    let root = live.stage().root_layer().unwrap();
    let pseudo_root = store.path("/");
    let world = store.path("/hello/world");
    let radius = store.property_path("/hello/world.radius");
    let mut query = live
        .stage()
        .prim(world, store)
        .unwrap()
        .attribute("radius")
        .unwrap()
        .query();
    assert_eq!(
        query
            .try_get(live.stage(), Time::Default)
            .unwrap()
            .unwrap()
            .value,
        Value::Double(2.0),
        "authored sphere radius"
    );
    query.try_get(live.stage(), Time::Default).unwrap();
    assert_eq!(
        query.work().cache_hits,
        1,
        "same-time queries reuse evaluation"
    );
    let mut range = live
        .stage()
        .prim_range(pseudo_root, store, PrimPredicate::DEFAULT)
        .pre_and_post();
    while let Some(visit) = range.next() {
        println!(
            "{} {}",
            if visit.is_post_visit {
                "leave"
            } else {
                "enter"
            },
            store.paths.display(visit.prim, &store.tokens)
        );
        if visit.prim == world && !visit.is_post_visit {
            range.prune_children();
        }
    }
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(root).property(radius),
        Value::Double(3.0),
    );
    let applied = live.apply(store, &edit).unwrap();
    assert_eq!(
        query
            .try_get(live.stage(), Time::Default)
            .unwrap()
            .unwrap()
            .value,
        Value::Double(3.0),
        "query refreshes after the edit"
    );
    assert!(
        document.reload(ReloadPolicy::PreserveDirty).is_err(),
        "reload protects unsaved edits"
    );
    let (store, live) = document.parts_mut();
    live.apply(store, &applied.inverse).unwrap();
    document.export_layer(root, "HelloWorld.usdc").unwrap();

    // A private preview belongs in the session layer. Saving and reloading the
    // document operates on published source layers while retaining this preview.
    let (store, live) = document.parts_mut();
    let session = LayerId(1_000);
    store.insert_layer(Layer::new(session));
    assert!(
        live.set_session_layer(Some(session)),
        "attach the private session"
    );
    live.synchronize(store);
    let mut preview = Transaction::new();
    preview.set_default(
        EditTarget::for_layer(session).property(radius),
        Value::Double(9.0),
    );
    let preview = live.apply(store, &preview).unwrap();
    let saved = document.save();
    assert!(saved.failures.is_empty(), "{:?}", saved.failures);
    assert_eq!(
        saved.saved,
        vec![root],
        "save the edited root, excluding sessions"
    );
    assert!(document.is_dirty(session), "save excludes private sessions");
    document.reload(ReloadPolicy::DiscardDirty).unwrap();
    assert!(
        !document.is_dirty(root),
        "successful reload restores the saved baseline"
    );
    let (store, live) = document.parts_mut();
    assert_eq!(
        query
            .try_get(live.stage(), Time::Default)
            .unwrap()
            .unwrap()
            .value,
        Value::Double(9.0),
        "source reload preserves the private session preview"
    );
    live.apply(store, &preview.inverse).unwrap();
    assert_eq!(
        query
            .try_get(live.stage(), Time::Default)
            .unwrap()
            .unwrap()
            .value,
        Value::Double(2.0),
        "clearing the preview reveals the saved radius"
    );
    println!("{}", directory.join("HelloWorld.usdc").display());
}
