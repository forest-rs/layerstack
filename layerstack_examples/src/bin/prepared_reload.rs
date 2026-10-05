// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validate a candidate scene and packaged resources before publishing reload.
//! Host budgets/renderer resources remain outside USD; package resolution and
//! the winning authoring layer come from the document and composed USD values.

use layerstack::{InMemoryStore, LayerId, StageOptions};
use layerstack_io::{
    AssetBytes, IoError, IoErrorKind, PreparedReload, ReloadPolicy, StageDocument, Storage,
};
use layerstack_schemas::{Scene, Time, assets::AssetReference, usd_geom::Mesh};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug)]
struct Memory(BTreeMap<String, Vec<u8>>);
impl Storage for Memory {
    fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
        Ok(asset.into())
    }
    fn read(&mut self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.0
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, identifier))
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.0.insert(identifier.into(), bytes.to_vec());
        Ok(())
    }
}
fn package(points: &str, image: &[u8]) -> Vec<u8> {
    let mesh = format!(
        "#usda 1.0\ndef Mesh \"Mesh\" {{ point3f[] points=[{points}]\n custom asset texture=@../images/color.png@ }}"
    );
    layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile::new(
            "root.usda",
            b"#usda 1.0\n(subLayers=[@geometry/mesh.usda@])",
        ),
        layerstack_usdz::PackageFile::new("geometry/mesh.usda", mesh.as_bytes()),
        // Opaque demonstration bytes: an engine supplies its image decoder.
        layerstack_usdz::PackageFile::new("images/color.png", image),
    ])
    .expect("supported package members")
}
#[derive(Debug)]
struct Resources {
    points: Arc<Vec<[f32; 3]>>,
    image: AssetBytes,
}
fn validate(
    candidate: &mut PreparedReload<'_, Memory>,
    path: layerstack::PathId,
) -> Result<Resources, Box<dyn std::error::Error>> {
    let (points, asset) = {
        let scene = Scene::new(candidate.stage().stage(), candidate.store());
        let mesh = Mesh::new(&scene, path).ok_or("candidate has no mesh")?;
        let points = mesh
            .try_points(Time::Default)?
            .ok_or("candidate has no points")?;
        if points.len() > 2 {
            return Err("host point budget exceeded".into());
        }
        let name = candidate
            .store()
            .tokens
            .lookup("texture")
            .ok_or("no texture attribute")?;
        let asset = AssetReference::read(
            &scene,
            layerstack::PropertyPath::new(path, name),
            Time::Default,
        )?
        .ok_or("no texture asset")?;
        (points, asset)
    };
    let image = candidate.read_asset_bytes(
        &asset.authored_path,
        asset.source.map(|source| source.layer),
    )?;
    if image.bytes.len() > 8 {
        return Err("host texture byte budget exceeded".into());
    }
    Ok(Resources { points, image })
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let backend = Memory(BTreeMap::from([(
        "scene.usdz".into(),
        package("(0,0,0)", &[1]),
    )]));
    let mut document = StageDocument::open_in(
        backend,
        store,
        "scene.usdz",
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
        layerstack_io::ImportPolicy::Strict,
    )?;
    let path = document.store_mut().path("/Mesh");
    let root: LayerId = document
        .stage()
        .stage()
        .root_layer()
        .expect("document root");
    let old_image = document.read_asset_bytes("images/color.png", Some(root))?;
    document.storage_mut().0.insert(
        "scene.usdz".into(),
        package("(1,0,0),(2,0,0),(3,0,0)", &[2]),
    );
    {
        let mut candidate = document.prepare_reload(ReloadPolicy::PreserveDirty)?;
        assert!(
            validate(&mut candidate, path).is_err(),
            "reject candidate exceeding the host geometry budget"
        );
        // Drop rejects the candidate; no source catalog or layers are published.
    }
    assert_eq!(
        document
            .read_asset_bytes("images/color.png", Some(root))?
            .bytes
            .as_ref(),
        &[1],
        "rejection retains the published package snapshot"
    );
    document
        .storage_mut()
        .0
        .insert("scene.usdz".into(), package("(1,0,0),(2,0,0)", &[3]));
    let mut candidate = document.prepare_reload(ReloadPolicy::PreserveDirty)?;
    let resources = validate(&mut candidate, path)?;
    let report = candidate.commit();
    // Keep the exact validated immutable inputs for renderer/GPU publication.
    assert_eq!(
        resources.points.len(),
        2,
        "validated point inputs survive commit"
    );
    assert_eq!(
        resources.image.identifier, "scene.usdz[images/color.png]",
        "the resource identity includes its containing package"
    );
    assert_eq!(
        resources.image.bytes.as_ref(),
        &[3],
        "commit keeps the exact candidate texture bytes"
    );
    assert_eq!(
        old_image.bytes.as_ref(),
        &[1],
        "previous resources remain valid until retired by the host"
    );
    println!(
        "Published {} layers and {} points; texture {}",
        report.layers.len(),
        resources.points.len(),
        resources.image.identifier
    );
    Ok(())
}
