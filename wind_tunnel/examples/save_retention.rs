// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Save/reopen ownership probe: `save_retention source.usdc fresh.usdc [snapshot]`.
//! The destination must not exist. Explicit reload is a measurement surrogate
//! for releasing the document's old sources, not a silent post-save rebind API.
use layerstack::{EditTarget, PropertyPath, StageOptions, Time, Transaction, Value};
use layerstack_io::{
    Filesystem, LoadOptions, ReloadPolicy, StageDocument, UsdcArrayLoading, UsdcReadOptions,
};
use std::{hint::black_box, io::Write, time::Instant};

fn report(
    document: &StageDocument<Filesystem>,
    root: layerstack::LayerId,
    old: &layerstack_usdc::RetainedValues,
    phase: &str,
    started: Instant,
) {
    let wall_us = started.elapsed().as_secs_f64() * 1e6;
    let current = document.retained_values(root).unwrap().stats();
    let old = old.stats();
    let rss = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok());
    println!(
        "{phase},{wall_us:.3},{},{},{},{},{},{}",
        current.input_bytes,
        current.element_bytes,
        current.decode_attempts,
        old.element_bytes,
        rss.map_or_else(|| "unavailable".into(), |n| n.to_string()),
        document.store().layer(root).unwrap().generation()
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args.next().ok_or("supply source.usdc")?;
    let target = args.next().ok_or("supply a new scratch.usdc destination")?;
    let keep_snapshot = match args.next().as_deref() {
        None => false,
        Some("snapshot") => true,
        _ => return Err("optional third argument must be snapshot".into()),
    };
    // Never replace the input or an existing user file.
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    output.write_all(&std::fs::read(source)?)?;
    drop(output);
    let started = Instant::now();
    let mut document = StageDocument::open_with(
        Filesystem::new(".", [])?,
        &target,
        StageOptions::default(),
        LoadOptions {
            usdc: UsdcReadOptions {
                arrays: UsdcArrayLoading::Retained,
                ..Default::default()
            },
            ..Default::default()
        },
    )?;
    let root_layer = document
        .stage()
        .stage()
        .root_layer()
        .ok_or("missing root")?;
    let root = document.store_mut().path("/");
    let old = document
        .retained_values(root_layer)
        .ok_or("source must be USDC")?
        .clone();
    println!(
        "phase,wall_us,encoded_bytes,cached_native_bytes,decode_attempts,old_cached_native_bytes,rss_kib,generation"
    );
    report(&document, root_layer, &old, "open", started);
    let view = document.view();
    let properties: Vec<PropertyPath> = document
        .stage()
        .stage()
        .traverse_all(root)
        .filter_map(|prim| view.prim(prim))
        .flat_map(|prim| prim.properties())
        .filter(|property| {
            property
                .declaration()
                .is_some_and(|d| d.type_name.is_some_and(|t| t.is_array()))
        })
        .map(|property| property.path())
        .collect();
    let attributes: Vec<_> = properties
        .iter()
        .filter_map(|property| view.attribute(*property))
        .collect();
    let started = Instant::now();
    for attribute in &attributes {
        black_box(attribute.try_read(Time::at(5.), |_| Some(()))?);
    }
    report(&document, root_layer, &old, "warm_all", started);
    let mut snapshot = keep_snapshot.then(|| document.store().snapshot());
    let first = document
        .stage()
        .stage()
        .traverse_all(root)
        .find(|p| *p != root)
        .ok_or("source must contain a prim")?;
    let documentation = document.store_mut().tokens().intern("documentation");
    let mut edit = Transaction::new();
    edit.set_metadata(
        EditTarget::for_layer(root_layer).prim(first),
        documentation,
        Value::String("save retention probe".into()).into(),
    );
    let (store, stage) = document.parts_mut();
    stage.apply(store, &edit)?;
    let mut cursor = stage.change_cursor();
    let generation = document.store().layer(root_layer).unwrap().generation();
    let started = Instant::now();
    let saved = document.save_layers(&[root_layer]);
    assert!(saved.failures.is_empty(), "{saved:?}");
    assert_eq!(saved.saved, [root_layer], "dirty root is saved");
    assert_eq!(
        document.store().layer(root_layer).unwrap().generation(),
        generation,
        "save preserves authored generation"
    );
    assert!(
        !document.is_dirty(root_layer),
        "successful save marks root clean"
    );
    document.synchronize();
    assert_eq!(
        document
            .stage()
            .changes_since(&mut cursor)
            .expect("save preserves change history")
            .count(),
        0,
        "save emits no scene edit"
    );
    report(&document, root_layer, &old, "save_keep_sources", started);
    let saved_bytes = std::fs::read(&target)?;
    let started = Instant::now();
    for _ in 0..100 {
        let mut budget = layerstack_usdc::DecodeBudget::for_input(saved_bytes.len());
        black_box(layerstack_usdc::CrateFile::open(&saved_bytes, &mut budget)?);
    }
    report(
        &document,
        root_layer,
        &old,
        "open_saved_tables_100",
        started,
    );
    let started = Instant::now();
    document.reload(ReloadPolicy::PreserveDirty)?;
    report(&document, root_layer, &old, "explicit_reload_cold", started);
    if !keep_snapshot {
        assert_eq!(
            old.stats().element_bytes,
            0,
            "document releases old numeric sources"
        );
    }
    let view = document.view();
    let attributes: Vec<_> = properties
        .iter()
        .filter_map(|property| view.attribute(*property))
        .collect();
    let started = Instant::now();
    for attribute in &attributes {
        black_box(attribute.try_read(Time::at(5.), |_| Some(()))?);
    }
    report(&document, root_layer, &old, "requery_all", started);
    let started = Instant::now();
    drop(snapshot.take());
    assert_eq!(
        old.stats().element_bytes,
        0,
        "last old snapshot releases numeric sources"
    );
    report(&document, root_layer, &old, "drop_old_snapshot", started);
    Ok(())
}
