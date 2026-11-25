// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Author a textured `OpenPBR` material with the checked-in `NodeDef` interfaces.

use super::support::scratch::fresh_scratch_dir;
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PathId, StageOptions, edit::EditTarget,
};
use layerstack_schemas::{
    Scene, SchemaEdit,
    materialx::{
        ExtractColor3, ExtractColor4, ExtractVector3, ImageColor3, ImageColor4, ImageVector3,
        LuminanceColor3, MaterialXDefault, MaterialXPort, MultiplyColor3, NormalMap,
        OpenPbrSurface, Place2dVector2, STANDARD_NODES, TexcoordVector2,
    },
    shading::{ConnectionModification::Replace, PortKind},
    usd_shade::Material,
};
use std::process::Command;
use std::sync::Arc;

const LAYER: LayerId = LayerId(1);

fn textured_material(with_packed_maps: bool) -> (InMemoryStore, LiveStage, PathId, PathId, PathId) {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LAYER));
    let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
    let mut live = LiveStage::compose(
        &mut store,
        LAYER,
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let material_path = store.path("/Paint");
    let albedo_path = store.path("/Paint/Albedo");
    let surface_path = store.path("/Paint/Surface");
    let map_paths = with_packed_maps.then(|| {
        [
            "/Paint/Texcoord",
            "/Paint/Placement",
            "/Paint/Packed",
            "/Paint/Roughness",
            "/Paint/Metalness",
            "/Paint/NormalImage",
            "/Paint/NormalMap",
        ]
        .map(|path| store.path(path))
    });
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LAYER));
    let material = Material::define(&mut edit, material_path).unwrap();
    let albedo = ImageColor3::define(&mut edit, albedo_path).unwrap();
    let surface = OpenPbrSurface::define(&mut edit, surface_path).unwrap();
    albedo.set_file(&mut edit, "albedo.exr").unwrap();
    albedo
        .create_file_input(&mut edit)
        .unwrap()
        .set_color_space(&mut edit, "lin_rec709_scene")
        .unwrap();
    surface.set_base_weight(&mut edit, 0.75).unwrap();

    let color = albedo.create_out_output(&mut edit).unwrap();
    surface
        .create_base_color_input(&mut edit)
        .unwrap()
        .connect_to_materialx_source(&mut edit, color, STANDARD_NODES, Replace)
        .unwrap();
    let output = surface.create_surface_output(&mut edit).unwrap();
    material
        .create_surface_output(&mut edit, "mtlx")
        .unwrap()
        .connect_to_source(&mut edit, &output, Replace)
        .unwrap();

    if let Some(
        [
            uv,
            placement,
            packed,
            roughness,
            metalness,
            normal_image,
            normal_map,
        ],
    ) = map_paths
    {
        let uv = TexcoordVector2::define(&mut edit, uv).unwrap();
        let placement = Place2dVector2::define(&mut edit, placement).unwrap();
        let packed = ImageVector3::define(&mut edit, packed).unwrap();
        let roughness = ExtractVector3::define(&mut edit, roughness).unwrap();
        let metalness = ExtractVector3::define(&mut edit, metalness).unwrap();
        let normal_image = ImageVector3::define(&mut edit, normal_image).unwrap();
        let normal_map = NormalMap::define(&mut edit, normal_map).unwrap();

        uv.set_index(&mut edit, 0).unwrap();
        placement.set_scale(&mut edit, [2.0, 3.0]).unwrap();
        let uv = uv.create_out_output(&mut edit).unwrap();
        placement
            .create_texcoord_input(&mut edit)
            .unwrap()
            .connect_to_materialx_source(&mut edit, uv, STANDARD_NODES, Replace)
            .unwrap();
        let placement = placement.create_out_output(&mut edit).unwrap();
        for texcoord in [
            albedo.create_texcoord_input(&mut edit).unwrap(),
            packed.create_texcoord_input(&mut edit).unwrap(),
            normal_image.create_texcoord_input(&mut edit).unwrap(),
        ] {
            texcoord
                .connect_to_materialx_source(&mut edit, &placement, STANDARD_NODES, Replace)
                .unwrap();
        }

        packed
            .set_file(&mut edit, "metallic_roughness.png")
            .unwrap();
        let packed = packed.create_out_output(&mut edit).unwrap();
        // Green holds roughness and blue holds metalness in the packed image.
        for (extract, channel, input) in [
            (
                roughness,
                1,
                surface.create_specular_roughness_input(&mut edit).unwrap(),
            ),
            (
                metalness,
                2,
                surface.create_base_metalness_input(&mut edit).unwrap(),
            ),
        ] {
            extract.set_index(&mut edit, channel).unwrap();
            extract
                .create_in_value_input(&mut edit)
                .unwrap()
                .connect_to_materialx_source(&mut edit, &packed, STANDARD_NODES, Replace)
                .unwrap();
            let channel = extract.create_out_output(&mut edit).unwrap();
            input
                .connect_to_materialx_source(&mut edit, channel, STANDARD_NODES, Replace)
                .unwrap();
        }

        normal_image.set_file(&mut edit, "normal.png").unwrap();
        normal_map.set_scale(&mut edit, 0.5).unwrap();
        let samples = normal_image.create_out_output(&mut edit).unwrap();
        normal_map
            .create_in_value_input(&mut edit)
            .unwrap()
            .connect_to_materialx_source(&mut edit, samples, STANDARD_NODES, Replace)
            .unwrap();
        let normal = normal_map.create_out_output(&mut edit).unwrap();
        surface
            .create_geometry_normal_input(&mut edit)
            .unwrap()
            .connect_to_materialx_source(&mut edit, normal, STANDARD_NODES, Replace)
            .unwrap();
    }

    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    (store, live, material_path, albedo_path, surface_path)
}

#[test]
fn standard_nodes_author_a_textured_openpbr_surface() {
    let (store, live, material_path, albedo_path, surface_path) = textured_material(false);
    let scene = Scene::new(live.stage(), &store);
    let albedo = ImageColor3::get(&scene, albedo_path).unwrap();
    let surface = OpenPbrSurface::get(&scene, surface_path).unwrap();
    assert_eq!(albedo.file().as_deref(), Some("albedo.exr"));
    assert_eq!(surface.base_weight(), Some(0.75));

    assert_eq!(&*ImageColor3::filtertype_default(), "linear");
    assert_eq!(OpenPbrSurface::specular_roughness_default(), 0.3);
    assert!(albedo.filtertype().is_none());
    assert!(albedo.filtertype_input().is_none());
    assert!(albedo.texcoord_input().is_none());
    assert!(surface.specular_roughness().is_none());
    assert!(surface.specular_roughness_input().is_none());
    assert!(surface.geometry_normal_input().is_none());
    assert!(surface.base_color().is_none());

    assert!(ImageColor3::DEFINITION.ports.contains(&MaterialXPort {
        name: "texcoord",
        direction: PortKind::Input,
        type_name: "vector2",
        usd_name: "inputs:texcoord",
        usd_type_name: "float2",
        default: Some(MaterialXDefault::GeomProp("UV0")),
    }));
    assert!(OpenPbrSurface::DEFINITION.ports.contains(&MaterialXPort {
        name: "out",
        direction: PortKind::Output,
        type_name: "surfaceshader",
        usd_name: "outputs:surface",
        usd_type_name: "token",
        default: None,
    }));

    let input = surface.base_color_input().unwrap();
    let sources = input.shader_sources();
    assert!(sources.issues.is_empty());
    assert_eq!(sources.sources.len(), 1);
    assert_eq!(
        sources.sources[0].output,
        albedo.out_output().unwrap().path()
    );

    let material = Material::get(&scene, material_path).unwrap();
    let source = material.compute_surface_source(&["mtlx"]);
    assert_eq!(source.shader, Some(surface_path));
    assert_eq!(source.context.as_deref(), Some("mtlx"));
    assert!(source.trace.issues.is_empty());
    assert_eq!(source.trace.sources.len(), 1);
    assert_eq!(
        source.trace.sources[0].output,
        surface.surface_output().unwrap().path()
    );
}

#[test]
fn an_empty_port_color_space_and_a_cleared_opinion_are_distinct() {
    use layerstack_schemas::color::ColorSpaceSource;

    let (mut store, mut live, _, image_path, _) = textured_material(false);
    let file = store.property_path("/Paint/Albedo.inputs:file");
    for (name, source) in [
        (Some(""), ColorSpaceSource::Attribute(file)),
        (None, ColorSpaceSource::Unassigned),
    ] {
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LAYER));
        let port = layerstack_schemas::shading::PortEdit::get(&mut edit, file)
            .unwrap()
            .unwrap();
        if let Some(name) = name {
            port.set_color_space(&mut edit, name).unwrap();
        } else {
            port.clear_color_space(&mut edit).unwrap();
        }
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let assignment = scene.compute_attribute_color_space_name(file).unwrap();
        assert_eq!(assignment.name.as_ref(), "");
        assert_eq!(assignment.source, source);
        assert_eq!(
            ImageColor3::get(&scene, image_path)
                .unwrap()
                .file()
                .as_deref(),
            Some("albedo.exr")
        );
    }
}

#[test]
fn openusd_reopens_textured_openpbr_usda_and_usdc() {
    let (store, _, _, _, _) = textured_material(false);
    verify_openusd(&store, "sparse");
}

#[test]
fn rgb_tint_modulates_the_base_color_texture() {
    let (mut store, mut live, _, albedo, surface) = textured_material(false);
    let tint_path = store.path("/Paint/Tint");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LAYER));
    let tint = MultiplyColor3::define(&mut edit, tint_path).unwrap();
    tint.set_in2(&mut edit, [0.25, 0.5, 1.0]).unwrap();
    let image = ImageColor3::define(&mut edit, albedo).unwrap();
    let rgb = image.create_out_output(&mut edit).unwrap();
    tint.create_in1_input(&mut edit)
        .unwrap()
        .connect_to_materialx_source(&mut edit, rgb, STANDARD_NODES, Replace)
        .unwrap();
    let tinted = tint.create_out_output(&mut edit).unwrap();
    OpenPbrSurface::define(&mut edit, surface)
        .unwrap()
        .create_base_color_input(&mut edit)
        .unwrap()
        .connect_to_materialx_source(&mut edit, tinted, STANDARD_NODES, Replace)
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let scene = Scene::new(live.stage(), &store);
    let tint = MultiplyColor3::get(&scene, tint_path).unwrap();
    assert_eq!(tint.in2(), Some([0.25, 0.5, 1.0]));
    verify_openusd(&store, "tinted");
}

#[test]
fn grayscale_luminance_drives_a_scalar_through_one_channel() {
    let (mut store, mut live, _, albedo, surface) = textured_material(false);
    let luma_path = store.path("/Paint/Luminance");
    let channel_path = store.path("/Paint/Brightness");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LAYER));
    let rgb = ImageColor3::define(&mut edit, albedo)
        .unwrap()
        .create_out_output(&mut edit)
        .unwrap();
    let luma = LuminanceColor3::define(&mut edit, luma_path).unwrap();
    luma.create_in_value_input(&mut edit)
        .unwrap()
        .connect_to_materialx_source(&mut edit, rgb, STANDARD_NODES, Replace)
        .unwrap();
    let gray = luma.create_out_output(&mut edit).unwrap();
    let brightness = ExtractColor3::define(&mut edit, channel_path).unwrap();
    brightness.set_index(&mut edit, 0).unwrap();
    brightness
        .create_in_value_input(&mut edit)
        .unwrap()
        .connect_to_materialx_source(&mut edit, gray, STANDARD_NODES, Replace)
        .unwrap();
    let scalar = brightness.create_out_output(&mut edit).unwrap();
    OpenPbrSurface::define(&mut edit, surface)
        .unwrap()
        .create_geometry_opacity_input(&mut edit)
        .unwrap()
        .connect_to_materialx_source(&mut edit, scalar, STANDARD_NODES, Replace)
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let scene = Scene::new(live.stage(), &store);
    let luma = LuminanceColor3::get(&scene, luma_path).unwrap();
    assert!(luma.lumacoeffs_input().is_none());
    assert_eq!(
        LuminanceColor3::lumacoeffs_default(),
        [0.272_228_7, 0.674_081_8, 0.053_689_5]
    );
    assert_eq!(
        ExtractColor3::get(&scene, channel_path)
            .unwrap()
            .in_value_input()
            .unwrap()
            .shader_sources()
            .sources[0]
            .output,
        luma.out_output().unwrap().path()
    );
    verify_openusd(&store, "luminance");
}

#[test]
fn rgba_alpha_drives_surface_opacity() {
    let (mut store, mut live, _, _, surface) = textured_material(false);
    let image_path = store.path("/Paint/Rgba");
    let alpha_path = store.path("/Paint/Alpha");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LAYER));
    let image = ImageColor4::define(&mut edit, image_path).unwrap();
    image.set_file(&mut edit, "cutout.png").unwrap();
    let rgba = image.create_out_output(&mut edit).unwrap();
    let alpha = ExtractColor4::define(&mut edit, alpha_path).unwrap();
    alpha.set_index(&mut edit, 3).unwrap();
    alpha
        .create_in_value_input(&mut edit)
        .unwrap()
        .connect_to_materialx_source(&mut edit, rgba, STANDARD_NODES, Replace)
        .unwrap();
    let opacity = alpha.create_out_output(&mut edit).unwrap();
    OpenPbrSurface::define(&mut edit, surface)
        .unwrap()
        .create_geometry_opacity_input(&mut edit)
        .unwrap()
        .connect_to_materialx_source(&mut edit, opacity, STANDARD_NODES, Replace)
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();

    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        ExtractColor4::get(&scene, alpha_path).unwrap().index(),
        Some(3)
    );
    verify_openusd(&store, "alpha");
}

#[test]
fn openusd_reopens_packed_openpbr_usda_and_usdc() {
    let (store, _, _, _, _) = textured_material(true);
    verify_openusd(&store, "packed");
}

fn verify_openusd(store: &InMemoryStore, graph: &str) {
    let explicit_python = std::env::var_os("LAYERSTACK_USD_PYTHON");
    let python = explicit_python.as_deref().unwrap_or("python3".as_ref());
    let available = Command::new(python)
        .args(["-c", "from pxr import Sdf, Usd, UsdShade"])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit_python.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr"
    );
    if !available {
        eprintln!("skipped MaterialX authoring oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }

    let layer = store.layer(LAYER).unwrap();
    let dir = fresh_scratch_dir(&format!("materialx_authoring_{graph}"));
    let usda = dir.join("paint.usda");
    let usdc = dir.join("paint.usdc");
    std::fs::write(
        &usda,
        layerstack_usda::save_usda(layer, store.tokens(), store.paths()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &usdc,
        layerstack_usdc::save_usdc(layer, store.tokens(), store.paths()).unwrap(),
    )
    .unwrap();
    let output = Command::new(python)
        .args([
            "-c",
            include_str!("../../scripts/materialx_authoring_oracle.py"),
        ])
        .arg(graph)
        .args([&usda, &usdc])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed to verify {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}
