// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Skel binding discovery and blend-shape weights against OpenUSD.
//!
//! `scripts/skel_discovery_oracle.py` records, for each `SkelRoot` of
//! `fixtures/skel_discovery/scene.usda`, every skinning target C++
//! `UsdSkelCache::ComputeSkelBindings` returns, whether it has joint
//! influences and blend shapes, and its blend-shaped and skinned points at
//! default time. Blend-shape weights come from the bound skeleton's
//! animation, wherever `skel:animationSource` is authored.

#![allow(missing_docs, reason = "integration tests")]

#[path = "support/schema_scene.rs"]
mod support;

use std::collections::BTreeMap;

use layerstack::{EditTarget, LayerId, ListOp, TargetPath, Time, Transaction};
use layerstack_schemas::{Scene, skel::BlendShapeCache};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/skel_discovery/oracle.json");
const SCENE: &str = include_str!("../fixtures/skel_discovery/scene.usda");

#[derive(Debug, Deserialize)]
struct Target {
    path: String,
    joint_influences: bool,
    blend_points: Option<Vec<[f32; 3]>>,
}

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    roots: BTreeMap<String, Vec<Target>>,
}

/// A morph-only mesh bound through `skel:skeleton` blends with its
/// skeleton's animation weights: `(0,1,0)` in C++, not the rest point.
#[test]
fn blend_shape_weights_come_from_the_bound_skeleton() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    let mut cache = BlendShapeCache::new(Time::Default);
    let mut checked = 0;
    for target in oracle.roots.values().flatten() {
        let Some(expected) = &target.blend_points else {
            continue;
        };
        if target.joint_influences {
            continue;
        }
        let points = cache
            .deformed_points(&scene, store.path(&target.path))
            .unwrap()
            .unwrap();
        assert_eq!(points, expected.as_slice(), "{}", target.path);
        checked += 1;
    }
    assert_eq!(checked, 2);
}

/// Rebinding the skeleton's animation reaches a retained cache.
#[test]
fn blend_shape_weights_follow_the_skeleton_animation_source() {
    let (mut store, mut live) = support::scene(SCENE);
    let mesh = store.path("/Root/MorphOnly");
    let mut cache = BlendShapeCache::new(Time::Default);
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        cache.deformed_points(&scene, mesh).unwrap().unwrap(),
        [[0., 1., 0.]]
    );
    let mut transaction = Transaction::new();
    transaction.set_targets(
        EditTarget::for_layer(LayerId(1))
            .property(store.property_path("/Root/Skel.skel:animationSource")),
        ListOp::explicit(Vec::<TargetPath>::new()),
    );
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    assert_eq!(
        cache.deformed_points(&scene, mesh).unwrap().unwrap(),
        [[0., 0., 0.]],
        "no animation, no weights"
    );
}
