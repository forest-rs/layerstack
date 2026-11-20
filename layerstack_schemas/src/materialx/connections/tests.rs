// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::Scene;
use crate::materialx::{ImageColor3, OpenPbrSurface, STANDARD_NODES};
use alloc::sync::Arc;
use layerstack::edit::EditTarget;
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PropertyDeclaration, PropertyKind, PropertyPath,
    StageOptions, Variability,
};

const ROOT: LayerId = LayerId(1);

fn stage() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(ROOT));
    let options = StageOptions {
        schemas: Some(Arc::new(crate::openusd(store.tokens()))),
        ..StageOptions::default()
    };
    let stage = LiveStage::compose(&mut store, ROOT, options);
    (store, stage)
}

#[test]
fn authored_and_missing_outputs_connect_in_list_order() {
    let (mut store, mut live) = stage();
    let surface_path = store.path("/Surface");
    let first_path = store.path("/First");
    let second_path = store.path("/Second");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    let surface = OpenPbrSurface::define(&mut edit, surface_path).unwrap();
    let first = ImageColor3::define(&mut edit, first_path).unwrap();
    ImageColor3::define(&mut edit, second_path).unwrap();
    let input = surface.create_base_color_input(&mut edit).unwrap();
    let first = first.create_out_output(&mut edit).unwrap();
    let baseline = edit.transaction().len();
    let second = ConnectionSource::output(second_path, "out");
    input
        .check_materialx_source(&mut edit, second.clone(), STANDARD_NODES)
        .unwrap();
    assert_eq!(edit.transaction().len(), baseline);
    assert_eq!(edit.property_kind(second_path, "outputs:out"), None);
    assert_eq!(
        input
            .connect_to_materialx_source(
                &mut edit,
                &first,
                STANDARD_NODES,
                ConnectionModification::Replace,
            )
            .unwrap(),
        first
    );
    let added = input
        .connect_to_materialx_source(
            &mut edit,
            second.clone(),
            STANDARD_NODES,
            ConnectionModification::Prepend,
        )
        .unwrap();
    assert_eq!(ConnectionSource::from(added), second);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let scene = Scene::new(live.stage(), &store);
    let surface = OpenPbrSurface::get(&scene, surface_path).unwrap();
    let input = surface.base_color_input().unwrap();
    let out = store.tokens().lookup("outputs:out").unwrap();
    assert_eq!(
        input.connected_sources().sources,
        [
            PropertyPath::new(second_path, out),
            PropertyPath::new(first_path, out)
        ],
    );
    let second = ImageColor3::get(&scene, second_path)
        .unwrap()
        .out_output()
        .unwrap();
    assert_eq!(second.property_type().unwrap().type_name(), "color3f");
}

#[test]
fn rejected_connections_preserve_authored_and_queued_edits() {
    let (mut store, mut live) = stage();
    let surface_path = store.path("/Surface");
    let original_path = store.path("/Original");
    let next_path = store.path("/Next");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    let surface = OpenPbrSurface::define(&mut edit, surface_path).unwrap();
    let original = ImageColor3::define(&mut edit, original_path).unwrap();
    let color = surface.create_base_color_input(&mut edit).unwrap();
    let roughness = surface.create_specular_roughness_input(&mut edit).unwrap();
    let normal = surface.create_geometry_normal_input(&mut edit).unwrap();
    let output = original.create_out_output(&mut edit).unwrap();
    color
        .connect_to_materialx_source(
            &mut edit,
            output,
            STANDARD_NODES,
            ConnectionModification::Replace,
        )
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    ImageColor3::define(&mut edit, next_path).unwrap();
    let source = ConnectionSource::output(next_path, "out");
    let before = edit.transaction().len();
    for (input, input_type) in [(&roughness, "float"), (&normal, "vector3")] {
        assert_eq!(
            input.connect_to_materialx_source(
                &mut edit,
                source.clone(),
                STANDARD_NODES,
                ConnectionModification::Replace,
            ),
            Err(MaterialXConnectionError::TypeMismatch {
                input: ConnectionSource::from(input),
                output: source.clone(),
                input_type,
                output_type: "color3",
            }),
        );
    }
    assert_eq!(edit.transaction().len(), before);
    assert_eq!(edit.property_kind(next_path, "outputs:out"), None);

    color
        .connect_to_materialx_source(
            &mut edit,
            source,
            STANDARD_NODES,
            ConnectionModification::Append,
        )
        .unwrap();
    let before = edit.transaction().len();
    let unknown_port = ConnectionSource::output(next_path, "missing");
    assert_eq!(
        color.connect_to_materialx_source(
            &mut edit,
            unknown_port.clone(),
            STANDARD_NODES,
            ConnectionModification::Replace,
        ),
        Err(MaterialXConnectionError::UnknownPort {
            port: unknown_port,
            definition: ImageColor3::ID,
        }),
    );
    let wrong_direction = ConnectionSource::input(next_path, "file");
    assert_eq!(
        color.connect_to_materialx_source(
            &mut edit,
            wrong_direction.clone(),
            STANDARD_NODES,
            ConnectionModification::Replace,
        ),
        Err(MaterialXConnectionError::WrongDirection {
            port: wrong_direction,
            expected: PortKind::Output,
        }),
    );
    assert_eq!(edit.transaction().len(), before);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let surface = OpenPbrSurface::get(&scene, surface_path).unwrap();
    let out = store.tokens().lookup("outputs:out").unwrap();
    assert_eq!(
        surface
            .base_color_input()
            .unwrap()
            .connected_sources()
            .sources,
        [
            PropertyPath::new(original_path, out),
            PropertyPath::new(next_path, out)
        ],
    );
}

#[test]
fn check_resolves_the_shader_identity_from_the_current_edit() {
    const CUSTOM_IMAGE: MaterialXNode = MaterialXNode {
        id: "ND_custom_image",
        ports: ImageColor3::DEFINITION.ports,
    };
    let (mut store, mut live) = stage();
    let surface_path = store.path("/Surface");
    let image_path = store.path("/Image");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    let surface = OpenPbrSurface::define(&mut edit, surface_path).unwrap();
    let input = surface.create_base_color_input(&mut edit).unwrap();
    let image = ImageColor3::define(&mut edit, image_path).unwrap();
    let output = image.create_out_output(&mut edit).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    image.node_def_api().set_id(&mut edit, CUSTOM_IMAGE.id);
    let before = edit.transaction().len();
    assert_eq!(
        input.check_materialx_source(&mut edit, &output, STANDARD_NODES),
        Err(MaterialXConnectionError::UnknownDefinition {
            prim: image_path,
            id: Some(CUSTOM_IMAGE.id.into())
        }),
    );
    let definitions = [OpenPbrSurface::DEFINITION, CUSTOM_IMAGE];
    input
        .check_materialx_source(&mut edit, &output, &definitions)
        .unwrap();
    assert_eq!(edit.transaction().len(), before);

    image
        .node_def_api()
        .set_implementation_source(&mut edit, NodeDefApiImplementationSource::SourceAsset);
    let before = edit.transaction().len();
    assert_eq!(
        input.connect_to_materialx_source(
            &mut edit,
            &output,
            &definitions,
            ConnectionModification::Replace,
        ),
        Err(MaterialXConnectionError::NotIdentifierBased {
            prim: image_path,
            source: NodeDefApiImplementationSource::SourceAsset,
        }),
    );
    assert_eq!(edit.transaction().len(), before);
    input
        .connect_to_source(&mut edit, output, ConnectionModification::Replace)
        .unwrap();
}

#[test]
fn declared_usd_type_must_match_the_definition() {
    let (mut store, mut live) = stage();
    let surface_path = store.path("/Surface");
    let image_path = store.path("/Image");
    let invalid_path = store.property_path("/Invalid.outputs:out");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    let surface = OpenPbrSurface::define(&mut edit, surface_path).unwrap();
    let input = surface.create_base_color_input(&mut edit).unwrap();
    let image = ImageColor3::define(&mut edit, image_path).unwrap();
    let output = image
        .create_output(&mut edit, "out", PropertyType::parse("color3f[]").unwrap())
        .unwrap();
    ImageColor3::define(&mut edit, invalid_path.prim_path()).unwrap();
    assert!(edit.declare_property(
        invalid_path,
        &PropertyDeclaration {
            kind: PropertyKind::Relationship,
            type_name: None,
            variability: Variability::Uniform,
            custom: false,
        }
    ));
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    let before = edit.transaction().len();
    assert_eq!(
        input.connect_to_materialx_source(
            &mut edit,
            &output,
            STANDARD_NODES,
            ConnectionModification::Replace,
        ),
        Err(MaterialXConnectionError::UsdTypeMismatch {
            port: ConnectionSource::from(&output),
            expected: "color3f",
            actual: "color3f[]".into(),
        }),
    );
    assert_eq!(
        input.check_materialx_source(
            &mut edit,
            ConnectionSource::output(invalid_path.prim_path(), "out"),
            STANDARD_NODES,
        ),
        Err(MaterialXConnectionError::Port(
            PortError::InvalidAttribute {
                prim: invalid_path.prim_path(),
                name: "outputs:out".into(),
            }
        )),
    );
    assert_eq!(edit.transaction().len(), before);
    input
        .connect_to_source(&mut edit, &output, ConnectionModification::Replace)
        .unwrap();
}
