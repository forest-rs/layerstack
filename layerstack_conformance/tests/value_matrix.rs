// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for attribute value resolution across value sources.
//!
//! `fixtures/value_matrix/oracle.json` records what OpenUSD 26.08 resolves
//! for each case's stage (`scripts/value_matrix_oracle.py`, which also writes
//! the cases' layers under `fixtures/value_matrix/<case>/`): every attribute
//! of every subject at the default time and at each query time, under held
//! and linear interpolation, with its resolve info, bracketing samples, time
//! samples, sample intervals, time variance and composed `customData`. The
//! script's docs describe the subjects; together they cover every pair of
//! value kind, value form or source (defaults, time samples, blocks,
//! animation blocks, splines, sparse array edits, value clips, schema
//! fallbacks, layer offsets and time code rates) and composition arc.
//!
//! Layerstack reads each value through `Attribute::get` and a retained
//! `AttributeQuery`, its source through `Attribute::explain` (whose value
//! must be the read's), its samples through `Attribute::sample_times` and
//! `sample_times_in_range`, and its time variance through
//! `Attribute::might_be_time_varying`. Bracketing samples are those the
//! composed sample grid brackets the time with, as
//! `UsdAttribute::GetBracketingTimeSamples` reports them. Asset paths
//! resolve against the layer of the winning opinion
//! (`Attribute::read_with_provenance`), as OpenUSD anchors them.
//!
//! An explanation names its source as `UsdResolveInfo` does, with two
//! differences of form: a sampled block in effect is
//! [`ValueSource::None`] with a [`OpinionRole::Block`] opinion whose samples
//! say where it is held, where OpenUSD names the time samples and marks the
//! value blocked; and an explanation lists the opinions a value composed,
//! where OpenUSD ends a chain that composed over nothing with a `None` link
//! and continues a path expression's chain past one that does not splice in
//! the next weaker one (`%_`), since the type composes.
//!
//! Layerstack must reproduce every recorded result, except for the named
//! divergences in [`DIVERGENCES`]: there it must produce the query's pinned
//! `expected` result, which OpenUSD does not.
//!
//! Spec: AOUSD Core §12.3 (value resolution: §12.3.1 defaults, §12.3.2 time
//! samples and layer offsets, §12.3.3 splines, §12.3.4 value clips, §12.3.5
//! fallbacks, §12.3.6 blocks), §12.2.5 (dictionaries combine), §9.4 (asset
//! paths anchor at their layer).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::Arc;

use layerstack::asset::anchor_asset_path;
use layerstack::{
    ArcKind, AttributeQuery, ExplainedOpinion, InterpolationType, LayerId, OpinionRole, PrimNode,
    PropertyPath, Stage, StageOptions, Time, Value, ValueExplanation, ValueSource,
};
use layerstack_conformance::usda_real::{LoadedStage, load_entry_usda};
use layerstack_conformance::workspace_root;
use serde::Deserialize;
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../fixtures/value_matrix/oracle.json");

/// The Core-versus-OpenUSD divergences Layerstack follows, with the OpenUSD
/// release each was confirmed against. `docs/generic-sparse-composition.md`
/// ("Divergences From OpenUSD") gives each one's policy, authority and
/// OpenUSD source line; the oracle script records the same names.
const DIVERGENCES: &[(&str, &str)] = &[
    // Before the first composed sample, hold it instead of OpenUSD's NaN
    // (AOUSD Core 12.5.1).
    ("nan-before-first-sample", "0.26.8"),
    // A held sampled block resolves the schema fallback (AOUSD Core 12.3.6,
    // 16.2.16.3).
    ("sampled-block-drops-fallback", "0.26.8"),
    // A default block resolves the schema fallback at the default time too
    // (AOUSD Core 12.3.6, 16.2.16.2).
    ("default-time-block-hides-fallback", "0.26.8"),
    // Edits over a default block compose over the empty array at the
    // default time too (AOUSD Core 12.3.6).
    ("default-time-edits-over-block", "0.26.8"),
    // Sampled path expressions anchor at the authoring prim, as defaults do.
    ("sampled-path-expression-anchoring", "0.26.8"),
    // An instance proxy's clip values vary as its prototype's do.
    ("instance-proxy-clips-not-varying", "0.26.8"),
    // Brackets follow the composed sample grid behind layer offsets.
    ("bracketing-composability-in-stage-time", "0.26.8"),
    // A missing clip value holds the last composed sample across a jump.
    ("missing-clip-values-across-jump", "0.26.8"),
];

/// Mismatches a later change fixes: the kind of result, the attribute's
/// name (a `*` in it matches any run of characters), and why it differs.
const KNOWN: &[(&str, &str, &str)] = &[
    ("source", "dbl", CLIP_NODES),
    ("source", "arr", CLIP_NODES),
    ("source", "tok", CLIP_NODES),
    ("source", "asset", CLIP_NODES),
    ("source", "tc", CLIP_NODES),
    ("source", "gap", CLIP_NODES),
    ("source", "radius", CLIP_NODES),
    ("customData", "cd*_dbl", DICTIONARY_ASSETS),
];

const CLIP_NODES: &str = "clip explanations do not name the clip set's node";
const DICTIONARY_ASSETS: &str = "dictionary asset paths cannot be anchored";

/// Whether `name` matches the [`KNOWN`] pattern `pattern`, in which one `*`
/// matches any run of characters.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        Some((head, tail)) => {
            name.len() >= head.len() + tail.len() && name.starts_with(head) && name.ends_with(tail)
        }
        None => pattern == name,
    }
}

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    divergences: BTreeMap<String, Divergence>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Divergence {
    openusd: String,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    /// Each resolve info the case's records name, as
    /// `source|arc|site|layer stack`, then each next weaker info.
    infos: Vec<String>,
    prims: Vec<PrimRecord>,
}

#[derive(Deserialize)]
struct PrimRecord {
    /// The query times; the records' value rows lead with the default time.
    times: Vec<f64>,
    /// The bounds of the closed and the open sample interval.
    interval: [f64; 2],
    attrs: Vec<AttrRecord>,
}

#[derive(Deserialize)]
struct AttrRecord {
    attr: String,
    defined: bool,
    #[serde(rename = "type")]
    type_name: Option<String>,
    #[serde(default)]
    samples: Vec<f64>,
    /// Samples in the full, the closed and the open interval.
    #[serde(default)]
    intervals: Vec<Vec<f64>>,
    #[serde(default)]
    varying: bool,
    custom_data: Option<Json>,
    #[serde(default)]
    held: Vec<Json>,
    /// Absent when every linear value is the held one.
    linear: Option<Vec<Json>>,
    #[serde(default)]
    info: Vec<usize>,
    #[serde(default)]
    brackets: Vec<Option<[f64; 2]>>,
    #[serde(default)]
    pins: Vec<Pin>,
}

/// Layerstack's result where OpenUSD's differs on purpose: the field
/// (`value`, `info`, `brackets` or `varying`), the interpolation of a value (`held`
/// or `linear`, else `""`), the index in its row, the expected result and
/// the divergence's name.
#[derive(Deserialize)]
struct Pin(String, String, usize, Json, String);

impl AttrRecord {
    fn values(&self, interpolation: InterpolationType) -> &[Json] {
        match interpolation {
            InterpolationType::Linear => self.linear.as_deref().unwrap_or(&self.held),
            _ => &self.held,
        }
    }

    fn pin(&self, field: &str, mode: &str, index: usize) -> Option<&Pin> {
        self.pins
            .iter()
            .find(|pin| pin.0 == field && pin.1 == mode && pin.2 == index)
    }

    /// The attribute's name, after its prim path.
    fn name(&self) -> &str {
        self.attr.rsplit_once('.').map_or("", |(_, name)| name)
    }
}

fn mode(interpolation: InterpolationType) -> &'static str {
    match interpolation {
        InterpolationType::Linear => "linear",
        _ => "held",
    }
}

/// One case's composed stage, with the schema fallbacks of OpenUSD's
/// schemas.
struct Composed {
    loaded: LoadedStage,
    stage: Stage,
    dir: PathBuf,
}

impl Composed {
    fn open(case: &str) -> Self {
        let dir = workspace_root()
            .join("layerstack_conformance/fixtures/value_matrix")
            .join(case);
        let mut loaded = load_entry_usda(&dir.join("root.usda"));
        assert!(loaded.invalid.is_empty(), "{case}: {:?}", loaded.invalid);
        let schemas = Arc::new(layerstack_schemas::openusd(&loaded.store.tokens));
        let stage = Stage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
        );
        Self { loaded, stage, dir }
    }

    fn layer_name(&self, layer: LayerId) -> &str {
        self.loaded
            .layer_names
            .get(&layer)
            .map_or("?", String::as_str)
    }

    /// `asset` anchored at `layer` and resolved under the case directory,
    /// relative to it; `""` when nothing resolves there.
    fn resolve_asset(&self, asset: &str, layer: Option<LayerId>) -> String {
        // Anchor under a stand-in root for the case directory, so the
        // anchored path is relative to it once the root is stripped.
        const ROOT: &str = "/case/";
        if asset.is_empty() {
            return String::new();
        }
        let location = layer.map_or_else(
            || format!("{ROOT}root.usda"),
            |layer| format!("{ROOT}{}", self.layer_name(layer)),
        );
        anchor_asset_path(asset, &location)
            .and_then(|anchored| anchored.strip_prefix(ROOT).map(str::to_string))
            .filter(|relative| self.dir.join(relative).is_file())
            .unwrap_or_default()
    }

    /// A value in the vectors' JSON form, asset paths anchored at `layer`.
    fn encode(&self, value: &Value, layer: Option<LayerId>) -> Json {
        let tokens = &self.loaded.store.tokens;
        match value {
            Value::Blocked | Value::Null | Value::AnimationBlock => Json::Null,
            Value::Bool(v) => Json::from(*v),
            Value::Int(v) => Json::from(*v),
            Value::Int64(v) => Json::from(*v),
            Value::UInt(v) => Json::from(*v),
            Value::UInt64(v) => Json::from(*v),
            Value::UChar(v) => Json::from(*v),
            Value::Float(v) => number(f64::from(*v)),
            Value::Double(v) | Value::TimeCode(v) => number(*v),
            Value::String(v) => Json::from(v.as_ref()),
            Value::Token(v) => Json::from(tokens.resolve(*v)),
            Value::Asset(path) => serde_json::json!({
                "asset": path.as_ref(),
                "resolved": self.resolve_asset(path, layer),
            }),
            Value::PathExpression(text) => serde_json::json!({ "expr": text.as_ref() }),
            Value::Vec3f(v) => {
                Json::from(v.iter().map(|c| number(f64::from(*c))).collect::<Vec<_>>())
            }
            Value::Vec3d(v) => Json::from(v.iter().map(|c| number(*c)).collect::<Vec<_>>()),
            Value::Dictionary(entries) => {
                let map = entries
                    .iter()
                    .map(|(key, value)| (key.to_string(), self.encode(value, layer)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect();
                serde_json::json!({ "dict": Json::Object(map) })
            }
            Value::Array(_) | Value::TypedArray(_) => {
                let array = value.array_ref().expect("array");
                Json::from(
                    array
                        .iter()
                        .map(|element| self.encode(&element, layer))
                        .collect::<Vec<_>>(),
                )
            }
            Value::Deferred(_) => {
                let materialized = value.try_materialize().expect("value decodes");
                self.encode(materialized, layer)
            }
            other => panic!("unexpected value {other:?}"),
        }
    }

    /// `node` as the oracle names a resolve info's node: `arc|site|layer
    /// stack`.
    fn node(&self, node: &PrimNode) -> String {
        let arc = match node.arc_kind() {
            ArcKind::Local => "local",
            ArcKind::Inherits => "inherits",
            ArcKind::Variants => "variants",
            ArcKind::Relocates => "relocates",
            ArcKind::References => "references",
            ArcKind::Payloads => "payloads",
            ArcKind::Specializes => "specializes",
        };
        let store = &self.loaded.store;
        let site = node.site().display(&store.paths, &store.tokens);
        format!("{arc}|{site}|{}", self.layer_name(node.layer_stack()))
    }

    /// The explanation as the oracle writes a resolve info:
    /// `source|arc|site|layer stack` of the opinion that decides the value,
    /// then those of the weaker opinions it composed, and `fallback|` when
    /// it composed over the schema fallback.
    fn info(&self, explanation: Option<&ValueExplanation<'_, Value>>) -> String {
        let Some(explanation) = explanation else {
            return "none|".into();
        };
        let source = match explanation.source {
            ValueSource::None => {
                // A sampled block in effect: OpenUSD names its samples.
                return match explanation
                    .opinions
                    .iter()
                    .find(|opinion| matches!(opinion.role, OpinionRole::Block))
                {
                    Some(block) if sampled(block) => match block.node {
                        Some(node) => format!("samples|{}", self.node(node)),
                        None => "samples|".into(),
                    },
                    _ => "none|".into(),
                };
            }
            ValueSource::Default => "default",
            ValueSource::TimeSamples { .. } => "samples",
            ValueSource::Spline => "spline",
            ValueSource::ValueClips => "clips",
            ValueSource::Fallback => return "fallback|".into(),
        };
        let mut links: Vec<String> = Vec::new();
        for opinion in explanation.contributors() {
            let Some(node) = opinion.node else {
                continue;
            };
            let link = if links.is_empty() {
                source
            } else if sampled(opinion) {
                "samples"
            } else if opinion.opinion.value.spline().is_some() {
                "spline"
            } else {
                "default"
            };
            links.push(format!("{link}|{}", self.node(node)));
        }
        if links.is_empty() {
            links.push(format!("{source}|"));
        }
        if explanation.seeded_by_fallback {
            links.push("fallback|".into());
        }
        links.join(" > ")
    }
}

/// Whether the opinion offered the query a time sample.
fn sampled(opinion: &ExplainedOpinion<'_>) -> bool {
    opinion.samples.iter().any(|sample| sample.time.is_finite())
}

fn number(x: f64) -> Json {
    if x.is_nan() {
        Json::from("NaN")
    } else {
        serde_json::Number::from_f64(x).map_or(Json::Null, Json::Number)
    }
}

/// Whether two encoded values agree, numbers within a relative `1e-6`.
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => {
            let (x, y) = (x.as_f64().expect("f64"), y.as_f64().expect("f64"));
            (x - y).abs() <= 1e-6 * x.abs().max(y.abs()).max(1.0)
        }
        (Json::Array(x), Json::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, value)| y.get(key).is_some_and(|other| same(value, other)))
        }
        _ => a == b,
    }
}

fn same_times(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| (x - y).abs() <= 1e-9 * x.abs().max(1.0))
}

/// The samples bracketing `time` in the composed grid `samples`, as
/// `UsdAttribute::GetBracketingTimeSamples` reports them: the first or
/// last sample twice outside the grid, a sample at `time` twice.
fn bracket(samples: &[f64], time: f64) -> Option<[f64; 2]> {
    let (first, last) = (*samples.first()?, *samples.last()?);
    if time <= first {
        return Some([first, first]);
    }
    if time >= last {
        return Some([last, last]);
    }
    match samples.binary_search_by(|sample| sample.total_cmp(&time)) {
        Ok(index) => Some([samples[index]; 2]),
        Err(index) => Some([samples[index - 1], samples[index]]),
    }
}

fn brackets_json(brackets: Option<[f64; 2]>) -> Json {
    brackets.map_or(Json::Null, |[a, b]| Json::from(vec![number(a), number(b)]))
}

/// One mismatch: the kind of result, the attribute, and what differs.
struct Failure {
    kind: &'static str,
    case: String,
    attr: String,
    message: String,
}

/// Mismatches, by what was compared.
#[derive(Default)]
struct Failures(Vec<Failure>);

impl Failures {
    fn push(&mut self, kind: &'static str, case: &Case, record: &AttrRecord, message: String) {
        self.0.push(Failure {
            kind,
            case: case.name.clone(),
            attr: record.attr.clone(),
            message,
        });
    }

    /// The failures no [`KNOWN`] entry names, and the entries that name
    /// none, as a report.
    fn report(&self) -> String {
        let known = |failure: &Failure| {
            KNOWN.iter().position(|(kind, name, _)| {
                *kind == failure.kind
                    && failure
                        .attr
                        .rsplit_once('.')
                        .is_some_and(|(_, attr)| matches_pattern(name, attr))
            })
        };
        let mut out = String::new();
        let mut used = BTreeSet::new();
        let mut by_kind: BTreeMap<&str, Vec<&Failure>> = BTreeMap::new();
        for failure in &self.0 {
            match known(failure) {
                Some(entry) => {
                    used.insert(entry);
                }
                None => by_kind.entry(failure.kind).or_default().push(failure),
            }
        }
        for (kind, failures) in by_kind {
            let _ = writeln!(out, "{kind}: {} mismatches", failures.len());
            for failure in failures {
                let _ = writeln!(
                    out,
                    "  {} {} {}",
                    failure.case, failure.attr, failure.message
                );
            }
        }
        for (entry, (kind, name, why)) in KNOWN.iter().enumerate() {
            if !used.contains(&entry) {
                let _ = writeln!(out, "known {kind} mismatch of `{name}` is gone: {why}");
            }
        }
        out
    }
}

fn time_label(time: Option<f64>) -> String {
    time.map_or_else(|| "default".to_string(), |t| t.to_string())
}

fn check_attribute(
    composed: &Composed,
    case: &Case,
    prim: &PrimRecord,
    record: &AttrRecord,
    failures: &mut Failures,
) {
    let store = &composed.loaded.store;
    let view = composed.stage.view(store);
    let path = PropertyPath::parse(&record.attr, &store.tokens, &store.paths).expect("path");
    let attribute = view.attribute(path);
    let Some(attribute) = attribute.filter(|_| record.defined) else {
        if attribute.is_some() != record.defined {
            failures.push(
                "defined",
                case,
                record,
                format!(
                    "layerstack {} != OpenUSD {}",
                    attribute.is_some(),
                    record.defined
                ),
            );
        }
        return;
    };

    let times: Vec<Option<f64>> = std::iter::once(None)
        .chain(prim.times.iter().copied().map(Some))
        .collect();
    for interpolation in [InterpolationType::Held, InterpolationType::Linear] {
        let mut query = AttributeQuery::new(path);
        let rows = times.iter().zip(record.values(interpolation)).enumerate();
        for (index, (time, openusd)) in rows {
            let at = time.map_or(Time::Default, |code| Time::At {
                code,
                interpolation,
            });
            let anchor = attribute
                .read_with_provenance(at, |v| Some(v.clone()))
                .and_then(|resolved| resolved.provenance)
                .map(|provenance| provenance.layer);
            let encode = |value: Option<Value>| {
                value.map_or(Json::Null, |value| composed.encode(&value, anchor))
            };
            let actual = encode(attribute.get(at).map(|resolved| resolved.value));
            let label = format!("t={} {interpolation:?}", time_label(*time));
            let pin = record.pin("value", mode(interpolation), index);
            let expected = pin.map_or(openusd, |pin| &pin.3);
            if !same(&actual, expected) {
                failures.push(
                    "value",
                    case,
                    record,
                    format!(
                        "{label}: layerstack {actual} != {} {expected}",
                        pin.map_or("OpenUSD".to_string(), |pin| format!("`{}`", pin.4))
                    ),
                );
            }
            let queried = encode(
                query
                    .get(&composed.stage, at)
                    .map(|resolved| resolved.value),
            );
            if !same(&queried, &actual) {
                failures.push(
                    "query",
                    case,
                    record,
                    format!("{label}: AttributeQuery {queried} != Attribute::get {actual}"),
                );
            }
            let explained = attribute.explain(at);
            let explained = encode(explained.and_then(|explained| explained.value));
            if !same(&explained, &actual) {
                failures.push(
                    "explained",
                    case,
                    record,
                    format!("{label}: Attribute::explain {explained} != Attribute::get {actual}"),
                );
            }
        }
    }

    for (row, (time, &index)) in times.iter().zip(&record.info).enumerate() {
        let at = time.map_or(Time::Default, Time::held);
        let actual = composed.info(attribute.explain(at).as_ref());
        let expected = record
            .pin("info", "", row)
            .and_then(|pin| pin.3.as_str())
            .unwrap_or(&case.infos[index]);
        let expected = expected.strip_suffix(" > none|").unwrap_or(expected);
        let head = |info: &str| info.split(" > ").next().unwrap_or_default().to_string();
        // A path expression's type composes, so OpenUSD continues its chain
        // past one that does not splice in the weaker one (`%_`); an
        // explanation stops at the last contributor.
        let expression = record.type_name.as_deref() == Some("pathExpression");
        let kind = if head(&actual) != head(expected) {
            "source"
        } else if actual == expected || expression && expected.starts_with(&format!("{actual} > "))
        {
            continue;
        } else {
            "chain"
        };
        failures.push(
            kind,
            case,
            record,
            format!(
                "t={}: layerstack {actual} != OpenUSD {expected}",
                time_label(*time)
            ),
        );
    }

    let samples = attribute.sample_times();
    if !same_times(&samples, &record.samples) {
        failures.push(
            "samples",
            case,
            record,
            format!("layerstack {samples:?} != OpenUSD {:?}", record.samples),
        );
    }
    let [lo, hi] = prim.interval;
    let intervals = [
        attribute.sample_times_in_range(..),
        attribute.sample_times_in_range(lo..=hi),
        attribute.sample_times_in_range((Bound::Excluded(lo), Bound::Excluded(hi))),
    ];
    for (which, (actual, expected)) in intervals.iter().zip(&record.intervals).enumerate() {
        if !same_times(actual, expected) {
            failures.push(
                "intervals",
                case,
                record,
                format!("interval {which}: layerstack {actual:?} != OpenUSD {expected:?}"),
            );
        }
    }
    for (index, (time, openusd)) in prim.times.iter().zip(&record.brackets).enumerate() {
        let actual = brackets_json(bracket(&samples, *time));
        let openusd = brackets_json(*openusd);
        let pin = record.pin("brackets", "", index);
        let expected = pin.map_or(&openusd, |pin| &pin.3);
        if !same(&actual, expected) {
            failures.push(
                "brackets",
                case,
                record,
                format!("t={time}: layerstack {actual} != OpenUSD {expected}"),
            );
        }
    }
    let varying = attribute.might_be_time_varying();
    let expected = record
        .pin("varying", "", 0)
        .map_or(Json::from(record.varying), |pin| pin.3.clone());
    if expected != varying {
        failures.push(
            "varying",
            case,
            record,
            format!("layerstack {varying} != OpenUSD {expected}"),
        );
    }
    if let Some(expected) = &record.custom_data {
        let actual = attribute
            .metadata("customData")
            .and_then(|resolved| {
                let anchor = resolved.provenance.as_ref().map(|p| p.layer);
                resolved
                    .into_value()
                    .map(|resolved| composed.encode(&resolved.value, anchor))
            })
            .and_then(|encoded| encoded.get("dict").cloned())
            .unwrap_or(Json::Null);
        if !same(&actual, expected) {
            failures.push(
                "customData",
                case,
                record,
                format!("layerstack {actual} != OpenUSD {expected}"),
            );
        }
    }
}

fn oracle() -> Oracle {
    serde_json::from_str(ORACLE).expect("valid oracle.json")
}

#[test]
fn value_matrix_matches_openusd() {
    let oracle = oracle();
    assert_eq!(oracle.openusd_version, "26.8");
    let mut failures = Failures::default();
    let mut checked = 0;
    for case in &oracle.cases {
        let composed = Composed::open(&case.name);
        for prim in &case.prims {
            for record in &prim.attrs {
                check_attribute(&composed, case, prim, record, &mut failures);
                checked += 1;
            }
        }
    }
    assert!(checked > 1000, "only {checked} attributes");
    let report = failures.report();
    assert!(
        report.is_empty(),
        "value matrix mismatches against OpenUSD {}:\n{report}",
        oracle.openusd_version
    );
}

/// The vectors pin exactly the named divergences, each confirmed against
/// the recorded OpenUSD release and each shown by some result whose
/// OpenUSD value differs from Layerstack's expected one.
#[test]
fn divergences_are_exactly_the_named_ones() {
    let oracle = oracle();
    let recorded: Vec<(&str, &str)> = oracle
        .divergences
        .iter()
        .map(|(name, divergence)| (name.as_str(), divergence.openusd.as_str()))
        .collect();
    let mut named = DIVERGENCES.to_vec();
    named.sort_unstable();
    assert_eq!(recorded, named, "recorded divergences and versions");

    let mut shown = BTreeSet::new();
    for case in &oracle.cases {
        for prim in &case.prims {
            for record in &prim.attrs {
                for pin in &record.pins {
                    assert!(
                        DIVERGENCES.iter().any(|(known, _)| *known == pin.4),
                        "{} {}: unnamed divergence `{}`",
                        case.name,
                        record.attr,
                        pin.4
                    );
                    let openusd = match pin.0.as_str() {
                        "value" => {
                            let interpolation = if pin.1 == "linear" {
                                InterpolationType::Linear
                            } else {
                                InterpolationType::Held
                            };
                            record.values(interpolation)[pin.2].clone()
                        }
                        "brackets" => brackets_json(record.brackets[pin.2]),
                        "varying" => Json::from(record.varying),
                        "info" => Json::from(case.infos[record.info[pin.2]].as_str()),
                        other => panic!("unknown pinned field `{other}`"),
                    };
                    assert!(
                        !same(&openusd, &pin.3),
                        "{} {}: OpenUSD no longer differs for `{}`",
                        case.name,
                        record.attr,
                        pin.4
                    );
                    shown.insert(pin.4.as_str());
                }
            }
        }
    }
    for (name, _) in DIVERGENCES {
        assert!(shown.contains(name), "no result shows `{name}`");
    }
}

/// Every subject reaches every composition arc, and the matrix holds every
/// value kind.
#[test]
fn matrix_covers_every_arc_and_kind() {
    let oracle = oracle();
    let cases: BTreeSet<&str> = oracle.cases.iter().map(|case| case.name.as_str()).collect();
    for arc in [
        "sublayers",
        "references",
        "payloads",
        "inherits",
        "specializes",
        "variants",
        "relocates",
        "instance_proxies",
    ] {
        assert!(cases.contains(arc), "no `{arc}` case");
    }
    for case in &oracle.cases {
        let names: BTreeSet<&str> = case
            .prims
            .iter()
            .flat_map(|prim| prim.attrs.iter().map(AttrRecord::name))
            .collect();
        for kind in [
            "dbl", "int", "tc", "arr", "tok", "str", "asset", "assets", "expr",
        ] {
            assert!(
                names.contains(format!("ss_{kind}").as_str()),
                "{}: no `ss_{kind}`",
                case.name
            );
        }
    }
}
