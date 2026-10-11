// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Wide/deep traversal probe. Setup/composition excluded; warmed medians of seven
//! 75 ms windows. An optional output path records ordered namespace identities.
use layerstack::{InMemoryStore, Layer, LayerId, PrimPredicate, PrimSpec, Stage, StageOptions};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

fn median(mut operation: impl FnMut() -> usize, expected: usize) -> f64 {
    assert_eq!(
        operation(),
        expected,
        "workload visits every populated prim"
    );
    let mut samples = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let mut count = 0;
        while start.elapsed() < Duration::from_millis(75) {
            assert_eq!(
                black_box(operation()),
                expected,
                "timed work stays equivalent"
            );
            count += 1;
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / f64::from(count));
    }
    samples.sort_by(f64::total_cmp);
    samples[3]
}
fn main() {
    let mut identities = String::new();
    println!("shape,n,inspection_us,predicate_all_us,predicate_default_us");
    for shape in ["wide", "deep"] {
        for n in [32, 64, 128, 256] {
            let mut store = InMemoryStore::default();
            let mut layer = Layer::new(LayerId(1));
            let mut path = String::new();
            for i in 0..n {
                if shape == "wide" {
                    path = format!("/P{i}");
                } else {
                    path.push_str("/P");
                }
                let id = store.path(&path);
                layer.insert_prim(id, PrimSpec::def());
            }
            store.insert_layer(layer);
            let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
            let root = store.path("/");
            let snapshot = |predicate| {
                stage
                    .prim_range(root, &store, predicate)
                    .filter(|p| *p != root)
                    .map(|p| store.paths.display(p, &store.tokens))
                    .collect::<Vec<_>>()
            };
            let all_paths = snapshot(PrimPredicate::ALL);
            assert_eq!(
                all_paths,
                snapshot(PrimPredicate::DEFAULT),
                "predicates visit identical ordered paths in this fixture"
            );
            identities.push_str(&format!("{shape}\t{n}\t{}\n", all_paths.join(" ")));
            let inspection = median(|| stage.traverse(root).count(), n + 1);
            let all = median(
                || stage.prim_range(root, &store, PrimPredicate::ALL).count(),
                n + 1,
            );
            let default = median(
                || {
                    stage
                        .prim_range(root, &store, PrimPredicate::DEFAULT)
                        .count()
                },
                n + 1,
            );
            println!("{shape},{n},{inspection:.3},{all:.3},{default:.3}");
        }
    }
    if let Some(output) = std::env::args_os().nth(1) {
        std::fs::write(output, identities).expect("write requested namespace identities");
    }
}
