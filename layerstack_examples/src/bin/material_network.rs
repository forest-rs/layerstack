// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retain a textured Preview Surface graph and observe parameter-only changes.
//! Shader execution, texture anchoring/loading and rendering stay with the host.
use layerstack::{
    AssetResolveError, AssetResolver, EditTarget, InMemoryStore, LayerId, LiveStage, PathInterner,
    ResolvedAsset, StageOptions, TokenInterner, Transaction, Value,
};
use layerstack_schemas::{
    Scene, Time,
    shading::{MaterialNetworkCache, MaterialTerminal},
};
use std::sync::Arc;

struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let parsed = layerstack_usda::parser::parse(
        r#"#usda 1.0
 def Material "Paint" {
    token outputs:surface.connect = </Paint/Surface.outputs:surface>
    def Shader "Surface" {
        uniform token info:id = "UsdPreviewSurface"
        token outputs:surface
        color3f inputs:diffuseColor.connect = </Paint/Texture.outputs:rgb>
    }
    def Shader "Texture" {
        uniform token info:id = "UsdUVTexture"
        asset inputs:file = @textures/paint.png@
        float2 inputs:st.connect = </Paint/Coordinates.outputs:result>
        float3 outputs:rgb
    }
    def Shader "Coordinates" {
        uniform token info:id = "UsdTransform2d"
        float2 inputs:in.connect = </Paint/UV.outputs:result>
        float inputs:rotation = 30
        float2 outputs:result
    }
    def Shader "UV" {
        uniform token info:id = "UsdPrimvarReader_float2"
        string inputs:varname = "st"
        float2 outputs:result
    }
 }
"#,
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
    store.insert_layer(emitted.layer);
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            with_provenance: false,
            ..StageOptions::default()
        },
    );
    let material = store.path("/Paint");
    let mut cache = MaterialNetworkCache::new(Time::Default, &[]);
    let first = cache.get(
        &Scene::new(live.stage(), &store),
        material,
        MaterialTerminal::Surface,
    )?;
    let preview = first.preview_surface(16);
    assert!(preview.is_complete(), "{:?}", preview.issues);
    println!(
        "Captured {} nodes; primvars {:?}; resources {:?}.",
        first.network.nodes.len(),
        first.network.primvars,
        first.network.resources
    );
    let repeated = cache.get(
        &Scene::new(live.stage(), &store),
        material,
        MaterialTerminal::Surface,
    )?;
    assert!(
        Arc::ptr_eq(&first.network, &repeated.network),
        "same immutable material handle"
    );
    let rotation = store.property_path("/Paint/Coordinates.inputs:rotation");
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(rotation),
        Value::Float(60.),
    );
    live.apply(&mut store, &edit)?;
    // Dependency snapshots refresh the capture even without feeding a notice.
    let changed = cache.get(
        &Scene::new(live.stage(), &store),
        material,
        MaterialTerminal::Surface,
    )?;
    assert_eq!(
        first.revisions.topology, changed.revisions.topology,
        "rotation does not change graph connectivity"
    );
    assert_ne!(
        first.revisions.parameters, changed.revisions.parameters,
        "the coordinate rotation changed"
    );
    assert_eq!(
        first.revisions.resources, changed.revisions.resources,
        "texture spelling and source remain the same"
    );
    assert!(
        first.preview_surface(16).is_complete(),
        "old immutable handoff survives edits"
    );
    println!(
        "Only parameters changed: {:?} -> {:?}; {:?}.",
        first.revisions,
        changed.revisions,
        cache.stats()
    );
    Ok(())
}
