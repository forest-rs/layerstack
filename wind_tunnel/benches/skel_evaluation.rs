// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared-skeleton evaluation across animated mesh parts.
#![allow(
    clippy::cast_precision_loss,
    reason = "small deterministic benchmark inputs"
)]
use criterion::{Criterion, criterion_group, criterion_main};
use layerstack::Time;
use layerstack_schemas::{
    Scene,
    skel::{SkelCache, SkinningQuery},
};
use std::{hint::black_box, time::Duration};
#[path = "../../layerstack_conformance/tests/support/schema_scene.rs"]
mod support;

fn source(meshes: usize, dual: bool) -> String {
    let joints = (0..128)
        .map(|i| format!("\"j{i}\""))
        .collect::<Vec<_>>()
        .join(",");
    let matrices = vec!["((1,0,0,0),(0,1,0,0),(0,0,1,0),(0,0,0,1))"; 128].join(",");
    let zero = vec!["(0,0,0)"; 128].join(",");
    let moved = vec!["(0.25,0.5,0.75)"; 128].join(",");
    let rotations = vec!["(1,0,0,0)"; 128].join(",");
    let scales = vec!["(1,1,1)"; 128].join(",");
    let points = (0..256)
        .map(|i| format!("({},1,2)", i as f32 * 0.01))
        .collect::<Vec<_>>()
        .join(",");
    let mut text = format!(
        "#usda 1.0\ndef SkelRoot \"Rig\" (prepend apiSchemas = [\"SkelBindingAPI\"]) {{\nrel skel:skeleton = </Rig/Skeleton>\nrel skel:animationSource = </Rig/Animation>\nint[] primvars:skel:jointIndices = [0,1,2,3] (elementSize = 4)\nfloat[] primvars:skel:jointWeights = [0.25,0.25,0.25,0.25] (elementSize = 4)\ndef Skeleton \"Skeleton\" {{\nuniform token[] joints = [{joints}]\nuniform matrix4d[] restTransforms = [{matrices}]\nuniform matrix4d[] bindTransforms = [{matrices}]\n}}\ndef SkelAnimation \"Animation\" {{\nuniform token[] joints = [{joints}]\nfloat3[] translations = [{zero}]\nfloat3[] translations.timeSamples = {{0: [{zero}], 1: [{moved}]}}\nquatf[] rotations = [{rotations}]\nhalf3[] scales = [{scales}]\n}}\n"
    );
    for i in 0..meshes {
        text.push_str(&format!(
            "def Mesh \"M{i}\" {{\npoint3f[] points = [{points}]\n}}\n"
        ));
    }
    text.push_str("}\n");
    if dual {
        text = text.replace("rel skel:skeleton = </Rig/Skeleton>", "rel skel:skeleton = </Rig/Skeleton>\n token primvars:skel:skinningMethod = \"dualQuaternion\"");
    }
    text
}
fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("skel_evaluation");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(1));
    for dual in [false, true] {
        let method = if dual { "dqs_" } else { "" };
        for meshes in [8, 64] {
            let (mut store, live) = support::scene(&source(meshes, dual));
            let paths: Vec<_> = (0..meshes)
                .map(|i| store.path(&format!("/Rig/M{i}")))
                .collect();
            let scene = Scene::new(live.stage(), &store);
            let queries: Vec<_> = paths
                .iter()
                .map(|&p| SkinningQuery::new(&scene, p).unwrap().unwrap())
                .collect();
            let mut frame = 0;
            group.bench_function(format!("{method}snapshot_frames/{meshes}"), |b| {
                b.iter(|| {
                    frame = (frame + 1) % 20;
                    let time = Time::at(f64::from(frame) * 0.05);
                    for q in &queries {
                        black_box(q.compute_deformed_points(time).unwrap());
                    }
                });
            });
            let mut retained = SkelCache::new(Time::at(0.));
            for (&path, q) in paths.iter().zip(&queries) {
                assert_eq!(
                    retained.deformed_points(&scene, path).unwrap().unwrap(),
                    q.compute_deformed_points(Time::at(0.)).unwrap(),
                    "retained output matches snapshot"
                );
            }
            let mut frame = 0;
            group.bench_function(format!("{method}retained_frames/{meshes}"), |b| {
                b.iter(|| {
                    frame = (frame + 1) % 20;
                    retained.set_time(Time::at(f64::from(frame) * 0.05));
                    for &path in &paths {
                        black_box(retained.deformed_points(&scene, path).unwrap().unwrap());
                    }
                });
            });
            group.bench_function(format!("{method}retained_same_time/{meshes}"), |b| {
                b.iter(|| {
                    for &path in &paths {
                        black_box(retained.deformed_points(&scene, path).unwrap().unwrap());
                    }
                });
            });
        }
    }
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
