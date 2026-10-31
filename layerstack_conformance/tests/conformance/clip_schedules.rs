// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Which clips a clip set schedules, loads and reads, replayed call by
//! call from `clip_schedules_oracle.py` (see the script for the scene and
//! what it records).
//!
//! The document loads exactly what each clip query plan demands
//! ([`StageDocument::prepare_clip_query`]); after every OpenUSD call the
//! clip layers it holds must be among those OpenUSD has opened, so it
//! never loads a clip OpenUSD does not open, such as an unscheduled one.
//!
//! OpenUSD: `Usd_ClipSet` (`pxr/usd/usd/clipSet.cpp`),
//! `UsdAttribute::Get` and `GetTimeSamples`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use layerstack::value_clips::ClipIssueKind;
use layerstack::{PropertyPath, StageOptions, Time};
use layerstack_io::{Filesystem, StageDocument};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/clip_schedules/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd: String,
    times: Vec<f64>,
    open: Vec<String>,
    get: Vec<Get>,
    samples: Vec<Samples>,
}

#[derive(Deserialize)]
struct Get {
    attribute: String,
    values: Vec<Read>,
    loaded: Vec<String>,
}

/// A value OpenUSD read, or the error it raised.
#[derive(Deserialize)]
#[serde(untagged)]
enum Read {
    Value(Option<f64>),
    Error(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Samples {
    attribute: String,
    times: Vec<f64>,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert!(
        oracle.openusd.starts_with("26."),
        "oracle from OpenUSD {}",
        oracle.openusd
    );
    oracle
}

/// The fixture directory; under WASI, relative to the preopened crate
/// directory.
fn directory() -> PathBuf {
    if cfg!(target_os = "wasi") {
        return PathBuf::from("fixtures/clip_schedules");
    }
    layerstack_conformance::workspace_root().join("layerstack_conformance/fixtures/clip_schedules")
}

fn open() -> StageDocument<Filesystem> {
    let storage = Filesystem::new(directory(), []).expect("fixture directory");
    StageDocument::open(storage, "root.usda", StageOptions::default()).expect("root.usda")
}

/// The clip and manifest layers the document holds, by file name.
fn loaded(document: &StageDocument<Filesystem>) -> BTreeSet<String> {
    document
        .store()
        .layer_ids()
        .into_iter()
        .filter_map(|id| document.identifier(id))
        .filter_map(|identifier| Path::new(identifier).file_name()?.to_str())
        .filter(|name| !matches!(*name, "root.usda" | "weak.usda"))
        .map(str::to_owned)
        .collect()
}

fn assert_loaded_within(document: &StageDocument<Filesystem>, openusd: &[String], call: &str) {
    let openusd: BTreeSet<_> = openusd.iter().cloned().collect();
    let ours = loaded(document);
    assert!(
        ours.is_subset(&openusd),
        "{call}: layerstack loaded {ours:?}, OpenUSD only {openusd:?}"
    );
}

fn property(document: &mut StageDocument<Filesystem>, path: &str) -> PropertyPath {
    document.store_mut().property_path(path)
}

/// `Get` at each time and `GetTimeSamples`, loading what each timed plan
/// demands: only the clips `active` schedules are clips, and a jump
/// discontinuity that starts `times` is dropped.
#[test]
fn clip_reads_match_openusd() {
    let oracle = oracle();
    let mut document = open();
    assert_loaded_within(&document, &oracle.open, "open");
    for get in &oracle.get {
        let path = property(&mut document, &get.attribute);
        for (&time, read) in oracle.times.iter().zip(&get.values) {
            let prepared = document.prepare_clip_query(path, Time::at(time), 16, 64);
            let label = format!("{} at {time}", get.attribute);
            match read {
                // OpenUSD raises where the read needs a clip that does not
                // parse; the plan reports the failed load.
                Read::Error(_) => assert!(!prepared.failures.is_empty(), "{label}: {prepared:?}"),
                // A clip that does not parse, active at `time`, reads as a
                // gap in both, OpenUSD warning and the plan reporting it.
                Read::Value(expected) => {
                    let view = document.view();
                    let attribute = view.attribute(path).expect("attribute");
                    let value = attribute.value::<f32>(Time::at(time)).map(f64::from);
                    match (value, expected) {
                        (Some(a), Some(b)) => assert!((a - b).abs() < 1e-6, "{label}: {a} != {b}"),
                        _ => assert_eq!(value, *expected, "{label}"),
                    }
                }
            }
        }
        assert_loaded_within(&document, &get.loaded, &format!("get {}", get.attribute));
    }

    for samples in &oracle.samples {
        let path = property(&mut document, &samples.attribute);
        let view = document.view();
        let times = view.attribute(path).expect("attribute").sample_times();
        assert_eq!(times, samples.times, "{} samples", samples.attribute);
    }
}

/// A rejected schedule demands nothing, and its plans say why;
/// OpenUSD opens no clip of a set it rejects (`Usd_ClipSet::New`).
#[test]
fn rejected_schedules_demand_nothing() {
    let oracle = oracle();
    let mut document = open();
    for name in ["/DupActive.size", "/TripleTimes.size"] {
        let path = property(&mut document, name);
        for time in oracle.times.iter().map(|&t| Time::at(t))
        {
            let plan = document.stage().stage().plan_clip_query(path, time, 64);
            assert!(plan.demands.is_empty(), "{name} at {time:?}: {plan:?}");
            assert!(
                plan.issues
                    .iter()
                    .any(|issue| issue.kind == ClipIssueKind::InvalidSchedule),
                "{name} at {time:?}: {plan:?}"
            );
        }
    }
    assert!(loaded(&document).is_empty());
}
