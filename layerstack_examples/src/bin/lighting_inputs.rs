// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Capture lighting, inspect readiness, and update engine-owned GPU staging data.
use layerstack::{
    AssetResolveError, AssetResolver, EditTarget, InMemoryStore, Layer, LayerId, LiveStage,
    PathInterner, ResolvedAsset, StageOptions, TargetPath, TokenInterner,
};
use layerstack_schemas::{
    MembershipCache, Scene, SchemaEdit, Time,
    affine::AffineFactors,
    light::{LightCache, LightInputs, LightListMode},
    usd_lux::{DomeLight, ShadowApi, ShapingApi, SphereLight},
};
use std::sync::Arc;

// The host keeps layer locations; texture loading would use its own resource system.
struct EngineAssets;
impl AssetResolver for EngineAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, layer: LayerId) -> Option<&str> {
        (layer == LayerId(1)).then_some("/project/lighting.usda")
    }
}

// An example adapter's staging layout, not a portable shader ABI. A real engine
// chooses its buffer alignment, light units, working color space and asset handles.
#[derive(Debug)]
struct EngineLight {
    color_intensity: [f32; 4],
    exposure_normalize: [f32; 4],
}
impl EngineLight {
    fn capture(
        inputs: &LightInputs,
    ) -> Result<Self, layerstack_schemas::light::LightParameterError> {
        let p = inputs.photometry()?;
        Ok(Self {
            color_intensity: [p.color[0], p.color[1], p.color[2], p.intensity],
            exposure_normalize: [
                p.exposure,
                if p.normalize { 1. } else { 0. },
                p.diffuse,
                p.specular,
            ],
        })
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let light = store.path("/Key");
    let root = store.path("/");
    let dome = store.path("/Environment");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let key = SphereLight::define(&mut edit, light);
    key.set_radius(&mut edit, 0.25);
    key.light_api().set_intensity(&mut edit, 10.);
    ShapingApi::apply(&mut edit, light)
        .expect("existing light")
        .set_shaping_cone_angle(&mut edit, 45.);
    ShadowApi::apply(&mut edit, light)
        .expect("existing light")
        .set_shadow_distance(&mut edit, -1.);
    DomeLight::define(&mut edit, dome).set_texture_file(&mut edit, "./sky.exr");
    let transaction = edit.finish();
    live.apply(&mut store, &transaction)?;
    let mut cache = LightCache::new(Time::Default, &["engine"]);
    let scene = Scene::new(live.stage(), &store);
    let discovered = cache
        .discover(&scene, root, LightListMode::IgnoreCache)?
        .to_vec();
    assert_eq!(
        discovered,
        [TargetPath::Prim(dome), TargetPath::Prim(light)],
        "discover authored emitters in namespace order"
    );
    let first = cache.capture(&scene, light)?;
    let revisions = first.revisions;
    let mut staging = EngineLight::capture(first.inputs)?;
    assert_eq!(
        first.inputs.shaping()?.expect("applied shaping").cone_angle,
        45.,
        "USD half-angle remains in degrees"
    );
    assert_eq!(
        first.inputs.shadow()?.expect("applied shadow").distance,
        -1.,
        "unlimited shadow distance remains a sentinel"
    );
    let factors = AffineFactors::compute(&first.inputs.world_transform)?;
    let trs = factors.to_trs(1e-10)?;
    assert_eq!(trs.scale, [1.; 3], "identity transform admits checked TRS");
    let environment = cache
        .capture(&scene, dome)?
        .inputs
        .environment()?
        .expect("dome");
    let asset = environment
        .texture_file
        .constant()
        .expect("constant texture")
        .asset_reference()
        .expect("asset storage");
    let anchored = asset.anchor(&EngineAssets)?;
    assert_eq!(
        anchored.identifier, "/project/sky.exr",
        "authoring layer anchors the texture"
    );
    assert!(
        anchored.source.is_some(),
        "source capture does not need global tracing"
    );
    println!(
        "{}: {:?}",
        store.paths.display(light, &store.tokens),
        staging
    );
    let targets = [TargetPath::Prim(light), TargetPath::Prim(dome)];
    let mut links = MembershipCache::new();
    let membership = links.capture_light_links(&scene, light, &targets)?;
    assert!(
        membership
            .illumination
            .membership
            .iter()
            .all(|m| m.is_included()),
        "default linking includes the root"
    );
    let link_revision = membership.illumination.revisions.decisions;

    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    // The edit handle owns a path, so it survives beyond the view used to obtain it.
    key.light_api().set_intensity(&mut edit, 20.);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction)?;
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    links.apply_changes(&scene, &applied.changes);
    let membership = links.capture_light_links(&scene, light, &targets)?;
    assert_eq!(
        membership.illumination.revisions.decisions, link_revision,
        "photometric changes preserve membership-buffer content"
    );
    assert!(
        !membership.illumination.evaluated,
        "photometric changes preserve compiled link queries"
    );
    let next = cache.capture(&scene, light)?;
    if next.revisions.parameters != revisions.parameters {
        staging = EngineLight::capture(next.inputs)?;
    }
    assert_eq!(
        next.revisions.transform, revisions.transform,
        "parameter edits preserve transform uploads"
    );
    assert_eq!(
        staging.color_intensity[3], 20.,
        "changed intensity reaches engine staging"
    );
    assert_eq!(
        staging.exposure_normalize[0], 0.,
        "exposure remains in stops"
    );
    println!(
        "updated parameter staging: {staging:?}; work: {:?}",
        cache.stats()
    );
    Ok(())
}
