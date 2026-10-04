// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Exercises the exact runnable multi-producer example, including source edits and OpenUSD round trips.
#[path = "../../layerstack_examples/examples/support/generated_assets.rs"]
mod support;
use layerstack::{
    EditTarget, FieldValue, InMemoryStore, ListOp, NodeId, Stage, StageOptions, Time, Transaction,
    TypedArray, Value,
};
use layerstack_schemas::{
    BindingOptions, MaterialPurpose, MeshPublicationError, Scene, SchemaEdit,
    bounds::{BoundsCache, BoundsOptions},
    point_instancer::InstanceTransformOptions,
    primvar::Primvar,
    usd_geom::{Mesh, PointInstancer},
    usd_proc::GenerativeProcedural,
    usd_shade::Material,
};
use std::{path::Path, process::Command, sync::Arc};

fn height(cache: &mut BoundsCache, scene: &Scene<'_>, scatter: layerstack::PathId) -> f64 {
    cache
        .world_bound(scene, scatter)
        .unwrap()
        .aligned_range()
        .max[1]
}

fn recipe_edit(
    generated: &mut support::GeneratedScene,
    layer: layerstack::LayerId,
    property: &str,
    value: Value,
) -> layerstack::Changes {
    let path = generated.store.property_path(property);
    let mut edit = Transaction::new();
    edit.set_default(EditTarget::for_layer(layer).property(path), value);
    generated
        .live
        .apply(&mut generated.store, &edit)
        .unwrap()
        .changes
}

#[test]
fn recipes_drive_shared_geometry_material_inputs_and_dependent_bounds() {
    let mut generated = support::GeneratedScene::new(4);
    let native = generated.store.path("/World/Native_0/Geometry");
    let scene = Scene::new(generated.live.stage(), &generated.store);
    let initial = Mesh::new(&scene, native).unwrap().points().unwrap();
    let topology = generated.geometry.face_vertex_indices.clone();
    assert!(Arc::ptr_eq(
        &initial,
        &Mesh::new(&scene, generated.terrain)
            .unwrap()
            .points()
            .unwrap()
    ));
    assert_eq!(generated.asset_generator.work().evaluations, 1);
    assert_eq!(
        generated.evaluate_asset(Time::Default).unwrap(),
        layerstack::Changes::default()
    );
    assert_eq!(
        generated.evaluate_terrain(Time::Default).unwrap(),
        layerstack::Changes::default()
    );
    assert_eq!(generated.asset_generator.work().evaluations, 1);
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(
        height(
            &mut cache,
            &Scene::new(generated.live.stage(), &generated.store),
            generated.scatter
        ),
        1.
    );
    let changes = recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:height",
        Value::Float(3.),
    );
    cache.apply_changes(
        &Scene::new(generated.live.stage(), &generated.store),
        &changes,
    );
    assert_eq!(
        height(
            &mut cache,
            &Scene::new(generated.live.stage(), &generated.store),
            generated.scatter
        ),
        1.,
        "USD composition does not execute the recipe"
    );
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:texture",
        Value::Asset("textures/moss.png".into()),
    );
    let changes = generated.evaluate_asset(Time::Default).unwrap();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    cache.apply_changes(&scene, &changes);
    assert_eq!(height(&mut cache, &scene, generated.scatter), 3.);
    assert!(Arc::ptr_eq(
        &topology,
        &Mesh::new(&scene, native)
            .unwrap()
            .face_vertex_indices()
            .unwrap()
    ));
    assert!(
        Arc::ptr_eq(
            &initial,
            &Mesh::new(&scene, generated.terrain)
                .unwrap()
                .points()
                .unwrap()
        ),
        "other producers' buffers remain untouched"
    );
    let texture = generated.store.path("/Assets/Tree/Material/Texture");
    assert_eq!(
        generated
            .live
            .stage()
            .prim(texture, &generated.store)
            .unwrap()
            .attribute("inputs:file")
            .unwrap()
            .get(Time::Default)
            .unwrap()
            .value,
        Value::Asset("textures/moss.png".into())
    );
    assert!(
        generated
            .asset_generator
            .dependencies()
            .any(|d| d.prim == generated.terrain && d.property == "points")
    );
    // A masked authored edit changes neither the composed input nor the output.
    recipe_edit(
        &mut generated,
        support::ROOT,
        "/Recipes/Tree.primvars:height",
        Value::Float(3.),
    );
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:height",
        Value::Float(9.),
    );
    let evaluations = generated.asset_generator.work().evaluations;
    assert_eq!(
        generated.evaluate_asset(Time::Default).unwrap(),
        layerstack::Changes::default()
    );
    assert_eq!(generated.asset_generator.work().evaluations, evaluations);
    // The host orders two producer updates, then forwards both change reports.
    recipe_edit(
        &mut generated,
        support::TERRAIN,
        "/Recipes/Terrain.primvars:height",
        Value::Float(2.),
    );
    let changes = generated.evaluate_terrain(Time::Default).unwrap();
    cache.apply_changes(
        &Scene::new(generated.live.stage(), &generated.store),
        &changes,
    );
    let changes = generated.evaluate_asset(Time::Default).unwrap();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    cache.apply_changes(&scene, &changes);
    assert_eq!(height(&mut cache, &scene, generated.scatter), 6.);
    assert_eq!(
        Mesh::new(&scene, native).unwrap().points().unwrap()[2][1],
        6.
    );
    let quad = generated.store.tokens.intern("quad");
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:topology",
        Value::Token(quad),
    );
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:normals",
        Value::Bool(false),
    );
    generated.evaluate_asset(Time::Default).unwrap();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    assert_eq!(
        &**Mesh::new(&scene, native)
            .unwrap()
            .face_vertex_counts()
            .unwrap(),
        &[4]
    );
    assert!(Primvar::new(&scene, native, "normals").is_none());
    assert!(
        Primvar::new(&scene, native, "st")
            .unwrap()
            .compute_flattened(Time::Default)
            .unwrap()
            .is_some()
    );
}

#[test]
fn invalid_missing_and_recreated_recipes_preserve_last_good_publication() {
    let mut generated = support::GeneratedScene::new(4);
    let initial = generated.geometry.points.clone();
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:height",
        Value::Float(-1.),
    );
    assert!(generated.evaluate_asset(Time::Default).is_err());
    assert!(Arc::ptr_eq(
        &initial,
        &Mesh::new(
            &Scene::new(generated.live.stage(), &generated.store),
            generated.source
        )
        .unwrap()
        .points()
        .unwrap()
    ));
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:height",
        Value::Float(1.),
    );
    assert_eq!(
        generated.evaluate_asset(Time::Default).unwrap(),
        layerstack::Changes::default()
    );
    let mut deletion = Transaction::new();
    deletion.remove_spec(EditTarget::for_layer(support::TERRAIN).prim(generated.terrain));
    generated
        .live
        .apply(&mut generated.store, &deletion)
        .unwrap();
    assert!(generated.evaluate_asset(Time::Default).is_err());
    assert!(Arc::ptr_eq(
        &initial,
        &Mesh::new(
            &Scene::new(generated.live.stage(), &generated.store),
            generated.source
        )
        .unwrap()
        .points()
        .unwrap()
    ));
    generated.evaluate_terrain(Time::Default).unwrap();
    assert_eq!(
        generated.evaluate_asset(Time::Default).unwrap(),
        layerstack::Changes::default()
    );
    let recipe = generated.asset_generator.recipe();
    let mut deletion = Transaction::new();
    deletion.remove_spec(EditTarget::for_layer(support::ASSET).prim(recipe));
    let removed = generated
        .live
        .apply(&mut generated.store, &deletion)
        .unwrap();
    assert!(generated.evaluate_asset(Time::Default).is_err());
    assert!(
        generated.live.stage().has_prim(generated.source),
        "recipe removal has an explicit output-lifecycle policy"
    );
    generated
        .live
        .apply(&mut generated.store, &removed.inverse)
        .unwrap();
    assert_eq!(
        generated.evaluate_asset(Time::Default).unwrap(),
        layerstack::Changes::default()
    );
}

#[test]
fn material_publication_uses_owned_opinions_and_rejects_partial_updates() {
    let mut generated = support::GeneratedScene::new(4);
    let file = generated
        .store
        .property_path("/Assets/Tree/Material/Texture.inputs:file");
    let initial = generated.geometry.points.clone();
    let mut conflict = Transaction::new();
    conflict.remove_spec(EditTarget::for_layer(support::ASSET).property(file));
    conflict.create_property(
        EditTarget::for_layer(support::ASSET).property(file),
        layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "int",
            false,
            Value::Int(0),
        ))
        .with_default(Value::Int(17)),
    );
    let conflicting = generated
        .live
        .apply(&mut generated.store, &conflict)
        .unwrap();
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:height",
        Value::Float(2.),
    );
    assert!(matches!(
        generated.evaluate_asset(Time::Default),
        Err(support::RecipeError::Publication(_))
    ));
    assert!(
        Arc::ptr_eq(
            &initial,
            &Mesh::new(
                &Scene::new(generated.live.stage(), &generated.store),
                generated.source
            )
            .unwrap()
            .points()
            .unwrap()
        ),
        "invalid material declarations cannot partially publish geometry"
    );
    generated
        .live
        .apply(&mut generated.store, &conflicting.inverse)
        .unwrap();
    let evaluations = generated.asset_generator.work().evaluations;
    let mut sample = Transaction::new();
    sample.set_time_sample(
        EditTarget::for_layer(support::ASSET).property(file),
        1.,
        Value::Asset("textures/old-sample.png".into()),
    );
    generated.live.apply(&mut generated.store, &sample).unwrap();
    recipe_edit(
        &mut generated,
        support::ROOT,
        "/Assets/Tree/Material/Texture.inputs:file",
        Value::Asset("textures/moss.png".into()),
    );
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:texture",
        Value::Asset("textures/moss.png".into()),
    );
    generated.evaluate_asset(Time::Default).unwrap();
    assert_eq!(
        generated.asset_generator.work().evaluations,
        evaluations + 1
    );
    let local = generated.store.layers[&support::ASSET]
        .property(file)
        .unwrap();
    assert_eq!(
        local.default,
        Some(Value::Asset("textures/moss.png".into())),
        "equal composed values do not hide stale producer opinions"
    );
    assert!(
        local
            .time_samples
            .as_ref()
            .is_none_or(|samples| samples.as_slice().is_empty())
    );
    assert_eq!(
        generated.store.layers[&support::ROOT]
            .property(file)
            .unwrap()
            .default,
        Some(Value::Asset("textures/moss.png".into())),
        "other producers' overrides remain intact"
    );
    assert_eq!(generated.geometry.points[2][1], 2.);
}

#[test]
fn shared_sources_invalidate_native_and_point_instances_and_recover_after_recreation() {
    let mut generated = support::GeneratedScene::new(4);
    let native = generated.store.path("/World/Native_0");
    let other = generated.store.path("/World/Native_1");
    let proxy = generated.store.path("/World/Native_0/Geometry");
    assert!(generated.live.stage().is_instance(native));
    assert_eq!(
        generated.live.stage().instance_prototype(native),
        generated.live.stage().instance_prototype(other)
    );
    assert_eq!(generated.live.stage().prototypes().count(), 1);
    assert!(
        matches!(generated.geometry.prepare(generated.live.stage(), &mut generated.store, &EditTarget::for_layer(support::ASSET), proxy, &[]), Err(MeshPublicationError::InstanceProxy(p)) if p == proxy)
    );
    let scene = Scene::new(generated.live.stage(), &generated.store);
    let mesh = Mesh::new(&scene, proxy).unwrap();
    assert!(Arc::ptr_eq(
        &mesh.points().unwrap(),
        &generated.geometry.points
    ));
    let bound = mesh.compute_bound_material(&MaterialPurpose::All, BindingOptions::default());
    assert_eq!(
        generated
            .store
            .paths
            .display(bound.material.unwrap(), &generated.store.tokens)
            .to_string(),
        "/World/Materials/Override"
    );
    let mut query = generated
        .live
        .stage()
        .prim(proxy, &generated.store)
        .unwrap()
        .attribute("points")
        .unwrap()
        .query();
    query
        .try_get(generated.live.stage(), Time::Default)
        .unwrap()
        .unwrap();
    let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
    assert_eq!(height(&mut cache, &scene, generated.scatter), 1.);
    let unchanged_topology = mesh.face_vertex_indices().unwrap();
    let terrain_points = Mesh::new(&scene, generated.terrain)
        .unwrap()
        .points()
        .unwrap();
    generated.geometry.points =
        Arc::new(vec![[0., 0., 0.], [1., 0., 0.], [1., 3., 0.], [0., 1., 0.]]);
    let changes = generated.publish();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    cache.apply_changes(&scene, &changes);
    assert_eq!(height(&mut cache, &scene, generated.scatter), 3.);
    assert!(Arc::ptr_eq(
        &unchanged_topology,
        &Mesh::new(&scene, proxy)
            .unwrap()
            .face_vertex_indices()
            .unwrap()
    ));
    assert!(Arc::ptr_eq(
        &terrain_points,
        &Mesh::new(&scene, generated.terrain)
            .unwrap()
            .points()
            .unwrap()
    ));
    let resolved = query
        .try_get(generated.live.stage(), Time::Default)
        .unwrap()
        .unwrap()
        .value;
    let Value::TypedArray(TypedArray::Vec3f(points)) = resolved else {
        panic!("native points")
    };
    assert!(Arc::ptr_eq(&points, &generated.geometry.points));
    let mut deletion = Transaction::new();
    deletion.remove_spec(EditTarget::for_layer(support::ASSET).prim(generated.source));
    let changes = generated
        .live
        .apply(&mut generated.store, &deletion)
        .unwrap()
        .changes;
    let scene = Scene::new(generated.live.stage(), &generated.store);
    cache.apply_changes(&scene, &changes);
    assert!(!generated.live.stage().has_prim(proxy));
    assert!(
        cache
            .world_bound(&scene, generated.scatter)
            .unwrap()
            .aligned_range()
            .is_empty()
    );
    assert!(
        query
            .try_get(generated.live.stage(), Time::Default)
            .unwrap()
            .is_none()
    );
    let changes = generated.publish();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    cache.apply_changes(&scene, &changes);
    assert_eq!(height(&mut cache, &scene, generated.scatter), 3.);
    assert!(
        query
            .try_get(generated.live.stage(), Time::Default)
            .unwrap()
            .is_some()
    );
    generated.geometry.face_vertex_counts = Arc::new(vec![4]);
    generated.geometry.face_vertex_indices = Arc::new(vec![0, 1, 2, 3]);
    generated.geometry.primvars[0].indices = Some(Arc::new(vec![0, 1, 2, 5]));
    generated.geometry.primvars.remove(1);
    generated.publish();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    let mesh = Mesh::new(&scene, proxy).unwrap();
    assert_eq!(&**mesh.face_vertex_counts().unwrap(), &[4]);
    assert!(Primvar::new(&scene, proxy, "normals").is_none());
    assert!(
        Primvar::new(&scene, proxy, "st")
            .unwrap()
            .compute_flattened(Time::Default)
            .unwrap()
            .is_some()
    );
}

fn animate(generated: &mut support::GeneratedScene) {
    let scene = Scene::new(generated.live.stage(), &generated.store);
    let mesh = Mesh::new(&scene, generated.source).unwrap().edit();
    let target = EditTarget::for_node_layer(
        generated.live.stage(),
        &generated.store,
        generated.source,
        NodeId::ROOT,
        support::ASSET,
    )
    .unwrap();
    assert_eq!(
        target.map_to_spec_time(12.),
        1.,
        "stage time maps through the source sublayer offset"
    );
    let mut edit = SchemaEdit::new(generated.live.stage(), &mut generated.store, target);
    mesh.set_points_shared_at(&mut edit, 12., generated.geometry.points.clone());
    let mut animated_points = Vec::with_capacity(32);
    animated_points.extend([[0., 0., 0.], [1., 0., 0.], [1., 3., 0.], [0., 1., 0.]]);
    let owned_pointer = animated_points.as_ptr();
    mesh.set_points_owned_at(&mut edit, 14., animated_points);
    mesh.set_extent_owned_at(&mut edit, 12., vec![[0., 0., 0.], [1., 1., 0.]]);
    mesh.set_extent_owned_at(&mut edit, 14., vec![[0., 0., 0.], [1., 3., 0.]]);
    let transaction = edit.finish();
    generated
        .live
        .apply(&mut generated.store, &transaction)
        .unwrap();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    let source = Mesh::new(&scene, generated.source).unwrap();
    let owned_sample = source
        .points_at(14., layerstack::InterpolationType::Linear)
        .unwrap();
    assert_eq!(
        owned_sample.as_ptr(),
        owned_pointer,
        "schema authoring transfers the element allocation"
    );
    assert_eq!(
        owned_sample.capacity(),
        32,
        "schema authoring preserves spare capacity"
    );
    assert!(
        Arc::ptr_eq(
            &source
                .points_at(12., layerstack::InterpolationType::Linear)
                .unwrap(),
            &generated.geometry.points
        ),
        "exact samples retain their source allocation"
    );
    assert!(
        !Arc::ptr_eq(
            &source
                .points_at(13., layerstack::InterpolationType::Linear)
                .unwrap(),
            &generated.geometry.points
        ),
        "interpolation materializes a new result"
    );
    let inactive = generated.store.tokens.intern("inactiveIds");
    let mut edit = Transaction::new();
    edit.set_metadata(
        EditTarget::for_layer(support::ROOT).prim(generated.scatter),
        inactive,
        FieldValue::Int64ListOp(ListOp::explicit(vec![102])),
    );
    generated.live.apply(&mut generated.store, &edit).unwrap();
}

#[test]
fn missing_prototypes_and_default_republication_retire_previous_samples() {
    let mut generated = support::GeneratedScene::new(4);
    animate(&mut generated);
    let mut cache = BoundsCache::new(Time::at(13.), BoundsOptions::default());
    assert_eq!(
        height(
            &mut cache,
            &Scene::new(generated.live.stage(), &generated.store),
            generated.scatter
        ),
        2.
    );
    let tree = generated.store.path("/Assets/Tree");
    let mut deletion = Transaction::new();
    deletion.remove_spec(EditTarget::for_layer(support::ASSET).prim(tree));
    let removed = generated
        .live
        .apply(&mut generated.store, &deletion)
        .unwrap();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    cache.apply_changes(&scene, &removed.changes);
    assert!(cache.world_bound(&scene, generated.scatter).is_err());
    assert!(
        cache
            .prototype_dependencies(&scene, generated.scatter)
            .contains(&tree)
    );
    let restored = generated
        .live
        .apply(&mut generated.store, &removed.inverse)
        .unwrap();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    cache.apply_changes(&scene, &restored.changes);
    assert_eq!(height(&mut cache, &scene, generated.scatter), 2.);
    // Remove owned samples in authored time without mapping their time twice.
    let target = EditTarget::for_node_layer(
        generated.live.stage(),
        &generated.store,
        generated.source,
        NodeId::ROOT,
        support::ASSET,
    )
    .unwrap();
    let update = generated
        .geometry
        .prepare(
            generated.live.stage(),
            &mut generated.store,
            &target,
            generated.source,
            &generated.publication.properties,
        )
        .unwrap();
    let applied = generated
        .live
        .apply(&mut generated.store, &update.transaction)
        .unwrap();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    cache.apply_changes(&scene, &applied.changes);
    assert_eq!(height(&mut cache, &scene, generated.scatter), 1.);
    let points = Mesh::new(&scene, generated.source)
        .unwrap()
        .points_at(13., layerstack::InterpolationType::Linear)
        .unwrap();
    assert!(Arc::ptr_eq(&points, &generated.geometry.points));
    assert!(
        generated
            .geometry
            .prepare(
                generated.live.stage(),
                &mut generated.store,
                &target,
                generated.source,
                &update.properties
            )
            .unwrap()
            .transaction
            .is_empty()
    );
}

fn report(stage: &Stage, store: &InMemoryStore) -> serde_json::Value {
    let scene = Scene::new(stage, store);
    let find = |text: &str| {
        store
            .paths
            .lookup(
                &layerstack::Path::root().join(
                    &text
                        .split('/')
                        .skip(1)
                        .map(|s| store.tokens.lookup(s).unwrap())
                        .collect::<Vec<_>>(),
                ),
            )
            .unwrap()
    };
    let source = find("/Assets/Tree/Geometry");
    let proxy = find("/World/Native_0/Geometry");
    let scatter = find("/World/Scatter");
    let mesh = Mesh::new(&scene, proxy).unwrap();
    let mut cache = BoundsCache::new(Time::at(13.), BoundsOptions::default());
    let bounds = cache.world_bound(&scene, scatter).unwrap().aligned_range();
    let instancer = PointInstancer::new(&scene, scatter).unwrap();
    let time = Time::at(13.);
    let transforms = instancer
        .compute_instance_transforms(time, time, InstanceTransformOptions::default())
        .unwrap();
    let uv = Primvar::new(&scene, proxy, "st").unwrap();
    let normals = Primvar::new(&scene, proxy, "normals").unwrap();
    let recipe = GenerativeProcedural::new(&scene, find("/Recipes/Tree")).unwrap();
    let mut evaluator = support::asset_generator(recipe.path());
    let generated = evaluator.evaluate(&scene, time).unwrap();
    let texture_shader = find("/Assets/Tree/Material/Texture");
    let source_material = Material::new(&scene, find("/Assets/Tree/Material")).unwrap();
    let texture = stage
        .prim(texture_shader, store)
        .unwrap()
        .attribute("inputs:file")
        .unwrap()
        .get(time)
        .unwrap()
        .value;
    let instance_connections: Vec<Vec<_>> = (0..2)
        .map(|i| {
            stage
                .prim(find(&format!("/World/Native_{i}/Material/Surface")), store)
                .unwrap()
                .attribute("inputs:diffuseColor")
                .unwrap()
                .connections()
                .into_iter()
                .map(|target| target.display(&store.paths, &store.tokens))
                .collect()
        })
        .collect();
    serde_json::json!({
        "nativeInstances": stage.prototypes().flat_map(|p| p.instances().to_vec()).count(),
        "isProxy": stage.is_instance(find("/World/Native_0")),
        "points": &**mesh.points_at(13., layerstack::InterpolationType::Linear).unwrap(),
        "counts": &**mesh.face_vertex_counts().unwrap(),
        "indices": &**mesh.face_vertex_indices().unwrap(),
        "uvIndices": &**uv.indices(time).unwrap(),
        "uv": uv.compute_flattened(time).unwrap().unwrap().array_ref().unwrap().iter().map(|v| layerstack_schemas::value::read_float2(&v, &store.tokens).unwrap()).collect::<Vec<_>>(),
        "normalIndices": &**normals.indices(time).unwrap(),
        "normals": normals.compute_flattened(time).unwrap().unwrap().array_ref().unwrap().iter().map(|v| layerstack_schemas::value::read_float3(&v, &store.tokens).unwrap()).collect::<Vec<_>>(),
        "mask": instancer.compute_mask(time),
        "ids": transforms.iter().map(|t| t.id).collect::<Vec<_>>(),
        "transforms": transforms.iter().map(|t| t.matrix).collect::<Vec<_>>(),
        "bounds": [bounds.min,bounds.max],
        "sourceHeight": Mesh::new(&scene, source).unwrap().points_at(13.,layerstack::InterpolationType::Linear).unwrap()[2][1],
        "material": store.paths.display(mesh.compute_bound_material(&MaterialPurpose::All,BindingOptions::default()).material.unwrap(), &store.tokens).to_string(),
        "userTag": mesh.read_value("user:tag", layerstack_schemas::value::read_int),
        "proceduralSystem": recipe.procedural_system(),
        "recipeHeight": recipe.read_value_at("primvars:height", 13., layerstack::InterpolationType::Linear, layerstack_schemas::value::read_float),
        "recipeGeneratedPoints": &**generated.geometry.points,
        "recipeGeneratedCounts": &**generated.geometry.face_vertex_counts,
        "recipeTexture": generated.texture.as_deref(),
        "textureAsset": layerstack_schemas::value::read_asset(&texture, &store.tokens).unwrap().as_ref(),
        "sourceSurface": store.paths.display(source_material.compute_surface_source(&[]).shader.unwrap(), &store.tokens).to_string(),
        "instanceMaterialConnections": instance_connections,
    })
}

#[test]
fn publication_updates_an_unselected_mesh_without_changing_the_selected_cube() {
    let mut store = InMemoryStore::default();
    support::import(
        &mut store,
        r#"#usda 1.0
def Xform "Asset" (variants = { string shape = "cube" }; prepend variantSets = "shape") {
 variantSet "shape" = {
  "mesh" { def Mesh "Geometry" {} }
  "cube" { def Cube "Geometry" { double size = 7 } }
 }
}
"#,
        support::ROOT,
    );
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..Default::default()
    };
    let mut live = layerstack::LiveStage::compose(&mut store, support::ROOT, options);
    let path = store.path("/Asset/Geometry");
    let variant =
        layerstack::SpecPath::parse("/Asset{shape=mesh}", &mut store.tokens, &mut store.paths)
            .unwrap();
    let target = EditTarget::for_local_variant(support::ROOT, &variant);
    let data = support::mesh();
    let update = data
        .prepare(live.stage(), &mut store, &target, path, &[])
        .expect("the mapped authored Mesh is editable while its sibling Cube is selected");
    live.apply(&mut store, &update.transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    assert!(Mesh::new(&scene, path).is_none());
    let cube = layerstack_schemas::usd_geom::Cube::new(&scene, path).unwrap();
    assert_eq!(cube.size(), Some(7.));
    assert!(
        live.stage()
            .prim(path, &store)
            .unwrap()
            .attribute("points")
            .is_none(),
        "publication into the hidden branch must not leak to the selected Cube"
    );
    let asset = store.path("/Asset");
    let shape = store.tokens.intern("shape");
    let mesh = store.tokens.intern("mesh");
    let mut selection = Transaction::new();
    selection.set_variant_selection(
        EditTarget::for_layer(support::ROOT).prim(asset),
        shape,
        Some(mesh),
    );
    live.apply(&mut store, &selection).unwrap();
    assert!(Arc::ptr_eq(
        &Mesh::new(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .points()
            .unwrap(),
        &data.points,
    ));
}

#[test]
fn publication_checks_the_exact_variant_site_and_rejects_stale_preparations() {
    let mut store = InMemoryStore::default();
    support::import(
        &mut store,
        r#"#usda 1.0
def Xform "Asset" (variants = { string shape = "mesh" }; prepend variantSets = "shape") {
 variantSet "shape" = {
  "mesh" { def Mesh "Geometry" {} }
  "cube" { def Cube "Geometry" {} }
 }
}
"#,
        support::ROOT,
    );
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..Default::default()
    };
    let mut live = layerstack::LiveStage::compose(&mut store, support::ROOT, options);
    let path = store.path("/Asset/Geometry");
    let variant =
        layerstack::SpecPath::parse("/Asset{shape=mesh}", &mut store.tokens, &mut store.paths)
            .unwrap();
    let target = EditTarget::for_local_variant(support::ROOT, &variant);
    let data = support::mesh();
    let update = data
        .prepare(live.stage(), &mut store, &target, path, &[])
        .unwrap();
    live.apply(&mut store, &update.transaction).unwrap();
    let mapped = target.map_to_spec_path(path, &mut store.paths).unwrap();
    assert_eq!(
        store.tokens.resolve(
            store.layers[&support::ROOT]
                .prim_at(&mapped, &store.paths)
                .unwrap()
                .type_name
                .unwrap()
        ),
        "Mesh"
    );
    let variant =
        layerstack::SpecPath::parse("/Asset{shape=cube}", &mut store.tokens, &mut store.paths)
            .unwrap();
    let cube_target = EditTarget::for_local_variant(support::ROOT, &variant);
    assert!(
        matches!(data.prepare(live.stage(), &mut store, &cube_target, path, &[]), Err(MeshPublicationError::NotMesh(p)) if p == path)
    );
    let mut changed = data.clone();
    changed.points = Arc::new(vec![[0., 0., 0.], [1., 0., 0.], [1., 2., 0.], [0., 1., 0.]]);
    let pending = changed
        .prepare(live.stage(), &mut store, &target, path, &update.properties)
        .unwrap();
    let key = store.tokens.intern("documentation");
    let mut intervening = Transaction::new();
    intervening.set_metadata(target.prim(path), key, Value::string("another edit").into());
    live.apply(&mut store, &intervening).unwrap();
    assert!(live.apply(&mut store, &pending.transaction).is_err());
    assert!(Arc::ptr_eq(
        &Mesh::new(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .points()
            .unwrap(),
        &data.points
    ));
}

#[test]
fn authored_layers_round_trip_in_both_formats_and_match_openusd() {
    let mut generated = support::GeneratedScene::new(4);
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:height",
        Value::Float(3.),
    );
    recipe_edit(
        &mut generated,
        support::ASSET,
        "/Recipes/Tree.primvars:texture",
        Value::Asset("textures/moss.png".into()),
    );
    generated.evaluate_asset(Time::Default).unwrap();
    // Bake remains explicit: sampled recipes can be evaluated without replacing output.
    let height = generated
        .store
        .property_path("/Recipes/Tree.primvars:height");
    let target = EditTarget::for_node_layer(
        generated.live.stage(),
        &generated.store,
        generated.asset_generator.recipe(),
        NodeId::ROOT,
        support::ASSET,
    )
    .unwrap();
    let mut edit = Transaction::new();
    edit.set_time_sample(target.property(height), 12., Value::Float(1.));
    edit.set_time_sample(target.property(height), 14., Value::Float(3.));
    generated.live.apply(&mut generated.store, &edit).unwrap();
    let evaluations = generated.asset_generator.work().evaluations;
    let output = generated
        .asset_generator
        .evaluate(
            &Scene::new(generated.live.stage(), &generated.store),
            Time::at(12.),
        )
        .unwrap();
    assert!(
        Arc::ptr_eq(
            &output.geometry.points,
            &Mesh::new(
                &Scene::new(generated.live.stage(), &generated.store),
                generated.terrain
            )
            .unwrap()
            .points()
            .unwrap()
        ),
        "exact unchanged input samples retain their buffers"
    );
    assert_eq!(
        generated
            .asset_generator
            .evaluate(
                &Scene::new(generated.live.stage(), &generated.store),
                Time::at(13.)
            )
            .unwrap()
            .geometry
            .points[2][1],
        2.
    );
    assert_eq!(
        generated.asset_generator.work().evaluations,
        evaluations + 2
    );
    animate(&mut generated);
    let expected = report(generated.live.stage(), &generated.store);
    assert_eq!(
        expected["sourceHeight"], 3.,
        "baked sample at 12 retains the default publication's height 3"
    );
    assert_eq!(expected["recipeHeight"], 2.);
    assert_eq!(expected["recipeGeneratedPoints"][2][1], 2.);
    assert_eq!(expected["textureAsset"], "textures/moss.png");
    assert_eq!(
        expected["instanceMaterialConnections"],
        serde_json::json!([
            ["/World/Native_0/Material/Texture.outputs:rgb"],
            ["/World/Native_1/Material/Texture.outputs:rgb"],
        ])
    );
    assert_eq!(expected["ids"], serde_json::json!([101, 103]));
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("generated-assets");
    std::fs::create_dir_all(&base).unwrap();
    for binary in [false, true] {
        let directory = base.join(if binary { "usdc" } else { "usda" });
        std::fs::create_dir_all(&directory).unwrap();
        let mut imported = InMemoryStore::default();
        for (id, name) in [
            (support::ASSET, "assets.usd"),
            (support::TERRAIN, "terrain.usd"),
            (support::ROOT, "scene.usd"),
        ] {
            let layer = generated.store.layers.get(&id).unwrap();
            let bytes = if binary {
                layerstack_usdc::writer::save_layer(
                    layer,
                    &generated.store.tokens,
                    &generated.store.paths,
                )
                .unwrap()
            } else {
                layerstack_usda::save::save_usda(
                    layer,
                    &generated.store.tokens,
                    &generated.store.paths,
                )
                .unwrap()
                .into_bytes()
            };
            std::fs::write(directory.join(name), &bytes).unwrap();
            if binary {
                let read = layerstack_usdc::read_usdc(
                    &bytes,
                    id,
                    &mut imported.tokens,
                    &mut imported.paths,
                    &mut support::Sources,
                )
                .unwrap();
                imported.insert_layer(read.layer);
            } else {
                support::import(&mut imported, std::str::from_utf8(&bytes).unwrap(), id);
            }
        }
        let options = StageOptions {
            schemas: Some(Arc::new(layerstack_schemas::openusd(&mut imported.tokens))),
            ..Default::default()
        };
        let stage = Stage::compose(&mut imported, support::ROOT, options);
        assert_eq!(report(&stage, &imported), expected);
        let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
        let available = Command::new(&python)
            .args(["-c", "from pxr import Usd"])
            .output()
            .is_ok_and(|r| r.status.success());
        if !available {
            eprintln!("OpenUSD unavailable; set LAYERSTACK_USD_PYTHON to enable the native oracle");
            continue;
        }
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/generated_assets_oracle.py");
        let output = Command::new(&python)
            .arg(script)
            .arg(directory.join("scene.usd"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let oracle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(oracle, expected);
    }
}
