// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Author a textured `OpenPBR` material with the checked-in `NodeDef` interfaces.

use super::support::scratch::fresh_scratch_dir;
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PathId, StageOptions, edit::EditTarget,
};
use layerstack_schemas::{
    Scene, SchemaEdit,
    materialx::{ImageColor3, MaterialXDefault, MaterialXPort, OpenPbrSurface},
    shading::{ConnectionModification::Replace, PortKind},
    usd_shade::Material,
};
use std::process::Command;
use std::sync::Arc;

const LAYER: LayerId = LayerId(1);

fn textured_material() -> (InMemoryStore, LiveStage, PathId, PathId, PathId) {
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
        .connect_to_source(&mut edit, color, Replace)
        .unwrap();
    let output = surface.create_surface_output(&mut edit).unwrap();
    material
        .create_surface_output(&mut edit, "mtlx")
        .unwrap()
        .connect_to_source(&mut edit, &output, Replace)
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    (store, live, material_path, albedo_path, surface_path)
}

#[test]
fn standard_nodes_author_a_textured_openpbr_surface() {
    let (store, live, material_path, albedo_path, surface_path) = textured_material();
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

    let (store, _, _, _, _) = textured_material();
    let layer = store.layer(LAYER).unwrap();
    let dir = fresh_scratch_dir("materialx_authoring");
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
