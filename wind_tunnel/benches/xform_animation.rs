// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored stacks, mixed animation, and advancing frame evaluation.
#[path = "support/xform_selection_scene.rs"]
mod selection_scene;
#[path = "support/xform_scene.rs"]
mod xform_scene;
use criterion::{Criterion, criterion_group, criterion_main};
use layerstack::{EditTarget, PropertyPath, Transaction, Value};
use layerstack_schemas::{Scene, Time, XformCache, XformSelection};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

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
fn selection_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("xform_selection");
    group.sample_size(10);
    for (name, every, offset) in [
        ("static", 0, false),
        ("mixed", 10, false),
        ("dense", 1, false),
        ("layer_offset", 1, true),
    ] {
        let (store, live, all_paths, _) = selection_scene::scene(every, offset);
        let scene = Scene::new(live.stage(), &store);
        for count in [48, selection_scene::LEAVES] {
            let paths: Vec<_> = (0..count)
                .map(|i| all_paths[i * 1777 % all_paths.len()])
                .collect();
            let start = Time::at(0.);
            let mut cache = XformCache::new(start);
            let mut moving = XformSelection::new(&scene, paths.iter().copied(), start);
            let mut output = vec![None; count];
            let evaluate = |cache: &mut XformCache, output: &mut [Option<[[f64; 4]; 4]>]| {
                for (&path, row) in paths.iter().zip(output) {
                    *row = cache.local_to_world(&scene, path);
                }
            };
            evaluate(&mut cache, &mut output);
            for time in [Time::Default, Time::held(0.5), Time::at(7.5), Time::at(16.)] {
                cache.set_time(time);
                evaluate(&mut cache, &mut output);
                moving.set_time(&scene, time);
                assert_eq!(
                    moving.matrices(),
                    &output,
                    "{name} transforms differ for {count} selected paths at {time:?}",
                );
            }
            group.bench_function(format!("{name}/{count}/cache_cold"), |b| {
                b.iter(|| {
                    let mut cache = XformCache::new(start);
                    let mut output = vec![None; count];
                    evaluate(&mut cache, &mut output);
                    black_box((cache, output));
                });
            });
            group.bench_function(format!("{name}/{count}/selection_cold"), |b| {
                b.iter(|| {
                    black_box(XformSelection::new(&scene, paths.iter().copied(), start));
                });
            });
            let mut frame = 0;
            group.bench_function(format!("{name}/{count}/cache_frames"), |b| {
                b.iter(|| {
                    frame = (frame + 1) % 32;
                    cache.set_time(Time::at(f64::from(frame) * 0.5));
                    evaluate(&mut cache, &mut output);
                    black_box(&output);
                });
            });
            frame = 0;
            group.bench_function(format!("{name}/{count}/selection_frames"), |b| {
                b.iter(|| {
                    frame = (frame + 1) % 32;
                    moving.set_time(&scene, Time::at(f64::from(frame) * 0.5));
                    black_box(moving.matrices());
                });
            });
        }
    }
    for mode in ["cache", "selection"] {
        let (mut store, mut live, paths, translate) = selection_scene::scene(10, false);
        let time = Time::at(3.5);
        let scene = Scene::new(live.stage(), &store);
        let mut selection =
            (mode == "selection").then(|| XformSelection::new(&scene, paths.iter().copied(), time));
        let mut cache = XformCache::new(time);
        let mut output = Vec::new();
        if selection.is_none() {
            output.extend(paths.iter().map(|&path| cache.local_to_world(&scene, path)));
        }
        let mut edit = Transaction::new();
        edit.set_time_sample(
            EditTarget::for_layer(selection_scene::SOURCE)
                .property(PropertyPath::new(paths[0], translate)),
            3.,
            Value::Vec3d([3., 7., 12.]),
        );
        group.bench_function(format!("mixed/5000/{mode}_edit"), |b| {
            b.iter_custom(|iterations| {
                let mut total = Duration::ZERO;
                for _ in 0..iterations {
                    let applied = live.apply(&mut store, &edit).unwrap();
                    let scene = Scene::new(live.stage(), &store);
                    let start = Instant::now();
                    if let Some(selection) = &mut selection {
                        selection.apply_changes(&scene, &applied.changes);
                        black_box(selection.matrices());
                    } else {
                        cache.apply_changes(&scene, &applied.changes);
                        for (&path, row) in paths.iter().zip(&mut output) {
                            *row = cache.local_to_world(&scene, path);
                        }
                        black_box(&output);
                    }
                    total += start.elapsed();
                    edit = applied.inverse;
                }
                total
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench, selection_bench);
criterion_main!(benches);
