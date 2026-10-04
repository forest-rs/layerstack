// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Control deltas agree with full composition, including cross-scope arcs.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, LoadPolicy, PathId, PopulationMask, PrimSpec,
    Reference, Stage, StageOptions, SublayerEntry,
};

fn fixture(mask: bool) -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let source = store.path("/Asset");
    let host = store.path("/Host");
    let nested = store.path("/Asset/Nested");
    let deep = store.path("/Deep");
    let mut root = Layer::new(LayerId(1));
    let mut spec = PrimSpec::def();
    spec.payloads.explicit = Some(vec![Reference::with_asset(
        LayerId(2),
        source,
        "asset.usda",
    )]);
    root.insert_prim(host, spec);
    let mut spec = PrimSpec::def();
    spec.references.explicit = Some(vec![Reference::new(LayerId(1), store.path("/Host/Nested"))]);
    root.insert_prim(store.path("/Copy"), spec);
    for name in ["/Unrelated", "/Hidden", "/Hidden/Child"] {
        root.insert_prim(store.path(name), PrimSpec::def());
    }
    store.insert_layer(root);
    let mut asset = Layer::new(LayerId(2));
    asset.sublayers.push(SublayerEntry::new(LayerId(4)));
    asset.insert_prim(source, PrimSpec::def());
    let mut spec = PrimSpec::def();
    spec.payloads.explicit = Some(vec![Reference::with_asset(LayerId(3), deep, "deep.usda")]);
    asset.insert_prim(nested, spec);
    asset.insert_prim(store.path("/Asset/Child"), PrimSpec::def());
    store.insert_layer(asset);
    let mut layer = Layer::new(LayerId(3));
    layer.insert_prim(deep, PrimSpec::def());
    layer.insert_prim(store.path("/Deep/Leaf"), PrimSpec::def());
    store.insert_layer(layer);
    let mut layer = Layer::new(LayerId(4));
    layer.insert_prim(store.path("/Asset/Overlay"), PrimSpec::def());
    store.insert_layer(layer);
    let options = StageOptions {
        mask: mask.then(|| PopulationMask {
            include: vec![host, store.path("/Copy"), store.path("/Unrelated")],
        }),
        ..Default::default()
    };
    let live = LiveStage::compose(&mut store, LayerId(1), options);
    (store, live)
}
fn assert_full(store: &mut InMemoryStore, live: &LiveStage) {
    let full = Stage::compose(store, LayerId(1), live.options().clone());
    let root = store.path("/");
    let paths = |stage: &Stage| stage.traverse_all(root).collect::<Vec<PathId>>();
    assert_eq!(
        paths(live.stage()),
        paths(&full),
        "scoped traversal matches full composition"
    );
    for p in paths(&full) {
        assert_eq!(
            live.stage().children_of(p),
            full.children_of(p),
            "active child order at {p:?}"
        );
        assert_eq!(
            live.stage().all_children_of(p),
            full.all_children_of(p),
            "all child order at {p:?}"
        );
        assert_eq!(
            live.stage().is_loaded(p, &store.paths),
            full.is_loaded(p, &store.paths),
            "effective loading at {p:?}"
        );
    }
    assert_eq!(
        live.stage().used_layers(false),
        full.used_layers(false),
        "used layer inventory"
    );
    assert_eq!(
        live.stage().loadable_paths(&store.paths, root),
        full.loadable_paths(&store.paths, root),
        "loadable host inventory"
    );
    assert_eq!(
        live.stage().loaded_payload_paths(&store.paths),
        full.loaded_payload_paths(&store.paths),
        "loaded payload inventory"
    );
    assert_eq!(
        live.stage().composition_errors(),
        full.composition_errors(),
        "complete composition diagnostics"
    );
}
#[test]
fn payload_and_external_mute_deltas_match_full_recomposition() {
    for mask in [false, true] {
        let (mut store, mut live) = fixture(mask);
        let host = store.path("/Host");
        let nested = store.path("/Host/Nested");
        let copy = store.path("/Copy");
        let unrelated = store.path("/Unrelated");
        for i in 0..7 {
            match i {
                0 => {
                    live.unload(&store, host);
                }
                1 => {
                    live.load(&store, host, LoadPolicy::WithoutDescendants);
                }
                2 => {
                    live.load(&store, nested, LoadPolicy::WithDescendants);
                }
                3 => {
                    live.mute_layer(LayerId(4)).unwrap();
                }
                4 => {
                    live.unmute_layer(LayerId(4));
                }
                5 => {
                    live.mute_layer(LayerId(2)).unwrap();
                }
                _ => {
                    live.unmute_layer(LayerId(2));
                }
            }
            let changes = live.recompose_changes(&mut store);
            assert!(
                !live.recomposition_work().full_rebuild,
                "step {i} {:?}",
                live.recomposition_work()
            );
            assert!(!changes.resynced.contains(&unrelated));
            assert!(!changes.created.contains(&unrelated));
            assert!(live.stage().has_prim(copy));
            assert_full(&mut store, &live);
        }
        live.unload(&store, host);
        live.load(&store, host, LoadPolicy::WithDescendants);
        assert!(
            live.recompose_changes(&mut store).resynced.is_empty(),
            "cancelled control batch is a no-op"
        );
        assert_eq!(
            live.recomposition_work(),
            layerstack::RecompositionWork::default()
        );
        // A structural source edit concurrent with controls remains a full rebuild.
        live.unload(&store, host);
        let new = store.path("/New");
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .insert_prim(new, PrimSpec::def());
        live.synchronize(&mut store);
        assert!(live.recomposition_work().full_rebuild);
        assert_full(&mut store, &live);
    }
}

#[test]
fn global_composition_errors_force_complete_diagnostic_replacement() {
    let (mut store, _) = fixture(false);
    store
        .layers
        .get_mut(&LayerId(2))
        .unwrap()
        .sublayers
        .push(SublayerEntry::new(LayerId(2)));
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    assert!(!live.stage().composition_errors().is_empty());
    live.mute_layer(LayerId(2)).unwrap();
    live.recompose_changes(&mut store);
    assert!(live.recomposition_work().full_rebuild);
    assert_full(&mut store, &live);
    live.unmute_layer(LayerId(2));
    live.recompose_changes(&mut store);
    assert!(live.recomposition_work().full_rebuild);
    assert_full(&mut store, &live);
}

#[test]
fn control_recomposition_never_acknowledges_unincorporated_source_edits() {
    let (mut store, mut live) = fixture(false);
    let property = store.property_path("/Unrelated.weight");
    store.layers.get_mut(&LayerId(1)).unwrap().set_property(
        property,
        layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "double",
            false,
            layerstack::Value::Double(0.0),
        ))
        .with_default(layerstack::Value::Double(7.0)),
    );
    let host = store.path("/Host");
    live.unload(&store, host);
    live.recompose_changes(&mut store); // Deliberately did not notify first.
    assert!(live.recomposition_work().full_rebuild);
    assert_eq!(
        live.stage().resolve_field_path(property).unwrap().value,
        layerstack::Value::Double(7.0)
    );
    assert!(live.notify_changed_layers(&store).is_empty());
    assert_full(&mut store, &live);
}

#[test]
fn value_transactions_consume_pending_payload_controls() {
    let (mut store, _) = fixture(false);
    let host = store.path("/Host");
    let property = store.property_path("/Host.exposure");
    store.layers.get_mut(&LayerId(1)).unwrap().set_property(
        property,
        layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "double",
            false,
            layerstack::Value::Double(0.0),
        ))
        .with_default(layerstack::Value::Double(1.0)),
    );
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    live.unload(&store, host);
    let mut edit = layerstack::Transaction::new();
    edit.set_default(
        layerstack::EditTarget::for_layer(LayerId(1)).property(property),
        layerstack::Value::Double(2.0),
    );
    let applied = live.apply(&mut store, &edit).unwrap();
    assert!(applied.changes.removed.contains(&store.path("/Host/Child")));
    assert_full(&mut store, &live);
    live.apply(&mut store, &edit).unwrap();
    assert_eq!(
        live.recomposition_work(),
        layerstack::RecompositionWork::default()
    );
}
