// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transforms, visibility and purpose against OpenUSD.
//!
//! `fixtures/xform/scene.usda` exercises every transform op type and
//! Euler order in double, float and half precision, suffixed ops, pivots
//! and inverse ops, cancelling inverse pairs, ops naming no attribute or
//! no value, `!resetXformStack!` at different depths and after other ops,
//! non-`Xformable` prims between `Xformable` ones, and time-sampled ops;
//! and visibility, purpose visibility (`VisibilityAPI`) and purpose
//! authored at different depths, on non-`Imageable` prims, and through a
//! reference (`asset.usda`). `scripts/xform_oracle.py` records what
//! OpenUSD 26.08 computes for every prim, at the default time and at time
//! codes with linear and held interpolation, in `oracle.json`.
//!
//! Every value must agree: local transforms (and whether they reset the
//! transform stack) and local-to-world transforms through one shared
//! [`XformCache`] per time and through the views, visibility, effective
//! visibility for each purpose, and purpose info.
//!
//! Matrices agree to within `matrices::TOLERANCE` times the largest entry of
//! OpenUSD's (at least 1). Both compute each op and product as `Gf` does,
//! step for step; they differ only where the platform's `sin`, `cos` and
//! `acos` round differently from `libm`'s and where the C++ compiler fuses
//! multiply-adds, a few units in the last place per step. A wrong op, order
//! or sign differs by far more.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{PathId, Stage, StageOptions};
use layerstack_conformance::matrices::{TOLERANCE, relative_error};
use layerstack_conformance::usda_real::load_entry_usda;
use layerstack_conformance::workspace_root;
use layerstack_schemas::usd_geom::{Imageable, ImageablePurpose, Xformable};
use layerstack_schemas::{Scene, Time, XformCache};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/xform/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    prims: BTreeMap<String, PrimRecord>,
}

#[derive(Deserialize)]
struct PrimRecord {
    purpose: Option<PurposeRecord>,
    samples: BTreeMap<String, Sample>,
}

#[derive(Deserialize)]
struct PurposeRecord {
    purpose: String,
    inheritable: bool,
    authored_on: Option<String>,
}

#[derive(Deserialize)]
struct Sample {
    local: Option<[[f64; 4]; 4]>,
    resets_xform_stack: Option<bool>,
    world: [[f64; 4]; 4],
    visibility: Option<String>,
    effective_visibility: Option<BTreeMap<String, String>>,
}

fn time(key: &str) -> Time {
    match key.split_once('@') {
        None => Time::Default,
        Some(("linear", code)) => Time::at(code.parse().expect("a time code")),
        Some(("held", code)) => Time::held(code.parse().expect("a time code")),
        Some(_) => panic!("no time {key}"),
    }
}

#[test]
fn transforms_visibility_and_purpose_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("the oracle parses");
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let mut loaded =
        load_entry_usda(&workspace_root().join("layerstack_conformance/fixtures/xform/scene.usda"));
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let paths: BTreeMap<String, PathId> = oracle
        .prims
        .keys()
        .map(|p| (p.clone(), loaded.store.path(p)))
        .collect();
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);

    let root = store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("the pseudo-root");
    let ours: Vec<String> = stage
        .traverse(root)
        .skip(1)
        .map(|p| store.paths.display(p, &store.tokens))
        .collect();
    let theirs: Vec<&String> = oracle.prims.keys().collect();
    let mut sorted = ours.clone();
    sorted.sort();
    assert_eq!(sorted.iter().collect::<Vec<_>>(), theirs, "the same prims");

    let mut failures = Vec::new();
    let mut worst: f64 = 0.0;
    let mut checks = 0_usize;
    let keys: Vec<&String> = oracle
        .prims
        .values()
        .next()
        .expect("prims")
        .samples
        .keys()
        .collect();
    for key in keys {
        let time = time(key);
        let mut cache = XformCache::new(time);
        for (text, record) in &oracle.prims {
            let path = paths[text];
            let sample = &record.samples[key];
            let context = format!("{text} {key}");

            let xformable = Xformable::new(&scene, path);
            match (&sample.local, &xformable) {
                (Some(expected), Some(xformable)) => {
                    let local = xformable.local_transform(time);
                    let e = relative_error(&local.matrix, expected);
                    worst = worst.max(e);
                    checks += 1;
                    if e > TOLERANCE {
                        failures.push(format!(
                            "{context}: local {:?}, OpenUSD {expected:?}",
                            local.matrix
                        ));
                    }
                    if Some(local.resets_xform_stack) != sample.resets_xform_stack {
                        failures.push(format!("{context}: resets {}", local.resets_xform_stack));
                    }
                    let cached = cache.local_transform(&scene, path).expect("on the stage");
                    if cached != &local {
                        failures.push(format!("{context}: the cache's local transform differs"));
                    }
                }
                (None, None) => {}
                _ => failures.push(format!("{context}: Xformable differs")),
            }

            let world = cache.local_to_world(&scene, path).expect("on the stage");
            let e = relative_error(&world, &sample.world);
            worst = worst.max(e);
            checks += 1;
            if e > TOLERANCE {
                failures.push(format!(
                    "{context}: world {world:?}, OpenUSD {:?}",
                    sample.world
                ));
            }

            let imageable = Imageable::new(&scene, path);
            match (&sample.visibility, &imageable) {
                (Some(expected), Some(imageable)) => {
                    if imageable.compute_local_to_world(time) != world {
                        failures.push(format!("{context}: the view's world transform differs"));
                    }
                    let visibility = imageable.compute_visibility(time);
                    checks += 1;
                    if visibility.as_str() != expected {
                        failures.push(format!(
                            "{context}: visibility {visibility:?}, OpenUSD {expected}"
                        ));
                    }
                    for (purpose, expected) in
                        sample.effective_visibility.as_ref().expect("recorded")
                    {
                        let got = imageable.compute_effective_visibility(
                            &ImageablePurpose::from_token(purpose),
                            time,
                        );
                        checks += 1;
                        if got.as_str() != expected {
                            failures.push(format!(
                                "{context}: {purpose} visibility {got:?}, OpenUSD {expected}"
                            ));
                        }
                    }
                }
                (None, None) => {}
                _ => failures.push(format!("{context}: Imageable differs")),
            }
        }
        let stats = cache.stats();
        assert_eq!(
            stats.world_computed,
            oracle.prims.len(),
            "each world transform once at {key}"
        );
    }

    for (text, record) in &oracle.prims {
        let Some(expected) = &record.purpose else {
            continue;
        };
        let imageable = Imageable::new(&scene, paths[text]).expect("imageable");
        let info = imageable.compute_purpose_info();
        checks += 1;
        if info.purpose.as_str() != expected.purpose || info.inheritable != expected.inheritable {
            failures.push(format!(
                "{text}: purpose {info:?}, OpenUSD {} (inheritable {})",
                expected.purpose, expected.inheritable
            ));
        }
        let authored_on = info
            .authored_on
            .map(|p| store.paths.display(p, &store.tokens));
        if authored_on != expected.authored_on {
            failures.push(format!(
                "{text}: purpose authored on {authored_on:?}, OpenUSD {:?}",
                expected.authored_on
            ));
        }
        if info.inheritable != info.authored_on.is_some() {
            failures.push(format!(
                "{text}: an inheritable purpose names where it is authored"
            ));
        }
    }

    eprintln!(
        "{checks} checks against OpenUSD {}; largest matrix difference {worst:e} (relative)",
        oracle.openusd_version
    );
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Ops that cannot contribute as written are evaluated as OpenUSD
/// evaluates them and reported, each with why.
#[test]
fn problem_ops_are_reported() {
    use layerstack_schemas::{XformProblem, XformProblemKind as K};

    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/xform/problems.usda"),
    );
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let path = loaded.store.path("/Problems");
    let scene = Scene::new(&stage, &loaded.store);
    let xformable = Xformable::new(&scene, path).expect("an Xform");

    let ordered = xformable.ordered_xform_ops();
    let names: Vec<&str> = ordered.ops.iter().map(|op| op.name).collect();
    assert_eq!(
        names,
        [
            "xformOp:translate:unset",
            "xformOp:bogus",
            "xformOp:scale:scalar",
            "!invert!xformOp:transform:flat",
            "xformOp:translate"
        ]
    );

    let local = xformable.local_transform(Time::Default);
    let problem = |op: &str, kind| XformProblem {
        op: op.into(),
        kind,
    };
    assert_eq!(
        local.problems,
        [
            problem("xformOp:translate:absent", K::NoAttribute),
            problem("xformOp:rotateX:relationship", K::NoAttribute),
            problem("!invert!xformOp:transform:flat", K::Singular),
            problem("xformOp:scale:scalar", K::ValueType),
            problem("xformOp:bogus", K::UnknownOpType),
            problem("xformOp:translate:unset", K::NoValue),
        ]
    );
    // The singular inverse is OpenUSD's, a scale by `f32::MAX`, applied
    // after the translation.
    let max = f64::from(f32::MAX);
    assert_eq!(
        local.matrix,
        [
            [max, 0.0, 0.0, 0.0],
            [0.0, max, 0.0, 0.0],
            [0.0, 0.0, max, 0.0],
            [max, 2.0 * max, 3.0 * max, 1.0],
        ]
    );
}

/// After an edit, invalidating the edited prim recomputes it and its
/// descendants alone, and the cache agrees with a fresh one.
#[test]
fn an_invalidated_cache_recomputes_the_edited_subtree() {
    use layerstack::edit::{EditTarget, Transaction};
    use layerstack::{LiveStage, PropertyPath, Value};

    let mut loaded =
        load_entry_usda(&workspace_root().join("layerstack_conformance/fixtures/xform/scene.usda"));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options);
    let root = loaded
        .store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("the pseudo-root");
    let hierarchy = loaded.store.path("/Hierarchy");
    let translate = loaded.store.tokens.intern("xformOp:translate");
    let all: Vec<PathId> = live.stage().traverse(root).skip(1).collect();
    let under: Vec<PathId> = live.stage().traverse(hierarchy).collect();

    let mut cache = XformCache::new(Time::Default);
    {
        let scene = Scene::new(live.stage(), &loaded.store);
        for path in &all {
            cache.local_to_world(&scene, *path);
        }
    }
    let before = cache.stats();

    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(loaded.root_layer).property(PropertyPath::new(hierarchy, translate)),
        Value::Vec3d([0.0, -5.0, 2.0]),
    );
    live.apply(&mut loaded.store, &edit).expect("applies");

    let scene = Scene::new(live.stage(), &loaded.store);
    cache.invalidate(&scene, hierarchy);
    let mut fresh = XformCache::new(Time::Default);
    for path in &all {
        assert_eq!(
            cache.local_to_world(&scene, *path),
            fresh.local_to_world(&scene, *path),
            "{}",
            loaded.store.paths.display(*path, &loaded.store.tokens)
        );
    }
    let after = cache.stats();
    assert_eq!(
        after.world_computed - before.world_computed,
        under.len(),
        "the subtree alone"
    );
}

/// The computations are pure steps over each prim's inputs and its
/// parent's result, which any caller (an incremental graph among them) can
/// fold down namespace itself; the inputs name the properties they read.
#[test]
fn pure_steps_fold_to_what_the_views_compute() {
    use layerstack_schemas::{
        LocalTransformInputs, PurposeInfo, PurposeInputs, Visibility, VisibilityInputs,
    };

    let mut loaded =
        load_entry_usda(&workspace_root().join("layerstack_conformance/fixtures/xform/scene.usda"));
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(
            &mut loaded.store.tokens,
        ))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let pivot = loaded.store.path("/Ops/Pivot");
    let root = loaded
        .store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("the pseudo-root");
    let store = &loaded.store;
    let scene = Scene::new(&stage, store);

    let inputs = LocalTransformInputs::read(&scene, pivot, Time::Default);
    assert_eq!(
        inputs.properties().collect::<Vec<_>>(),
        [
            "xformOpOrder",
            "xformOp:translate",
            "xformOp:translate:pivot",
            "xformOp:rotateXYZ",
            "xformOp:scale",
            "xformOp:translate:pivot"
        ]
    );

    // Fold each step from the root down, as a graph would, one node per
    // prim, and compare with the views.
    let time = Time::held(1.5);
    let mut world = std::collections::HashMap::new();
    let mut visibility = std::collections::HashMap::new();
    let mut guide = std::collections::HashMap::new();
    let mut purpose: std::collections::HashMap<PathId, PurposeInfo> =
        std::collections::HashMap::new();
    for path in stage.traverse(root).skip(1) {
        let parent = store
            .paths
            .lookup(&store.paths.resolve(path).parent().expect("a parent"))
            .expect("interned");
        let parent_world = world.get(&parent).copied().unwrap_or([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]);
        let local = LocalTransformInputs::read(&scene, path, time).evaluate();
        world.insert(path, local.local_to_world(&parent_world));
        let own = VisibilityInputs::read(&scene, path, time);
        let parent_visibility = visibility
            .get(&parent)
            .copied()
            .unwrap_or(Visibility::Inherited);
        visibility.insert(path, Visibility::inherit(parent_visibility, &own));
        let parent_guide = guide.get(&parent).copied().flatten();
        guide.insert(
            path,
            own.purpose_visibility(&ImageablePurpose::Guide, parent_guide),
        );
        let info = PurposeInfo::inherit(
            purpose.get(&parent),
            &PurposeInputs::read(&scene, path),
            path,
        );
        purpose.insert(path, info);

        if let Some(imageable) = Imageable::new(&scene, path) {
            assert_eq!(imageable.compute_local_to_world(time), world[&path]);
            assert_eq!(imageable.compute_visibility(time), visibility[&path]);
            assert_eq!(
                imageable.compute_effective_visibility(&ImageablePurpose::Guide, time),
                Visibility::effective(visibility[&path], &ImageablePurpose::Guide, guide[&path])
            );
            assert_eq!(imageable.compute_purpose_info(), purpose[&path]);
        }
    }
}
