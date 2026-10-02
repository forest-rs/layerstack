// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained deformation matches fresh queries after times, edits and undo.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    InMemoryStore, LayerId, ListOp, LiveStage, PathId, PropertyPath, TargetPath, Value,
    edit::{EditTarget, Transaction},
};
use layerstack_schemas::{
    Scene, Time,
    skel::{SkelCache, SkelError, SkinningQuery},
};
const ID: [[f64; 4]; 4] = [
    [1., 0., 0., 0.],
    [0., 1., 0., 0.],
    [0., 0., 1., 0.],
    [0., 0., 0., 1.],
];
fn compare(
    cache: &mut SkelCache,
    store: &InMemoryStore,
    live: &LiveStage,
    paths: &[PathId],
    normals: bool,
) {
    let time = cache.time();
    let scene = Scene::new(live.stage(), store);
    for &path in paths {
        let query = SkinningQuery::new(&scene, path);
        let want = query
            .as_ref()
            .ok()
            .and_then(|q| q.as_ref())
            .map(|q| q.compute_deformed_points(time));
        match want {
            Some(Ok(points)) => assert_eq!(
                cache.deformed_points(&scene, path).unwrap().unwrap(),
                points,
                "retained points match a fresh binding"
            ),
            Some(Err(e)) => assert_eq!(
                cache.deformed_points(&scene, path),
                Err(e),
                "retained failures match fresh evaluation"
            ),
            None if query.is_err() => assert_eq!(
                cache.deformed_points(&scene, path),
                Err(query.as_ref().unwrap_err().clone()),
                "retained definition failures match fresh evaluation"
            ),
            None => assert!(
                cache.deformed_points(&scene, path).unwrap().is_none(),
                "unbound result matches fresh query"
            ),
        }
        if normals && let Some(q) = query.ok().flatten() {
            assert_eq!(
                cache.skinned_normals(&scene, path).unwrap().unwrap(),
                q.compute_skinned_normals(time).unwrap(),
                "retained normals match fresh query"
            );
        }
    }
}
fn default_edit(store: &mut InMemoryStore, path: PathId, name: &str, value: Value) -> Transaction {
    let token = store.tokens.intern(name);
    let mut t = Transaction::new();
    t.set_default(
        EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(path, token)),
        value,
    );
    t
}
#[test]
fn shared_pose_influences_and_outputs_survive_frame_changes() {
    for (source, names, normals) in [
        (
            include_str!("../fixtures/skel_blend_shapes.usda"),
            vec!["/Rig/Geometry/Mesh", "/Rig/Rigid"],
            false,
        ),
        (
            include_str!("../fixtures/skel_normals.usda"),
            vec![
                "/Rig/Geometry/Vertex",
                "/Rig/Geometry/Corners",
                "/Rig/Rigid",
            ],
            true,
        ),
    ] {
        let (mut store, live) = support::scene(source);
        let paths: Vec<_> = names.iter().map(|p| store.path(p)).collect();
        let mut cache = SkelCache::new(Time::Default);
        for time in [
            Time::Default,
            Time::at(1.),
            Time::at(2.),
            Time::held(2.),
            Time::at(3.),
            Time::Default,
        ] {
            cache.set_time(time);
            let before = cache.stats();
            compare(&mut cache, &store, &live, &paths, normals);
            assert_eq!(cache.stats().pose_evaluations - before.pose_evaluations, 1);
            assert_eq!(
                cache.stats().normal_matrices - before.normal_matrices,
                if normals { 2 } else { 0 }
            );
            let before = cache.stats();
            compare(&mut cache, &store, &live, &paths, normals);
            assert_eq!(cache.stats().misses, before.misses);
        }
        assert_eq!(cache.stats().skeleton_builds, 1);
        assert_eq!(cache.stats().binding_builds, paths.len() as u64);
        assert_eq!(cache.stats().influence_resolutions, paths.len() as u64);
        assert_eq!(
            cache.stats().inverse_bind_matrices,
            if normals { 2 } else { 3 }
        );
        let memory = cache.memory();
        assert!(memory.used_bytes > 0 && memory.capacity_bytes >= memory.used_bytes);
        cache.reset_stats();
        assert_eq!(cache.memory(), memory);
        cache.clear();
        assert_eq!(cache.memory().capacity_bytes, 0);
        assert_eq!(cache.memory().skeletons, 0);
    }
}
#[test]
fn static_outputs_survive_times_and_single_samples_cover_default_transitions() {
    let source = include_str!("../fixtures/skel_skinning.usda").replace(
        "rel skel:animationSource = </Rig/Animation>",
        "rel skel:animationSource = []",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Rigid");
    let mut cache = SkelCache::new(Time::Default);
    compare(&mut cache, &store, &live, &[path], false);
    cache.reset_stats();
    for time in [Time::at(1.), Time::at(2.), Time::Default] {
        let before = cache.stats();
        cache.set_time(time);
        assert_eq!(cache.stats(), before);
        compare(&mut cache, &store, &live, &[path], false);
    }
    assert_eq!(cache.stats().pose_evaluations, 0);
    assert_eq!(cache.stats().misses, 0);
    let source = include_str!("../fixtures/skel_skinning.usda").replace(
        "{1: [(0,4,0),(2,0,0)], 3: [(0,8,0),(6,0,0)]}",
        "{1: [(0,8,0),(6,0,0)]}",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Rigid");
    let mut cache = SkelCache::new(Time::Default);
    let default = cache
        .deformed_points(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .unwrap()
        .to_vec();
    cache.set_time(Time::at(2.));
    compare(&mut cache, &store, &live, &[path], false);
    assert_ne!(
        cache
            .deformed_points(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .unwrap(),
        default
    );
}
#[test]
fn precise_edits_preserve_independent_inputs_and_undo_restores_results() {
    let (mut store, mut live) =
        support::scene(&include_str!("../fixtures/skel_blend_shapes.usda").replace(
            "point3f[] points",
            "uniform token purpose = \"default\"\n point3f[] points",
        ));
    let mesh = store.path("/Rig/Geometry/Mesh");
    let rigid = store.path("/Rig/Rigid");
    let anim = store.path("/Rig/Animation");
    let geom = store.path("/Rig/Geometry");
    let shape = store.path("/Rig/Smile");
    let skel = store.path("/Rig/Skeleton");
    let paths = [mesh, rigid];
    let mut cache = SkelCache::new(Time::Default);
    compare(&mut cache, &store, &live, &paths, false);
    let edits = [
        (
            anim,
            "translations",
            Value::Array(vec![Value::Vec3f([0., 9., 0.]), Value::Vec3f([8., 0., 0.])]),
        ),
        (
            geom,
            "primvars:skel:jointWeights",
            Value::Array(vec![
                Value::Float(0.5),
                Value::Float(0.5),
                Value::Float(1.),
                Value::Float(0.),
            ]),
        ),
        (
            shape,
            "offsets",
            Value::Array(vec![Value::Vec3f([0., 9., 0.])]),
        ),
        (
            mesh,
            "points",
            Value::Array(vec![Value::Vec3f([9., 0., 0.]), Value::Vec3f([0., 8., 0.])]),
        ),
        (
            anim,
            "blendShapeWeights",
            Value::Array(vec![
                Value::Float(1.),
                Value::Float(-0.25),
                Value::Float(1.),
            ]),
        ),
        (
            skel,
            "bindTransforms",
            Value::Array(vec![
                Value::Matrix4d(Box::new(core::array::from_fn(
                    |i| ID[i / 4][i % 4]
                )));
                3
            ]),
        ),
    ];
    for (path, name, value) in edits {
        cache.reset_stats();
        let transaction = default_edit(&mut store, path, name, value);
        let applied = live.apply(&mut store, &transaction).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
        compare(&mut cache, &store, &live, &paths, false);
        if name == "translations" {
            assert_eq!(cache.stats().influence_resolutions, 0);
            assert_eq!(cache.stats().inverse_bind_matrices, 0);
            assert_eq!(cache.stats().pose_evaluations, 1);
        }
        if name == "points" {
            assert_eq!(cache.stats().binding_builds, 0);
            assert_eq!(cache.stats().pose_evaluations, 0);
            assert_eq!(cache.stats().misses, 1);
        }
        if name == "blendShapeWeights" {
            assert_eq!(cache.stats().pose_evaluations, 0);
            assert_eq!(cache.stats().influence_resolutions, 0);
            assert_eq!(cache.stats().misses, 1);
        }
        let undone = live.apply(&mut store, &applied.inverse).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
        compare(&mut cache, &store, &live, &paths, false);
    }
    // Unrelated property inventory does not invalidate geometry.
    let mut transaction = Transaction::new();
    let token = store.tokens.intern("purpose");
    let at = EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(mesh, token));
    transaction.set_default(at, Value::Token(store.tokens.intern("render")));
    let applied = live.apply(&mut store, &transaction).unwrap();
    cache.reset_stats();
    cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
    compare(&mut cache, &store, &live, &paths, false);
    assert_eq!(cache.stats().misses, 0);
}
#[test]
fn forwarded_animation_alias_edits_and_removals_refresh_definitions() {
    let source = include_str!("../fixtures/skel_skinning.usda").replace(
        "rel skel:animationSource = </Rig/Animation>",
        "rel skel:animationSource = </Alias.target>",
    );
    let source = format!("{source}\ndef Scope \"Alias\" {{\nrel target = </Rig/Animation>\n}}\n");
    let (mut store, mut live) = support::scene(&source);
    let rigid = store.path("/Rig/Rigid");
    let alias = store.path("/Alias");
    let skeleton = store.path("/Rig/Skeleton");
    let mut cache = SkelCache::new(Time::Default);
    compare(&mut cache, &store, &live, &[rigid], false);
    let before = cache
        .deformed_points(&Scene::new(live.stage(), &store), rigid)
        .unwrap()
        .unwrap()
        .to_vec();
    let token = store.tokens.intern("target");
    let at = EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(alias, token));
    let mut transaction = Transaction::new();
    transaction.set_targets(at, ListOp::explicit(vec![]));
    let changed = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &changed.changes);
    compare(&mut cache, &store, &live, &[rigid], false);
    assert_ne!(
        cache
            .deformed_points(&Scene::new(live.stage(), &store), rigid)
            .unwrap()
            .unwrap(),
        before
    );
    let undone = live.apply(&mut store, &changed.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    compare(&mut cache, &store, &live, &[rigid], false);
    let mut transaction = Transaction::new();
    transaction.remove_spec(EditTarget::for_layer(LayerId(1)).prim(skeleton));
    let removed = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &removed.changes);
    compare(&mut cache, &store, &live, &[rigid], false);
    let undone = live.apply(&mut store, &removed.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    compare(&mut cache, &store, &live, &[rigid], false);
}
#[test]
fn influence_errors_recover_and_unbound_queries_retry() {
    let (mut store, mut live) = support::scene(include_str!("../fixtures/skel_skinning.usda"));
    let path = store.path("/Rig/Rigid");
    let mut cache = SkelCache::new(Time::Default);
    compare(&mut cache, &store, &live, &[path], false);
    let transaction = default_edit(
        &mut store,
        path,
        "primvars:skel:jointIndices",
        Value::Array(vec![Value::Int(99)]),
    );
    let bad = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &bad.changes);
    assert!(matches!(
        cache.deformed_points(&Scene::new(live.stage(), &store), path),
        Err(SkelError::InvalidDeformation { .. })
    ));
    let undone = live.apply(&mut store, &bad.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    compare(&mut cache, &store, &live, &[path], false);
    let token = store.tokens.intern("skel:skeleton");
    let at = EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(path, token));
    let mut t = Transaction::new();
    t.set_targets(at, ListOp::explicit(Vec::<TargetPath>::new()));
    let unbound = live.apply(&mut store, &t).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &unbound.changes);
    compare(&mut cache, &store, &live, &[path], false);
    let undone = live.apply(&mut store, &unbound.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    compare(&mut cache, &store, &live, &[path], false);
}

#[test]
fn normal_subsets_ignore_other_singular_joints_and_preserve_error_indices() {
    let source = include_str!("../fixtures/skel_normals.usda")
        .replace("half3[] scales = [(2,0.5,1), (1,3,0.5)]", "half3[] scales = [(2,0.5,1), (0,0,0)]")
        .replace("int[] primvars:skel:jointIndices = [0,1] (elementSize = 2)", "uniform token[] skel:joints = [\"a\"]\n int[] primvars:skel:jointIndices = [0,0] (elementSize = 2)");
    let (mut store, mut live) = support::scene(&source);
    let path = store.path("/Rig/Rigid");
    let mut cache = SkelCache::new(Time::Default);
    compare(&mut cache, &store, &live, &[path], true);
    let b = store.tokens.intern("b");
    let transaction = default_edit(
        &mut store,
        path,
        "skel:joints",
        Value::Array(vec![Value::Token(b)]),
    );
    let changed = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &changed.changes);
    let scene = Scene::new(live.stage(), &store);
    let want = SkinningQuery::new(&scene, path)
        .unwrap()
        .unwrap()
        .compute_skinned_normals(Time::Default)
        .unwrap_err();
    assert_eq!(want, SkelError::SingularNormalTransform { joint: Some(0) });
    assert_eq!(cache.skinned_normals(&scene, path), Err(want));
    let undone = live.apply(&mut store, &changed.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    compare(&mut cache, &store, &live, &[path], true);
}
#[test]
fn temporal_influences_and_corner_edits_match_fresh_normals() {
    let source = include_str!("../fixtures/skel_normals.usda").replace(
        "float[] primvars:skel:jointWeights = [0.25,0.75,1,0] (elementSize = 2; interpolation = \"vertex\")",
        "float[] primvars:skel:jointWeights = [0.25,0.75,1,0] (elementSize = 2; interpolation = \"vertex\")\n float[] primvars:skel:jointWeights.timeSamples = {1: [0.25,0.75,1,0], 3: [1,0,0,1]}",
    );
    let (mut store, mut live) = support::scene(&source);
    let vertex = store.path("/Rig/Geometry/Vertex");
    let corners = store.path("/Rig/Geometry/Corners");
    let mut cache = SkelCache::new(Time::Default);
    for time in [
        Time::Default,
        Time::at(1.),
        Time::at(2.),
        Time::held(2.),
        Time::at(3.),
    ] {
        cache.set_time(time);
        compare(&mut cache, &store, &live, &[vertex, corners], true);
    }
    assert_eq!(cache.stats().influence_resolutions, 10);
    for (name, value) in [
        (
            "faceVertexIndices",
            Value::Array(vec![Value::Int(1), Value::Int(0), Value::Int(1)]),
        ),
        (
            "normals",
            Value::Array(vec![
                Value::Vec3f([0., 1., 0.]),
                Value::Vec3f([1., 0., 0.]),
                Value::Vec3f([0., 0., 1.]),
            ]),
        ),
    ] {
        let transaction = default_edit(&mut store, corners, name, value);
        let changed = live.apply(&mut store, &transaction).unwrap();
        cache.reset_stats();
        cache.apply_changes(&Scene::new(live.stage(), &store), &changed.changes);
        compare(&mut cache, &store, &live, &[vertex, corners], true);
        assert_eq!(cache.stats().pose_evaluations, 0);
        assert_eq!(cache.stats().influence_resolutions, 0);
        assert_eq!(cache.stats().point_vertices, 0);
        assert_eq!(cache.stats().normal_vectors, 3);
        let undone = live.apply(&mut store, &changed.inverse).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
        compare(&mut cache, &store, &live, &[vertex, corners], true);
    }
}
#[test]
fn point_only_bindings_do_not_require_valid_blend_weights() {
    let source = include_str!("../fixtures/skel_skinning.usda").replace(
        "def SkelAnimation \"Animation\" {",
        "def SkelAnimation \"Animation\" {\n uniform token[] blendShapes = [\"unused\"]\n float[] blendShapeWeights = [1,2]",
    );
    let (mut store, live) = support::scene(&source);
    let path = store.path("/Rig/Rigid");
    let mut cache = SkelCache::new(Time::Default);
    compare(&mut cache, &store, &live, &[path], false);
}

#[test]
fn instanced_rigs_discover_remapped_bindings_and_refresh_shared_source_edits() {
    use layerstack_schemas::usd_skel::SkelRoot;
    let source = include_str!("../fixtures/skel_skinning.usda")
        .replace("\"Rig\"", "\"Template\"")
        .replace("/Rig/", "/Template/");
    let source = format!(
        "{source}\ndef SkelRoot \"World\" {{\n def SkelRoot \"A\" (references = </Template>; instanceable = true) {{}}\n def SkelRoot \"B\" (references = </Template>; instanceable = true) {{}}\n}}\n"
    );
    let (mut store, mut live) = support::scene(&source);
    let root = store.path("/World");
    let paths: Vec<_> = [
        "/World/A/Geometry/Mesh",
        "/World/A/Rigid",
        "/World/B/Geometry/Mesh",
        "/World/B/Rigid",
    ]
    .iter()
    .map(|p| store.path(p))
    .collect();
    let a = store.path("/World/A");
    let b = store.path("/World/B");
    let anim = store.path("/Template/Animation");
    assert!(live.stage().is_instance(a) && live.stage().is_instance(b));
    let scene = Scene::new(live.stage(), &store);
    let root = SkelRoot::new(&scene, root).unwrap();
    assert!(root.skinning_queries().unwrap().is_empty());
    let discovered = root.skinning_queries_with_instance_proxies().unwrap();
    assert_eq!(
        discovered
            .iter()
            .map(SkinningQuery::geometry_path)
            .collect::<Vec<_>>(),
        paths
    );
    assert_ne!(
        discovered[0].skeleton_query().skeleton_path(),
        discovered[2].skeleton_query().skeleton_path()
    );
    let mut cache = SkelCache::new(Time::Default);
    compare(&mut cache, &store, &live, &paths, false);
    let transaction = default_edit(
        &mut store,
        anim,
        "translations",
        Value::Array(vec![Value::Vec3f([0., 9., 0.]), Value::Vec3f([8., 0., 0.])]),
    );
    let changed = live.apply(&mut store, &transaction).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &changed.changes);
    compare(&mut cache, &store, &live, &paths, false);
    let undone = live.apply(&mut store, &changed.inverse).unwrap();
    cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
    compare(&mut cache, &store, &live, &paths, false);
}
