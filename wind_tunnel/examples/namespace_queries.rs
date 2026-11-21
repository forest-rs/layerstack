// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Controls for namespace queries on small and wide authored prims.
//! Run with `cargo run --release -p wind_tunnel --example namespace_queries`.

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, Stage, StageOptions};

const ROOT_LAYER: LayerId = LayerId(1);

fn fixture(prims: usize, rows: usize) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let mut layer = Layer::new(ROOT_LAYER);
    let names: Vec<_> = (0..rows)
        .flat_map(|row| {
            [
                format!("table:row{row}:description:lang:en"),
                format!("table:row{row}:description:lang:sv"),
                format!("table:row{row}:amount"),
                format!("table:row{row}:code"),
            ]
        })
        .map(|name| store.tokens().intern(name))
        .collect();
    for prim in 0..prims {
        let path = store.path(&format!("/Table{prim}"));
        let mut spec = PrimSpec::def();
        for &name in &names {
            spec = spec.with_property(name, PropertySpec::relationship());
        }
        layer.insert_prim(path, spec);
    }
    store.insert_layer(layer);
    store
}

fn measure(label: &str, operations: usize, mut work: impl FnMut(usize)) {
    let start = Instant::now();
    for index in 0..operations {
        work(black_box(index));
    }
    println!(
        "{label},{:.3}",
        start.elapsed().as_secs_f64() * 1e9 / operations as f64
    );
}

fn measure_first_query(store: &mut InMemoryStore, rows: usize) {
    const REPETITIONS: u32 = 33;
    let mut first = Duration::ZERO;
    let mut second = Duration::ZERO;
    for _ in 0..REPETITIONS {
        let stage = Stage::compose(store, ROOT_LAYER, StageOptions::default());
        let prim = stage.view(store).prim("/Table0").unwrap();
        let start = Instant::now();
        let names = black_box(prim.authored_properties_in_namespace("table:row0:description"));
        first += start.elapsed();
        let start = Instant::now();
        let subsequent = black_box(prim.authored_properties_in_namespace("table:row1:description"));
        second += start.elapsed();
        assert_eq!(names.len(), 2, "the first query finds both localizations");
        assert_eq!(
            subsequent.len(),
            2,
            "the second query finds both localizations",
        );
    }
    for (case, elapsed) in [("first", first), ("second", second)] {
        println!(
            "{case}-hit-{rows},{:.3}",
            elapsed.as_secs_f64() * 1e9 / f64::from(REPETITIONS)
        );
    }
}

fn main() {
    println!("case,ns_per_operation");
    let mut many_small = fixture(1024, 4);
    measure("compose-many-small", 21, |_| {
        black_box(Stage::compose(
            &mut many_small,
            ROOT_LAYER,
            StageOptions::default(),
        ));
    });
    for rows in [4, 64, 8192] {
        let mut store = fixture(1, rows);
        measure(&format!("compose-{rows}"), 21, |_| {
            black_box(Stage::compose(
                &mut store,
                ROOT_LAYER,
                StageOptions::default(),
            ));
        });
        measure_first_query(&mut store, rows);
        let stage = Stage::compose(&mut store, ROOT_LAYER, StageOptions::default());
        let prim = stage.view(&store).prim("/Table0").unwrap();
        assert_eq!(
            prim.authored_properties_in_namespace("table:row0:description")
                .len(),
            2,
            "each row authors two localizations",
        );
        assert!(
            prim.authored_properties_in_namespace("absent").is_empty(),
            "the missing namespace has no authored properties",
        );
        assert_eq!(
            prim.authored_properties_in_namespace("table").len(),
            rows * 4,
            "the complete namespace contains every authored row",
        );
        let hits: Vec<_> = (0..rows)
            .map(|row| format!("table:row{row}:description"))
            .collect();
        let misses: Vec<_> = (0..rows)
            .map(|row| format!("table:row{row}:absent"))
            .collect();
        let names: Vec<_> = (0..rows)
            .map(|row| format!("table:row{row}:code"))
            .collect();
        for (kind, namespaces) in [("hit", &hits), ("miss", &misses)] {
            measure(&format!("{kind}-{rows}"), 8192, |index| {
                black_box(prim.authored_properties_in_namespace(&namespaces[index % rows]));
            });
        }
        measure(&format!("dense-{rows}"), 40, |_| {
            black_box(prim.authored_properties_in_namespace(black_box("table")));
        });
        measure(&format!("lookup-{rows}"), 2_000_000, |index| {
            black_box(prim.relationship(names[index % rows].as_str()));
        });
    }
}
