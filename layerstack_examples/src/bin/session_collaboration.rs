// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Two clients share live opinions beneath private session overrides. A host
//! maps durable network identities to store-local `LayerId`s, serializes incoming
//! batches, checks permissions and chooses conflict/merge policy. Transactions
//! and change cursors are local APIs, not a transport or replicated edit log.
//!
//! OpenUSD's C++ `UsdStage` documents this stack in its Stage Session Layers
//! section: `GetEditTargetForLocalLayer(GetSessionLayer())` selects the ephemeral
//! authoring layer. Here each transaction carries its own explicit edit target.
//! <https://openusd.org/release/api/class_usd_stage.html#Usd_SessionLayer>
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec, PropertyType,
    Specifier, StageOptions, SublayerEntry, Transaction, Value,
};

fn main() {
    let mut store = InMemoryStore::default();
    let prop = store.property_path("/World.exposure");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        prop.prim_path(),
        PrimSpec::def().with_property(
            prop.property(),
            PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)))
                .with_default(Value::Double(1.0)),
        ),
    );
    store.insert_layer(root);
    let mut shared = Layer::new(LayerId(2));
    shared.insert_prim(
        prop.prim_path(),
        PrimSpec::over().with_property(
            prop.property(),
            PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)))
                .with_default(Value::Double(2.0)),
        ),
    );
    store.insert_layer(shared);
    for id in [LayerId(3), LayerId(4)] {
        let mut session = Layer::new(id);
        session.sublayers.push(SublayerEntry::new(LayerId(2)));
        store.insert_layer(session);
    }
    let options = |session| StageOptions {
        session_layer: Some(session),
        ..Default::default()
    };
    let mut a = LiveStage::compose(&mut store, LayerId(1), options(LayerId(3)));
    let mut b = LiveStage::compose(&mut store, LayerId(1), options(LayerId(4)));
    let mut cursor_a = a.change_cursor();
    let mut cursor_b = b.change_cursor();
    let mut private = Transaction::new();
    private.set_default(
        EditTarget::for_layer(LayerId(3)).property(prop),
        Value::Double(10.0),
    );
    let private_edit = a.apply(&mut store, &private).unwrap();

    // Incoming shared batch: compare the authored slot, not a client's composed
    // value. Independent slot guards permit unrelated edits; generation guards
    // are available when the host requires a whole-layer revision match.
    let at = EditTarget::for_layer(LayerId(2)).property(prop);
    let mut incoming = Transaction::new();
    incoming.expect_default(at.clone(), Some(Value::Double(2.0)));
    incoming.set_default(at, Value::Double(3.0));
    incoming.apply(&mut store).unwrap();
    assert!(
        incoming.apply(&mut store).is_err(),
        "duplicate shared batch must be rejected"
    ); // A duplicate/stale batch.
    a.synchronize(&mut store);
    b.synchronize(&mut store);
    assert_eq!(
        a.stage().resolve_field_path(prop).unwrap().value,
        Value::Double(10.0),
        "private session opinion wins"
    );
    assert_eq!(
        b.stage().resolve_field_path(prop).unwrap().value,
        Value::Double(3.0),
        "shared opinion reaches client B"
    );
    assert_eq!(
        store.layers[&LayerId(1)].property(prop).unwrap().default,
        Some(Value::Double(1.0)),
        "shared and session edits leave the published root unchanged"
    );
    println!(
        "client A reports: {}",
        a.changes_since(&mut cursor_a).unwrap().count()
    );
    println!(
        "client B reports: {}",
        b.changes_since(&mut cursor_b).unwrap().count()
    );
    // Client A creates a shared prim. B's snapshot stays unchanged until its
    // own explicit synchronization, even though both borrow the same store.
    let child = store.path("/World/SharedLamp");
    let mut create = Transaction::new();
    create.create_prim(
        EditTarget::for_layer(LayerId(2)).prim(child),
        Specifier::Def,
        None,
    );
    let added = a.apply(&mut store, &create).unwrap();
    assert!(a.stage().has_prim(child), "A sees its committed creation");
    assert!(!b.stage().has_prim(child), "B retains its prior snapshot");
    println!(
        "A created SharedLamp; B sees it before synchronization: {}",
        b.stage().has_prim(child)
    );
    b.synchronize(&mut store);
    assert!(
        b.stage().has_prim(child),
        "B sees the shared creation after synchronization"
    );
    assert!(
        !a.recomposition_work().full_rebuild,
        "A replaces only the changed subtree"
    );
    assert!(
        !b.recomposition_work().full_rebuild,
        "B independently replaces only the changed subtree"
    );
    println!("A creation work: {:?}", a.recomposition_work());
    println!("B synchronization work: {:?}", b.recomposition_work());
    for (name, reports) in [
        (
            "A",
            a.changes_since(&mut cursor_a).unwrap().collect::<Vec<_>>(),
        ),
        (
            "B",
            b.changes_since(&mut cursor_b).unwrap().collect::<Vec<_>>(),
        ),
    ] {
        for changes in reports {
            println!(
                "{name} created: {:?}; resynced: {:?}",
                changes
                    .created
                    .iter()
                    .map(|p| store.paths.display(*p, &store.tokens))
                    .collect::<Vec<_>>(),
                changes
                    .resynced
                    .iter()
                    .map(|p| store.paths.display(*p, &store.tokens))
                    .collect::<Vec<_>>()
            );
        }
    }

    // Undo is another guarded authored batch; either client can apply it.
    b.apply(&mut store, &added.inverse).unwrap();
    assert!(
        a.stage().has_prim(child),
        "A retains its snapshot until synchronization"
    );
    a.synchronize(&mut store);
    assert!(
        !a.stage().has_prim(child) && !b.stage().has_prim(child),
        "both clients incorporate the shared removal"
    );
    println!("SharedLamp removed from both clients after independent synchronization.");

    // Releasing the private edit reveals the newest shared opinion, rather than
    // copying the old composed value back into the session layer.
    a.apply(&mut store, &private_edit.inverse).unwrap();
    assert_eq!(
        a.stage().resolve_field_path(prop).unwrap().value,
        Value::Double(3.0),
        "undoing the private override reveals the current shared value"
    );
    assert_eq!(
        b.stage().resolve_field_path(prop).unwrap().value,
        Value::Double(3.0),
        "client B's snapshot is unaffected by A's private undo"
    );
    println!("A released its private override; both clients now read shared exposure 3.");

    // Each consumer owns its cursor. Persistent publication or merging the live
    // layer is a separate host operation; saving the root does not save sessions.
}
