// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sample times, counts, bracketing samples and unions, on
//! [`Attribute`](layerstack::Attribute) and on a retained
//! [`AttributeQuery`], replayed call by call from
//! `sample_brackets_oracle.py` (see the script for the scene: layer
//! offsets, blocks, an animation block, a spline, sparse array edits, a
//! schema fallback and value clips).
//!
//! OpenUSD: `UsdAttribute::GetTimeSamples(InInterval)`,
//! `GetNumTimeSamples`, `GetBracketingTimeSamples`,
//! `GetUnionedTimeSamples(InInterval)` and the `UsdAttributeQuery` twins,
//! which the oracle checks answer the same.

use std::ops::Bound;
use std::path::PathBuf;
use std::sync::Arc;

use layerstack::{
    AttributeQuery, BracketingSamples, EditTarget, InMemoryStore, PropertyPath, StageOptions, Time,
    Transaction, Value,
};
use layerstack_io::{Filesystem, OpenOptions, StageDocument};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/sample_brackets/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd: String,
    intervals: Vec<Interval>,
    times: Vec<f64>,
    attributes: Vec<Reads>,
    unions: Vec<Union>,
}

/// `[min, max, min_closed, max_closed]`, `null` for an infinite end.
#[derive(Deserialize, Clone, Copy, Debug)]
struct Interval(Option<f64>, Option<f64>, bool, bool);

impl Interval {
    fn bounds(self) -> (Bound<f64>, Bound<f64>) {
        let end = |value: Option<f64>, closed: bool| match value {
            None => Bound::Unbounded,
            Some(value) if closed => Bound::Included(value),
            Some(value) => Bound::Excluded(value),
        };
        (end(self.0, self.2), end(self.1, self.3))
    }
}

#[derive(Deserialize)]
struct Reads {
    attribute: String,
    samples: Vec<f64>,
    count: usize,
    #[serde(rename = "in")]
    in_interval: Vec<Vec<f64>>,
    bracket: Vec<Bracket>,
    nan: Bracket,
}

#[derive(Deserialize)]
struct Union {
    attributes: Vec<String>,
    all: Vec<f64>,
    #[serde(rename = "in")]
    in_interval: Vec<Vec<f64>>,
}

/// `[lower, upper]`, `"none"` (success without samples), `false` (the call
/// fails) or `"error"` (it raises a coding error and fails).
#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum Bracket {
    Samples([f64; 2]),
    Failed(bool),
    Word(String),
}

/// A [`BracketingSamples`] as plain data, for comparing.
#[derive(Debug, PartialEq)]
enum Seen {
    Samples(f64, f64),
    NoSamples,
    NoValue,
}

impl Bracket {
    fn expected(&self) -> Seen {
        match self {
            Self::Samples([lower, upper]) => Seen::Samples(*lower, *upper),
            Self::Failed(false) => Seen::NoValue,
            Self::Word(word) if word == "none" => Seen::NoSamples,
            Self::Word(word) if word == "error" => Seen::NoValue,
            other => panic!("unknown bracket {other:?}"),
        }
    }
}

fn seen(bracketing: BracketingSamples) -> Seen {
    match bracketing {
        BracketingSamples::Samples(bracket) => Seen::Samples(bracket.lower, bracket.upper),
        BracketingSamples::NoSamples => Seen::NoSamples,
        BracketingSamples::NoValue => Seen::NoValue,
    }
}

/// Where layerstack deliberately answers differently, by attribute (or
/// `union ` and the union's attributes) and interval index: OpenUSD
/// 26.11's `_SamplesInIntervalResolver` (`pxr/usd/usd/stage.cpp`) lets a
/// weaker sample through when the dense sample that masks it lies on an
/// open interval bound: `(2, 5]` over `Mixed.e`'s dense 2 and sparse 5
/// reports the weaker 3, which `GetTimeSamples` rightly omits. layerstack
/// selects an interval from the composed grid, so an interval read is
/// always a run of the full one.
const DIVERGENCES: &[(&str, usize)] = &[("/Mixed.e", 3), ("union /Sparse.e /Mixed.e", 3)];

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
        return PathBuf::from("fixtures/sample_brackets");
    }
    layerstack_conformance::workspace_root().join("layerstack_conformance/fixtures/sample_brackets")
}

/// The stage, with OpenUSD's schemas (for `Ball.radius`'s fallback) and
/// the value clips every read needs loaded.
fn open(oracle: &Oracle) -> (StageDocument<Filesystem>, Vec<PropertyPath>) {
    let storage = Filesystem::new(directory(), []).expect("fixture directory");
    let store = InMemoryStore::default();
    let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
    let options = OpenOptions {
        stage: StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
        store: Some(store),
        ..OpenOptions::default()
    };
    let mut document = StageDocument::open(storage, "root.usda", options).expect("root.usda");
    let paths: Vec<_> = oracle
        .attributes
        .iter()
        .map(|reads| document.store_mut().property_path(&reads.attribute))
        .collect();
    for (&path, reads) in paths.iter().zip(&oracle.attributes) {
        let times = reads.samples.iter().chain(&oracle.times).copied();
        for time in core::iter::once(Time::Default).chain(times.map(Time::at)) {
            let prepared = document.prepare_clip_query(path, time, 16, 64);
            assert!(
                prepared.failures.is_empty(),
                "{}: {prepared:?}",
                reads.attribute
            );
        }
    }
    (document, paths)
}

fn in_range(times: &[f64], interval: Interval) -> Vec<f64> {
    use core::ops::RangeBounds;
    let bounds = interval.bounds();
    times
        .iter()
        .copied()
        .filter(|t| bounds.contains(t))
        .collect()
}

/// The interval read layerstack must give: OpenUSD's, or, where it
/// diverges, the run of the full grid.
fn expected_in(
    label: &str,
    index: usize,
    cpp: &[f64],
    full: &[f64],
    interval: Interval,
) -> Vec<f64> {
    if DIVERGENCES.contains(&(label, index)) {
        let grid = in_range(full, interval);
        assert_ne!(
            cpp, grid,
            "{label} in {interval:?}: OpenUSD agrees now; drop the divergence"
        );
        grid
    } else {
        cpp.to_vec()
    }
}

#[test]
fn attribute_sample_reads_match_openusd() {
    let oracle = oracle();
    let (document, paths) = open(&oracle);
    let view = document.view();
    for (&path, reads) in paths.iter().zip(&oracle.attributes) {
        let label = &reads.attribute;
        let attribute = view.attribute(path).expect(label);
        assert_eq!(attribute.sample_times(), reads.samples, "{label} samples");
        assert_eq!(attribute.sample_count(), reads.count, "{label} count");
        for (index, (&interval, cpp)) in oracle.intervals.iter().zip(&reads.in_interval).enumerate()
        {
            let expected = expected_in(label, index, cpp, &reads.samples, interval);
            assert_eq!(
                attribute.sample_times_in_range(interval.bounds()),
                expected,
                "{label} in {interval:?}"
            );
        }
        for (&time, bracket) in oracle.times.iter().zip(&reads.bracket) {
            assert_eq!(
                seen(attribute.bracketing_samples(time)),
                bracket.expected(),
                "{label} at {time}"
            );
        }
        assert_eq!(
            seen(attribute.bracketing_samples(f64::NAN)),
            reads.nan.expected(),
            "{label} at NaN"
        );
    }
}

#[test]
fn query_sample_reads_match_openusd_and_keep_the_grid() {
    let oracle = oracle();
    let (document, paths) = open(&oracle);
    let stage = document.stage().stage();
    for (&path, reads) in paths.iter().zip(&oracle.attributes) {
        let label = &reads.attribute;
        let mut query = AttributeQuery::new(path);
        assert_eq!(query.sample_times(stage), reads.samples, "{label} samples");
        assert_eq!(query.sample_count(stage), reads.count, "{label} count");
        for (index, (&interval, cpp)) in oracle.intervals.iter().zip(&reads.in_interval).enumerate()
        {
            let expected = expected_in(label, index, cpp, &reads.samples, interval);
            assert_eq!(
                query.sample_times_in_range(stage, interval.bounds()),
                expected,
                "{label} in {interval:?}"
            );
        }
        for (&time, bracket) in oracle.times.iter().zip(&reads.bracket) {
            assert_eq!(
                seen(query.bracketing_samples(stage, time)),
                bracket.expected(),
                "{label} at {time}"
            );
        }
        assert_eq!(
            seen(query.bracketing_samples(stage, f64::NAN)),
            reads.nan.expected()
        );
        // One composition served every read of the unchanged snapshot.
        let work = query.work();
        assert_eq!(work.sample_evaluations, 1, "{label}");
        assert_eq!(
            usize::try_from(work.sample_cache_hits).unwrap(),
            1 + oracle.intervals.len() + oracle.times.len() + 1,
            "{label}"
        );
    }
}

#[test]
fn unions_match_openusd() {
    let oracle = oracle();
    let (mut document, _) = open(&oracle);
    let paths: Vec<Vec<PropertyPath>> = oracle
        .unions
        .iter()
        .map(|union| {
            union
                .attributes
                .iter()
                .map(|name| document.store_mut().property_path(name))
                .collect()
        })
        .collect();
    let view = document.view();
    let stage = document.stage().stage();
    for (union, paths) in oracle.unions.iter().zip(&paths) {
        let label = format!("union {}", union.attributes.join(" "));
        let label = label.as_str();
        let attributes = || paths.iter().map(|&path| view.attribute(path).expect(label));
        let mut queries: Vec<_> = paths
            .iter()
            .map(|&path| AttributeQuery::new(path))
            .collect();
        assert_eq!(
            layerstack::Attribute::union_sample_times(attributes()),
            union.all,
            "union {:?}",
            union.attributes
        );
        assert_eq!(
            AttributeQuery::union_sample_times(&mut queries, stage),
            union.all,
            "query union {:?}",
            union.attributes
        );
        for (index, (&interval, cpp)) in oracle.intervals.iter().zip(&union.in_interval).enumerate()
        {
            let expected = expected_in(label, index, cpp, &union.all, interval);
            assert_eq!(
                layerstack::Attribute::union_sample_times_in_range(attributes(), interval.bounds()),
                expected,
                "union {:?} in {interval:?}",
                union.attributes
            );
            assert_eq!(
                AttributeQuery::union_sample_times_in_range(&mut queries, stage, interval.bounds()),
                expected,
                "query union {:?} in {interval:?}",
                union.attributes
            );
        }
    }
}

/// A query keeps its grid only for the snapshot it read: an edit that adds a
/// sample is seen by the next read, which composes the grid again.
#[test]
fn query_grid_follows_edits() {
    let oracle = oracle();
    let (mut document, _) = open(&oracle);
    let path = document.store_mut().property_path("/Plain.a");
    let mut query = AttributeQuery::new(path);
    let stage = document.stage().stage();
    assert_eq!(query.sample_times(stage), [1.0, 3.0, 5.0]);
    assert_eq!(
        seen(query.bracketing_samples(stage, 7.0)),
        Seen::Samples(5.0, 5.0)
    );
    let root = document.root_layer();
    let mut txn = Transaction::new();
    txn.set_time_sample(
        EditTarget::for_layer(root).property(path),
        8.0,
        Value::Double(8.0),
    );
    document.apply(&txn).expect("edit");
    let stage = document.stage().stage();
    assert_eq!(
        seen(query.bracketing_samples(stage, 7.0)),
        Seen::Samples(5.0, 8.0)
    );
    assert_eq!(query.sample_count(stage), 4);
    let work = query.work();
    assert_eq!((work.sample_evaluations, work.sample_cache_hits), (2, 2));
}
