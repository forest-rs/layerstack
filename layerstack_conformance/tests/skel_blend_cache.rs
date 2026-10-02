// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained morph-only geometry, independent of skeleton definitions.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    LayerId, PropertyPath, Value,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::{
    Scene, Time,
    skel::{BlendShapeCache, SkelCache},
};
fn source() -> String {
    include_str!("../fixtures/skel_blend_shapes.usda").replace("rel skel:skeleton = </Rig/Skeleton>","rel skel:skeleton = []").replace("point3f[] points = [(1,0,0), (0,2,0)]","point3f[] points = [(1,0,0), (0,2,0)]\n normal3f[] normals = [(0,0,1),(0,0,1)]\n uniform token purpose = \"default\"")
}
#[test]
fn skeleton_free_inputs_and_outputs_match_pinned_cpp_morph_results() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_blend_shapes.json")).unwrap();
    let (mut store, live) = support::scene(&source());
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = BlendShapeCache::new(Time::Default);
    assert!(
        SkelCache::new(Time::Default)
            .deformation_inputs(&scene, path)
            .unwrap()
            .is_none()
    );
    for row in oracle["frames"].as_array().unwrap() {
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        cache.set_time(time);
        let inputs = cache.inputs(&scene, path).unwrap().unwrap();
        inputs.validate_point_count(2).unwrap();
        let weights: Vec<f32> = serde_json::from_value(row["weights"].clone()).unwrap();
        assert_eq!(inputs.weights(), weights);
        assert_eq!(inputs.time(), time);
        let points: Vec<[f32; 3]> = serde_json::from_value(row["local"].clone()).unwrap();
        let normals: Vec<[f32; 3]> = serde_json::from_value(row["normals"].clone()).unwrap();
        assert_eq!(
            cache.deformed_points(&scene, path).unwrap().unwrap(),
            points
        );
        assert_eq!(
            cache.deformed_normals(&scene, path).unwrap().unwrap(),
            normals
        );
        let before = cache.stats();
        cache.deformed_points(&scene, path).unwrap().unwrap();
        cache.deformed_normals(&scene, path).unwrap().unwrap();
        assert_eq!(cache.stats().weight_evaluations, before.weight_evaluations);
        assert_eq!(cache.stats().point_vertices, before.point_vertices);
    }
    assert_eq!(cache.stats().definition_builds, 1);
    assert!(cache.memory().capacity_bytes >= cache.memory().used_bytes);
}
#[test]
fn edits_undo_removal_and_clear_issue_fresh_component_revisions() {
    let (mut store, mut live) = support::scene(&source());
    let path = store.path("/Rig/Geometry/Mesh");
    let anim = store.path("/Rig/Animation");
    let shape = store.path("/Rig/Smile");
    let mut cache = BlendShapeCache::new(Time::Default);
    let get = |cache: &mut BlendShapeCache, scene: &Scene<'_>| {
        let i = cache.inputs(scene, path).unwrap().unwrap();
        (i.definition_revision(), i.weight_revision())
    };
    let initial = get(&mut cache, &Scene::new(live.stage(), &store));
    let property = PropertyPath::new(path, store.tokens.intern("points"));
    let mut t = Transaction::new();
    t.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        Value::array(vec![Value::Vec3f([2., 0., 0.]), Value::Vec3f([0., 4., 0.])]),
    );
    let edit = live.apply(&mut store, &t).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &edit.changes);
    assert_eq!(get(&mut cache, &scene), initial);
    let property = PropertyPath::new(anim, store.tokens.intern("blendShapeWeights"));
    let mut t = Transaction::new();
    t.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        Value::array(vec![Value::Float(0.), Value::Float(1.), Value::Float(0.)]),
    );
    let edit = live.apply(&mut store, &t).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &edit.changes);
    let weighted = get(&mut cache, &scene);
    assert_eq!(weighted.0, initial.0);
    assert_ne!(weighted.1, initial.1);
    let undo = live.apply(&mut store, &edit.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undo.changes);
    let restored = get(&mut cache, &scene);
    assert_ne!(restored.1, weighted.1);
    let mut t = Transaction::new();
    t.remove_spec(EditTarget::for_layer(LayerId(1)).prim(shape));
    let edit = live.apply(&mut store, &t).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &edit.changes);
    assert!(cache.inputs(&scene, path).is_err());
    let undo = live.apply(&mut store, &edit.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undo.changes);
    let rebuilt = get(&mut cache, &scene);
    assert_ne!(rebuilt.0, restored.0);
    cache.clear();
    let cleared = get(&mut cache, &scene);
    assert_ne!(cleared.0, rebuilt.0);
    assert_ne!(cleared.1, rebuilt.1);
    assert_eq!(cache.stats().point_vertices, 0);
}
#[test]
fn bufferless_inputs_and_static_weights_survive_time_changes() {
    let source = source()
        .replace(
            "float[] blendShapeWeights.timeSamples = {1: [0.25,0.25,1], 3: [0.5,1.25,1]}",
            "",
        )
        .replace("point3f[] points = [(1,0,0), (0,2,0)]", "");
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let mut cache = BlendShapeCache::new(Time::Default);
    let i = cache.inputs(&scene, path).unwrap().unwrap();
    let before = (i.definition_revision(), i.weight_revision());
    for time in [Time::at(1.), Time::at(2.), Time::Default] {
        cache.set_time(time);
        let i = cache.inputs(&scene, path).unwrap().unwrap();
        assert_eq!((i.definition_revision(), i.weight_revision()), before);
    }
    assert_eq!(cache.stats().weight_evaluations, 1);
    assert!(cache.deformed_points(&scene, path).is_err());
}
