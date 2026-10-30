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
    store.paths.sort_paths(&mut created, &store.tokens);
    assert_eq!(added.changes.created, created);
    // The pseudo-root exists before the first prim and after the last one,
    // as `UsdStage::GetPseudoRoot` does: neither creates nor removes it.
    let (root, rock) = (store.path("/"), store.path("/Rock"));
    assert_eq!(before, [root]);
    assert_eq!(added.changes.created, [rock]);
    let removed = live.apply(&mut store, &added.inverse).unwrap();
    matches_clean(&mut store, &live);
    assert_eq!(removed.changes.removed, [rock]);
    assert!(live.stage().has_prim(root));
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
            PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
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
        crate::SpecPath::parse(path, &store.tokens, &store.paths).unwrap(),
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
fn index_is_built_from_the_layer_as_composed_or_not_at_all() {
    // Built on first use from the unchanged layer: as if built with the
    // composition.
    let (store, mut live) = scene();
    assert!(
        live.local_namespace.built().is_none(),
        "not built at compose"
    );
    let world = store.path("/World");
    let index = live.local_index(&store).unwrap();
    assert!(index.current(&store));
    assert_eq!(index.children(world).len(), 2);
    // A layer changed behind the stage's back before the first edit has no
    // current index, as one built with the composition would not be.
    let (mut store, mut live) = scene();
    store.layer_mut(ROOT).unwrap().touch();
    assert!(live.local_index(&store).is_none());
    // The edit then takes the general path, and composition finds an index
    // for the next one.
    let mut txn = Transaction::new();
    txn.create_prim(address(&mut store, "/World/C"), Specifier::Def, None);
    live.apply(&mut store, &txn).unwrap();
    matches_clean(&mut store, &live);
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
    assert!(live.local_index(&store).unwrap().current(&store));
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World/A"));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert_eq!(removed.changes.removed, [a, x]);
    assert_eq!(removed.changes.resynced, [a]);
    // The parent's child list changed, and nothing else of it: OpenUSD
    // does not report it.
    assert!(removed.changes.changed_info_only.is_empty());
    assert!(live.stage().children_of(world).is_some());
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
    assert!(live.local_index(&store).unwrap().current(&store));
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
    parent.rare_mut().prim_order = Some(alloc::vec![bn, cn, an]);
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
    assert!(live.local_index(&store).unwrap().current(&store));
}

/// A property spec added or removed resyncs the property, not its prim
/// (OpenUSD: `UsdStage::_HandleLayersDidChange` resyncs the property path).
#[test]
fn property_declaration_changes_resync_the_property_in_both_directions() {
    let (mut store, mut live) = scene();
    let owner = store.path("/World/B");
    let property = store.property_path("/World/B.newValue");
    let mut create = Transaction::new();
    create.create_property(
        address(&mut store, "/World/B.newValue"),
        PropertySpec::typed_attribute(PropertyType::parse("double").unwrap()),
    );
    let created = live.apply(&mut store, &create).unwrap();
    assert!(created.changes.resynced.is_empty());
    assert_eq!(created.changes.resynced_properties, [property]);
    assert!(created.changes.changed_info_only.is_empty());
    assert!(created.changes.created.is_empty());
    assert!(
        live.view(&store)
            .prim(owner)
            .unwrap()
            .attribute(property.property())
            .is_some()
    );
    let removed = live.apply(&mut store, &created.inverse).unwrap();
    assert!(removed.changes.resynced.is_empty());
    assert_eq!(removed.changes.resynced_properties, [property]);
    assert!(removed.changes.changed_info_only.is_empty());
    assert!(
        live.view(&store)
            .prim(owner)
            .unwrap()
            .attribute(property.property())
            .is_none()
    );
    live.apply(&mut store, &removed.inverse).unwrap();
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World/B.newValue"));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert!(removed.changes.resynced.is_empty());
    assert_eq!(removed.changes.resynced_properties, [property]);
    assert!(removed.changes.changed_info_only.is_empty());
    matches_clean(&mut store, &live);
}

/// With a field edit of another property, the prim is info-only with that
/// field alone; the new property's own default is covered by its resync.
#[test]
fn property_resyncs_leave_other_field_edits_info_only() {
    let (mut store, mut live) = scene();
    let owner = store.path("/World/B");
    let property = store.property_path("/World/B.newValue");
    let value = store.tokens.intern("value");
    let mut txn = Transaction::new();
    txn.create_property(
        address(&mut store, "/World/B.newValue"),
        PropertySpec::typed_attribute(PropertyType::parse("double").unwrap()),
    );
    txn.set_default(address(&mut store, "/World/B.newValue"), Value::Double(2.0));
    txn.set_default(address(&mut store, "/World/B.value"), Value::Double(3.0));
    let applied = live.apply(&mut store, &txn).unwrap();
    assert!(applied.changes.resynced.is_empty());
    assert_eq!(applied.changes.resynced_properties, [property]);
    assert_eq!(applied.changes.changed_info_only, [owner]);
    let fields = applied.changes.properties_for(owner).unwrap();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, value);
    assert_eq!(fields[0].field, crate::PropertyField::Default);
    matches_clean(&mut store, &live);
}

/// A property created with its prim's spec, or beneath a prim another edit
/// of the transaction resyncs, is covered by the prim's resync.
#[test]
fn prim_resyncs_cover_property_resyncs() {
    let (mut store, mut live) = scene();
    let mut txn = Transaction::new();
    txn.create_prim(address(&mut store, "/World/B/Z"), Specifier::Def, None);
    txn.create_property(
        address(&mut store, "/World/B/Z.newValue"),
        PropertySpec::typed_attribute(PropertyType::parse("double").unwrap()),
    );
    let applied = live.apply(&mut store, &txn).unwrap();
    assert_eq!(applied.changes.resynced, [store.path("/World/B/Z")]);
    assert!(applied.changes.resynced_properties.is_empty());
    let schema = store.tokens.intern("CollectionAPI:selection");
    let mut txn = Transaction::new();
    txn.add_applied_schema(address(&mut store, "/World/B"), schema);
    txn.create_property(
        address(&mut store, "/World/B.other"),
        PropertySpec::typed_attribute(PropertyType::parse("double").unwrap()),
    );
    let applied = live.apply(&mut store, &txn).unwrap();
    assert_eq!(applied.changes.resynced, [store.path("/World/B")]);
    assert!(applied.changes.resynced_properties.is_empty());
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
                .rare_mut()
                .inherits
                .make_mut()
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
                    .rare_mut()
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
        assert!(live.local_index(&store).is_none(), "case {case}");
        let mut txn = Transaction::new();
        txn.create_prim(address(&mut store, "/World/A/New"), Specifier::Def, None);
        let applied = live.apply(&mut store, &txn).unwrap();
        assert!(!applied.changes.resynced.contains(&store.path("/")));
        assert!(live.local_index(&store).is_none());
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
    assert!(live.local_index(&store).unwrap().current(&store));
    matches_clean(&mut store, &live);
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World/A"));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert_eq!(removed.changes.resynced, [store.path("/World/A")]);
    assert_eq!(removed.changes.removed.len(), 2);
    assert!(live.local_index(&store).unwrap().current(&store));
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
                    spec: PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
                        .with_default(Value::Double(number))
                        .into()
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
    spec.rare_mut().variant_sets.insert(choice, set);
    spec.rare_mut()
        .variant_set_names
        .make_mut()
        .prepend
        .push(choice);
    spec.rare_mut().variant_selections.insert(choice, first);
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let mut before = live.stage().source_sites(owner);
    before.sort_unstable();
    store
        .layer_mut(ROOT)
        .unwrap()
        .prims
        .get_mut(&owner)
        .unwrap()
        .rare_mut()
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
    host.rare_mut().variant_sets.insert(set_name, set);
    host.rare_mut()
        .variant_set_names
        .make_mut()
        .prepend
        .push(set_name);
    host.rare_mut().variant_selections.insert(set_name, choice);
    layer.insert_prim(
        a,
        PrimSpec::def().with_outer_variant_sites(alloc::vec![
            crate::spec_path::VariantSelectionSite {
                host_path: world,
                set: set_name,
                variant: choice,
            }
        ]),
    );
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    assert!(live.local_index(&store).is_none());
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World"));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert!(live.local_index(&store).unwrap().current(&store));
    let restored = live.apply(&mut store, &removed.inverse).unwrap();
    assert!(live.local_index(&store).is_none());
    assert_eq!(restored.changes.resynced, [world]);
    matches_clean(&mut store, &live);
    let removed_again = live.apply(&mut store, &restored.inverse).unwrap();
    assert!(live.local_index(&store).unwrap().current(&store));
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
        .rare_mut()
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
            spec.rare_mut().specializes.make_mut().prepend.push(b);
        } else {
            spec.rare_mut().inherits.make_mut().prepend.push(b);
        }
        store.layer_mut(ROOT).unwrap().insert_prim(a, spec);
        // No explicit notification: the changed generation must invalidate
        // the proof before the subsequent namespace transaction composes.
        let mut txn = Transaction::new();
        txn.create_prim(address(&mut store, "/World/A/New"), Specifier::Def, None);
        let result = live.apply(&mut store, &txn).unwrap();
        assert_eq!(result.changes.resynced, [store.path("/")]);
        assert!(live.local_index(&store).is_none());
        matches_clean(&mut store, &live);
        assert!(live.stage().has_prim(store.path("/World/A/Y")));
        live.apply(&mut store, &result.inverse).unwrap();
        matches_clean(&mut store, &live);
    }
}

#[test]
fn boundary_order_keeps_inactive_names_until_after_reordering() {
    for (inactive, sparse) in (0..16).flat_map(|mask| (0..3).map(move |sparse| (mask, sparse))) {
        let mut store = InMemoryStore::default();
        let world = store.path("/World");
        let mut layer = Layer::new(ROOT);
        let mut parent = PrimSpec::def();
        parent.authored_children = ["B", "D", "A", "C"]
            .map(|name| store.tokens.intern(name))
            .to_vec();
        if sparse == 1 {
            parent.authored_children = ["Ghost", "D"]
                .map(|name| store.tokens.intern(name))
                .to_vec();
        } else if sparse == 2 {
            parent.authored_children.clear();
        }
        parent.rare_mut().prim_order = Some(
            ["Ghost", "D", "B"]
                .map(|name| store.tokens.intern(name))
                .to_vec(),
        );
        layer.insert_prim(world, parent);
        for (i, name) in ["A", "B", "C", "D"].into_iter().enumerate() {
            let path = store.path(&alloc::format!("/World/{name}"));
            let mut spec = PrimSpec::def();
            spec.active = Some(inactive & (1 << i) == 0);
            layer.insert_prim(path, spec);
        }
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
        let mut txn = Transaction::new();
        txn.remove_spec(address(&mut store, "/World/B"));
        txn.create_prim(address(&mut store, "/World/Between"), Specifier::Def, None);
        for _ in 0..4 {
            txn = live.apply(&mut store, &txn).unwrap().inverse;
            matches_clean(&mut store, &live);
        }
    }
}

#[test]
fn batch_removals_include_disjoint_and_overlapping_subtrees() {
    let (mut store, mut live) = scene();
    let before = store.layer(ROOT).unwrap().prims.clone();
    let mut txn = Transaction::new();
    // Removing a descendant first leaves the original source index as a
    // superset when its ancestor is removed later in the same transaction.
    for path in ["/World/A/X", "/World/A", "/World/B", "/World/B/Y"] {
        txn.remove_spec(address(&mut store, path));
    }
    for _ in 0..3 {
        let removed = live.apply(&mut store, &txn).unwrap();
        assert_eq!(removed.changes.removed.len(), 4);
        assert_eq!(removed.changes.resynced.len(), 2);
        assert_eq!(store.layer(ROOT).unwrap().prims.len(), 1);
        matches_clean(&mut store, &live);
        live.apply(&mut store, &removed.inverse).unwrap();
        assert_eq!(store.layer(ROOT).unwrap().prims, before);
        matches_clean(&mut store, &live);
    }
    let mut failing = txn.clone();
    failing.create_prim(address(&mut store, "/World"), Specifier::Def, None);
    assert!(live.apply(&mut store, &failing).is_err());
    assert_eq!(store.layer(ROOT).unwrap().prims, before);
    assert!(live.local_index(&store).unwrap().current(&store));
    matches_clean(&mut store, &live);
}

#[test]
fn creation_between_removals_does_not_hide_new_descendants() {
    let (mut store, mut live) = scene();
    let before = store.layer(ROOT).unwrap().prims.clone();
    let mut txn = Transaction::new();
    txn.remove_spec(address(&mut store, "/World/A/X"));
    txn.create_prim(
        address(&mut store, "/World/B/New/Leaf"),
        Specifier::Def,
        None,
    );
    txn.remove_spec(address(&mut store, "/World/B"));
    let removed = live.apply(&mut store, &txn).unwrap();
    assert_eq!(store.layer(ROOT).unwrap().prims.len(), 2);
    matches_clean(&mut store, &live);
    live.apply(&mut store, &removed.inverse).unwrap();
    assert_eq!(store.layer(ROOT).unwrap().prims, before);
    matches_clean(&mut store, &live);
}

#[test]
fn removal_after_inverse_restoration_finds_restored_descendants() {
    let (mut store, mut live) = scene();
    let mut remove = Transaction::new();
    remove.remove_spec(address(&mut store, "/World/B"));
    let mut restore = live.apply(&mut store, &remove).unwrap().inverse;
    let before = store.layer(ROOT).unwrap().prims.clone();
    // Raw inverse steps add paths absent from this transaction's snapshot.
    restore.remove_spec(address(&mut store, "/World/B"));
    let applied = live.apply(&mut store, &restore).unwrap();
    assert_eq!(store.layer(ROOT).unwrap().prims, before);
    assert!(applied.changes.created.is_empty());
    assert!(applied.changes.removed.is_empty());
    matches_clean(&mut store, &live);
}

#[test]
fn compacted_child_lists_merge_scattered_edits_across_parents() {
    let (mut store, _) = scene();
    for i in 0..64 {
        let parent = if i % 2 == 0 { "A" } else { "B" };
        let path = store.path(&alloc::format!("/World/{parent}/Part{i}"));
        store
            .layer_mut(ROOT)
            .unwrap()
            .insert_prim(path, PrimSpec::def());
    }
    let before = store.layer(ROOT).unwrap().prims.clone();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let mut txn = Transaction::new();
    for i in 0..64 {
        let parent = if i % 2 == 0 { "A" } else { "B" };
        if i % 3 == 0 {
            txn.remove_spec(address(
                &mut store,
                &alloc::format!("/World/{parent}/Part{i}"),
            ));
        } else if i % 3 == 1 {
            txn.create_prim(
                address(&mut store, &alloc::format!("/World/{parent}/Added{i}/Leaf")),
                Specifier::Def,
                None,
            );
        }
    }
    for _ in 0..3 {
        let applied = live.apply(&mut store, &txn).unwrap();
        matches_clean(&mut store, &live);
        live.apply(&mut store, &applied.inverse).unwrap();
        assert_eq!(store.layer(ROOT).unwrap().prims, before);
        matches_clean(&mut store, &live);
    }
}
