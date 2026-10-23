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
//!
//! C++ binds every skinnable prim bound to an active skeleton: with valid
//! influences or not, with blend shapes or not, skipping an inbetween at
//! weight 1 and dropping mismatched influence interpolation with warnings.
//! `SkelRoot::bindings` lists the same prims, and reports what it left out
//! on each binding.

#![allow(missing_docs, reason = "integration tests")]

#[path = "support/schema_scene.rs"]
mod support;

use std::collections::BTreeMap;

use layerstack::{EditTarget, LayerId, ListOp, TargetPath, Time, Transaction};
use layerstack_schemas::{
    Scene,
    skel::{BlendShapeCache, SkelProblemReason},
    usd_skel::SkelRoot,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/skel_discovery/oracle.json");
const SCENE: &str = include_str!("../fixtures/skel_discovery/scene.usda");

#[derive(Debug, Deserialize)]
struct Target {
    path: String,
    skeleton: String,
    joint_influences: bool,
    blend_shapes: bool,
    blend_points: Option<Vec<[f32; 3]>>,
    skinned_points: Option<Vec<[f32; 3]>>,
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

#[test]
fn skel_bindings_match_openusd_and_report_problems() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    let text = |path| store.paths.display(path, &store.tokens).to_string();
    let mut shapes = BlendShapeCache::new(Time::Default);
    for (root, expected) in &oracle.roots {
        let found = SkelRoot::new(&scene, store.path(root)).unwrap().bindings();
        assert!(found.problems.is_empty(), "{root}: {:?}", found.problems);
        let mut bindings: Vec<_> = found.bindings.iter().collect();
        bindings.sort_by_key(|binding| text(binding.geometry));
        assert_eq!(bindings.len(), expected.len(), "{root}");
        for (binding, target) in bindings.into_iter().zip(expected) {
            let path = &target.path;
            assert_eq!(text(binding.geometry), *path);
            assert_eq!(text(binding.skeleton), target.skeleton, "{path}");
            assert_eq!(
                binding.skinning.is_some(),
                target.joint_influences,
                "{path}"
            );
            assert_eq!(
                binding.blend_shapes.is_some(),
                target.blend_shapes,
                "{path}"
            );
            let skinned = binding
                .skinning
                .as_ref()
                .map(|query| query.compute_skinned_points(Time::Default).unwrap());
            assert_eq!(skinned, target.skinned_points, "{path}");
            let blended = binding.blend_shapes.as_ref().map(|_| {
                shapes
                    .deformed_points(&scene, binding.geometry)
                    .unwrap()
                    .unwrap()
                    .to_vec()
            });
            assert_eq!(blended, target.blend_points, "{path}");
            // Each problem C++ warns about stays with its binding.
            let reasons: Vec<_> = binding
                .problems
                .iter()
                .map(|problem| {
                    let property = problem
                        .property
                        .map(|p| p.display(&store.paths, &store.tokens).to_string());
                    match &problem.reason {
                        SkelProblemReason::InvalidInbetweenWeight(weight) => {
                            format!("inbetween {weight} {property:?}")
                        }
                        SkelProblemReason::JointInfluences(_) => format!("influences {property:?}"),
                        other => format!("{other:?}"),
                    }
                })
                .collect();
            let expected: &[&str] = match path.as_str() {
                "/Root/BadInbetween" => &[r#"inbetween 1 Some("/Root/Side.inbetweens:bad")"#],
                "/Root/Mismatched" => {
                    &[r#"influences Some("/Root/Mismatched.primvars:skel:jointWeights")"#]
                }
                "/Root3/HalfBound" => {
                    &[r#"influences Some("/Root3/HalfBound.primvars:skel:jointWeights")"#]
                }
                _ => &[],
            };
            assert_eq!(reasons, expected, "{path}");
        }
    }
}

/// A `skel:skeleton` that targets something other than a skeleton leaves
/// that prim unbound and reported, and its siblings bound.
#[test]
fn an_invalid_skeleton_target_is_reported_beside_the_other_bindings() {
    let source = SCENE.replace(
        "rel skel:skeleton = </Root3/Skel>\n    }\n\n    def Mesh \"HalfBound\"",
        "rel skel:skeleton = </Root3>\n    }\n\n    def Mesh \"HalfBound\"",
    );
    assert_ne!(source, SCENE);
    let (store, live) = support::scene(&source);
    let scene = Scene::new(live.stage(), &store);
    let found = SkelRoot::new(&scene, store.path("/Root3"))
        .unwrap()
        .bindings();
    let [problem] = found.problems.as_slice() else {
        panic!("{:?}", found.problems);
    };
    assert_eq!(problem.prim, store.path("/Root3/Plain"));
    assert!(matches!(
        problem.reason,
        SkelProblemReason::SkeletonTarget(_)
    ));
    assert_eq!(found.bindings.len(), 1, "HalfBound stays bound");
    assert_eq!(found.problems().count(), 2);
}
