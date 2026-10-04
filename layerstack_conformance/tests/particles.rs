// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Particle schema behavior and splat precision selection against both C++ overloads.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack_schemas::{
    Scene, SchemaEdit, usd_vol::ParticleField3DGaussianSplat, volume::SplatData,
};
use serde::Deserialize;
#[derive(Deserialize)]
struct Oracle {
    version: String,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    path: String,
    channels: Vec<Channel>,
}
#[derive(Deserialize)]
struct Channel {
    channel: String,
    uses_float: bool,
    name: String,
    property: String,
}
#[test]
fn all_splat_channels_match_cpp_at_earliest_time() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../fixtures/particles/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, live) = support::scene(include_str!("../fixtures/particles/scene.usda"));
    for row in oracle.rows {
        let path = store.path(&row.path);
        let scene = Scene::new(live.stage(), &store);
        let splat = ParticleField3DGaussianSplat::new(&scene, path).unwrap();
        assert_eq!(row.channels.len(), SplatData::ALL.len());
        for (channel, data) in row.channels.into_iter().zip(SplatData::ALL) {
            let selected = splat.attribute_in_use(data);
            assert_eq!(
                selected.uses_float, channel.uses_float,
                "{} {}",
                row.path, channel.channel
            );
            assert_eq!(
                selected.name, channel.name,
                "{} {}",
                row.path, channel.channel
            );
            assert_eq!(
                selected.property.display(&store.paths, &store.tokens),
                channel.property
            );
        }
    }
}
#[test]
fn builtin_particle_apis_author_read_and_change_precision_with_undo() {
    let (mut store, mut live) = support::scene("#usda 1.0\n");
    let path = store.path("/Splat");
    let mut edit = SchemaEdit::new(
        live.stage(),
        &mut store,
        layerstack::edit::EditTarget::for_layer(layerstack::LayerId(1)),
    );
    let handle = ParticleField3DGaussianSplat::define(&mut edit, path);
    handle
        .particle_field_position_attribute_api()
        .set_positionsh(&mut edit, &[[1., 2., 3.]]);
    handle
        .particle_field_orientation_attribute_api()
        .set_orientations(&mut edit, &[[0., 0., 0., 1.]]);
    handle
        .particle_field_scale_attribute_api()
        .set_scalesh(&mut edit, &[[2., 3., 4.]]);
    handle
        .particle_field_opacity_attribute_api()
        .set_opacities(&mut edit, &[0.5]);
    handle
        .particle_field_spherical_harmonics_attribute_api()
        .set_radiance_spherical_harmonics_coefficientsh(&mut edit, &[[0.25, 0.5, 0.75]]);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let splat = ParticleField3DGaussianSplat::new(&scene, path).unwrap();
    assert_eq!(
        SplatData::ALL.map(|data| splat.attribute_in_use(data).uses_float),
        [false, true, false, true, false]
    );
    assert_eq!(
        splat.particle_field_position_attribute_api().positionsh(),
        Some(vec![[1., 2., 3.]])
    );
    assert_eq!(
        splat
            .particle_field_orientation_attribute_api()
            .orientations(),
        Some(vec![[0., 0., 0., 1.]].into())
    );
    assert_eq!(
        splat.particle_field_scale_attribute_api().scalesh(),
        Some(vec![[2., 3., 4.]])
    );
    assert_eq!(
        splat.particle_field_opacity_attribute_api().opacities(),
        Some(vec![0.5].into())
    );
    assert_eq!(
        splat
            .particle_field_spherical_harmonics_attribute_api()
            .radiance_spherical_harmonics_coefficientsh(),
        Some(vec![[0.25, 0.5, 0.75]])
    );
    let mut edit = SchemaEdit::new(
        live.stage(),
        &mut store,
        layerstack::edit::EditTarget::for_layer(layerstack::LayerId(1)),
    );
    handle
        .particle_field_position_attribute_api()
        .set_positions_at(&mut edit, 2., &[[4., 5., 6.]]);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let splat = ParticleField3DGaussianSplat::new(&scene, path).unwrap();
    assert!(splat.attribute_in_use(SplatData::Positions).uses_float);
    assert_eq!(
        splat
            .particle_field_position_attribute_api()
            .positions_at(2., layerstack::InterpolationType::Held),
        Some(vec![[4., 5., 6.]].into())
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert!(
        !ParticleField3DGaussianSplat::new(&Scene::new(live.stage(), &store), path)
            .unwrap()
            .attribute_in_use(SplatData::Positions)
            .uses_float
    );
}
