// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Opt into retained USDC arrays, read one checked value, and inspect decode work.
//!
//! OpenUSD's binary layer format separates structural tables from value payloads;
//! Layerstack assembles structure on open and optionally retains numeric arrays
//! until demand reads. Unlike payload composition, this delays value decoding.
//! <https://openusd.org/release/tut_converting_between_layer_formats.html>
//! AOUSD Core §16.3.9–§16.3.10 (value representation and payloads).
//!
//! Run without arguments for a generated mesh, or pass an existing USDC/USDZ asset
//! and an array property: `lazy_io scene.usdc /World/Mesh.points`.
use layerstack::{EditTarget, InMemoryStore, StageOptions, Time};
use layerstack_io::{
    Filesystem, LoadOptions, ReloadPolicy, StageDocument, UsdcArrayLoading, UsdcReadOptions,
};
use layerstack_schemas::{SchemaEdit, usd_geom::Mesh};
use std::{path::PathBuf, sync::Arc};

fn fixture() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("layerstack-lazy-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let mut store = InMemoryStore::default();
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut document = StageDocument::create_in(
        Filesystem::new(&directory, [])?,
        store,
        "Mesh.usda",
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    )?;
    let (store, live) = document.parts_mut();
    let root = live.stage().root_layer().unwrap();
    let mesh_path = store.path("/Mesh");
    let mut edit = SchemaEdit::new(live.stage(), store, EditTarget::for_layer(root));
    let mesh = Mesh::define(&mut edit, mesh_path);
    mesh.set_points(
        &mut edit,
        &[[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
    );
    mesh.set_face_vertex_counts(&mut edit, &[4]);
    mesh.set_face_vertex_indices(&mut edit, &[0, 1, 2, 3]);
    let transaction = edit.finish();
    live.apply(store, &transaction)?;
    document.export_layer(root, "Mesh.usdc")?;
    Ok(directory.join("Mesh.usdc"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let provided = arguments.next();
    let generated = provided.is_none();
    let asset = match provided {
        Some(path) => PathBuf::from(path),
        None => fixture()?,
    };
    let property = arguments
        .next()
        .map(|argument| argument.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/Mesh.points".into());
    let (prim_name, attribute_name) = property
        .rsplit_once('.')
        .ok_or("use an absolute property path such as /Mesh.points")?;
    let directory = asset
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let file = asset
        .file_name()
        .ok_or("asset path must name a file")?
        .to_str()
        .ok_or("asset filename must be UTF-8")?;
    let mut document = StageDocument::open_with(
        Filesystem::new(directory, [])?,
        file,
        StageOptions::default(),
        LoadOptions {
            usdc: UsdcReadOptions {
                arrays: UsdcArrayLoading::Retained,
                decode_budget: None,
            },
            ..Default::default()
        },
    )?;
    let root = document.stage().stage().root_layer().unwrap();
    let values = document
        .retained_values(root)
        .ok_or("root must be USDC or a USDZ package with a USDC root")?
        .clone();
    let before = values.stats();
    println!("{}: before read {before:?}", asset.display());
    let path = document.store_mut().path(prim_name);
    let mut query = document
        .stage()
        .stage()
        .prim(path, document.store())
        .ok_or("prim is absent")?
        .attribute(attribute_name)
        .ok_or("attribute is absent")?
        .query();
    // Fallible queries distinguish missing/blocked values from damaged retained
    // numeric data or an exhausted decode budget. Propagate the original error.
    let captured = query
        .try_get(document.stage().stage(), Time::Default)?
        .ok_or("attribute has no default value")?
        .value;
    let after = values.stats();
    println!("{property}: after checked read {after:?}");
    query.try_get(document.stage().stage(), Time::Default)?;
    assert_eq!(
        values.stats().decode_attempts,
        after.decode_attempts,
        "repeated reads reuse the retained source"
    );
    if generated {
        assert_eq!(
            before.decode_attempts, 0,
            "opening the generated mesh does not decode its arrays"
        );
        assert_eq!(
            after.decode_attempts, 1,
            "reading points decodes only that source"
        );
        let points = layerstack_schemas::value::try_read_float3_array_shared(
            &captured,
            &document.store().tokens,
        )?
        .ok_or("points have an incompatible type")?;
        assert_eq!(
            points.len(),
            4,
            "the checked fixture contains four mesh points"
        );
    }
    document.reload(ReloadPolicy::PreserveDirty)?;
    assert_eq!(
        values.stats().decode_attempts,
        after.decode_attempts,
        "an old retained handle keeps its original pool across reload"
    );
    assert_eq!(
        values.stats().input_bytes,
        before.input_bytes,
        "captured values keep their immutable encoded input alive"
    );
    query.try_get(document.stage().stage(), Time::Default)?;
    println!(
        "After reload, the query refreshes against a new source pool: {:?}",
        document.retained_values(root).unwrap().stats()
    );
    Ok(())
}
