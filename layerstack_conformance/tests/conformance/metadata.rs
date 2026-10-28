// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plugin metadata registration and composed reads match OpenUSD 26.8.
use layerstack::{
    FieldValue, InMemoryStore, Layer, LayerId, ListOp, MetadataTarget, PrimSpec, PropertyEntry,
    PropertySpec, Stage, StageOptions, SublayerEntry, Value,
};
use layerstack_schemas::{Domain, PrimView, Scene};
use std::sync::Arc;

#[test]
fn generated_metadata_retains_types_targets_and_defaults_without_creating_properties() {
    let store = InMemoryStore::default();
    let registry = layerstack_schemas::registry(&[Domain::UsdShade], store.tokens());
    let render_type = registry
        .metadata(store.tokens().lookup("renderType").unwrap())
        .unwrap();
    assert_eq!(render_type.value_type.name(), "token");
    assert!(render_type.applies_to(MetadataTarget::Attribute));
    assert!(render_type.applies_to(MetadataTarget::Relationship));
    assert!(!render_type.applies_to(MetadataTarget::Prim));
    assert!(
        registry
            .metadata(store.tokens().intern("metersPerUnit"))
            .is_none()
    );
    let shader = registry
        .schema_definition(store.tokens().lookup("Shader").unwrap())
        .unwrap();
    assert!(
        shader
            .property(store.tokens().lookup("connectability").unwrap())
            .is_none()
    );
    let mut builder = layerstack::SchemaRegistry::builder();
    builder
        .register_metadata(
            layerstack::MetadataDefinition::new(
                store.tokens().intern("colorSpaceHint"),
                layerstack::MetadataType::parse("int").unwrap(),
                [MetadataTarget::Attribute],
            ),
            store.tokens(),
        )
        .unwrap();
    // usdShade's own field, declared otherwise: refused, as OpenUSD keeps
    // the first registration.
    assert!(
        builder
            .register_metadata(
                layerstack::MetadataDefinition::new(
                    store.tokens().intern("connectability"),
                    layerstack::MetadataType::parse("int").unwrap(),
                    [MetadataTarget::Attribute],
                ),
                store.tokens(),
            )
            .is_err()
    );
    assert!(
        layerstack_schemas::register(&mut builder, &[Domain::UsdShade], store.tokens()).is_ok(),
        "usdShade's tables declare its fields as the built-in table does"
    );
}

#[test]
fn composed_metadata_and_layer_defaults_match_cpp() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../../fixtures/metadata.json")).unwrap();
    assert_eq!(oracle["version"], layerstack_schemas::OPENUSD_VERSION);
    let mut store = InMemoryStore::default();
    let mesh = store.path("/Mesh");
    let missing = store.path("/Missing");
    let mesh_type = store.tokens().intern("Mesh");
    let normals = store.tokens().intern("normals");
    let binding = store.tokens().intern("binding");
    let interpolation = store.tokens().intern("interpolation");
    let sdr = store.tokens().intern("sdrMetadata");
    let render_type = store.tokens().intern("renderType");
    let inactive = store.tokens().intern("inactiveIds");
    let meters = store.tokens().intern("metersPerUnit");
    let registry = Arc::new(layerstack_schemas::openusd(store.tokens()));
    let mut root = Layer::new(LayerId(1));
    root.sublayers_mut().push(SublayerEntry::new(LayerId(2)));
    root.insert_prim(mesh, PrimSpec::def().with_type_name(mesh_type));
    root.prim_mut(mesh)
        .unwrap()
        .properties
        .push(entry(normals, PropertySpec::attribute()));
    store.insert_layer(root);
    store.insert_layer(Layer::new(LayerId(2)));
    let options = StageOptions {
        schemas: Some(registry),
        ..StageOptions::default()
    };
    let read_defaults = |store: &mut InMemoryStore| {
        let stage = Stage::compose(store, LayerId(1), options.clone());
        assert_eq!(stage.root_layer(), Some(LayerId(1)));
        let scene = Scene::new(&stage, store);
        let metadata = scene.metadata();
        assert_eq!(
            serde_json::json!({"metersPerUnit": metadata.meters_per_unit(), "upAxis": metadata.up_axis(),
            "kilogramsPerUnit": metadata.kilograms_per_unit(), "renderSettingsPrimPath": metadata.render_settings_prim_path().as_deref()}),
            oracle["stage_defaults"]
        );
        let prim = PrimView::new(scene, mesh);
        let property = prim.property_metadata("normals").unwrap();
        assert_eq!(
            serde_json::json!({"interpolation": property.interpolation(), "elementSize": property.element_size(),
            "unauthoredValuesIndex": property.unauthored_values_index()}),
            oracle["property_defaults"]
        );
        assert!(
            PrimView::new(scene, missing)
                .property_metadata("normals")
                .is_none()
        );
        assert!(prim.property_metadata("absent").is_none());
        assert!(
            prim.property_metadata("points").is_some(),
            "schema-defined attributes exist"
        );
    };
    read_defaults(&mut store);
    store
        .layer_mut(LayerId(2))
        .unwrap()
        .set_metadata(meters, Value::Double(10.0));
    read_defaults(&mut store);
    let mut weak = PrimSpec::over();
    weak.set_field(
        inactive,
        FieldValue::Int64ListOp(Box::new(ListOp::explicit(vec![1, 2]))),
    );
    weak.properties.push(entry(
        normals,
        PropertySpec::attribute()
            .with_metadata(interpolation, Value::Token(store.tokens().intern("vertex")))
            .with_metadata(
                sdr,
                Value::Dictionary(vec![
                    ("weak".into(), Value::string("yes")),
                    (
                        "nested".into(),
                        Value::Dictionary(vec![("left".into(), Value::Int(1))]),
                    ),
                ]),
            ),
    ));
    store.layer_mut(LayerId(2)).unwrap().insert_prim(mesh, weak);
    let terminal = store.tokens().intern("terminal");
    let root = store.layer_mut(LayerId(1)).unwrap();
    root.set_metadata(meters, Value::Double(0.5));
    let spec = root.prim_mut(mesh).unwrap();
    spec.set_field(
        inactive,
        FieldValue::Int64ListOp(Box::new(ListOp::appended(vec![3]).with_deleted(vec![1]))),
    );
    Arc::make_mut(&mut spec.properties[0].spec).set_metadata(
        sdr,
        Value::Dictionary(vec![
            ("strong".into(), Value::string("yes")),
            (
                "nested".into(),
                Value::Dictionary(vec![("right".into(), Value::Int(2))]),
            ),
        ]),
    );
    spec.properties.push(entry(
        binding,
        PropertySpec::relationship().with_metadata(render_type, Value::Token(terminal)),
    ));
    let stage = Stage::compose(&mut store, LayerId(1), options);
    let scene = Scene::new(&stage, &store);
    let prim = PrimView::new(scene, mesh);
    let property = prim.property_metadata("normals").unwrap();
    assert_eq!(
        serde_json::json!(scene.metadata().meters_per_unit()),
        oracle["authored_stage"]
    );
    assert_eq!(
        serde_json::json!(property.interpolation()),
        oracle["interpolation"]
    );
    assert_eq!(
        serde_json::json!(prim.inactive_ids()),
        oracle["inactive_ids"]
    );
    assert_eq!(
        serde_json::json!(prim.property_metadata("binding").unwrap().render_type()),
        oracle["relationship_render_type"]
    );
    assert!(
        prim.property_metadata("binding")
            .unwrap()
            .interpolation()
            .is_none(),
        "target kinds are checked"
    );
    fn json(value: &Value) -> serde_json::Value {
        match value {
            Value::String(text) => serde_json::json!(&**text),
            Value::Int(number) => serde_json::json!(number),
            Value::Dictionary(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.to_string(), json(value)))
                    .collect(),
            ),
            other => panic!("unexpected fixture value: {other:?}"),
        }
    }
    assert_eq!(
        json(&Value::Dictionary(property.sdr_metadata().unwrap())),
        oracle["dictionary"]
    );
}

fn entry(name: layerstack::TokenId, spec: PropertySpec) -> PropertyEntry {
    PropertyEntry {
        name,
        spec: spec.into(),
    }
}

#[test]
fn empty_stage_retains_its_root_and_reads_only_layer_applicable_defaults() {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
    let element_size = store.tokens().lookup("elementSize").unwrap();
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    assert_eq!(Scene::new(&stage, &store).metadata().up_axis(), Some("Y"));
    assert!(stage.layer_metadata(element_size, &store).is_none());
}

use crate::support::schema_scene as support;

#[test]
fn registered_asset_array_metadata_retains_its_crate_type() {
    use layerstack_usdc::{CrateFile, DecodeBudget, value_rep::CrateValue, value_type::ValueType};
    let (store, _) = support::scene(
        r#"#usda 1.0
        def "Empty" (payloadAssetDependencies = []) {}
        def "Populated" (payloadAssetDependencies = [@a.usda@, @b.usda@]) {}
    "#,
    );
    let bytes = layerstack_usdc::writer::save_layer(
        store.layer(LayerId(1)).unwrap(),
        store.tokens(),
        store.paths(),
    )
    .unwrap();
    let mut budget = DecodeBudget::for_input(bytes.len());
    let file = CrateFile::open(&bytes, &mut budget).unwrap();
    for (path, expected) in [("/Empty", vec![]), ("/Populated", vec!["a.usda", "b.usda"])] {
        let field = file
            .spec(path)
            .unwrap()
            .field("payloadAssetDependencies")
            .unwrap();
        assert_eq!(
            field.representation().value_type().unwrap(),
            ValueType::AssetPath,
            "{path}"
        );
        assert!(field.representation().is_array(), "{path}");
        let CrateValue::Array(values) = field.decode(&mut budget).unwrap() else {
            panic!("expected asset array for {path}");
        };
        let actual: Vec<_> = values
            .into_iter()
            .map(|v| match v {
                CrateValue::AssetPath(path) => path,
                _ => panic!("expected asset path"),
            })
            .collect();
        assert_eq!(actual, expected);
    }
}
