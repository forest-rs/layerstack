// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fresh-process prototype storage probe. Run `prototype_storage 1000 64`.
//! Source buffers are small; repeated composed indexes dominate this workload.
//! Measure peak resident memory externally, e.g. `/usr/bin/time -l` on macOS.

use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Reference, Stage, StageOptions, Value};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let count: usize = args
        .get(1)
        .map_or(Ok(1000), |s| s.parse())
        .expect("instance count");
    let children: usize = args
        .get(2)
        .map_or(Ok(64), |s| s.parse())
        .expect("child count");
    let mut store = InMemoryStore::default();
    let asset_path = store.path("/Asset");
    let world = store.path("/World");
    let fields: Vec<_> = (0..4)
        .map(|i| store.tokens.intern(format!("field{i}")))
        .collect();
    let mut asset = Layer::new(LayerId(2));
    let mut names = Vec::new();
    for i in 0..children {
        names.push(store.tokens.intern(format!("Child{i}")));
        let path = store.path(&format!("/Asset/Child{i}"));
        let mut spec = PrimSpec::def();
        for (n, &field) in fields.iter().enumerate() {
            spec.set_field(field, Value::Int(i32::try_from(i + n).unwrap()));
        }
        asset.insert_prim(path, spec);
    }
    asset.insert_prim(asset_path, PrimSpec::def().with_children(names));
    store.insert_layer(asset);
    let mut layer = Layer::new(LayerId(1));
    let mut names = Vec::new();
    for i in 0..count {
        names.push(store.tokens.intern(format!("Instance{i}")));
        let path = store.path(&format!("/World/Instance{i}"));
        layer.insert_prim(
            path,
            PrimSpec::def()
                .with_instanceable(true)
                .with_reference(Reference::with_asset(LayerId(2), asset_path, "asset.usdc")),
        );
    }
    layer.insert_prim(world, PrimSpec::def().with_children(names));
    store.insert_layer(layer);
    let start = Instant::now();
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let compose_ms = start.elapsed().as_secs_f64() * 1000.;
    assert!(
        stage.composition_errors().is_empty(),
        "{:?}",
        stage.composition_errors()
    );
    let root = store.path("/");
    let prims = stage.traverse(root).count();
    let start = Instant::now();
    let mut checksum = 0_i64;
    for prim in stage.traverse(root) {
        if let Some(value) = stage.resolve_value(prim, fields[0])
            && let layerstack::ResolvedValue::Scalar(Value::Int(v)) = value.value
        {
            checksum += i64::from(v);
        }
    }
    black_box(&stage);
    let query_ms = start.elapsed().as_secs_f64() * 1000.;
    assert_eq!(
        checksum,
        i64::try_from(count * children * children.saturating_sub(1) / 2).unwrap(),
        "every proxy must preserve its source value"
    );
    let storage = stage.composition_storage();
    println!(
        "{{\"instances\":{count},\"children\":{children},\"prims\":{prims},\"record_buffers\":{},\"logical_opinions\":{},\"physical_opinions\":{},\"compose_ms\":{compose_ms:.3},\"query_ms\":{query_ms:.3}}}",
        storage.record_buffers, storage.logical_opinions, storage.physical_opinions
    );
}
