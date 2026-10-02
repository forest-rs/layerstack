// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Animated mesh hulls are explicit and retain reductions independently of world transforms.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    LayerId, PropertyPath, Value,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::{
    Scene, Time, XformCache,
    bounds::Range3d,
    skel::{BlendShapeCache, SkelCache, SkinningQuery},
};
fn hull(points: &[[f32; 3]]) -> Range3d {
    let mut range = Range3d::default();
    for point in points {
        let p = point.map(f64::from);
        range.union_with(Range3d { min: p, max: p });
    }
    range
}
#[test]
fn skeleton_hulls_match_cpp_vertices_and_reuse_reductions() {
    let text=include_str!("../fixtures/skel_blend_shapes.usda").replace("def Skeleton \"Skeleton\" {","def Skeleton \"Skeleton\" { double3 xformOp:translate = (10,20,30)\n uniform token[] xformOpOrder = [\"xformOp:translate\"]").replace("point3f[] points", "float3[] extent = [(-1,-1,-1),(1,1,1)]\n point3f[] points");
    let (mut store, live) = support::scene(&text);
    let path = store.path("/Rig/Geometry/Mesh");
    let scene = Scene::new(live.stage(), &store);
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/skel_blend_shapes.json")).unwrap();
    let mut cache = SkelCache::new(Time::Default);
    for row in oracle["frames"].as_array().unwrap() {
        let time = row["time"].as_f64().map_or(Time::Default, Time::at);
        cache.set_time(time);
        let points: Vec<[f32; 3]> = serde_json::from_value(row["skinned"].clone()).unwrap();
        let expected = hull(&points);
        assert_eq!(
            cache.deformed_mesh_bounds(&scene, path).unwrap().unwrap(),
            expected
        );
        assert_eq!(
            SkinningQuery::new(&scene, path)
                .unwrap()
                .unwrap()
                .compute_deformed_mesh_bounds(time)
                .unwrap(),
            expected
        );
        let before = cache.stats();
        let mut xforms = XformCache::new(time);
        let world = cache
            .deformed_world_mesh_bounds(&scene, path, &mut xforms)
            .unwrap()
            .unwrap()
            .aligned_range();
        assert_eq!(
            world.min,
            core::array::from_fn(|i| expected.min[i] + [10., 20., 30.][i])
        );
        assert_eq!(
            world.max,
            core::array::from_fn(|i| expected.max[i] + [10., 20., 30.][i])
        );
        assert_eq!(cache.stats().bound_evaluations, before.bound_evaluations);
        assert_eq!(cache.stats().point_vertices, before.point_vertices);
        assert!(
            cache
                .deformed_world_mesh_bounds(&scene, path, &mut XformCache::new(Time::at(99.)))
                .is_err()
        );
    }
    assert_eq!(cache.stats().bound_evaluations, 4);
    assert_eq!(cache.stats().bound_vertices, 8);
}
#[test]
fn morph_hulls_refresh_on_weights_and_undo_but_world_edits_preserve_reduction() {
    let source=include_str!("../fixtures/skel_blend_shapes.usda").replace("rel skel:skeleton = </Rig/Skeleton>","rel skel:skeleton = []").replace("uniform token[] skel:blendShapes = [\"smile\", \"frown\"]", "uniform token[] skel:blendShapes = [\"smile\", \"frown\"]\n double3 xformOp:translate = (0,0,0)\n uniform token[] xformOpOrder = [\"xformOp:translate\"]");
    let (mut store, mut live) = support::scene(&source);
    let path = store.path("/Rig/Geometry/Mesh");
    let anim = store.path("/Rig/Animation");
    let mut cache = BlendShapeCache::new(Time::Default);
    let mut xforms = XformCache::new(Time::Default);
    let original = cache
        .deformed_mesh_bounds(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .unwrap();
    let before = cache.stats();
    let mut t = Transaction::new();
    let p = PropertyPath::new(path, store.tokens.intern("xformOp:translate"));
    t.set_default(
        EditTarget::for_layer(LayerId(1)).property(p),
        Value::Vec3d([10., 0., 0.]),
    );
    let edit = live.apply(&mut store, &t).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &edit.changes);
    xforms.apply_changes(&scene, &edit.changes);
    let world = cache
        .deformed_world_mesh_bounds(&scene, path, &mut xforms)
        .unwrap()
        .unwrap()
        .aligned_range();
    assert_eq!(world.min[0], original.min[0] + 10.);
    assert_eq!(cache.stats().bound_evaluations, before.bound_evaluations);
    let mut t = Transaction::new();
    let p = PropertyPath::new(anim, store.tokens.intern("blendShapeWeights"));
    t.set_default(
        EditTarget::for_layer(LayerId(1)).property(p),
        Value::array(vec![Value::Float(0.), Value::Float(1.), Value::Float(0.)]),
    );
    let edit = live.apply(&mut store, &t).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &edit.changes);
    assert_ne!(
        cache.deformed_mesh_bounds(&scene, path).unwrap().unwrap(),
        original
    );
    let undo = live.apply(&mut store, &edit.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undo.changes);
    assert_eq!(
        cache.deformed_mesh_bounds(&scene, path).unwrap().unwrap(),
        original
    );
    assert_eq!(cache.stats().bound_evaluations, 3);
}
#[test]
fn empty_mesh_is_empty_and_nonfinite_points_do_not_form_partial_bounds() {
    for (points, valid) in [("[]", true), ("[(nan,0,0)]", false)] {
        let text = format!(
            "#usda 1.0\ndef Mesh \"Mesh\" (prepend apiSchemas = [\"SkelBindingAPI\"]) {{ uniform token[] skel:blendShapes = [\"empty\"] rel skel:blendShapeTargets = [</Shape>] point3f[] points = {points} }} def BlendShape \"Shape\" {{ uniform vector3f[] offsets = [] }}"
        );
        let (mut store, live) = support::scene(&text);
        let path = store.path("/Mesh");
        let mut cache = BlendShapeCache::new(Time::Default);
        let result = cache.deformed_mesh_bounds(&Scene::new(live.stage(), &store), path);
        if valid {
            assert!(result.unwrap().unwrap().is_empty());
        } else {
            assert!(result.is_err());
        }
    }
}
