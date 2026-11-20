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
        ExtractVector3, ImageColor3, ImageVector3, MaterialXDefault, MaterialXPort, NormalMap,
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
fn openusd_reopens_textured_openpbr_usda_and_usdc() {
    let (store, _, _, _, _) = textured_material(false);
    verify_openusd(&store, "sparse");
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
