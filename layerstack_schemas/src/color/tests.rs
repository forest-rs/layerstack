// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use alloc::vec;
#[test]
fn native_rec709_matrix_transfer_and_alpha() {
    let srgb = ColorSpaceDefinition::builtin("srgb_rec709_scene").unwrap();
    let linear = ColorSpaceDefinition::builtin("lin_rec709_scene").unwrap();
    let matrix = linear.rgb_to_xyz();
    // Captured Gf.ColorSpace values, including float primary normalization.
    let expected = [
        [0.4123909, 0.35758436, 0.18048081],
        [0.21263906, 0.7151687, 0.07219232],
        [0.019330824, 0.11919474, 0.95053214],
    ];
    for i in 0..3 {
        for j in 0..3 {
            assert!((matrix[i][j] - expected[i][j]).abs() < 5e-7);
        }
    }
    let t = srgb.transform_to(&linear).unwrap();
    let value = t.convert_rgba([0.5, 0.02, -0.1, 0.4]).unwrap();
    assert!((value[0] - 0.21404114).abs() < 2e-6);
    assert!((value[1] - 0.0015476042).abs() < 5e-7);
    assert!((value[2] + 0.007738015).abs() < 5e-7);
    assert_eq!(value[3], 0.4);
    let roundtrip = linear
        .transform_to(&srgb)
        .unwrap()
        .convert_rgb([value[0], value[1], value[2]])
        .unwrap();
    for (a, b) in roundtrip.into_iter().zip([0.5, 0.02, -0.1]) {
        assert!((a - b).abs() < 2e-6);
    }
    assert_eq!(
        t.convert_rgb([f32::NAN, 0., 0.]),
        Err(ColorError::InvalidColor)
    );
}
#[test]
fn every_builtin_exists_and_custom_identity_is_valid() {
    for &name in BUILTIN_COLOR_SPACES {
        assert!(ColorSpaceDefinition::builtin(name).is_some(), "{name}");
    }
    assert!(ColorSpaceDefinition::builtin("not-a-space").is_none());
    let definition = ColorSpaceDefinition::from_chromaticities(
        "custom",
        [[1., 0.], [0., 1.], [0., 0.], [1. / 3., 1. / 3.]],
        1.,
        0.,
    )
    .unwrap();
    assert_eq!(
        definition.rgb_to_xyz(),
        [[1., 0., 0.], [0., 1., 0.], [0., 0., 0.9999998]]
    );
    assert!(matches!(
        ColorSpaceDefinition::from_chromaticities("bad", [[0.; 2]; 4], 1., 0.),
        Err(ColorError::InvalidChromaticities)
    ));
    assert!(matches!(
        ColorSpaceDefinition::from_chromaticities("bad", definition.chromaticities, 0., 0.),
        Err(ColorError::InvalidTransferFunction)
    ));
}
#[test]
fn ancestors_custom_named_definition_and_unknown_assignment_stops_inheritance() {
    use crate::SchemaEdit;
    use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions, edit::EditTarget};
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let mut stage = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let parent = store.path("/Parent");
    let child = store.path("/Parent/Child");
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    // Typed is abstract; author ordinary prims through the base edit.
    edit.define(parent, "Scope");
    edit.define(child, "Scope");
    ColorSpaceApi::apply(&mut edit, parent)
        .unwrap()
        .set_color_space_name(&mut edit, "named-custom");
    ColorSpaceDefinitionApi::apply(&mut edit, parent, "definition_instance")
        .unwrap()
        .set_name(&mut edit, "named-custom");
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(stage.stage(), &store);
    let name = scene.compute_color_space_name(child).unwrap();
    assert_eq!(&*name.name, "named-custom");
    assert_eq!(name.source, ColorSpaceSource::Prim(parent));
    let definition = scene.color_space_definition(child, &name.name).unwrap();
    assert_eq!(
        definition.source,
        Some((parent, Arc::from("definition_instance")))
    );
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    ColorSpaceApi::apply(&mut edit, child)
        .unwrap()
        .set_color_space_name(&mut edit, "unknown-custom");
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    assert_eq!(
        Scene::new(stage.stage(), &store).compute_color_space_name(child),
        Err(ColorError::UnknownColorSpace {
            name: Arc::from("unknown-custom"),
            prim: child
        })
    );
}

#[test]
fn authored_attribute_metadata_bypasses_name_validation_and_empty_suppresses_inheritance() {
    use crate::SchemaEdit;
    use layerstack::{
        InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec, StageOptions, Value,
        edit::EditTarget,
    };
    let mut store = InMemoryStore::default();
    let path = store.path("/Prim");
    let property = store.tokens.intern("tint");
    let color_space = store.tokens.intern("colorSpace");
    let unknown = store.tokens.intern("custom-unknown");
    let empty_name = store.tokens.intern("");
    let empty_property = store.tokens.intern("emptyTint");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        path,
        PrimSpec::def()
            .with_property(
                property,
                PropertySpec::attribute()
                    .with_default(Value::Vec3f([1.; 3]))
                    .with_metadata(color_space, Value::Token(unknown)),
            )
            .with_property(
                empty_property,
                PropertySpec::attribute()
                    .with_default(Value::Vec3f([1.; 3]))
                    .with_metadata(color_space, Value::Token(empty_name)),
            ),
    );
    store.insert_layer(layer);
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let mut stage = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let mut edit = SchemaEdit::new(stage.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    ColorSpaceApi::apply(&mut edit, path)
        .unwrap()
        .set_color_space_name(&mut edit, "lin_rec709_scene");
    let transaction = edit.finish();
    stage.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(stage.stage(), &store);
    let attribute = PropertyPath::new(path, property);
    let assignment = scene.compute_attribute_color_space_name(attribute).unwrap();
    assert_eq!(&*assignment.name, "custom-unknown");
    assert_eq!(assignment.source, ColorSpaceSource::Attribute(attribute));
    assert!(matches!(
        scene.color_space_definition(path, &assignment.name),
        Err(ColorError::UnknownColorSpace { .. })
    ));
    let empty_assignment = scene
        .compute_attribute_color_space_name(PropertyPath::new(path, empty_property))
        .unwrap();
    assert!(empty_assignment.name.is_empty());
    assert_eq!(
        empty_assignment.source,
        ColorSpaceSource::Attribute(PropertyPath::new(path, empty_property))
    );
    let missing = PropertyPath::new(path, store.tokens.lookup("colorSpace:name").unwrap());
    assert_eq!(
        scene
            .compute_attribute_color_space_name(missing)
            .unwrap()
            .source,
        ColorSpaceSource::Prim(path)
    );
}

#[test]
fn ap1_conversion_uses_openusd_preadapted_d65_primaries() {
    let source = ColorSpaceDefinition::builtin("lin_ap1_scene").unwrap();
    let destination = ColorSpaceDefinition::builtin("lin_rec709_scene").unwrap();
    let output = source
        .transform_to(&destination)
        .unwrap()
        .convert_rgb([0.8, 0.3, 0.1])
        .unwrap();
    // Python Gf.ColorSpace.Convert oracle, release 26.08.
    for (actual, expected) in output.into_iter().zip([1.169177, 0.2369814, 0.05740389]) {
        assert!((actual - expected).abs() < 2e-6);
    }
}

#[test]
fn schema_metadata_empty_and_block_preserve_color_precedence() {
    use layerstack::{
        InMemoryStore, Layer, LayerId, ListOp, PrimSpec, PropertyDefinition, PropertySpec,
        SchemaDefinition, SchemaKind, SchemaRegistry, Stage, StageOptions, Value,
    };
    let mut store = InMemoryStore::default();
    let parent = store.path("/Parent");
    let child = store.path("/Parent/Child");
    let type_name = store.tokens.intern("TestColor");
    let color = store.tokens.intern("colorSpace");
    let api_schemas = store.tokens.intern("apiSchemas");
    let api = store.tokens.intern("ColorSpaceAPI");
    let assignment = store.tokens.intern("colorSpace:name");
    let ancestor_name = store.tokens.intern("lin_rec709_scene");
    let unknown = store.tokens.intern("unknown-schema-fallback");
    let empty = store.tokens.intern("");
    let tint = store.tokens.intern("tint");
    let empty_tint = store.tokens.intern("emptyTint");
    let blocked_tint = store.tokens.intern("blockedTint");
    let authored_empty = store.tokens.intern("authoredEmpty");
    let native = crate::openusd(&mut store.tokens);
    let mut builder = SchemaRegistry::builder();
    for definition in native.schemas() {
        builder.register(definition.clone());
    }
    builder.register(
        SchemaDefinition::new(type_name, SchemaKind::ConcreteTyped)
            .with_property(
                PropertyDefinition::attribute(tint).with_metadata(color, Value::Token(unknown)),
            )
            .with_property(
                PropertyDefinition::attribute(empty_tint).with_metadata(color, Value::Token(empty)),
            )
            .with_property(
                PropertyDefinition::attribute(blocked_tint)
                    .with_metadata(color, Value::Token(unknown)),
            )
            .with_property(
                PropertyDefinition::attribute(authored_empty)
                    .with_metadata(color, Value::Token(unknown)),
            ),
    );
    let schemas = Arc::new(builder.build(&mut store.tokens));
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        parent,
        PrimSpec::def()
            .with_field(
                api_schemas,
                layerstack::FieldValue::TokenListOp(ListOp {
                    explicit: Some(vec![api]),
                    ..ListOp::default()
                }),
            )
            .with_property(
                assignment,
                PropertySpec::attribute().with_default(Value::Token(ancestor_name)),
            ),
    );
    layer.insert_prim(
        child,
        PrimSpec::def()
            .with_type_name(type_name)
            .with_property(
                blocked_tint,
                PropertySpec::attribute().with_metadata(color, Value::Blocked),
            )
            .with_property(
                authored_empty,
                PropertySpec::attribute().with_metadata(color, Value::Token(empty)),
            ),
    );
    store.insert_layer(layer);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let scene = Scene::new(&stage, &store);
    for (property, name, source) in [
        (
            tint,
            "unknown-schema-fallback",
            ColorSpaceSource::Schema(PropertyPath::new(child, tint)),
        ),
        (
            empty_tint,
            "",
            ColorSpaceSource::Schema(PropertyPath::new(child, empty_tint)),
        ),
        (
            blocked_tint,
            "lin_rec709_scene",
            ColorSpaceSource::Prim(parent),
        ),
        (
            authored_empty,
            "",
            ColorSpaceSource::Attribute(PropertyPath::new(child, authored_empty)),
        ),
    ] {
        let result = scene
            .compute_attribute_color_space_name(PropertyPath::new(child, property))
            .unwrap();
        assert_eq!(result.name.as_ref(), name);
        assert_eq!(result.source, source);
    }
    assert!(
        stage
            .resolve_property_metadata(child, blocked_tint, color)
            .is_none()
    );
    assert!(
        stage
            .resolve_authored_property_metadata(child, tint, color)
            .is_none()
    );
}
