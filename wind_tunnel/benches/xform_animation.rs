// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored stacks, mixed animation, and advancing frame evaluation.
#[path = "support/xform_scene.rs"]
mod xform_scene;
use criterion::{Criterion, criterion_group, criterion_main};
use layerstack_schemas::{Scene, Time, XformCache};
use std::hint::black_box;

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("xform_animation");
    group.sample_size(10);
    for (name, every, pivot) in [
        ("static_trs", 0, false),
        ("mixed_trs", 10, false),
        ("animated_trs", 1, false),
        ("mixed_pivot", 10, true),
    ] {
        let (store, stage, paths) = xform_scene::scene(10_000, every, pivot);
        let scene = Scene::new(&stage, &store);
        group.bench_function(format!("{name}/cold"), |b| {
            b.iter(|| {
                let mut cache = XformCache::new(Time::at(0.5));
                for &path in &paths {
                    black_box(cache.local_to_world(&scene, path));
                }
                black_box(cache);
            });
        });
        let mut cache = XformCache::new(Time::at(0.0));
        for &path in &paths {
            black_box(cache.local_to_world(&scene, path));
        }
        let mut frame = 0;
        group.bench_function(format!("{name}/frames"), |b| {
            b.iter(|| {
                frame = (frame + 1) % 20;
                cache.set_time(Time::at(f64::from(frame) * 0.5));
                for &path in &paths {
                    black_box(cache.local_to_world(&scene, path));
                }
            });
        });
    }
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
