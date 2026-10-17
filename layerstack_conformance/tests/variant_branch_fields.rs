// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `active` and `instanceable` authored on a selected variant branch are
//! opinions of the prim hosting the variant set, as OpenUSD 26.08 composes
//! them: `/V`'s branch deactivates it, and `/I`'s branch makes it an
//! instance of its branch's reference.
//!
//! Spec: AOUSD Core §7.6.7 (variant specs contribute prim spec fields),
//! §11.3 (active, instanceable).

use layerstack::{Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};

#[test]
fn variant_branches_author_active_and_instanceable() {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/variant_branch_fields/scene.usda");
    let mut loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let store = &mut loaded.store;
    let (v, child, i) = (store.path("/V"), store.path("/V/Child"), store.path("/I"));
    assert!(stage.has_prim(v));
    assert!(!stage.is_active(v));
    assert!(
        !stage.has_prim(child),
        "an inactive prim's children are not composed"
    );
    assert!(stage.is_instance(i));
}
