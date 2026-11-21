// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Author both surface contexts from an imported metallic-workflow record,
//! bind the material to a mesh, and inspect the composed terminals after
//! reopening the USDA layer.

use layerstack::{EditTarget, InMemoryStore, Layer, LayerId, LiveStage, NoAssets, StageOptions};
use layerstack_schemas::{
    BindingOptions, MaterialPurpose, Scene, SchemaEdit,
    materialx::{OpenPbrSurface, PbrMaterialInputs, define_pbr_material},
    shading::nodes::PreviewSurface,
    usd_geom::{Mesh, MeshSubdivisionScheme},
    usd_shade::{Material, MaterialBindingApi},
};
use std::sync::Arc;

fn options(store: &InMemoryStore) -> StageOptions {
    StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(store.tokens()))),
        ..StageOptions::default()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    let root = LayerId(1);
    store.insert_layer(Layer::new(root));
    let mesh_path = store.path("/Model/Panel");
    let material_path = store.path("/Model/Looks/Paint");
    let stage_options = options(&store);
    let mut live = LiveStage::compose(&mut store, root, stage_options);

    // The importer has already converted its color into the stage working space.
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(root));
    let mesh = Mesh::define(&mut edit, mesh_path)?;
    mesh.set_points(&mut edit, &[[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]);
    mesh.set_face_vertex_counts(&mut edit, &[3]);
    mesh.set_face_vertex_indices(&mut edit, &[0, 1, 2]);
    mesh.set_subdivision_scheme(&mut edit, MeshSubdivisionScheme::None);
    let paint = define_pbr_material(
        &mut edit,
        material_path,
        PbrMaterialInputs {
            base_color: [0.18, 0.34, 0.63],
            roughness: 0.42,
            metallic: 0.2,
            opacity: 1.0,
        },
    )?;
    MaterialBindingApi::apply(&mut edit, mesh_path)?.bind(
        &mut edit,
        paint.material.path(),
        None,
        &MaterialPurpose::All,
    )?;
    let transaction = edit.finish();
    live.apply(&mut store, &transaction)?;
    let text =
        layerstack_usda::save_usda(store.layer(root).unwrap(), store.tokens(), store.paths())?;

    let mut reopened = InMemoryStore::default();
    let layer = layerstack_usda::read_usda(
        &text,
        root,
        reopened.tokens(),
        reopened.paths(),
        &mut NoAssets,
    );
    assert!(layer.parse_diagnostics.is_empty(), "the saved layer parses");
    assert!(layer.lower_diagnostics.is_empty(), "the saved layer lowers");
    assert!(
        layer.emitted.diagnostics.is_empty(),
        "the saved layer emits"
    );
    reopened.insert_layer(layer.emitted.layer);
    let stage_options = options(&reopened);
    let live = LiveStage::compose(&mut reopened, root, stage_options);
    let scene = Scene::new(live.stage(), &reopened);
    let mesh = reopened.path("/Model/Panel");
    let binding = MaterialBindingApi::get(&scene, mesh)
        .ok_or("missing mesh material binding API")?
        .compute_bound_material(&MaterialPurpose::All, BindingOptions::default());
    let material = Material::get(&scene, binding.material.ok_or("missing bound material")?)
        .ok_or("missing bound material prim")?;
    let mtlx = material
        .compute_surface_source(&["mtlx"])
        .shader
        .and_then(|path| OpenPbrSurface::get(&scene, path))
        .ok_or("missing OpenPBR surface")?;
    let fallback = material
        .compute_surface_source(&[])
        .shader
        .and_then(|path| PreviewSurface::get(&scene, path))
        .ok_or("missing Preview Surface fallback")?;
    let base_color = material
        .input("baseColor")
        .ok_or("missing shared base color")?;
    for input in [mtlx.base_color_input(), fallback.diffuse_color_input()] {
        assert_eq!(
            input
                .ok_or("missing shader color input")?
                .connected_sources()
                .sources,
            [base_color.path()],
            "both shader inputs resolve to the material interface"
        );
    }
    println!("{text}");
    Ok(())
}
