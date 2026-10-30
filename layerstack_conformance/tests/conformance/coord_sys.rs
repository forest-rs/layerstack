// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Coordinate-system bindings with inheritance against OpenUSD.
//!
//! `scripts/coord_sys_oracle.py` records, for every prim of
//! `fixtures/coord_sys/scene.usda`, C++ `GetLocalBindingsForPrim`,
//! `FindBindingsWithInheritanceForPrim` and `FindBindingWithInheritance`
//! for each name. The scene overrides an inherited name, blocks one (which
//! is then inherited from further up), authors a relationship without its
//! applied instance, applies an instance without a relationship and binds
//! through a forwarding relationship. One `CoordSysCache` answers every
//! prim in traversal order and a fresh one answers each prim alone.

use crate::support::schema_scene as support;

use std::collections::BTreeMap;

use layerstack::{EditTarget, LayerId, ListOp, Transaction};
use layerstack_schemas::{CoordSysBinding, CoordSysCache, Scene, local_coord_sys_bindings};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/coord_sys/oracle.json");
const SCENE: &str = include_str!("../../fixtures/coord_sys/scene.usda");

#[derive(Debug, Deserialize, PartialEq)]
struct Binding {
    name: String,
    relationship: String,
    target: String,
}

#[derive(Deserialize)]
struct Prim {
    local: Vec<Binding>,
    inherited: Vec<Binding>,
    find: BTreeMap<String, Option<Binding>>,
}

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    prims: BTreeMap<String, Prim>,
}

fn record(store: &layerstack::InMemoryStore, binding: &CoordSysBinding) -> Binding {
    Binding {
        // C++ names a binding by its relationship's base name.
        name: binding.base_name(),
        relationship: binding
            .relationship
            .display(store.paths(), store.tokens())
            .to_string(),
        target: binding
            .target
            .display(store.paths(), store.tokens())
            .to_string(),
    }
}

#[test]
fn coord_sys_bindings_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    let mut shared = CoordSysCache::new();
    for (path, expected) in &oracle.prims {
        let prim = store.path(path);
        let local: Vec<_> = local_coord_sys_bindings(&scene, prim)
            .iter()
            .map(|b| record(&store, b))
            .collect();
        assert_eq!(local, expected.local, "{path} local");
        for cache in [&mut shared, &mut CoordSysCache::new()] {
            let inherited: Vec<_> = cache
                .bindings(&scene, prim)
                .iter()
                .map(|b| record(&store, b))
                .collect();
            assert_eq!(inherited, expected.inherited, "{path} inherited");
            for (name, found) in &expected.find {
                let binding = cache
                    .binding(&scene, prim, name)
                    .map(|b| record(&store, &b));
                assert_eq!(binding.as_ref(), found.as_ref(), "{path} {name}");
            }
        }
    }
    assert!(shared.stats().hits > 0, "ancestors are shared");
}

/// Blocking an ancestor's binding and invalidating that ancestor lets its
/// descendants inherit the name from further up.
#[test]
fn coord_sys_cache_follows_invalidation() {
    let (mut store, mut live) = support::scene(SCENE);
    let model = store.path("/World/Model");
    let paint = store.path("/World/Model/Paint");
    let leaf = store.path("/World/Model/Geom/Box");
    let mut cache = CoordSysCache::new();
    let found = cache
        .binding(&Scene::new(live.stage(), &store), leaf, "paintSpace")
        .map(|binding| binding.target.prim_path());
    assert_eq!(found, Some(paint));
    let mut transaction = Transaction::new();
    transaction.set_targets(
        EditTarget::for_layer(LayerId(1))
            .property(store.property_path("/World/Model.coordSys:paintSpace:binding")),
        ListOp::explicit(Vec::new()),
    );
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    assert!(
        cache.binding(&scene, leaf, "paintSpace").is_some(),
        "held until invalidated"
    );
    cache.invalidate(&scene, model);
    assert_eq!(cache.binding(&scene, leaf, "paintSpace"), None);
}
