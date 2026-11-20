// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{BatchError, LayerBatch};
use crate::{
    Scene, SchemaEdit, openusd,
    usd::CollectionApi,
    usd_geom::{Mesh, MeshSubdivisionScheme, Xform},
};
use alloc::{sync::Arc, vec};
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, Path, PropertyKind, PropertyType,
    Specifier, Stage, StageOptions, TargetPath, TypedArray, Value, Variability,
};

#[test]
fn authored_fields_match_generated_edits_and_compose_through_schema_views() {
    let mut store = InMemoryStore::default();
    let path = store.path("/Mesh");
    let target = store.path("/Target");
    let schemas = Arc::new(openusd(store.tokens()));
    let options = StageOptions {
        schemas: Some(schemas.clone()),
        ..StageOptions::default()
    };
    let none = store.tokens().intern("none");
    let points = Arc::new(vec![[0., 1., 2.], [3., 4., 5.]]);
    let generated_id = LayerId(1);
    store.insert_layer(Layer::new(generated_id));
    let mut live = LiveStage::compose(&mut store, generated_id, options.clone());
    let mut edit = SchemaEdit::new(
        live.stage(),
        &mut store,
        EditTarget::for_layer(generated_id),
    );
    let mesh = Mesh::define(&mut edit, path).unwrap();
    mesh.set_points_shared(&mut edit, points.clone());
    mesh.set_subdivision_scheme(&mut edit, MeshSubdivisionScheme::None);
    CollectionApi::apply(&mut edit, path, "parts")
        .unwrap()
        .set_includes(&mut edit, &[TargetPath::Prim(target)]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let id = LayerId(2);
    let mut batch = LayerBatch::new(id, &store, &schemas).unwrap();
    batch
        .define(path, Mesh::SCHEMA, |prim| {
            prim.set(
                "points",
                Value::TypedArray(TypedArray::Vec3f(points.clone())),
            )?;
            prim.set(Mesh::SUBDIVISION_SCHEME, Value::Token(none))?;
            prim.apply(CollectionApi::SCHEMA, Some("parts"))?
                .set_targets(CollectionApi::INCLUDES, vec![TargetPath::Prim(target)])?;
            Ok(())
        })
        .unwrap();
    batch.define(target, Xform::SCHEMA, |_| Ok(())).unwrap();
    let layer = batch.finish();
    let authored = layer.prim(path).unwrap();
    let generated = store.layer(generated_id).unwrap().prim(path).unwrap();
    assert_eq!(authored.fields, generated.fields, "applied schema list op");
    assert_eq!(authored.properties, generated.properties);
    assert_eq!(authored.properties.len(), 3, "only the requested opinions");
    let prop = |name: &str| {
        authored
            .property(store.tokens().lookup(name).unwrap())
            .unwrap()
    };
    assert_eq!(
        prop("points")
            .type_name
            .as_ref()
            .map(PropertyType::type_name),
        Some("point3f[]")
    );
    assert_eq!(prop("subdivisionScheme").variability, Variability::Uniform);
    let Some(Value::TypedArray(TypedArray::Vec3f(authored_points))) = &prop("points").default
    else {
        panic!("points should retain the transferred native array");
    };
    assert!(Arc::ptr_eq(authored_points, &points));

    store.insert_layer(layer);
    let stage = Stage::compose(&mut store, id, options);
    let scene = Scene::new(&stage, &store);
    let mesh = Mesh::get(&scene, path).unwrap();
    assert_eq!(mesh.points().as_deref(), Some(points.as_ref()));
    assert_eq!(mesh.subdivision_scheme(), Some(MeshSubdivisionScheme::None));
    assert_eq!(
        CollectionApi::get(&scene, path, "parts")
            .unwrap()
            .includes(),
        [TargetPath::Prim(target)]
    );
}

#[test]
fn child_first_definitions_preserve_authored_children_and_leave_missing_ancestors_untyped() {
    let store = InMemoryStore::default();
    let child = store.path("/Root/Group/Child");
    let sibling = store.path("/Root/Group/Sibling");
    let group = store.path("/Root/Group");
    let root = store.path("/Root");
    let schemas = openusd(store.tokens());
    let mut batch = LayerBatch::new(LayerId(1), &store, &schemas).unwrap();
    batch.define(child, Mesh::SCHEMA, |_| Ok(())).unwrap();
    batch.define(sibling, Mesh::SCHEMA, |_| Ok(())).unwrap();
    batch.define(group, Xform::SCHEMA, |_| Ok(())).unwrap();
    assert_eq!(
        batch.define(group, Mesh::SCHEMA, |_| Ok(())),
        Err(BatchError::AlreadyDefined("/Root/Group".into()))
    );

    let layer = batch.finish();
    let root_spec = layer.prim(root).unwrap();
    assert_eq!(root_spec.specifier, Some(Specifier::Def));
    assert_eq!(root_spec.type_name, None);
    let group_spec = layer.prim(group).unwrap();
    assert_eq!(group_spec.type_name, store.tokens().lookup("Xform"));
    assert_eq!(
        group_spec.authored_children,
        [
            store.tokens().intern("Child"),
            store.tokens().intern("Sibling")
        ]
    );
    assert_eq!(
        layer.prim(store.path("/")).unwrap().authored_children,
        [store.tokens().intern("Root")]
    );
}

#[test]
fn failed_definitions_leave_no_prim_or_ancestor_and_can_be_retried() {
    let store = InMemoryStore::default();
    let path = store.path("/Group/Mesh");
    let orphan = store.path("/Discarded/Child");
    let invalid = store
        .paths()
        .intern(Path::root().join(&[store.tokens().intern("not a name")]));
    let schemas = openusd(store.tokens());
    let mut batch = LayerBatch::new(LayerId(1), &store, &schemas).unwrap();
    assert_eq!(
        batch.define(invalid, Mesh::SCHEMA, |_| Ok(())),
        Err(BatchError::InvalidPath("/not a name".into()))
    );
    for schema in ["UnknownSchema", "Mesh\0unexpected", CollectionApi::SCHEMA] {
        assert_eq!(
            batch.define(path, schema, |_| Ok(())),
            Err(BatchError::NotConcreteSchema(schema.into()))
        );
    }
    let bad_property = batch.define(orphan, Mesh::SCHEMA, |prim| {
        prim.set("points", Value::array(vec![Value::Vec3f([0.; 3])]))?;
        prim.set("unknownAttribute", Value::Bool(true))
    });
    assert_eq!(
        bad_property,
        Err(BatchError::UnknownProperty("unknownAttribute".into()))
    );
    let mixed = Value::array(vec![Value::Vec3f([0.; 3]), Value::Int(1)]);
    assert_eq!(
        batch.define(path, Mesh::SCHEMA, |prim| prim.set("points", mixed)),
        Err(BatchError::ValueTypeMismatch {
            name: "points".into(),
            expected: PropertyType::parse("point3f[]").unwrap()
        })
    );
    assert!(matches!(
        batch.define(path, Mesh::SCHEMA, |prim| {
            prim.apply(CollectionApi::SCHEMA, None)?;
            Ok(())
        }),
        Err(BatchError::CannotApply { .. })
    ));
    assert_eq!(
        batch.define(path, Mesh::SCHEMA, |prim| prim
            .set_targets("points", vec![])),
        Err(BatchError::WrongPropertyKind {
            name: "points".into(),
            expected: PropertyKind::Relationship
        })
    );
    assert!(matches!(
        batch.define(path, Mesh::SCHEMA, |prim| {
            prim.apply(CollectionApi::SCHEMA, Some("parts"))?
                .set(CollectionApi::INCLUDES, Value::Bool(true))
        }),
        Err(BatchError::WrongPropertyKind {
            expected: PropertyKind::Attribute,
            ..
        })
    ));
    let target = TargetPath::Prim(path);
    assert!(matches!(
        batch.define(path, Mesh::SCHEMA, |prim| {
            prim.apply(CollectionApi::SCHEMA, Some("parts"))?
                .set_targets(CollectionApi::INCLUDES, vec![target, target])
        }),
        Err(BatchError::DuplicateTarget { target: repeated, .. }) if repeated == target
    ));
    batch.define(path, Mesh::SCHEMA, |_| Ok(())).unwrap();

    let layer = batch.finish();
    assert_eq!(
        layer.prim(store.path("/")).unwrap().authored_children,
        [store.tokens().intern("Group")]
    );
    assert!(layer.prim(path).unwrap().properties.is_empty());
    assert!(layer.prim(path).unwrap().fields.is_empty());
    assert!(layer.prim(orphan).is_none());
    assert!(layer.prim(store.path("/Discarded")).is_none());
}

#[test]
fn schema_tokens_must_belong_to_the_store_that_will_own_the_layer() {
    let owner = InMemoryStore::default();
    let other = InMemoryStore::default();
    let schemas = openusd(owner.tokens());
    assert!(matches!(
        LayerBatch::new(LayerId(1), &other, &schemas),
        Err(BatchError::ForeignSchemaRegistry)
    ));
}

#[test]
fn repeated_writes_replace_the_opinion_and_repeated_api_application_is_idempotent() {
    let mut store = InMemoryStore::default();
    let path = store.path("/Mesh");
    let first = store.path("/First");
    let last = store.path("/Last");
    let schemas = Arc::new(openusd(store.tokens()));
    let id = LayerId(1);
    let mut batch = LayerBatch::new(id, &store, &schemas).unwrap();
    batch
        .define(path, Mesh::SCHEMA, |prim| {
            for point in [[1.; 3], [2.; 3]] {
                prim.set("points", Value::array(vec![Value::Vec3f(point)]))?;
            }
            for target in [first, last] {
                prim.apply(CollectionApi::SCHEMA, Some("parts"))?
                    .set_targets(CollectionApi::INCLUDES, vec![TargetPath::Prim(target)])?;
            }
            Ok(())
        })
        .unwrap();
    let layer = batch.finish();
    let spec = layer.prim(path).unwrap();
    assert_eq!(spec.properties.len(), 2);
    store.insert_layer(layer);
    let stage = Stage::compose(
        &mut store,
        id,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let scene = Scene::new(&stage, &store);
    assert_eq!(
        Mesh::get(&scene, path)
            .unwrap()
            .points()
            .unwrap()
            .as_slice(),
        &[[2.; 3]]
    );
    assert_eq!(CollectionApi::instances(&scene, path).len(), 1);
    assert_eq!(
        CollectionApi::get(&scene, path, "parts")
            .unwrap()
            .includes(),
        [TargetPath::Prim(last)]
    );
}

#[test]
fn applied_api_writes_use_the_composed_declaration_and_earlier_applications() {
    use layerstack::{CannotApply, PropertyDefinition, SchemaDefinition, SchemaRegistryBuilder};

    let store = InMemoryStore::default();
    let path = store.path("/Panel");
    let t = store.tokens();
    let attribute = |name, ty| {
        PropertyDefinition::attribute(t.intern(name)).with_type(PropertyType::parse(ty).unwrap())
    };
    let mut builder = SchemaRegistryBuilder::default();
    builder.register(
        SchemaDefinition::typed(t.intern("Panel")).with_property(attribute("size", "int")),
    );
    let measures = t.intern("MeasuresAPI");
    builder.register(SchemaDefinition::api(measures).with_property(attribute("weight", "double")));
    builder.register(
        SchemaDefinition::api(t.intern("DisplayAPI"))
            .with_can_only_apply_to(measures)
            .with_property(attribute("size", "float"))
            .with_property(attribute("weight", "float"))
            .with_property(attribute("display", "bool")),
    );
    let schemas = builder.build(t);
    let mut batch = LayerBatch::new(LayerId(1), &store, &schemas).unwrap();
    batch
        .define(path, "Panel", |prim| {
            assert!(matches!(
                prim.apply("DisplayAPI", None),
                Err(BatchError::CannotApply {
                    reason: CannotApply::NotATarget { .. },
                    ..
                })
            ));
            let mut measures = prim.apply("MeasuresAPI", None)?;
            assert_eq!(
                measures.set("size", Value::Int(3)),
                Err(BatchError::UnknownProperty("size".into()))
            );
            let mut display = prim.apply("DisplayAPI", None)?;
            display.set("size", Value::Int(3))?;
            display.set("weight", Value::Double(0.5))?;
            display.set("display", Value::Bool(true))
        })
        .unwrap();
    let layer = batch.finish();
    let prim = layer.prim(path).unwrap();
    for (name, expected) in [("size", "int"), ("weight", "double"), ("display", "bool")] {
        let prop = prim.property(t.intern(name)).unwrap();
        assert_eq!(prop.type_name.as_ref().unwrap().type_name(), expected);
    }
}
