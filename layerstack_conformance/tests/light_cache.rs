// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Engine lighting capture and recovery through public APIs.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{EditTarget, InterpolationType, LayerId, PropertyPath, Transaction, Value};
use layerstack_schemas::{
    Scene, Time,
    light::{LightCache, LightListMode, LightShape},
};
fn set(
    store: &mut layerstack::InMemoryStore,
    path: layerstack::PathId,
    name: &str,
    value: Value,
) -> Transaction {
    let token = store.tokens.intern(name);
    let mut t = Transaction::new();
    t.set_default(
        EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(path, token)),
        value,
    );
    t
}
#[test]
fn capture_and_discovery_reuse_unrelated_edits_with_component_upload_stamps() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n float inputs:intensity = 1\n}\ndef Scope \"Other\" { float custom = 1 }\n",
    );
    let path = store.path("/Light");
    let root = store.path("/");
    let other = store.path("/Other");
    let mut cache = LightCache::new(Time::Default, &[]);
    cache
        .discover(
            &Scene::new(live.stage(), &store),
            root,
            LightListMode::IgnoreCache,
        )
        .unwrap();
    let first = cache
        .capture(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .revisions;
    let t = set(&mut store, other, "custom", Value::Float(2.));
    let applied = live.apply(&mut store, &t).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    assert!(
        !cache
            .capture(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .evaluated
    );
    let t = set(&mut store, path, "inputs:intensity", Value::Float(12.));
    let applied = live.apply(&mut store, &t).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    let sample = cache
        .capture(&Scene::new(live.stage(), &store), path)
        .unwrap();
    assert!(sample.evaluated);
    assert_eq!(sample.inputs.photometry().unwrap().intensity, 12.);
    assert_ne!(sample.revisions.parameters, first.parameters);
    assert_eq!(sample.revisions.transform, first.transform);
    assert_eq!(sample.revisions.shader, first.shader);
    assert_eq!(sample.revisions.relationships, first.relationships);
    cache
        .discover(
            &Scene::new(live.stage(), &store),
            root,
            LightListMode::IgnoreCache,
        )
        .unwrap();
    assert_eq!(cache.stats().discovery_hits, 1);
    assert_eq!(cache.memory().lights, 1);
    assert!(cache.memory().inputs > 0);
    live.apply(&mut store, &applied.inverse)
        .map(|a| cache.apply_changes(&Scene::new(live.stage(), &store), &a.changes))
        .unwrap();
    assert_ne!(
        cache
            .capture(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .inputs
            .photometry()
            .unwrap()
            .intensity,
        12.
    );
}
#[test]
fn retained_time_preserves_static_components_and_refreshes_animated_constants() {
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Animated\" {\n float inputs:intensity.timeSamples = {1: 1, 3: 3}\n}\ndef SphereLight \"Static\" {}\n",
    );
    let animated = store.path("/Animated");
    let fixed = store.path("/Static");
    let scene = Scene::new(live.stage(), &store);
    let time = |code| Time::At {
        code,
        interpolation: InterpolationType::Linear,
    };
    let mut cache = LightCache::new(time(1.), &[]);
    let old = cache.capture(&scene, animated).unwrap().revisions;
    let fixed_old = cache.capture(&scene, fixed).unwrap().revisions;
    cache.set_time(time(2.));
    let fixed_now = cache.capture(&scene, fixed).unwrap();
    assert!(!fixed_now.evaluated);
    assert_eq!(fixed_now.inputs.time, time(2.));
    assert_eq!(fixed_now.revisions, fixed_old);
    let current = cache.capture(&scene, animated).unwrap();
    assert_eq!(current.inputs.photometry().unwrap().intensity, 2.);
    assert_ne!(current.revisions.parameters, old.parameters);
    assert_eq!(current.revisions.transform, old.transform);
}
#[test]
fn provider_edits_and_path_recreation_refresh_retained_data() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n float inputs:intensity.connect = </Interface.inputs:gain>\n}\ndef NodeGraph \"Interface\" {\n float inputs:gain = 2\n}\n",
    );
    let path = store.path("/Light");
    let source = store.path("/Interface");
    let mut cache = LightCache::new(Time::Default, &[]);
    let old = cache
        .capture(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .revisions;
    let t = set(&mut store, source, "inputs:gain", Value::Float(5.));
    let applied = live.apply(&mut store, &t).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    let next = cache
        .capture(&Scene::new(live.stage(), &store), path)
        .unwrap();
    assert_eq!(next.inputs.float("intensity").unwrap(), 5.);
    assert_ne!(next.revisions.parameters, old.parameters);
    let mut t = Transaction::new();
    t.remove_spec(EditTarget::for_layer(LayerId(1)).prim(path));
    let applied = live.apply(&mut store, &t).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    assert!(
        cache
            .capture(&Scene::new(live.stage(), &store), path)
            .is_err()
    );
    assert_eq!(cache.memory().lights, 0);
    let restored = live.apply(&mut store, &applied.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &restored.changes);
    assert!(
        cache
            .capture(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .revisions
            .parameters
            > old.parameters
    );
}
#[test]
fn missing_external_filter_repair_and_removal_refresh_diagnostics() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n rel light:filters = </Filter>\n}\n",
    );
    let light = store.path("/Light");
    let filter = store.path("/Filter");
    let mut cache = LightCache::new(Time::Default, &[]);
    assert_eq!(
        cache
            .capture(&Scene::new(live.stage(), &store), light)
            .unwrap()
            .inputs
            .relationship_issues
            .len(),
        1
    );
    let mut edit = layerstack_schemas::SchemaEdit::new(
        live.stage(),
        &mut store,
        EditTarget::for_layer(LayerId(1)),
    );
    layerstack_schemas::usd_lux::LightFilter::define(&mut edit, filter);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    assert!(
        cache
            .capture(&Scene::new(live.stage(), &store), light)
            .unwrap()
            .inputs
            .relationship_issues
            .is_empty()
    );
    let removed = live.apply(&mut store, &applied.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &removed.changes);
    assert_eq!(
        cache
            .capture(&Scene::new(live.stage(), &store), light)
            .unwrap()
            .inputs
            .relationship_issues
            .len(),
        1
    );
}
#[test]
fn shape_flags_and_context_changes_have_explicit_retained_revisions() {
    let (mut store, mut live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n bool treatAsPoint = false\n uniform token gpu:light:shaderId = \"GpuSphere\"\n}\n",
    );
    let light = store.path("/Light");
    let mut cache = LightCache::new(Time::Default, &[]);
    let first = cache
        .capture(&Scene::new(live.stage(), &store), light)
        .unwrap()
        .revisions;
    let t = set(&mut store, light, "treatAsPoint", Value::Bool(true));
    let applied = live.apply(&mut store, &t).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    let next = cache
        .capture(&Scene::new(live.stage(), &store), light)
        .unwrap();
    assert_ne!(next.revisions.parameters, first.parameters);
    assert!(matches!(
        next.inputs.shape().unwrap(),
        LightShape::Sphere {
            treat_as_point: true,
            ..
        }
    ));
    cache.set_render_contexts(&["gpu"]);
    let next = cache
        .capture(&Scene::new(live.stage(), &store), light)
        .unwrap();
    assert_eq!(next.inputs.shader.id, "GpuSphere");
    assert_ne!(next.revisions.shader, first.shader);
    assert_eq!(next.revisions.transform, first.transform);
    let last = next.revisions;
    cache.clear();
    let fresh = cache
        .capture(&Scene::new(live.stage(), &store), light)
        .unwrap();
    assert!(fresh.revisions.parameters > last.parameters);
}
#[test]
fn retained_blocks_refresh_across_default_and_numeric_time_in_both_directions() {
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef SphereLight \"Light\" {\n float inputs:specular = None\n}\n",
    );
    let light = store.path("/Light");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = LightCache::new(Time::Default, &[]);
    assert!(
        cache
            .capture(&scene, light)
            .unwrap()
            .inputs
            .float("specular")
            .is_err()
    );
    cache.set_time(Time::At {
        code: 1.,
        interpolation: InterpolationType::Linear,
    });
    assert_eq!(
        cache
            .capture(&scene, light)
            .unwrap()
            .inputs
            .float("specular")
            .unwrap(),
        1.
    );
    cache.set_time(Time::At {
        code: 2.,
        interpolation: InterpolationType::Linear,
    });
    assert!(!cache.capture(&scene, light).unwrap().evaluated);
    cache.set_time(Time::Default);
    assert!(
        cache
            .capture(&scene, light)
            .unwrap()
            .inputs
            .float("specular")
            .is_err()
    );
}
