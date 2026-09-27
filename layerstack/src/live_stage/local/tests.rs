// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::edit::{Address, Transaction};
use crate::{InMemoryStore, Layer, LiveStage, PropertySpec, PropertyType, Specifier, Stage, Value};

const ROOT: LayerId = LayerId(1);

#[test]
fn first_prim_and_last_prim_removal_match_empty_population() {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(ROOT));
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let before: Vec<_> = live.stage().prim_paths().collect();
    let mut txn = Transaction::new();
    txn.create_prim(address(&mut store, "/Rock"), Specifier::Def, None);
    let added = live.apply(&mut store, &txn).unwrap();
    matches_clean(&mut store, &live);
    let mut created: Vec<_> = live
        .stage()
        .prim_paths()
        .filter(|p| !before.contains(p))
        .collect();
    created.sort_unstable();
    assert_eq!(added.changes.created, created);
    let removed = live.apply(&mut store, &added.inverse).unwrap();
    matches_clean(&mut store, &live);
    assert_eq!(removed.changes.removed, created);
}

fn scene() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let value = store.tokens.intern("value");
    let mut layer = Layer::new(ROOT);
    for name in ["/World", "/World/A", "/World/A/X", "/World/B", "/World/B/Y"] {
        let path = store.path(name);
        let mut spec = PrimSpec::def();
        spec.set_property(
            value,
            PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)))
                .with_default(Value::Double(1.0)),
        );
        layer.insert_prim(path, spec);
    }
    store.insert_layer(layer);
    let live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    (store, live)
}

fn address(store: &mut InMemoryStore, path: &str) -> Address {
    Address::spec(
        ROOT,
        crate::SpecPath::parse(path, &mut store.tokens, &mut store.paths).unwrap(),
    )
}

fn matches_clean(store: &mut InMemoryStore, live: &LiveStage) {
    let fresh = Stage::compose(store, ROOT, StageOptions::default());
    let mut before: Vec<_> = live.stage().prim_paths().collect();
    let mut after: Vec<_> = fresh.prim_paths().collect();
    before.sort_unstable();
    after.sort_unstable();
    assert_eq!(before, after);
    for path in before {
        assert_eq!(live.stage().children_of(path), fresh.children_of(path));
        let names = fresh.authored_property_names(path, store);
        assert_eq!(live.stage().authored_property_names(path, store), names);
        for name in names {
            assert_eq!(
                live.stage().resolve_property_declaration(path, name),
                fresh.resolve_property_declaration(path, name)
            );
            let property = crate::PropertyPath::new(path, name);
            assert_eq!(
                live.stage().resolve_property_path(property),
                fresh.resolve_property_path(property)
            );
        }
        assert_eq!(live.stage().prim_stack(path), fresh.prim_stack(path));
    }
    assert_eq!(
        live.stage().composition_errors(),
        fresh.composition_errors()
    );
}

#[test]
fn local_subtree_delete_undo_and_value_sequence_has_precise_reports() {
    let (mut store, mut live) = scene();
    let a = store.path("/World/A");
    let x = store.path("/World/A/X");
    let world = store.path("/World");
    let b = store.path("/World/B");
    let mut value = Transaction::new();
    value.set_default(address(&mut store, "/World/B.value"), Value::Double(4.0));
    let info = live.apply(&mut store, &value).unwrap();
    assert_eq!(info.changes.changed_info_only, [b]);
    assert!(live.local_namespace.as_ref().unwrap().current(&store));
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World/A"));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert_eq!(removed.changes.removed, [a, x]);
    assert_eq!(removed.changes.resynced, [a]);
    assert_eq!(removed.changes.changed_info_only, [world]);
    assert!(removed.changes.created.is_empty());
    matches_clean(&mut store, &live);
    let restored = live.apply(&mut store, &removed.inverse).unwrap();
    assert_eq!(restored.changes.created, [a, x]);
    assert_eq!(restored.changes.resynced, [a]);
    matches_clean(&mut store, &live);
    live.apply(&mut store, &value).unwrap();
    let mut create = Transaction::new();
    create.create_prim(address(&mut store, "/World/C/Leaf"), Specifier::Def, None);
    let added = live.apply(&mut store, &create).unwrap();
    let c = store.path("/World/C");
    assert_eq!(added.changes.resynced, [c]);
    assert_eq!(added.changes.created.len(), 2);
    assert!(live.local_namespace.as_ref().unwrap().current(&store));
    matches_clean(&mut store, &live);
}

#[test]
fn mixed_edits_keep_untouched_hierarchy_and_follow_current_transaction_namespace() {
    let (mut store, mut live) = scene();
    let mut txn = Transaction::new();
    txn.create_prim(address(&mut store, "/World/A/New"), Specifier::Def, None);
    txn.remove_spec(address(&mut store, "/World/A"));
    txn.set_default(address(&mut store, "/World/B.value"), Value::Double(8.0));
    let applied = live.apply(&mut store, &txn).unwrap();
    assert_eq!(applied.changes.removed.len(), 2);
    assert!(applied.changes.created.is_empty());
    assert_eq!(
        live.stage().children_of(store.path("/World/B")).unwrap(),
        [store.path("/World/B/Y")]
    );
    matches_clean(&mut store, &live);
    live.apply(&mut store, &applied.inverse).unwrap();
    matches_clean(&mut store, &live);
}

#[test]
fn deleting_child_and_editing_parent_preserves_complete_child_list() {
    let (mut store, mut live) = scene();
    let mut txn = Transaction::new();
    txn.remove_spec(address(&mut store, "/World/A"));
    txn.set_default(address(&mut store, "/World.value"), Value::Double(9.0));
    let report = live.apply(&mut store, &txn).unwrap();
    assert_eq!(report.changes.changed_info_only, [store.path("/World")]);
    matches_clean(&mut store, &live);
    live.apply(&mut store, &report.inverse).unwrap();
    matches_clean(&mut store, &live);
}

#[test]
fn inactive_and_unsupported_namespace_edits_are_conservative() {
    let (mut store, mut live) = scene();
    let a = store.path("/World/A");
    store
        .layer_mut(ROOT)
        .unwrap()
        .prims
        .get_mut(&a)
        .unwrap()
        .active = Some(false);
    live.notify_structural_change();
    let report = live.apply(&mut store, &Transaction::new()).unwrap();
    assert_eq!(report.changes.resynced, [store.path("/")]);
    matches_clean(&mut store, &live);
    let mut create = Transaction::new();
    create.create_prim(address(&mut store, "/World/A/Hidden"), Specifier::Def, None);
    let report = live.apply(&mut store, &create).unwrap();
    assert!(report.changes.created.is_empty());
    assert!(report.changes.resynced.is_empty());
    matches_clean(&mut store, &live);
}

#[test]
fn authored_child_order_survives_delete_and_exact_undo() {
    let (mut store, _) = scene();
    let world = store.path("/World");
    let a = store.path("/World/A");
    let b = store.path("/World/B");
    let c = store.path("/World/C");
    let an = store.tokens.intern("A");
    let bn = store.tokens.intern("B");
    let cn = store.tokens.intern("C");
    let layer = store.layer_mut(ROOT).unwrap();
    layer.insert_prim(c, PrimSpec::def());
    let parent = layer.prims.get_mut(&world).unwrap();
    parent.authored_children = alloc::vec![cn, an, bn];
    parent.prim_order = Some(alloc::vec![bn, cn, an]);
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    assert_eq!(live.stage().children_of(world).unwrap(), [b, c, a]);
    let mut txn = Transaction::new();
    txn.remove_spec(address(&mut store, "/World/C"));
    let applied = live.apply(&mut store, &txn).unwrap();
    assert_eq!(live.stage().children_of(world).unwrap(), [b, a]);
    matches_clean(&mut store, &live);
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(live.stage().children_of(world).unwrap(), [b, c, a]);
    matches_clean(&mut store, &live);
    assert!(live.local_namespace.as_ref().unwrap().current(&store));
}

#[test]
fn property_declaration_changes_resync_the_owner_in_both_directions() {
    let (mut store, mut live) = scene();
    let owner = store.path("/World/B");
    let mut create = Transaction::new();
    create.create_property(
        address(&mut store, "/World/B.newValue"),
        PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0))),
    );
    let created = live.apply(&mut store, &create).unwrap();
    assert_eq!(created.changes.resynced, [owner]);
    assert!(created.changes.changed_info_only.is_empty());
    assert!(created.changes.created.is_empty());
    let removed = live.apply(&mut store, &created.inverse).unwrap();
    assert_eq!(removed.changes.resynced, [owner]);
    assert!(removed.changes.changed_info_only.is_empty());
    live.apply(&mut store, &removed.inverse).unwrap();
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World/B.newValue"));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert_eq!(removed.changes.resynced, [owner]);
    assert!(removed.changes.changed_info_only.is_empty());
    matches_clean(&mut store, &live);
}

#[test]
fn applied_schema_changes_resync_the_owner_and_undo() {
    let (mut store, mut live) = scene();
    let owner = store.path("/World/B");
    let schema = store.tokens.intern("CollectionAPI:selection");
    let mut txn = Transaction::new();
    txn.add_applied_schema(address(&mut store, "/World/B"), schema);
    let applied = live.apply(&mut store, &txn).unwrap();
    assert_eq!(applied.changes.resynced, [owner]);
    assert!(applied.changes.changed_info_only.is_empty());
    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(undone.changes.resynced, [owner]);
    assert!(undone.changes.changed_info_only.is_empty());
    matches_clean(&mut store, &live);
}

#[test]
fn pending_external_declarations_and_schemas_are_not_reported_as_info_only() {
    for schema_edit in [false, true] {
        let (mut store, mut live) = scene();
        let owner = store.path("/World/B");
        let mut txn = Transaction::new();
        if schema_edit {
            let schema = store.tokens.intern("CollectionAPI:selection");
            txn.add_applied_schema(address(&mut store, "/World/B"), schema);
        } else {
            txn.create_property(
                address(&mut store, "/World/B.newValue"),
                PropertySpec::attribute(),
            );
        }
        txn.apply(&mut store).unwrap();
        live.notify_layer_prim_edits(ROOT, &[owner]);
        let applied = live.apply(&mut store, &Transaction::new()).unwrap();
        assert_eq!(applied.changes.resynced, [owner]);
        assert!(applied.changes.changed_info_only.is_empty());
        matches_clean(&mut store, &live);
    }
}

#[test]
fn masks_arcs_variants_and_instanceable_use_general_composition() {
    for case in 0..4 {
        let (mut store, _) = scene();
        let a = store.path("/World/A");
        let b = store.path("/World/B");
        let mut options = StageOptions::default();
        match case {
            0 => {
                options.mask = Some(crate::PopulationMask {
                    include: alloc::vec![a],
                });
            }
            1 => store
                .layer_mut(ROOT)
                .unwrap()
                .prims
                .get_mut(&a)
                .unwrap()
                .inherits
                .prepend
                .push(b),
            2 => {
                let variant = store.tokens.intern("shape");
                store
                    .layer_mut(ROOT)
                    .unwrap()
                    .prims
                    .get_mut(&a)
                    .unwrap()
                    .variant_sets
                    .insert(variant, crate::VariantSetSpec::default());
            }
            _ => {
                store
                    .layer_mut(ROOT)
                    .unwrap()
                    .prims
                    .get_mut(&a)
                    .unwrap()
                    .instanceable = Some(true);
            }
        }
        let mut live = LiveStage::compose(&mut store, ROOT, options.clone());
        assert!(live.local_namespace.is_none(), "case {case}");
        let mut txn = Transaction::new();
        txn.create_prim(address(&mut store, "/World/A/New"), Specifier::Def, None);
        let applied = live.apply(&mut store, &txn).unwrap();
        assert_eq!(applied.changes.resynced, [store.path("/")]);
        assert!(live.local_namespace.is_none());
        let fresh = Stage::compose(&mut store, ROOT, options);
        let mut actual: Vec<_> = live.stage().prim_paths().collect();
        let mut expected: Vec<_> = fresh.prim_paths().collect();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        for path in actual {
            assert_eq!(live.stage().children_of(path), fresh.children_of(path));
            assert_eq!(live.stage().prim_stack(path), fresh.prim_stack(path));
        }
    }
}

#[test]
fn failed_transaction_rolls_back_without_consuming_the_namespace_index() {
    let (mut store, mut live) = scene();
    let mut txn = Transaction::new();
    txn.remove_spec(address(&mut store, "/World/A"));
    txn.create_prim(address(&mut store, "/World/B"), Specifier::Def, None);
    assert!(live.apply(&mut store, &txn).is_err());
    assert!(live.local_namespace.as_ref().unwrap().current(&store));
    matches_clean(&mut store, &live);
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World/A"));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert_eq!(removed.changes.resynced, [store.path("/World/A")]);
    assert_eq!(removed.changes.removed.len(), 2);
    assert!(live.local_namespace.as_ref().unwrap().current(&store));
    matches_clean(&mut store, &live);
}

#[test]
fn scoped_variant_change_resyncs_even_when_unqualified_source_sites_are_equal() {
    let (mut store, _) = scene();
    let owner = store.path("/World/A");
    let choice = store.tokens.intern("choice");
    let first = store.tokens.intern("first");
    let second = store.tokens.intern("second");
    let value = store.tokens.intern("branchValue");
    let mut set = crate::VariantSetSpec::default();
    for (variant, number) in [(first, 1.0), (second, 2.0)] {
        set.variants.insert(
            variant,
            crate::VariantSpec {
                properties: alloc::vec![crate::PropertyEntry {
                    name: value,
                    spec: PropertySpec::typed_attribute(PropertyType::new(
                        "double",
                        false,
                        Value::Double(0.0)
                    ))
                    .with_default(Value::Double(number))
                }],
                ..crate::VariantSpec::default()
            },
        );
    }
    let spec = store
        .layer_mut(ROOT)
        .unwrap()
        .prims
        .get_mut(&owner)
        .unwrap();
    spec.variant_sets.insert(choice, set);
    spec.variant_set_order.push(choice);
    spec.variant_selections.insert(choice, first);
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let mut before = live.stage().source_sites(owner);
    before.sort_unstable();
    store
        .layer_mut(ROOT)
        .unwrap()
        .prims
        .get_mut(&owner)
        .unwrap()
        .variant_selections
        .insert(choice, second);
    live.notify_layer_prim_edits(ROOT, &[owner]);
    let applied = live.apply(&mut store, &Transaction::new()).unwrap();
    let mut after = live.stage().source_sites(owner);
    after.sort_unstable();
    assert_eq!(before, after);
    assert_eq!(applied.changes.resynced, [owner]);
    assert!(applied.changes.changed_info_only.is_empty());
    matches_clean(&mut store, &live);
}

#[test]
fn undo_restoring_variant_specs_leaves_the_local_fast_path() {
    let (mut store, _) = scene();
    let world = store.path("/World");
    let a = store.path("/World/A");
    let set_name = store.tokens.intern("appearance");
    let choice = store.tokens.intern("bright");
    let child = store.tokens.intern("A");
    let mut set = crate::VariantSetSpec::default();
    set.variants.insert(
        choice,
        crate::VariantSpec {
            authored_children: alloc::vec![child],
            ..crate::VariantSpec::default()
        },
    );
    let layer = store.layer_mut(ROOT).unwrap();
    let host = layer.prims.get_mut(&world).unwrap();
    host.variant_sets.insert(set_name, set);
    host.variant_set_order.push(set_name);
    host.variant_selections.insert(set_name, choice);
    layer.insert_prim(
        a,
        PrimSpec {
            outer_variant_sites: alloc::vec![crate::spec_path::VariantSelectionSite {
                host_path: world,
                set: set_name,
                variant: choice,
            }],
            ..PrimSpec::def()
        },
    );
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    assert!(live.local_namespace.is_none());
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World"));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert!(live.local_namespace.as_ref().unwrap().current(&store));
    let restored = live.apply(&mut store, &removed.inverse).unwrap();
    assert!(live.local_namespace.is_none());
    assert_eq!(restored.changes.resynced, [store.path("/")]);
    matches_clean(&mut store, &live);
    let removed_again = live.apply(&mut store, &restored.inverse).unwrap();
    assert!(live.local_namespace.as_ref().unwrap().current(&store));
    assert_eq!(removed_again.changes.removed, removed.changes.removed);
    matches_clean(&mut store, &live);
}

#[test]
fn deep_edits_retain_ancestor_siblings_and_inactive_context() {
    let (mut store, _) = scene();
    let world = store.path("/World");
    let a = store.path("/World/A");
    let b = store.path("/World/B");
    let bn = store.tokens.intern("B");
    let an = store.tokens.intern("A");
    store
        .layer_mut(ROOT)
        .unwrap()
        .prims
        .get_mut(&world)
        .unwrap()
        .prim_order = Some(alloc::vec![bn, an]);
    store
        .layer_mut(ROOT)
        .unwrap()
        .prims
        .get_mut(&b)
        .unwrap()
        .active = Some(false);
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    for name in ["/World/A/X/New", "/World/B/Y/Hidden"] {
        let mut txn = Transaction::new();
        txn.create_prim(address(&mut store, name), Specifier::Def, None);
        for _ in 0..4 {
            txn = live.apply(&mut store, &txn).unwrap().inverse;
            assert_eq!(live.stage().children_of(world).unwrap(), [a]);
            matches_clean(&mut store, &live);
        }
    }
}

#[test]
fn external_arc_edits_expire_the_local_composition_proof() {
    for specializes in [false, true] {
        let (mut store, mut live) = scene();
        let a = store.path("/World/A");
        let b = store.path("/World/B");
        let mut spec = store.layer(ROOT).unwrap().prims[&a].clone();
        if specializes {
            spec.specializes.prepend.push(b);
        } else {
            spec.inherits.prepend.push(b);
        }
        store.layer_mut(ROOT).unwrap().insert_prim(a, spec);
        // No explicit notification: the changed generation must invalidate
        // the proof before the subsequent namespace transaction composes.
        let mut txn = Transaction::new();
        txn.create_prim(address(&mut store, "/World/A/New"), Specifier::Def, None);
        let result = live.apply(&mut store, &txn).unwrap();
        assert_eq!(result.changes.resynced, [store.path("/")]);
        assert!(live.local_namespace.is_none());
        matches_clean(&mut store, &live);
        assert!(live.stage().has_prim(store.path("/World/A/Y")));
        live.apply(&mut store, &result.inverse).unwrap();
        matches_clean(&mut store, &live);
    }
}
