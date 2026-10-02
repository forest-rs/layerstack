// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point-instance masks, motion, prototype transforms and bounds match C++.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{LayerId, StageOptions, Time, edit::EditTarget};
use layerstack_schemas::{
    Scene, SchemaEdit,
    bounds::{BoundsCache, BoundsOptions},
    point_instancer::{InstanceTransformOptions, PointInstancerError},
    retained::{Query, QueryAnswer, QuerySession},
    usd_geom::{Cube, PointInstancer},
};
use std::sync::Arc;
const SOURCE: &str = include_str!("../fixtures/point_instancer_behavior.usda");
fn matrices(
    actual: &[layerstack_schemas::point_instancer::InstanceTransform],
    expected: &serde_json::Value,
) {
    assert_eq!(
        actual.len(),
        expected.as_array().unwrap().len(),
        "surviving instance count"
    );
    for (instance, expected) in actual.iter().zip(expected.as_array().unwrap()) {
        for (row, expected) in instance.matrix.iter().zip(expected.as_array().unwrap()) {
            for (&value, expected) in row.iter().zip(expected.as_array().unwrap()) {
                assert!(
                    (value - expected.as_f64().unwrap()).abs() < 1e-8,
                    "{value} != {expected}"
                );
            }
        }
    }
}
#[test]
fn masks_motion_and_prototype_local_transforms_match_cpp() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/point_instancer_behavior.json")).unwrap();
    let (mut store, live) = support::scene(SOURCE);
    let path = store.path("/World/Instances");
    let implicit = store.path("/World/ImplicitIds");
    let interpolated = store.path("/World/Interpolated");
    let compacted = store.path("/World/Compacted");
    let misaligned = store.path("/World/Misaligned");
    let scene = Scene::new(live.stage(), &store);
    let instancer = PointInstancer::new(&scene, path).unwrap();
    assert_eq!(
        serde_json::json!(instancer.compute_mask(Time::Default)),
        expected["mask"]
    );
    for (label, time, base, include, mask) in [
        ("default", Time::Default, Time::Default, true, true),
        ("allDefault", Time::Default, Time::Default, true, false),
        ("motion", Time::at(2.), Time::at(0.), true, false),
        ("motionBase2", Time::at(2.), Time::at(2.), true, false),
        ("noPrototype", Time::at(2.), Time::at(0.), false, false),
    ] {
        let options = InstanceTransformOptions {
            include_prototype_transform: include,
            apply_mask: mask,
        };
        let actual = instancer
            .compute_instance_transforms(time, base, options)
            .unwrap();
        matrices(&actual, &expected[label]);
        for (position, instance) in actual.iter().enumerate() {
            assert_eq!(instance.index, position);
            assert_eq!(instance.id, 100 + i64::try_from(position).unwrap());
        }
    }
    let implicit = PointInstancer::new(&scene, implicit).unwrap();
    assert_eq!(
        serde_json::json!(implicit.compute_mask(Time::Default)),
        expected["implicitMask"]
    );
    let interpolated = PointInstancer::new(&scene, interpolated).unwrap();
    matrices(
        &interpolated
            .compute_instance_transforms(
                Time::at(2.),
                Time::at(0.),
                InstanceTransformOptions::default(),
            )
            .unwrap(),
        &expected["interpolated"],
    );
    let compacted = PointInstancer::new(&scene, compacted)
        .unwrap()
        .compute_instance_transforms(
            Time::Default,
            Time::Default,
            InstanceTransformOptions::default(),
        )
        .unwrap();
    matrices(&compacted, &expected["compacted"]);
    assert_eq!(compacted.len(), 1);
    assert_eq!((compacted[0].index, compacted[0].id), (2, 102));
    matrices(
        &PointInstancer::new(&scene, misaligned)
            .unwrap()
            .compute_instance_transforms(
                Time::at(2.),
                Time::at(0.),
                InstanceTransformOptions::default(),
            )
            .unwrap(),
        &expected["misaligned"],
    );
    assert_eq!(
        instancer
            .compute_instance_transforms(
                Time::at(1.),
                Time::Default,
                InstanceTransformOptions::default()
            )
            .unwrap_err(),
        PointInstancerError::InvalidTime
    );
}
#[test]
fn computed_bounds_match_cpp_and_follow_external_prototype_edits() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/point_instancer_behavior.json")).unwrap();
    let (mut store, mut live) = support::scene(SOURCE);
    let path = store.path("/World/Instances");
    let prototype = store.path("/World/Prototype");
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    for (label, time) in [
        ("boundsDefault", Time::Default),
        ("boundsAt2", Time::at(2.)),
    ] {
        cache.set_time(time);
        let bounds = cache
            .world_bound(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .aligned_range();
        assert_eq!(serde_json::json!([bounds.min, bounds.max]), expected[label]);
    }
    let cube = Cube::new(&Scene::new(live.stage(), &store), prototype)
        .unwrap()
        .edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    cube.set_size(&mut edit, 4.);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    let changed = cache.world_bound(&scene, path).unwrap();
    let mut fresh = BoundsCache::new(Time::at(2.), BoundsOptions::default());
    assert_eq!(changed, fresh.world_bound(&scene, path).unwrap());
    assert_ne!(
        serde_json::json!([changed.aligned_range().min, changed.aligned_range().max]),
        expected["boundsAt2"]
    );
}
#[test]
fn retained_bounds_route_prototype_changes_outside_the_query_namespace() {
    let (mut store, _) = support::scene(SOURCE);
    let path = store.path("/World/Instances");
    let prototype = store.path("/World/Prototype");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut session = QuerySession::new(
        store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
        Time::Default,
        BoundsOptions::default(),
    );
    let query = session.observe(Query::WorldBound(path));
    assert!(matches!(
        session.poll(query).unwrap().answer,
        QueryAnswer::WorldBound(Ok(_))
    ));
    assert!(!session.poll(query).unwrap().evaluated);
    let cube = Cube::new(&session.scene(), prototype).unwrap().edit();
    let mut edit = session.edit(EditTarget::for_layer(LayerId(1)));
    cube.set_size(&mut edit, 6.);
    let transaction = edit.finish();
    session.apply(&transaction).unwrap();
    let result = session.poll(query).unwrap();
    assert!(result.evaluated && result.answer_changed);
    assert!(!result.causes.is_empty());
}
#[test]
fn invalid_topology_and_prototype_cycles_fail_explicitly() {
    for (text, expected) in [
        (
            "#usda 1.0\ndef PointInstancer \"I\" {\nint[] protoIndices = [0]\npoint3f[] positions = []\n}",
            PointInstancerError::LengthMismatch("positions"),
        ),
        (
            "#usda 1.0\ndef PointInstancer \"I\" {\nint[] protoIndices = [-1]\npoint3f[] positions = [(0,0,0)]\n}",
            PointInstancerError::InvalidPrototypeIndex {
                instance: 0,
                index: -1,
            },
        ),
    ] {
        let (mut store, live) = support::scene(text);
        let path = store.path("/I");
        let view = PointInstancer::new(&Scene::new(live.stage(), &store), path).unwrap();
        let options = InstanceTransformOptions {
            include_prototype_transform: false,
            apply_mask: false,
        };
        assert_eq!(
            view.compute_instance_transforms(Time::Default, Time::Default, options)
                .unwrap_err(),
            expected
        );
    }
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef PointInstancer \"I\" {\nrel prototypes = </I>\nint[] protoIndices = [0]\npoint3f[] positions = [(0,0,0)]\n}",
    );
    let path = store.path("/I");
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(
        cache.world_bound(&Scene::new(live.stage(), &store), path),
        Err(layerstack_schemas::bounds::BoundsError::PointInstancerCycle(path))
    );
}

#[test]
fn failed_retained_bounds_recover_when_an_external_prototype_is_created() {
    let text = "#usda 1.0\ndef PointInstancer \"I\" {\nrel prototypes = </Missing>\nint[] protoIndices = [0]\npoint3f[] positions = [(0,0,0)]\n}";
    let (mut store, _) = support::scene(text);
    let path = store.path("/I");
    let missing = store.path("/Missing");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut session = QuerySession::new(
        store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
        Time::Default,
        BoundsOptions::default(),
    );
    let query = session.observe(Query::WorldBound(path));
    assert!(matches!(
        session.poll(query).unwrap().answer,
        QueryAnswer::WorldBound(Err(_))
    ));
    assert_eq!(
        session.bound_prototype_dependencies(query),
        Some(vec![missing])
    );
    let mut edit = session.edit(EditTarget::for_layer(LayerId(1)));
    Cube::define(&mut edit, missing);
    let transaction = edit.finish();
    session.apply(&transaction).unwrap();
    let result = session.poll(query).unwrap();
    assert!(result.evaluated && result.answer_changed);
    assert!(matches!(result.answer, QueryAnswer::WorldBound(Ok(_))));
}

#[test]
fn nested_prototype_dependencies_route_edits_transitively() {
    let text = r#"#usda 1.0
def PointInstancer "I" {
    rel prototypes = </P/Inner>
    int[] protoIndices = [0]
    point3f[] positions = [(2,0,0)]
}
def Scope "P" {
    def PointInstancer "Inner" {
        rel prototypes = </Cube>
        int[] protoIndices = [0]
        point3f[] positions = [(3,0,0)]
    }
}
def Cube "Cube" {}
"#;
    let (mut store, _) = support::scene(text);
    let path = store.path("/I");
    let inner = store.path("/P/Inner");
    let cube = store.path("/Cube");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut session = QuerySession::new(
        store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
        Time::Default,
        BoundsOptions::default(),
    );
    let query = session.observe(Query::WorldBound(path));
    let initial = session.poll(query).unwrap().answer;
    assert!(matches!(initial, QueryAnswer::WorldBound(Ok(_))));
    let mut deps = session.bound_prototype_dependencies(query).unwrap();
    deps.sort_unstable();
    let mut expected = vec![inner, cube];
    expected.sort_unstable();
    assert_eq!(deps, expected);
    let cube = Cube::new(&session.scene(), cube).unwrap().edit();
    let mut edit = session.edit(EditTarget::for_layer(LayerId(1)));
    cube.set_size(&mut edit, 6.);
    let transaction = edit.finish();
    session.apply(&transaction).unwrap();
    let changed = session.poll(query).unwrap();
    assert!(changed.evaluated && changed.answer_changed);
    let QueryAnswer::WorldBound(Ok(bounds)) = changed.answer else {
        panic!("valid nested bounds")
    };
    assert_eq!(bounds.aligned_range().min, [2., -3., -3.]);
    assert_eq!(bounds.aligned_range().max, [8., 3., 3.]);
}

#[test]
fn computed_extents_reject_unsupported_policies_but_authored_extents_work() {
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef PointInstancer \"I\" {}\ndef PointInstancer \"Authored\" {\nfloat3[] extent = [(-1,-1,-1), (1,1,1)]\n}",
    );
    let path = store.path("/I");
    let authored = store.path("/Authored");
    let scene = Scene::new(live.stage(), &store);
    for (ignore_visibility, use_extents_hint) in [(true, false), (false, true)] {
        let mut cache = BoundsCache::new(
            Time::Default,
            BoundsOptions {
                ignore_visibility,
                use_extents_hint,
                ..BoundsOptions::default()
            },
        );
        assert_eq!(
            cache.world_bound(&scene, path),
            Err(layerstack_schemas::bounds::BoundsError::UnsupportedInstancerPolicy(path))
        );
        assert_eq!(
            cache
                .world_bound(&scene, authored)
                .unwrap()
                .aligned_range()
                .min,
            [-1.; 3]
        );
    }
}
