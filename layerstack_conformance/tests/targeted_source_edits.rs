// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared authored deltas preserve scoped discovery and agree with fresh stages.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PathId, PrimSpec, Reference, Specifier,
    Stage, StageOptions, SublayerEntry, Transaction,
};

fn fixture(width: usize) -> (InMemoryStore, LiveStage, LiveStage) {
    let mut store = InMemoryStore::default();
    let asset = store.path("/Asset");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(asset, PrimSpec::def());
    root.insert_prim(
        store.path("/Copy"),
        PrimSpec::def().with_reference(Reference::new(LayerId(1), asset)),
    );
    for i in 0..width {
        root.insert_prim(store.path(&format!("/Unrelated{i}")), PrimSpec::def());
    }
    store.insert_layer(root);
    store.insert_layer(Layer::new(LayerId(2)));
    for id in [LayerId(3), LayerId(4)] {
        let mut session = Layer::new(id);
        session.sublayers.push(SublayerEntry::new(LayerId(2)));
        store.insert_layer(session);
    }
    let mut open = |id| {
        LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                session_layer: Some(id),
                ..Default::default()
            },
        )
    };
    let (a, b) = (open(LayerId(3)), open(LayerId(4)));
    (store, a, b)
}
fn assert_fresh(store: &mut InMemoryStore, live: &LiveStage) {
    let full = Stage::compose(store, LayerId(1), live.options().clone());
    let root = store.path("/");
    let actual: Vec<PathId> = live.stage().traverse_all(root).collect();
    let expected: Vec<PathId> = full.traverse_all(root).collect();
    assert_eq!(
        actual, expected,
        "complete traversal agrees with fresh composition"
    );
    for p in actual {
        assert_eq!(
            live.stage().children_of(p),
            full.children_of(p),
            "children at {p:?}"
        );
        assert_eq!(
            live.stage().variant_selections(p, store),
            full.variant_selections(p, store),
            "selections at {p:?}"
        );
        assert_eq!(
            live.stage().prim_stack(p),
            full.prim_stack(p),
            "source provenance at {p:?}"
        );
    }
    assert_eq!(
        live.stage().used_layers(false),
        full.used_layers(false),
        "used layer inventory"
    );
    assert_eq!(
        live.stage().composition_errors(),
        full.composition_errors(),
        "complete diagnostic replacement"
    );
}
#[test]
fn clients_consume_shared_structure_independently_and_discovery_stays_bounded() {
    let (mut store, mut a, mut b) = fixture(10_000);
    let child = store.path("/Asset/New/Leaf");
    let copy = store.path("/Copy/New/Leaf");
    let mut edit = Transaction::new();
    edit.create_prim(
        EditTarget::for_layer(LayerId(2)).prim(child),
        Specifier::Def,
        None,
    );
    let mut cursor_a = a.change_cursor();
    let mut cursor_b = b.change_cursor();
    let applied = a.apply(&mut store, &edit).unwrap();
    assert!(!a.recomposition_work().full_rebuild);
    assert!(a.stage().has_prim(child) && a.stage().has_prim(copy));
    assert!(!b.stage().has_prim(child));
    b.synchronize(&mut store);
    assert!(!b.recomposition_work().full_rebuild);
    for live in [&a, &b] {
        let work = live.recomposition_work();
        assert!(work.inspected_source_paths < 100, "{work:?}");
        assert!(work.indexed_source_paths < 10, "{work:?}");
        assert!(work.composed_prim_indexes < 20, "{work:?}");
        assert_fresh(&mut store, live);
    }
    assert_eq!(a.changes_since(&mut cursor_a).unwrap().count(), 1);
    assert_eq!(b.changes_since(&mut cursor_b).unwrap().count(), 1);
    a.apply(&mut store, &applied.inverse).unwrap();
    b.synchronize(&mut store);
    assert!(!a.recomposition_work().full_rebuild && !b.recomposition_work().full_rebuild);
    assert!(!a.stage().has_prim(child) && !b.stage().has_prim(copy));
    assert_fresh(&mut store, &a);
    assert_fresh(&mut store, &b);
    b.synchronize(&mut store);
    assert_eq!(
        b.recomposition_work(),
        layerstack::RecompositionWork::default()
    );
}
#[test]
fn history_expiry_unknown_writes_and_reload_require_honest_full_rebuilds() {
    let (mut store, mut a, mut b) = fixture(4);
    for i in 0..65 {
        let child = store.path(&format!("/Asset/Child{i}"));
        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .insert_prim(child, PrimSpec::def());
        a.synchronize(&mut store);
        assert!(!a.recomposition_work().full_rebuild);
    }
    b.synchronize(&mut store);
    assert!(b.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &b);
    let layer = store.layers.get_mut(&LayerId(2)).unwrap();
    layer.prims.clear();
    layer.touch();
    a.synchronize(&mut store);
    assert!(a.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &a);
    let replacement = store.layers[&LayerId(2)].clone();
    store.insert_layer(replacement);
    a.synchronize(&mut store);
    assert!(a.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &a);
}
#[test]
fn empty_reference_targets_can_appear_disappear_and_return_without_global_rebuilds() {
    let (mut store, _, _) = fixture(4);
    let source = store.path("/Library");
    let host = store.path("/External");
    store.layers.get_mut(&LayerId(1)).unwrap().insert_prim(
        host,
        PrimSpec::def().with_reference(Reference::with_asset(LayerId(5), source, "library.usda")),
    );
    store.insert_layer(Layer::new(LayerId(5)));
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let mut edit = Transaction::new();
    edit.create_prim(
        EditTarget::for_layer(LayerId(5)).prim(source),
        Specifier::Def,
        None,
    );
    let inverse = edit.apply(&mut store).unwrap();
    live.synchronize(&mut store);
    assert!(!live.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &live);
    let restore = inverse.apply(&mut store).unwrap();
    live.synchronize(&mut store);
    assert!(!live.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &live);
    restore.apply(&mut store).unwrap();
    live.synchronize(&mut store);
    assert!(!live.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &live);
}

#[test]
fn shared_variants_propagate_through_references_classes_and_specializes() {
    let (mut store, _, _) = fixture(16);
    let source = store.path("/Asset");
    let class = store.path("/Class");
    let inherited = store.path("/Inherited");
    let specialized = store.path("/Specialized");
    let layer = store.layers.get_mut(&LayerId(1)).unwrap();
    layer.insert_prim(
        class,
        PrimSpec::class().with_reference(Reference::new(LayerId(1), source)),
    );
    layer.insert_prim(inherited, PrimSpec::def().with_inherit(class));
    layer.insert_prim(specialized, PrimSpec::def().with_specialize(class));
    let options = StageOptions {
        session_layer: Some(LayerId(3)),
        ..Default::default()
    };
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    let variant =
        layerstack::SpecPath::parse("/Asset{shape=high}", &mut store.tokens, &mut store.paths)
            .unwrap();
    let target = EditTarget::for_local_variant(LayerId(2), &variant);
    let child = store.path("/Asset/VariantChild");
    let choice = store.tokens.intern("shape");
    let high = store.tokens.intern("high");
    let mut edit = Transaction::new();
    edit.create_prim(target.prim(child), Specifier::Def, None);
    edit.set_variant_selection(
        EditTarget::for_layer(LayerId(2)).prim(source),
        choice,
        Some(high),
    );
    let inverse = edit.apply(&mut store).unwrap();
    live.synchronize(&mut store);
    assert!(!live.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &live);
    for path in [
        "/Asset/VariantChild",
        "/Copy/VariantChild",
        "/Inherited/VariantChild",
        "/Specialized/VariantChild",
    ] {
        assert!(live.stage().has_prim(store.path(path)), "{path}");
    }
    inverse.apply(&mut store).unwrap();
    live.synchronize(&mut store);
    assert!(!live.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &live);
}

#[test]
fn precise_value_notifications_do_not_rediscover_the_wide_scene() {
    let (mut store, _, _) = fixture(1_000);
    let prop = store.property_path("/Asset.exposure");
    store.layers.get_mut(&LayerId(1)).unwrap().set_property(
        prop,
        layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "double",
            false,
            layerstack::Value::Double(0.0),
        ))
        .with_default(layerstack::Value::Double(1.0)),
    );
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            session_layer: Some(LayerId(3)),
            ..Default::default()
        },
    );
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(prop),
        layerstack::Value::Double(2.0),
    );
    edit.apply(&mut store).unwrap();
    live.synchronize(&mut store);
    let work = live.recomposition_work();
    assert!(
        !work.full_rebuild && work.inspected_source_paths < 20 && work.indexed_source_paths == 0,
        "{work:?}"
    );
    let copy = store.property_path("/Copy.exposure");
    assert_eq!(
        live.stage().resolve_field_path(copy).unwrap().value,
        layerstack::Value::Double(2.0)
    );
    assert_fresh(&mut store, &live);
}

#[test]
fn deleting_descendants_prunes_only_empty_implicit_ancestors() {
    let mut store = InMemoryStore::default();
    let leaf = store.path("/A/B/Leaf");
    let sibling = store.path("/A/Sibling");
    let unrelated = store.path("/Other");
    let mut layer = Layer::new(LayerId(1));
    for path in [leaf, sibling, unrelated] {
        layer.insert_prim(path, PrimSpec::def());
    }
    store.insert_layer(layer);
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    for path in [leaf, sibling] {
        let mut edit = Transaction::new();
        edit.remove_spec(EditTarget::for_layer(LayerId(1)).prim(path));
        edit.apply(&mut store).unwrap();
        live.synchronize(&mut store);
        assert!(!live.recomposition_work().full_rebuild);
        assert_fresh(&mut store, &live);
        assert!(live.stage().has_prim(unrelated));
    }
    assert!(!live.stage().has_prim(store.path("/A")));
    assert!(!live.stage().has_prim(store.path("/A/B")));
}

#[test]
fn recreated_layer_with_equal_generations_does_not_reuse_departed_namespace() {
    let mut store = InMemoryStore::default();
    let source = store.path("/Asset");
    let host = store.path("/Host");
    let old = store.path("/Asset/Old");
    let new = store.path("/Asset/New");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        host,
        PrimSpec::def().with_reference(Reference::with_asset(LayerId(2), source, "asset.usda")),
    );
    store.insert_layer(root);
    let mut original = Layer::new(LayerId(2));
    original.insert_prim(source, PrimSpec::def());
    original.insert_prim(old, PrimSpec::def());
    let original_generation = original.generation();
    store.insert_layer(original);
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    store.layers.remove(&LayerId(2));
    live.synchronize(&mut store);
    assert!(live.recomposition_work().full_rebuild);
    assert_fresh(&mut store, &live);
    let mut replacement = Layer::new(LayerId(2));
    replacement.insert_prim(source, PrimSpec::def());
    replacement.insert_prim(new, PrimSpec::def());
    assert_eq!(replacement.generation(), original_generation);
    store.insert_layer(replacement);
    live.synchronize(&mut store);
    assert!(live.recomposition_work().full_rebuild);
    assert!(live.stage().has_prim(store.path("/Host/New")));
    assert!(!live.stage().has_prim(store.path("/Host/Old")));
    assert_fresh(&mut store, &live);
}

#[test]
fn value_transactions_consume_pending_source_deltas() {
    let mut store = InMemoryStore::default();
    let source = store.path("/Asset");
    let child = store.path("/Asset/Child");
    let host = store.path("/Host");
    let property = store.property_path("/Host.exposure");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        host,
        PrimSpec::def().with_payload(Reference::with_asset(LayerId(2), source, "asset.usda")),
    );
    root.set_property(
        property,
        layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "double",
            false,
            layerstack::Value::Double(0.0),
        ))
        .with_default(layerstack::Value::Double(1.0)),
    );
    store.insert_layer(root);
    let mut asset = Layer::new(LayerId(2));
    asset.insert_prim(source, PrimSpec::def());
    asset.insert_prim(child, PrimSpec::def());
    store.insert_layer(asset);
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        layerstack::Value::Double(2.0),
    );
    let new = store.path("/Asset/New");
    store
        .layers
        .get_mut(&LayerId(2))
        .unwrap()
        .insert_prim(new, PrimSpec::def());
    live.notify_changed_layers(&store);
    live.apply(&mut store, &edit).unwrap();
    assert!(live.stage().has_prim(store.path("/Host/New")));
    assert_fresh(&mut store, &live);
    live.apply(&mut store, &edit).unwrap();
    assert_eq!(
        live.recomposition_work(),
        layerstack::RecompositionWork::default()
    );
}

#[test]
fn newly_composed_stacks_receive_relocation_notifications() {
    let mut store = InMemoryStore::default();
    let host = store.path("/Host");
    let asset = store.path("/Asset");
    let child = store.path("/Asset/Child");
    let renamed = store.path("/Asset/Renamed");
    let library = store.path("/Library");
    let library_child = store.path("/Library/Child");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(host, PrimSpec::def());
    store.insert_layer(root);
    let mut layer = Layer::new(LayerId(2));
    layer.insert_prim(
        asset,
        PrimSpec::def().with_reference(Reference::with_asset(LayerId(3), library, "library.usda")),
    );
    store.insert_layer(layer);
    let mut layer = Layer::new(LayerId(3));
    layer.insert_prim(library, PrimSpec::def());
    layer.insert_prim(library_child, PrimSpec::def());
    store.insert_layer(layer);
    let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
    store.layers.get_mut(&LayerId(1)).unwrap().insert_prim(
        host,
        PrimSpec::def().with_reference(Reference::with_asset(LayerId(2), asset, "asset.usda")),
    );
    live.synchronize(&mut store);
    assert!(!live.recomposition_work().full_rebuild);
    store.layers.get_mut(&LayerId(2)).unwrap().relocates = vec![layerstack::Relocate {
        source: child,
        target: Some(renamed),
    }];
    live.notify_relocates_edit(LayerId(2));
    live.recompose(&mut store);
    assert!(live.recomposition_work().full_rebuild);
    assert!(live.stage().has_prim(store.path("/Host/Renamed")));
    assert!(!live.stage().has_prim(store.path("/Host/Child")));
    assert_fresh(&mut store, &live);
}
