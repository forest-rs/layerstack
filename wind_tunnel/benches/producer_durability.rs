// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Repeated publication of immutable generated points at representative sizes.
//! Snapshot construction and initial publication are outside measured requests.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use layerstack::{EditTarget, InMemoryStore, Layer, LayerId, LiveStage, StageOptions};
use layerstack_schemas::GeneratedMesh;
use std::{hint::black_box, sync::Arc};

fn publication(c: &mut Criterion) {
    let mut group = c.benchmark_group("retained_mesh_publication");
    for count in [100, 4_096, 1_000_000] {
        group.bench_function(BenchmarkId::new("unchanged", count), |b| {
            let mut store = InMemoryStore::default();
            let root = LayerId(1);
            store.insert_layer(Layer::new(root));
            let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
            let target = EditTarget::for_layer(root);
            let path = store.path("/Generated");
            let mesh = GeneratedMesh {
                points: Arc::new(vec![[1., 2., 3.]; count]),
                ..Default::default()
            }
            .into_validated()
            .unwrap();
            let initial = mesh
                .prepare(live.stage(), &mut store, &target, path, &[])
                .unwrap();
            live.apply(&mut store, &initial.transaction).unwrap();
            let mut properties = initial.properties;
            b.iter(|| {
                let prepared = mesh
                    .prepare(live.stage(), &mut store, &target, path, &properties)
                    .unwrap();
                let applied = live.apply(&mut store, &prepared.transaction).unwrap();
                properties = prepared.properties;
                black_box(applied.changes);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, publication);
criterion_main!(benches);
