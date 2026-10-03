// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Capture lighting, inspect readiness, and update engine-owned GPU staging data.
use layerstack::{EditTarget, InMemoryStore, Layer, LayerId, LiveStage, StageOptions, TargetPath};
use layerstack_schemas::{
    Scene, SchemaEdit, Time,
    light::{LightCache, LightInputs, LightLinkMembership, LightListMode},
    usd_lux::SphereLight,
};
use std::sync::Arc;

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
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let key = SphereLight::define(&mut edit, light);
    key.set_radius(&mut edit, 0.25);
    key.light_api().set_intensity(&mut edit, 10.);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction)?;
    let mut cache = LightCache::new(Time::Default, &["engine"]);
    let scene = Scene::new(live.stage(), &store);
    let discovered = cache
        .discover(&scene, root, LightListMode::IgnoreCache)?
        .to_vec();
    assert_eq!(
        discovered,
        [TargetPath::Prim(light)],
        "discover the authored emitter"
    );
    let first = cache.capture(&scene, light)?;
    let revisions = first.revisions;
    let mut staging = EngineLight::capture(first.inputs)?;
    println!(
        "{}: {:?}",
        store.paths.display(light, &store.tokens),
        staging
    );
    let membership = LightLinkMembership::read(&scene, light, &[TargetPath::Prim(light)])?;
    assert_eq!(
        membership.illumination.included,
        [true],
        "default linking includes the root"
    );

    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    // The edit handle owns a path, so it survives beyond the view used to obtain it.
    key.light_api().set_intensity(&mut edit, 20.);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction)?;
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
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
