// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Phase probe: `stage_workflows root.usd output.usdc`. Sources are never saved in
//! place. Each phase reports wall time, current RSS (KiB when `ps` is available),
//! encoded ownership/copies, directory work and numeric decode/cache storage.
//! Run the built binary under the host memory profiler for peak allocation/RSS;
//! phase-boundary RSS is not peak memory and is not mapped address space.
use layerstack::{EditTarget, PrimPredicate, PropertyPath, StageOptions, Transaction};
use layerstack_io::{Filesystem, LoadOptions, StageDocument, UsdcArrayLoading, UsdcReadOptions};
use std::{hint::black_box, time::Instant};

fn report(document: &StageDocument<Filesystem>, phase: &str, started: Instant, units: usize) {
    let elapsed = started.elapsed().as_secs_f64() * 1e6;
    let rss = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse::<u64>().ok());
    let mut attempts = 0;
    let mut native = 0;
    let mut remaining = 0;
    for &id in &document.load_report().layers {
        if let Some(values) = document.retained_values(id) {
            let stats = values.stats();
            attempts += stats.decode_attempts;
            native += stats.element_bytes;
            remaining += stats.remaining_units;
        }
    }
    let work = document.package_cache_stats();
    println!(
        "{phase},{elapsed:.3},{units},{},{},{},{},{},{},{attempts},{native},{remaining}",
        rss.map_or_else(|| "unavailable".into(), |rss| rss.to_string()),
        work.resident_backing_bytes,
        work.encoded_bytes_copied,
        work.opens,
        work.index_parses,
        work.index_hits
    );
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args
        .next()
        .ok_or("usage: stage_workflows source.usd output.usdc")?;
    let output = args.next().ok_or("supply a distinct output.usdc path")?;
    let storage = Filesystem::new(".", [])?;
    use layerstack_io::Storage;
    if storage.identify(&source, None)? == storage.identify(&output, None)? {
        return Err("output must differ from source".into());
    }
    println!(
        "phase,wall_us,work_units,rss_kib,package_backing_bytes,encoded_copied_bytes,package_opens,index_parses,index_hits,decode_attempts,native_element_bytes,remaining_decode_units"
    );
    let started = Instant::now();
    let mut document = StageDocument::open_with(
        storage,
        &source,
        StageOptions::default(),
        LoadOptions {
            usdc: UsdcReadOptions {
                arrays: UsdcArrayLoading::Retained,
                ..Default::default()
            },
            ..Default::default()
        },
    )?;
    report(
        &document,
        "open_zero_arrays",
        started,
        document.load_report().layers.len(),
    );
    let root = document.store_mut().path("/");
    let started = Instant::now();
    let count = document
        .stage()
        .stage()
        .prim_range(root, document.store(), PrimPredicate::DEFAULT)
        .count();
    report(&document, "traverse", started, black_box(count));
    let started = Instant::now();
    let properties: Vec<_> = document
        .stage()
        .stage()
        .traverse_all(root)
        .flat_map(|prim| {
            document
                .stage()
                .stage()
                .property_names(prim, document.store())
                .into_iter()
                .map(move |field| PropertyPath::new(prim, field))
        })
        .filter(|property| {
            document
                .stage()
                .stage()
                .resolve_property_declaration(property.prim_path(), property.property())
                .is_some_and(|d| d.type_name.is_some_and(|t| t.is_array))
        })
        .collect();
    report(
        &document,
        "inventory_zero_arrays",
        started,
        properties.len(),
    );
    let started = Instant::now();
    if let Some(property) = properties.first() {
        black_box(document.stage().stage().try_resolve_property_path_at_time(
            *property,
            5.,
            layerstack::InterpolationType::Linear,
        )?);
    }
    report(
        &document,
        "demand_one_array",
        started,
        usize::from(!properties.is_empty()),
    );
    let started = Instant::now();
    for &property in &properties {
        black_box(document.stage().stage().try_resolve_property_path_at_time(
            property,
            5.,
            layerstack::InterpolationType::Linear,
        )?);
    }
    report(&document, "demand_all_arrays", started, properties.len());
    let started = Instant::now();
    for &property in &properties {
        black_box(document.stage().stage().try_resolve_property_path_at_time(
            property,
            5.,
            layerstack::InterpolationType::Linear,
        )?);
    }
    report(&document, "demand_all_warm", started, properties.len());
    let root_layer = document
        .stage()
        .stage()
        .root_layer()
        .ok_or("root layer absent")?;
    let prim = document
        .stage()
        .stage()
        .traverse_all(root)
        .find(|prim| *prim != root);
    let documentation = document.store_mut().tokens.intern("documentation");
    let started = Instant::now();
    let mut inverse = None;
    if let Some(prim) = prim {
        let mut transaction = Transaction::new();
        transaction.set_metadata(
            EditTarget::for_layer(root_layer).prim(prim),
            documentation,
            layerstack::Value::String("workflow probe edit".into()).into(),
        );
        let (store, stage) = document.parts_mut();
        inverse = Some(stage.apply(store, &transaction)?.inverse);
    }
    report(
        &document,
        "edit_and_synchronize",
        started,
        usize::from(inverse.is_some()),
    );
    if let Some(inverse) = inverse {
        let (store, stage) = document.parts_mut();
        stage.apply(store, &inverse)?;
    }
    let started = Instant::now();
    document.export_layer(root_layer, &output)?;
    report(&document, "serialize_and_replace", started, 1);
    Ok(())
}
