// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tests: opinion edits notified to a live stage that change which
//! children a prim has, or their order, with inactive children among
//! them. `layerstack_conformance`'s `inactive_children` replays the same
//! kinds of edit as transactions against OpenUSD's records.

use alloc::{format, string::String, vec, vec::Vec};

use super::LiveStage;
use super::as_fresh::assert_as_fresh;
use crate::{InMemoryStore, Layer, LayerId, PathId, PrimSpec, Reference, StageOptions, TokenId};

const ROOT: LayerId = LayerId(1);
const ASSET: LayerId = LayerId(2);

/// `/P` with `A`, an inactive `B` holding `Inner`, `C` and an inactive
/// `D`; `/R` referencing `/Asset` of another layer, with `X`, `Y` and an
/// inactive `Z`.
fn scene() -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let mut root = Layer::new(ROOT);
    let mut asset = Layer::new(ASSET);
    let names = |store: &InMemoryStore, names: &[&str]| -> Vec<TokenId> {
        names.iter().map(|name| store.tokens.intern(name)).collect()
    };
    let mut parent = PrimSpec::def();
    parent.authored_children = names(&store, &["A", "B", "C", "D"]);
    root.insert_prim(store.path("/P"), parent);
    root.insert_prim(store.path("/P/A"), PrimSpec::def());
    let mut b = PrimSpec::def().with_active(false);
    b.authored_children = names(&store, &["Inner"]);
    root.insert_prim(store.path("/P/B"), b);
    root.insert_prim(store.path("/P/B/Inner"), PrimSpec::def());
    root.insert_prim(store.path("/P/C"), PrimSpec::def());
    root.insert_prim(store.path("/P/D"), PrimSpec::def().with_active(false));
    let r = PrimSpec::def().with_reference(Reference::new(ASSET, store.path("/Asset")));
    root.insert_prim(store.path("/R"), r);
    let mut host = PrimSpec::def();
    host.authored_children = names(&store, &["X", "Y", "Z"]);
    asset.insert_prim(store.path("/Asset"), host);
    asset.insert_prim(store.path("/Asset/X"), PrimSpec::def());
    asset.insert_prim(store.path("/Asset/Y"), PrimSpec::def());
    asset.insert_prim(store.path("/Asset/Z"), PrimSpec::def().with_active(false));
    store.insert_layer(root);
    store.insert_layer(asset);
    store
}

/// The names of the children of `prim`, inactive ones included.
fn all_children(live: &LiveStage, store: &InMemoryStore, prim: PathId) -> Vec<String> {
    live.stage()
        .all_children_of(prim)
        .unwrap_or_default()
        .iter()
        .map(|child| {
            let leaf = store.paths.resolve(*child).leaf().unwrap();
            String::from(store.tokens.resolve(leaf))
        })
        .collect()
}

/// Edits the spec at `path` of `layer` in place, as a host does before
/// it notifies the live stage of the prims whose opinions changed.
fn edit_spec(
    store: &mut InMemoryStore,
    layer: LayerId,
    path: &str,
    edit: impl FnOnce(&mut PrimSpec),
) {
    let path = store.path(path);
    edit(
        store
            .layers
            .get_mut(&layer)
            .unwrap()
            .prims
            .get_mut(&path)
            .unwrap(),
    );
}

/// A `primOrder` that moves only inactive children changes the full child
/// list and leaves the active one as it was; the live stage still lists
/// the children a fresh composition does, and resyncs the parent, as
/// OpenUSD resyncs it for a reorder.
///
/// Spec: AOUSD Core §11 (child order); OpenUSD: `UsdPrim::GetAllChildren`.
#[test]
fn an_order_moving_inactive_children_recomposes_the_child_list() {
    let mut store = scene();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let parent = store.path("/P");
    let d = store.tokens.intern("D");
    let a = store.tokens.intern("A");
    edit_spec(&mut store, ROOT, "/P", |spec| {
        spec.rare_mut().prim_order = Some(vec![d, a]);
    });
    live.notify_prim_edit(parent);
    let changes = live.recompose_changes(&mut store);
    assert_as_fresh(&live, &mut store, "order");
    assert_eq!(all_children(&live, &store, parent), ["D", "A", "B", "C"]);
    assert!(changes.is_resynced(parent, &store.paths), "{changes:?}");
}

/// Deactivating or activating a prim with no children changes no child
/// list of its own, but its parent's: the live stage lists the active
/// children a fresh composition does, and resyncs the prim, as OpenUSD
/// resyncs a prim whose `active` changes.
///
/// Spec: AOUSD Core §7.6 (`active`), §11 (stage population).
#[test]
fn activating_a_childless_prim_recomposes_its_parent_child_list() {
    let mut store = scene();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let parent = store.path("/P");
    for (path, active) in [("/P/C", false), ("/P/D", true), ("/P/C", true)] {
        let prim = store.path(path);
        edit_spec(&mut store, ROOT, path, |spec| spec.active = Some(active));
        live.notify_prim_edit(prim);
        let changes = live.recompose_changes(&mut store);
        let context = format!("{path} active = {active}");
        assert_as_fresh(&live, &mut store, &context);
        assert!(
            changes.is_resynced(prim, &store.paths),
            "{context}: {changes:?}"
        );
    }
    assert_eq!(all_children(&live, &store, parent), ["A", "B", "C", "D"]);
}

/// An order authored in a referenced layer that moves an inactive child,
/// notified at its source site, recomposes the referencing prim's child
/// list.
///
/// Spec: AOUSD Core §10.3.2.2 (references), §11 (child order).
#[test]
fn an_order_in_a_referenced_layer_moving_inactive_children_recomposes() {
    let mut store = scene();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let r = store.path("/R");
    let asset = store.path("/Asset");
    let z = store.tokens.intern("Z");
    let x = store.tokens.intern("X");
    edit_spec(&mut store, ASSET, "/Asset", |spec| {
        spec.rare_mut().prim_order = Some(vec![z, x]);
    });
    live.notify_layer_prim_edits(ASSET, &[asset]);
    let changes = live.recompose_changes(&mut store);
    assert_as_fresh(&live, &mut store, "referenced order");
    assert_eq!(all_children(&live, &store, r), ["Z", "X", "Y"]);
    assert!(changes.is_resynced(r, &store.paths), "{changes:?}");
}

/// An opinion that changes no child of a prim with inactive children
/// replaces its index alone: the masked composition composes the inactive
/// children too, so it tells them from active ones, and finds the
/// hierarchy unchanged, where listing them as active would resync the
/// prim's subtree. A notified edit carries no fields, so the prim is
/// reported resynced; a transaction reports it info-only, as the
/// conformance replay checks against OpenUSD.
#[test]
fn an_opinion_beside_inactive_children_is_info_only() {
    let mut store = scene();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    for (layer, path, prim) in [(ROOT, "/P", "/P"), (ASSET, "/Asset", "/R")] {
        let site = store.path(path);
        let prim = store.path(prim);
        let documentation = store.tokens.intern("documentation");
        edit_spec(&mut store, layer, path, |spec| {
            spec.set_field(documentation, crate::Value::string("noted"));
        });
        live.notify_layer_prim_edits(layer, &[site]);
        let changes = live.recompose_changes(&mut store);
        assert_as_fresh(&live, &mut store, path);
        assert_eq!(changes.resynced, [prim], "{path}");
        let work = live.recomposition_work();
        assert_eq!(work.replaced_prim_indexes, 1, "{path}: {work:?}");
    }
}
