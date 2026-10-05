// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{
    GeneratedMesh, Scene, SchemaEdit,
    usd_geom::{Mesh, PointInstancer},
    usd_shade::Material,
};
use alloc::{sync::Arc, vec};
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, StageOptions, Time, Transaction,
    TypedArray, Value,
};

#[test]
fn transform_only_update_keeps_mesh_and_primvar_components() {
    let (mut store, mut live, path, _) = fixture();
    let mesh = Mesh::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let op = mesh
        .add_translate_op(&mut edit, crate::XformOpPrecision::Double)
        .unwrap();
    op.set(&mut edit, [1., 0., 0.]).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let old = observer.record_at(path).unwrap().clone();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    op.set(&mut edit, [2., 0., 0.]).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let changes = update(&mut observer, &mut live, &mut store);
    let now = observer.record_at(path).unwrap();
    assert_eq!(changes.changed.len(), 1);
    assert!(changes.changed[0].components.transform);
    assert!(!changes.changed[0].components.geometry);
    assert_eq!(changes.work.meshes, 0);
    assert!(Arc::ptr_eq(
        now.mesh.as_ref().unwrap(),
        old.mesh.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(&now.primvars, &old.primvars));
    assert_eq!(now.transform.local_to_world[3][0], 2.);
}

fn triangle() -> GeneratedMesh {
    GeneratedMesh {
        points: Arc::new(vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]),
        face_vertex_counts: Arc::new(vec![3]),
        face_vertex_indices: Arc::new(vec![0, 1, 2]),
        primvars: vec![],
    }
}
fn fixture() -> (InMemoryStore, LiveStage, layerstack::PathId, GeneratedMesh) {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    let path = store.path("/Mesh");
    let mesh = triangle();
    let publication = mesh
        .prepare(
            live.stage(),
            &mut store,
            &EditTarget::for_layer(LayerId(1)),
            path,
            &[],
        )
        .unwrap();
    live.apply(&mut store, &publication.transaction).unwrap();
    (store, live, path, mesh)
}
fn update(
    observer: &mut SceneObserver,
    live: &mut LiveStage,
    store: &mut InMemoryStore,
) -> SceneUpdate {
    observer.update(live, store, Time::Default).unwrap()
}

#[test]
fn initial_records_are_checked_shared_and_noop_has_no_work() {
    let (mut store, mut live, path, mesh) = fixture();
    let mut observer = SceneObserver::default();
    let first = update(&mut observer, &mut live, &mut store);
    assert_eq!(first.added.len(), 1);
    assert_eq!(first.work.meshes, 1);
    let record = observer.record(&first.added[0]).unwrap();
    assert_eq!(record.path, path);
    assert!(Arc::ptr_eq(
        &record.mesh.as_ref().unwrap().polygons.mesh().points,
        &mesh.points
    ));
    let another = SceneObserver::default();
    assert!(another.record(&first.added[0]).is_none());
    let clean = update(&mut observer, &mut live, &mut store);
    assert!(clean.is_empty());
    assert_eq!(clean.work, SceneWork::default());
}

#[test]
fn material_assignment_edits_preserve_geometry_owners() {
    let (mut store, mut live, path, _) = fixture();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let geometry = observer.record_at(path).unwrap().mesh.clone().unwrap();
    let material = store.path("/Material");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Material::define(&mut edit, material);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let binding = store.property_path("/Mesh.material:binding");
    let mut transaction = Transaction::new();
    transaction.set_targets(
        EditTarget::for_layer(LayerId(1)).property(binding),
        layerstack::ListOp::explicit(vec![layerstack::TargetPath::Prim(material)]),
    );
    live.apply(&mut store, &transaction).unwrap();
    let change = update(&mut observer, &mut live, &mut store);
    assert_eq!(change.work.meshes, 0);
    assert!(Arc::ptr_eq(
        &observer.record_at(path).unwrap().mesh.clone().unwrap(),
        &geometry
    ));
    assert_eq!(
        observer.record_at(path).unwrap().materials.binding.material,
        Some(material)
    );
    assert!(
        change
            .changed
            .iter()
            .any(|c| c.components.materials && !c.components.geometry)
    );
    assert_eq!(change.materials, vec![material]);
    assert!(observer.material(material).is_some());
}

#[test]
fn delete_recreate_and_lost_history_retire_handles() {
    let (mut store, mut live, path, mesh) = fixture();
    let mut observer = SceneObserver::default();
    let old = update(&mut observer, &mut live, &mut store).added[0].clone();
    let target = EditTarget::for_layer(LayerId(1));
    let mut deletion = Transaction::new();
    deletion.remove_spec(target.prim(path));
    live.apply(&mut store, &deletion).unwrap();
    let publication = mesh
        .prepare(live.stage(), &mut store, &target, path, &[])
        .unwrap();
    live.apply(&mut store, &publication.transaction).unwrap();
    let change = update(&mut observer, &mut live, &mut store);
    assert!(change.removed.contains(&old));
    assert_eq!(change.added.len(), 1);
    assert!(observer.record(&old).is_none());
    let current = change.added[0].clone();
    live.set_change_history_budget(layerstack::ChangeHistoryBudget {
        max_batches: 0,
        max_retained_bytes: 0,
    });
    let points = store.property_path("/Mesh.points");
    let mut change = Transaction::new();
    change.set_default(
        target.property(points),
        Value::TypedArray(TypedArray::Vec3f(Arc::new(vec![
            [0., 0., 0.],
            [2., 0., 0.],
            [0., 2., 0.],
        ]))),
    );
    live.apply(&mut store, &change).unwrap();
    let recovery = update(&mut observer, &mut live, &mut store);
    assert_eq!(recovery.reset, Some(SceneReset::HistoryLost));
    assert!(observer.record(&current).is_none());
    assert_eq!(recovery.added.len(), 1);
}

#[test]
fn foreign_store_and_invalid_geometry_preserve_last_successful_state() {
    let (mut store, mut live, path, _) = fixture();
    let mut observer = SceneObserver::default();
    let handle = update(&mut observer, &mut live, &mut store).added[0].clone();
    let revision = observer.revision();
    let mut foreign = InMemoryStore::default();
    assert!(matches!(
        observer.update(&mut live, &mut foreign, Time::Default),
        Err(SceneError::ForeignStore)
    ));
    let counts = store.property_path("/Mesh.faceVertexCounts");
    let mut transaction = Transaction::new();
    transaction.set_default(
        EditTarget::for_layer(LayerId(1)).property(counts),
        Value::TypedArray(TypedArray::Int(Arc::new(vec![4]))),
    );
    live.apply(&mut store, &transaction).unwrap();
    assert!(matches!(
        observer.update(&mut live, &mut store, Time::Default),
        Err(SceneError::Mesh { .. })
    ));
    assert_eq!(observer.revision(), revision);
    assert_eq!(observer.record(&handle).unwrap().path, path);
    transaction = Transaction::new();
    transaction.set_default(
        EditTarget::for_layer(LayerId(1)).property(counts),
        Value::TypedArray(TypedArray::Int(Arc::new(vec![3]))),
    );
    live.apply(&mut store, &transaction).unwrap();
    update(&mut observer, &mut live, &mut store);
    assert!(observer.record(&handle).is_some());
}

#[test]
fn point_instances_keep_ids_original_indices_and_masks() {
    let (mut store, mut live, mesh_path, _) = fixture();
    let point_path = store.path("/Scatter");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let points = PointInstancer::define(&mut edit, point_path);
    points
        .set_prototypes(&mut edit, &[layerstack::TargetPath::Prim(mesh_path)])
        .set_proto_indices(&mut edit, &[0, 0])
        .set_positions(&mut edit, &[[0., 0., 0.], [3., 0., 0.]])
        .set_ids(&mut edit, &[42, 99])
        .set_invisible_ids(&mut edit, &[42]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let record = observer.record_at(point_path).unwrap();
    let instances = record.point_instances.as_ref().unwrap();
    assert_eq!(instances.transforms.source_len(), 2);
    assert_eq!(instances.prototypes, vec![mesh_path]);
    let surviving: vec::Vec<_> = instances.surviving().collect();
    assert_eq!(surviving.len(), 1);
    assert_eq!((surviving[0].index, surviving[0].id), (1, 99));
    let all: vec::Vec<_> = instances.transforms.iter().collect();
    assert_eq!(all[0].id, 42);
    let geometry = observer.record_at(mesh_path).unwrap().mesh.clone().unwrap();
    let scene = Scene::new(live.stage(), &store);
    let scatter = PointInstancer::new(&scene, point_path).unwrap().edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    scatter.set_invisible_ids(&mut edit, &[]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let change = update(&mut observer, &mut live, &mut store);
    assert!(change.changed.iter().any(|c| c.components.point_instances));
    assert_eq!(
        observer
            .record_at(point_path)
            .unwrap()
            .point_instances
            .as_ref()
            .unwrap()
            .surviving()
            .count(),
        2
    );
    assert!(Arc::ptr_eq(
        &observer.record_at(mesh_path).unwrap().mesh.clone().unwrap(),
        &geometry
    ));
}

fn native_fixture() -> (
    InMemoryStore,
    LiveStage,
    layerstack::PathId,
    layerstack::PathId,
) {
    use layerstack::{PrimSpec, PropertySpec, PropertyType, Reference};
    let mut store = InMemoryStore::default();
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let asset = store.path("/Asset");
    let geometry = store.path("/Asset/Geom");
    let tint = store.tokens.intern("primvars:tint");
    let xform = store.tokens.intern("Xform");
    let mesh_type = store.tokens.intern("Mesh");
    let mut layer = Layer::new(LayerId(1));
    let child_name = store.tokens.intern("Geom");
    layer.insert_prim(
        asset,
        PrimSpec::class()
            .with_type_name(xform)
            .with_children(vec![child_name]),
    );
    let mesh = triangle();
    let mut spec = PrimSpec::def().with_type_name(mesh_type);
    for (name, value, ty, scalar) in [
        (
            "points",
            TypedArray::Vec3f(mesh.points),
            "point3f",
            Value::Vec3f([0.; 3]),
        ),
        (
            "faceVertexCounts",
            TypedArray::Int(mesh.face_vertex_counts),
            "int",
            Value::Int(0),
        ),
        (
            "faceVertexIndices",
            TypedArray::Int(mesh.face_vertex_indices),
            "int",
            Value::Int(0),
        ),
    ] {
        spec.set_property(
            store.tokens.intern(name),
            PropertySpec::typed_attribute(PropertyType::new(ty, true, scalar))
                .with_default(Value::TypedArray(value)),
        );
    }
    layer.insert_prim(geometry, spec);
    for (name, color) in [("/A", [1., 0., 0.]), ("/B", [0., 1., 0.])] {
        let path = store.path(name);
        let spec = PrimSpec::def()
            .with_type_name(xform)
            .with_reference(Reference::new(LayerId(1), asset))
            .with_instanceable(true)
            .with_property(
                tint,
                PropertySpec::typed_attribute(PropertyType::new(
                    "color3f",
                    false,
                    Value::Vec3f([0.; 3]),
                ))
                .with_default(Value::Vec3f(color)),
            );
        layer.insert_prim(path, spec);
    }
    let a = store.path("/A/Geom");
    let b = store.path("/B/Geom");
    store.insert_layer(layer);
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    (store, live, a, b)
}

#[test]
fn native_geometry_shares_but_inherited_primvars_are_occurrence_local() {
    let (mut store, mut live, a, b) = native_fixture();
    let mut observer = SceneObserver::default();
    let first = update(&mut observer, &mut live, &mut store);
    assert_eq!(first.work.meshes, 1);
    let a_record = observer.record_at(a).unwrap();
    let b_record = observer.record_at(b).unwrap();
    assert!(Arc::ptr_eq(
        a_record.mesh.as_ref().unwrap(),
        b_record.mesh.as_ref().unwrap()
    ));
    assert_eq!(a_record.primvars[0].value, Value::Vec3f([1., 0., 0.]));
    assert_eq!(b_record.primvars[0].value, Value::Vec3f([0., 1., 0.]));
    assert_eq!(b_record.geometry_source.as_ref(), Some(&a_record.handle));
    let root = store.path("/B");
    assert_eq!(
        observer
            .record_at(root)
            .unwrap()
            .native_instance
            .as_ref()
            .unwrap()
            .members
            .len(),
        1
    );
    let geometry = a_record.mesh.clone().unwrap();
    let old_primvars = a_record.primvars.clone();
    let property = store.property_path("/B.primvars:tint");
    let mut change = Transaction::new();
    change.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        Value::Vec3f([0., 0., 1.]),
    );
    live.apply(&mut store, &change).unwrap();
    let changes = update(&mut observer, &mut live, &mut store);
    assert_eq!(changes.work.meshes, 0);
    assert_eq!(
        observer.record_at(b).unwrap().primvars[0].value,
        Value::Vec3f([0., 0., 1.])
    );
    assert!(Arc::ptr_eq(
        &observer.record_at(a).unwrap().primvars,
        &old_primvars
    ));
    assert!(Arc::ptr_eq(
        observer.record_at(b).unwrap().mesh.as_ref().unwrap(),
        &geometry
    ));
    let property = store.property_path("/Asset/Geom.points");
    let mut change = Transaction::new();
    change.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        Value::TypedArray(TypedArray::Vec3f(Arc::new(vec![
            [0., 0., 0.],
            [4., 0., 0.],
            [0., 4., 0.],
        ]))),
    );
    live.apply(&mut store, &change).unwrap();
    let changes = update(&mut observer, &mut live, &mut store);
    assert_eq!(changes.work.meshes, 1);
    assert!(Arc::ptr_eq(
        observer.record_at(a).unwrap().mesh.as_ref().unwrap(),
        observer.record_at(b).unwrap().mesh.as_ref().unwrap()
    ));
    assert_eq!(
        observer
            .record_at(b)
            .unwrap()
            .mesh
            .as_ref()
            .unwrap()
            .polygons
            .mesh()
            .points[1][0],
        4.
    );
}

#[test]
fn inherited_appearance_and_missing_material_target_refresh_without_geometry_work() {
    let (mut store, mut live, a, b) = native_fixture();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let geometry = observer.record_at(a).unwrap().mesh.clone().unwrap();
    let missing = store.path("/Later");
    let binding = store.property_path("/B.material:binding");
    let visibility = store.property_path("/A.visibility");
    let invisible = store.tokens.intern("invisible");
    let mut change = Transaction::new();
    change.set_targets(
        EditTarget::for_layer(LayerId(1)).property(binding),
        layerstack::ListOp::explicit(vec![layerstack::TargetPath::Prim(missing)]),
    );
    change.set_default(
        EditTarget::for_layer(LayerId(1)).property(visibility),
        Value::Token(invisible),
    );
    live.apply(&mut store, &change).unwrap();
    let changes = update(&mut observer, &mut live, &mut store);
    assert_eq!(changes.work.meshes, 0);
    assert_eq!(
        observer.record_at(a).unwrap().appearance.visibility,
        crate::Visibility::Invisible
    );
    assert_eq!(
        observer.record_at(b).unwrap().materials.binding.material,
        None
    );
    assert_eq!(
        observer
            .record_at(b)
            .unwrap()
            .materials
            .binding
            .binding
            .as_ref()
            .unwrap()
            .target,
        missing
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    Material::define(&mut edit, missing);
    let change = edit.finish();
    live.apply(&mut store, &change).unwrap();
    let changes = update(&mut observer, &mut live, &mut store);
    assert_eq!(
        observer.record_at(b).unwrap().materials.binding.material,
        Some(missing)
    );
    assert_eq!(changes.work.meshes, 0);
    assert!(Arc::ptr_eq(
        observer.record_at(a).unwrap().mesh.as_ref().unwrap(),
        &geometry
    ));
}

#[derive(Debug)]
struct FailedArray(layerstack::ArrayReadError);
impl layerstack::DeferredArraySource for FailedArray {
    fn materialize(&self) -> Result<&TypedArray, &layerstack::ArrayReadError> {
        Err(&self.0)
    }
    fn element_kind(&self) -> Value {
        Value::Vec3f([0.; 3])
    }
}

#[test]
fn decode_failure_keeps_property_error_and_does_not_advance_cursor() {
    let (mut store, mut live, path, _) = fixture();
    let mut observer = SceneObserver::default();
    let first = update(&mut observer, &mut live, &mut store);
    let revision = observer.revision();
    let property = store.property_path("/Mesh.points");
    let value = Value::TypedArray(TypedArray::Deferred(Arc::new(FailedArray(
        layerstack::ArrayReadError::InvalidData("corrupt points".into()),
    ))));
    let mut change = Transaction::new();
    change.set_default(EditTarget::for_layer(LayerId(1)).property(property), value);
    live.apply(&mut store, &change).unwrap();
    assert!(
        matches!(observer.update(&mut live, &mut store, Time::Default), Err(SceneError::Decode { property: failed, .. }) if failed == property)
    );
    assert_eq!(observer.revision(), revision);
    assert_eq!(observer.record(&first.added[0]).unwrap().path, path);
}

#[test]
fn changed_point_count_revalidates_primvars_atomically_before_retry() {
    let (mut store, mut live, mesh_path, _) = fixture();
    let path = store.path("/Scatter");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let scatter = PointInstancer::define(&mut edit, path);
    scatter
        .set_prototypes(&mut edit, &[layerstack::TargetPath::Prim(mesh_path)])
        .set_positions(&mut edit, &[[0.; 3]; 2])
        .set_proto_indices(&mut edit, &[0; 2]);
    let primvar = scatter
        .create_primvar(
            &mut edit,
            "weight",
            layerstack::PropertyType::new("float", true, Value::Float(0.)),
        )
        .unwrap();
    primvar
        .set_interpolation(&mut edit, "vertex")
        .unwrap()
        .set(
            &mut edit,
            Value::TypedArray(TypedArray::Float(Arc::new(vec![1., 2.]))),
        )
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let handle = observer.record_at(path).unwrap().handle.clone();
    let revision = observer.revision();
    let old_points = observer
        .record_at(path)
        .unwrap()
        .point_instances
        .clone()
        .unwrap();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    scatter
        .set_positions(&mut edit, &[[0.; 3]; 3])
        .set_proto_indices(&mut edit, &[0; 3]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    assert!(matches!(
        observer.update(&mut live, &mut store, Time::Default),
        Err(SceneError::Mesh { .. })
    ));
    assert_eq!(observer.revision(), revision);
    assert!(Arc::ptr_eq(
        observer
            .record(&handle)
            .unwrap()
            .point_instances
            .as_ref()
            .unwrap(),
        &old_points
    ));
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    primvar
        .set(
            &mut edit,
            Value::TypedArray(TypedArray::Float(Arc::new(vec![1., 2., 3.]))),
        )
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    update(&mut observer, &mut live, &mut store);
    assert_eq!(
        observer
            .record(&handle)
            .unwrap()
            .point_instances
            .as_ref()
            .unwrap()
            .transforms
            .source_len(),
        3
    );
}

#[test]
fn unsupported_geometry_is_explicit_for_ordinary_and_native_instance_roots() {
    for native in [false, true] {
        let mut store = InMemoryStore::default();
        let schemas = Arc::new(crate::openusd(&mut store.tokens));
        let path = store.path("/Sphere");
        let source = store.path("/Asset");
        let ty = store.tokens.intern("Sphere");
        let mut layer = Layer::new(LayerId(1));
        let mut spec = layerstack::PrimSpec::def().with_type_name(ty);
        if native {
            layer.insert_prim(source, layerstack::PrimSpec::class().with_type_name(ty));
            spec = spec
                .with_reference(layerstack::Reference::new(LayerId(1), source))
                .with_instanceable(true);
        }
        layer.insert_prim(path, spec);
        store.insert_layer(layer);
        let mut live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..Default::default()
            },
        );
        let mut observer = SceneObserver::default();
        assert!(
            matches!(observer.update(&mut live, &mut store, Time::Default), Err(SceneError::UnsupportedGeometry { prim, .. }) if prim == path)
        );
        assert!(observer.records().next().is_none());
    }
}

#[test]
fn registry_must_define_geometry_and_shading_not_merely_intern_names() {
    let mut store = InMemoryStore::default();
    let schemas = Arc::new(crate::registry(
        &[crate::Domain::UsdGeom],
        &mut store.tokens,
    ));
    let material = store.tokens.intern("Material");
    let path = store.path("/Material");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(path, layerstack::PrimSpec::def().with_type_name(material));
    store.insert_layer(layer);
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    assert!(matches!(
        SceneObserver::default().update(&mut live, &mut store, Time::Default),
        Err(SceneError::MissingSchemas)
    ));
}

#[test]
fn minimal_custom_mesh_registry_reports_missing_property_without_panicking() {
    let mut store = InMemoryStore::default();
    let mut registry = layerstack::SchemaRegistry::builder();
    for name in ["Mesh", "Material", "PointInstancer"] {
        registry.register(layerstack::SchemaDefinition::typed(
            store.tokens.intern(name),
        ));
    }
    let schemas = Arc::new(registry.build(&mut store.tokens));
    let path = store.path("/Mesh");
    let mesh = store.tokens.lookup("Mesh").unwrap();
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(path, layerstack::PrimSpec::def().with_type_name(mesh));
    store.insert_layer(layer);
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    assert!(
        matches!(SceneObserver::default().update(&mut live, &mut store, Time::Default), Err(SceneError::MissingProperty { prim, name }) if prim == path && name == "points")
    );
}

#[test]
fn equivalent_point_inputs_at_new_time_preserve_snapshot_and_emit_no_delta() {
    let (mut store, mut live, mesh_path, _) = fixture();
    let path = store.path("/Scatter");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let scatter = PointInstancer::define(&mut edit, path);
    scatter
        .set_prototypes(&mut edit, &[layerstack::TargetPath::Prim(mesh_path)])
        .set_positions(&mut edit, &[[0.; 3]; 2])
        .set_proto_indices(&mut edit, &[0; 2]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let inputs = observer
        .record_at(path)
        .unwrap()
        .point_instances
        .clone()
        .unwrap();
    let timed = observer
        .update(&mut live, &mut store, Time::at(1.))
        .unwrap();
    assert!(timed.is_empty());
    assert!(Arc::ptr_eq(
        &inputs,
        observer
            .record_at(path)
            .unwrap()
            .point_instances
            .as_ref()
            .unwrap()
    ));
    let positions = store.property_path("/Scatter.positions");
    let mut change = Transaction::new();
    change.set_default(
        EditTarget::for_layer(LayerId(1)).property(positions),
        Value::TypedArray(TypedArray::Vec3f(Arc::new(vec![[0.; 3]; 2]))),
    );
    live.apply(&mut store, &change).unwrap();
    let timed = observer
        .update(&mut live, &mut store, Time::at(1.))
        .unwrap();
    assert!(timed.is_empty());
    assert!(Arc::ptr_eq(
        &inputs,
        observer
            .record_at(path)
            .unwrap()
            .point_instances
            .as_ref()
            .unwrap()
    ));
    let prototype = Mesh::new(&Scene::new(live.stage(), &store), mesh_path)
        .unwrap()
        .edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let op = prototype
        .add_translate_op(&mut edit, crate::XformOpPrecision::Double)
        .unwrap();
    op.set(&mut edit, [1., 0., 0.]).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let changed = observer
        .update(&mut live, &mut store, Time::at(1.))
        .unwrap();
    assert!(changed.changed.iter().any(|c| c.components.point_instances));
    assert_eq!(
        observer
            .record_at(path)
            .unwrap()
            .point_instances
            .as_ref()
            .unwrap()
            .transforms
            .iter()
            .next()
            .unwrap()
            .matrix[3][0],
        1.
    );
}

#[test]
fn equivalent_geometry_from_new_owners_and_static_time_keeps_validated_owner() {
    let (mut store, mut live, path, mesh) = fixture();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let geometry = observer.record_at(path).unwrap().mesh.clone().unwrap();
    let points = store.property_path("/Mesh.points");
    let mut change = Transaction::new();
    change.set_default(
        EditTarget::for_layer(LayerId(1)).property(points),
        Value::TypedArray(TypedArray::Vec3f(Arc::new(mesh.points.as_ref().clone()))),
    );
    live.apply(&mut store, &change).unwrap();
    let changed = update(&mut observer, &mut live, &mut store);
    assert!(changed.is_empty());
    assert_eq!(changed.work.meshes, 0);
    assert_eq!(changed.work.mesh_comparison_entries, mesh.points.len());
    assert!(Arc::ptr_eq(
        &geometry,
        observer.record_at(path).unwrap().mesh.as_ref().unwrap()
    ));
    let timed = observer
        .update(&mut live, &mut store, Time::at(1.))
        .unwrap();
    assert!(timed.is_empty());
    assert_eq!(timed.work.meshes, 0);
    assert!(Arc::ptr_eq(
        &geometry,
        observer.record_at(path).unwrap().mesh.as_ref().unwrap()
    ));
}

#[test]
fn explicit_clear_rebinds_store_without_reusing_old_handles() {
    let (mut store, mut live, _, _) = fixture();
    let mut observer = SceneObserver::default();
    let old = update(&mut observer, &mut live, &mut store).added[0].clone();
    let (mut replacement, mut new_stage, _, _) = fixture();
    assert!(matches!(
        observer.update(&mut new_stage, &mut replacement, Time::Default),
        Err(SceneError::ForeignStore)
    ));
    observer.clear();
    assert!(observer.record(&old).is_none());
    assert!(observer.store_identity().is_none());
    let new = update(&mut observer, &mut new_stage, &mut replacement).added[0].clone();
    assert_ne!(old, new);
    assert!(observer.record(&old).is_none());
}

#[test]
fn indexed_primvars_keep_source_mapping_and_reject_bad_indices_atomically() {
    let (mut store, mut live, path, mut mesh) = fixture();
    let indices = Arc::new(vec![0, 1, 2]);
    let values = Arc::new(vec![[0., 0.], [1., 0.], [0., 1.]]);
    mesh.primvars.push(crate::MeshPrimvar {
        name: "primvars:st".into(),
        type_name: layerstack::PropertyType::new("texCoord2f", true, Value::Vec2f([0.; 2])),
        value: Value::TypedArray(TypedArray::Vec2f(values.clone())),
        interpolation: "faceVarying".into(),
        element_size: 1,
        indices: Some(indices.clone()),
    });
    let publication = mesh
        .prepare(
            live.stage(),
            &mut store,
            &EditTarget::for_layer(LayerId(1)),
            path,
            &[],
        )
        .unwrap();
    live.apply(&mut store, &publication.transaction).unwrap();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let primvars = observer.record_at(path).unwrap().primvars.clone();
    assert!(Arc::ptr_eq(primvars[0].indices.as_ref().unwrap(), &indices));
    assert!(
        matches!(&primvars[0].value, Value::TypedArray(TypedArray::Vec2f(captured)) if Arc::ptr_eq(captured, &values))
    );
    let revision = observer.revision();
    let property = store.property_path("/Mesh.primvars:st:indices");
    let mut change = Transaction::new();
    change.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        Value::TypedArray(TypedArray::Int(Arc::new(vec![0, 3, 2]))),
    );
    live.apply(&mut store, &change).unwrap();
    assert!(matches!(
        observer.update(&mut live, &mut store, Time::Default),
        Err(SceneError::Primvar {
            source: crate::primvar::PrimvarError::InvalidIndex { .. },
            ..
        })
    ));
    assert_eq!(observer.revision(), revision);
    assert!(Arc::ptr_eq(
        &observer.record_at(path).unwrap().primvars,
        &primvars
    ));
}

#[test]
fn removing_native_representative_retains_surviving_geometry_and_retargets_links() {
    let (mut store, mut live, a, b) = native_fixture();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let a_handle = observer.record_at(a).unwrap().handle.clone();
    let b_handle = observer.record_at(b).unwrap().handle.clone();
    let geometry = observer.record_at(b).unwrap().mesh.clone().unwrap();
    let root = store.path("/A");
    let mut deletion = Transaction::new();
    deletion.remove_spec(EditTarget::for_layer(LayerId(1)).prim(root));
    live.apply(&mut store, &deletion).unwrap();
    let changes = update(&mut observer, &mut live, &mut store);
    assert!(changes.removed.contains(&a_handle));
    assert!(observer.record(&a_handle).is_none());
    let surviving = observer.record(&b_handle).unwrap();
    assert!(surviving.geometry_source.is_none());
    assert!(Arc::ptr_eq(surviving.mesh.as_ref().unwrap(), &geometry));
    assert_eq!(changes.work.meshes, 0);
}

#[test]
fn face_subsets_refresh_material_components_and_invalid_family_is_atomic() {
    use crate::usd_geom::{GeomSubset, GeomSubsetElementType};
    let (mut store, mut live, mesh, _) = fixture();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    let geometry = observer.record_at(mesh).unwrap().mesh.clone().unwrap();
    let subset = store.path("/Mesh/Face");
    let material = store.path("/Paint");
    let indices = Arc::new(vec![0]);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let face = GeomSubset::define(&mut edit, subset);
    face.set_element_type(&mut edit, GeomSubsetElementType::Face)
        .set_family_name(&mut edit, "materialBind")
        .set_indices_shared(&mut edit, indices.clone());
    Material::define(&mut edit, material);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let family = store.property_path("/Mesh.subsetFamily:materialBind:familyType");
    let non_overlapping = store.tokens.intern("nonOverlapping");
    let binding = store.property_path("/Mesh/Face.material:binding");
    let target = EditTarget::for_layer(LayerId(1));
    let mut transaction = Transaction::new();
    transaction.create_property(
        target.property(family),
        layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
            "token",
            false,
            Value::Token(non_overlapping),
        ))
        .with_default(Value::Token(non_overlapping)),
    );
    transaction.set_targets(
        target.property(binding),
        layerstack::ListOp::explicit(vec![layerstack::TargetPath::Prim(material)]),
    );
    live.apply(&mut store, &transaction).unwrap();
    let changes = update(&mut observer, &mut live, &mut store);
    let record = observer.record_at(mesh).unwrap();
    let family = record.materials.subsets.as_ref().unwrap();
    assert_eq!(family.subsets.len(), 1);
    assert!(Arc::ptr_eq(&family.subsets[0].indices, &indices));
    assert_eq!(family.subsets[0].material.material, Some(material));
    assert!(
        changes
            .changed
            .iter()
            .any(|c| c.components.materials && !c.components.geometry)
    );
    assert!(Arc::ptr_eq(record.mesh.as_ref().unwrap(), &geometry));
    let old_materials = record.materials.clone();
    let revision = observer.revision();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
    face.set_indices(&mut edit, &[2]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    assert!(matches!(
        observer.update(&mut live, &mut store, Time::Default),
        Err(SceneError::Materials { .. })
    ));
    assert_eq!(observer.revision(), revision);
    assert!(Arc::ptr_eq(
        &observer.record_at(mesh).unwrap().materials,
        &old_materials
    ));
}

#[test]
fn motion_time_change_updates_point_snapshot_without_geometry_revalidation() {
    let (mut store, mut live, mesh, _) = fixture();
    let path = store.path("/Scatter");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let scatter = PointInstancer::define(&mut edit, path);
    scatter
        .set_prototypes(&mut edit, &[layerstack::TargetPath::Prim(mesh)])
        .set_proto_indices(&mut edit, &[0])
        .set_positions_at(&mut edit, 0., &[[0.; 3]])
        .set_velocities_at(&mut edit, 0., &[[1., 0., 0.]]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut observer = SceneObserver::default();
    observer
        .update(&mut live, &mut store, Time::at(0.))
        .unwrap();
    let old = observer
        .record_at(path)
        .unwrap()
        .point_instances
        .clone()
        .unwrap();
    let later = observer
        .update(&mut live, &mut store, Time::at(24.))
        .unwrap();
    let now = observer
        .record_at(path)
        .unwrap()
        .point_instances
        .as_ref()
        .unwrap();
    assert!(!Arc::ptr_eq(&old, now));
    assert!(later.changed.iter().any(|c| c.components.point_instances));
    assert_ne!(
        old.transforms.iter().next().unwrap().matrix,
        now.transforms.iter().next().unwrap().matrix
    );
    assert_eq!(later.work.meshes, 0);
    let before_rate = now.transforms.iter().next().unwrap().matrix;
    let rate = store.tokens.intern("timeCodesPerSecond");
    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .set_metadata(rate, Value::Double(48.));
    let rate_change = observer
        .update(&mut live, &mut store, Time::at(24.))
        .unwrap();
    assert!(
        rate_change
            .changed
            .iter()
            .any(|c| c.components.point_instances)
    );
    assert_ne!(
        before_rate,
        observer
            .record_at(path)
            .unwrap()
            .point_instances
            .as_ref()
            .unwrap()
            .transforms
            .iter()
            .next()
            .unwrap()
            .matrix
    );
    assert_eq!(rate_change.work.meshes, 0);
}

#[test]
fn replacement_stage_in_same_domain_reports_reset_and_retires_all_handles() {
    let (mut store, mut live, _, _) = fixture();
    let mut observer = SceneObserver::default();
    let old = update(&mut observer, &mut live, &mut store).added[0].clone();
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let mut replacement = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    let rebuilt = update(&mut observer, &mut replacement, &mut store);
    assert_eq!(rebuilt.reset, Some(SceneReset::DifferentStage));
    assert!(rebuilt.removed.contains(&old));
    assert!(observer.record(&old).is_none());
    assert_eq!(rebuilt.added.len(), 1);
    assert_ne!(rebuilt.added[0], old);
}

#[test]
fn external_primvar_id_forwarding_refreshes_when_empty_or_missing_targets_change() {
    let (mut store, mut live, mesh, _) = fixture();
    let relay = store.path("/Relay");
    let forwarded = store.property_path("/Relay.forward");
    let destination = store.path("/Destination");
    let target = EditTarget::for_layer(LayerId(1));
    let mut transaction = Transaction::new();
    transaction.create_prim(target.prim(relay), layerstack::Specifier::Def, None);
    transaction.create_property(
        target.property(forwarded),
        layerstack::PropertySpec::relationship(),
    );
    live.apply(&mut store, &transaction).unwrap();
    let mesh_edit = Mesh::new(&Scene::new(live.stage(), &store), mesh)
        .unwrap()
        .edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target.clone());
    let id = mesh_edit
        .create_primvar(
            &mut edit,
            "id",
            layerstack::PropertyType::new("string", false, Value::string("")),
        )
        .unwrap();
    id.set(&mut edit, Value::string("local"))
        .unwrap()
        .set_id_target(&mut edit, Some(layerstack::TargetPath::Property(forwarded)))
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut observer = SceneObserver::default();
    update(&mut observer, &mut live, &mut store);
    assert!(observer.record_at(mesh).unwrap().primvars.is_empty());
    assert!(observer.record_at(mesh).unwrap().has_id_target_primvars);
    let geometry = observer.record_at(mesh).unwrap().mesh.clone().unwrap();
    let mut transaction = Transaction::new();
    transaction.set_targets(
        target.property(forwarded),
        layerstack::ListOp::explicit(vec![layerstack::TargetPath::Prim(destination)]),
    );
    live.apply(&mut store, &transaction).unwrap();
    let changed = update(&mut observer, &mut live, &mut store);
    assert!(changed.changed.iter().any(|c| c.components.primvars));
    assert_eq!(
        observer.record_at(mesh).unwrap().primvars[0].value,
        Value::string("/Destination")
    );
    assert!(Arc::ptr_eq(
        observer.record_at(mesh).unwrap().mesh.as_ref().unwrap(),
        &geometry
    ));
}
