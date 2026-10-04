// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Two producers publish into a caller-owned stage; Sylva places a shared authored asset.
//! Run with `cargo run -p layerstack_examples --example generated_assets`.
//! The optional output directory receives the three authored USDA layers.
#[path = "support/generated_assets.rs"]
mod support;
use layerstack_schemas::{
    Scene, Time,
    bounds::{BoundsCache, BoundsOptions},
    usd_geom::Mesh,
};
use std::sync::Arc;

fn main() {
    let mut generated = support::GeneratedScene::new(100);
    let source_points = Mesh::new(
        &Scene::new(generated.live.stage(), &generated.store),
        generated.source,
    )
    .unwrap()
    .points()
    .unwrap();
    assert!(
        Arc::ptr_eq(&source_points, &generated.geometry.points),
        "typed reads share source storage"
    );
    let mut bounds = BoundsCache::new(Time::Default, BoundsOptions::default());
    let before = bounds
        .world_bound(
            &Scene::new(generated.live.stage(), &generated.store),
            generated.scatter,
        )
        .unwrap()
        .aligned_range();
    let untouched_topology = generated.geometry.face_vertex_indices.clone();
    let mut points = generated.geometry.points.as_ref().clone(); // Explicit materialization for editing.
    points[2][1] = 3.;
    generated.geometry.points = Arc::new(points);
    let changes = generated.publish();
    let scene = Scene::new(generated.live.stage(), &generated.store);
    bounds.apply_changes(&scene, &changes);
    let after = bounds
        .world_bound(&scene, generated.scatter)
        .unwrap()
        .aligned_range();
    assert!(
        after.max[1] > before.max[1],
        "prototype changes invalidate scatter bounds"
    );
    assert!(
        Arc::ptr_eq(
            &untouched_topology,
            &Mesh::new(&scene, generated.source)
                .unwrap()
                .face_vertex_indices()
                .unwrap()
        ),
        "point updates preserve topology storage"
    );
    println!(
        "Two producers, {} native prototypes, 100 native instances and 99 visible point instances; scatter height {} -> {}.",
        generated.live.stage().prototypes().count(),
        before.max[1],
        after.max[1]
    );
    assert!(
        Mesh::new(&scene, generated.terrain).is_some(),
        "the independent terrain producer remains published"
    );
    let directory = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("layerstack-generated-assets"));
    std::fs::create_dir_all(&directory).unwrap();
    for (id, name) in [
        (support::ROOT, "scene.usda"),
        (support::ASSET, "assets.usd"),
        (support::TERRAIN, "terrain.usd"),
    ] {
        let text = layerstack_usda::save::save_usda(
            generated.store.layers.get(&id).unwrap(),
            &generated.store.tokens,
            &generated.store.paths,
        )
        .unwrap();
        std::fs::write(directory.join(name), text).unwrap();
    }
    println!("{}", directory.join("scene.usda").display());
}
