// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for time-sampled array resolution.
//!
//! `tests/data/temporal_sparse.json` records OpenUSD's resolved values for
//! each case (`scripts/temporal_sparse_oracle.py`, OpenUSD 26.08). Each case is
//! a set of USDA layers; every query resolves one attribute at one time under
//! held and under linear interpolation. Most cases compose sparse array edits;
//! others pin how scalar samples hold or interpolate. Layerstack must
//! reproduce OpenUSD's value, except for the named divergences in
//! [`DIVERGENCES`]: there it must produce the query's `expected` value, which
//! OpenUSD does not. Any other difference is a failure.
//!
//! Where OpenUSD defines it, resolving the composed stage must also equal
//! resolving OpenUSD's flattened layer of that stage.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, InterpolationType, Layer, LayerId,
    PathInterner, PropertyPath, ResolvedAsset, Stage, StageOptions, TokenInterner, Value,
};
use layerstack_usda::{emit, lower, parser::parse_cst};
use serde::Deserialize;

const VECTORS: &str = include_str!("data/temporal_sparse.json");

/// The Core-versus-OpenUSD divergences Layerstack follows, with the OpenUSD
/// release each was confirmed against. `docs/generic-sparse-composition.md`
/// ("Divergences From OpenUSD") gives each one's policy, Core section and
/// OpenUSD source line; the oracle script records the same names.
const DIVERGENCES: &[(&str, &str)] = &[
    // Before the first composed sample, hold it instead of OpenUSD's NaN
    // (AOUSD Core 12.5.1).
    ("nan-before-first-sample", "0.26.8"),
    // Keep composing weaker series after the query moves to the upper
    // sample (sparse-array-edits proposal, `Evaluate`).
    ("override-early-stop", "0.26.8"),
    // A held sampled block ends the fold (AOUSD Core 12.3.6).
    ("transparent-sampled-block", "0.26.8"),
];

#[derive(Deserialize)]
struct Vectors {
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
    layers: BTreeMap<String, String>,
    queries: Vec<Query>,
    flattened_layer: Option<String>,
    /// Values must match OpenUSD bit for bit rather than within a tolerance.
    #[serde(default)]
    exact: bool,
    /// With `exact`, the units in the last place allowed (for results that
    /// go through the C library's `acos` and `sin`).
    #[serde(default)]
    ulps: u64,
}

/// How closely a case's values must match.
#[derive(Clone, Copy)]
enum Tolerance {
    /// Within a relative `1e-5`.
    Relative,
    /// Within this many units in the last place; `0` is bit for bit.
    Ulps(u64),
}

impl Case {
    fn tolerance(&self) -> Tolerance {
        if self.exact {
            Tolerance::Ulps(self.ulps)
        } else {
            Tolerance::Relative
        }
    }
}

#[derive(Deserialize)]
struct Query {
    attr: String,
    time: Option<f64>,
    interp: String,
    openusd: Option<serde_json::Value>,
    expected: Option<serde_json::Value>,
    divergence: Option<String>,
}

impl Query {
    fn interpolation(&self) -> InterpolationType {
        match self.interp.as_str() {
            "held" => InterpolationType::Held,
            "linear" => InterpolationType::Linear,
            other => panic!("unknown interpolation `{other}`"),
        }
    }

    /// The value Layerstack must produce: OpenUSD's, unless the query names
    /// one of the [`DIVERGENCES`].
    fn expected(&self) -> Option<Resolved> {
        if self.divergence.is_some() {
            decode(self.expected.as_ref())
        } else {
            decode(self.openusd.as_ref())
        }
    }
}

/// Decodes a recorded value; `None` stays `None`.
fn decode(raw: Option<&serde_json::Value>) -> Option<Resolved> {
    raw.map(|value| match value {
        serde_json::Value::Array(items) => {
            Resolved::Array(items.iter().map(Element::from_json).collect())
        }
        serde_json::Value::Object(tuple) => Resolved::Scalar(Element::from_json(&tuple["tuple"])),
        scalar => Resolved::Scalar(Element::from_json(scalar)),
    })
}

/// A resolved attribute value, flattened to numeric components.
#[derive(Clone, Debug)]
enum Resolved {
    Scalar(Element),
    Array(Vec<Element>),
}

impl Resolved {
    fn from_value(value: &Value) -> Self {
        match value {
            Value::Array(items) => Self::Array(items.iter().map(Element::from_value).collect()),
            scalar => Self::Scalar(Element::from_value(scalar)),
        }
    }
}

/// One array element, flattened to its numeric components.
#[derive(Clone, Debug)]
struct Element(Vec<f64>);

impl Element {
    /// Reads a number or a list of components; the oracle writes NaN as the
    /// string `"NaN"`.
    fn from_json(value: &serde_json::Value) -> Self {
        let number = |value: &serde_json::Value| match value {
            serde_json::Value::Number(n) => n.as_f64().expect("f64"),
            serde_json::Value::String(s) if s == "NaN" => f64::NAN,
            other => panic!("unexpected JSON number {other}"),
        };
        match value {
            serde_json::Value::Array(items) => Self(items.iter().map(number).collect()),
            scalar => Self(vec![number(scalar)]),
        }
    }

    /// Flattens a value to its components; quaternions list the real part
    /// first, as the vectors do.
    fn from_value(value: &Value) -> Self {
        let floats = |v: &[f32]| v.iter().copied().map(f64::from).collect();
        let halves = |v: &[u16]| v.iter().copied().map(half_to_f64).collect();
        Self(match value {
            Value::Int(v) => vec![f64::from(*v)],
            #[allow(clippy::cast_precision_loss, reason = "test values are small")]
            Value::Int64(v) => vec![*v as f64],
            Value::Half(v) => vec![half_to_f64(*v)],
            Value::Float(v) => vec![f64::from(*v)],
            Value::Double(v) => vec![*v],
            Value::Vec2h(v) => halves(v),
            Value::Vec3h(v) => halves(v),
            Value::Vec4h(v) => halves(v),
            Value::Vec2f(v) => floats(v),
            Value::Vec3f(v) => floats(v),
            Value::Vec4f(v) => floats(v),
            Value::Vec2d(v) => v.to_vec(),
            Value::Vec3d(v) => v.to_vec(),
            Value::Vec4d(v) => v.to_vec(),
            Value::Matrix2d(v) => v.to_vec(),
            Value::Matrix3d(v) => v.to_vec(),
            Value::Matrix4d(v) => v.to_vec(),
            Value::Quath([i, j, k, r]) => halves(&[*r, *i, *j, *k]),
            Value::Quatf([i, j, k, r]) => floats(&[*r, *i, *j, *k]),
            Value::Quatd([i, j, k, r]) => vec![*r, *i, *j, *k],
            other => panic!("unexpected value {other:?}"),
        })
    }

    /// Whether every component is within `tolerance` (NaN matching NaN).
    fn close(&self, other: &Self, tolerance: Tolerance) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().zip(&other.0).all(|(a, b)| match tolerance {
                Tolerance::Relative => (a - b).abs() <= 1e-5 * a.abs().max(1.0),
                Tolerance::Ulps(ulps) => {
                    (a.is_nan() && b.is_nan())
                        || a.to_bits() == b.to_bits()
                        || (a.is_sign_positive() == b.is_sign_positive()
                            && a.to_bits().abs_diff(b.to_bits()) <= ulps)
                }
            })
    }
}

/// Widens IEEE 754 binary16 bits exactly.
fn half_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = f64::from(bits & 0x3ff);
    sign * match exponent {
        0 => mantissa * 2_f64.powi(-24),
        0x1f if mantissa == 0.0 => f64::INFINITY,
        0x1f => f64::NAN,
        _ => (1024.0 + mantissa) * 2_f64.powi(exponent - 25),
    }
}

fn same(a: Option<&Resolved>, b: Option<&Resolved>) -> bool {
    same_within(a, b, Tolerance::Relative)
}

fn same_within(a: Option<&Resolved>, b: Option<&Resolved>, tolerance: Tolerance) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Resolved::Scalar(a)), Some(Resolved::Scalar(b))) => a.close(b, tolerance),
        (Some(Resolved::Array(a)), Some(Resolved::Array(b))) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.close(y, tolerance))
        }
        _ => false,
    }
}

fn show_element(element: &Element) -> String {
    match element.0.as_slice() {
        [x] => format!("{x:?}"),
        xs => format!("{xs:?}"),
    }
}

fn show(value: Option<&Resolved>) -> String {
    match value {
        None => "none".into(),
        Some(Resolved::Scalar(element)) => show_element(element),
        Some(Resolved::Array(items)) => {
            let parts: Vec<String> = items.iter().map(show_element).collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

/// Resolves asset paths against the case's in-memory USDA sources.
struct MemoryResolver<'a> {
    sources: &'a BTreeMap<String, String>,
    by_name: BTreeMap<String, LayerId>,
    next_layer_id: u64,
    pending: Vec<Layer>,
}

impl AssetResolver for MemoryResolver<'_> {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let name = asset_path.trim_start_matches("./");
        if let Some(id) = self.by_name.get(name) {
            return Ok(ResolvedAsset {
                layer_id: *id,
                resolved_path: Arc::from(name),
                layer: None,
            });
        }
        let sources = self.sources;
        let source = sources.get(name).ok_or(AssetResolveError::NotFound)?;
        let layer_id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        self.by_name.insert(name.to_string(), layer_id);
        let layer = emit_layer(source, layer_id, tokens, paths, self);
        Ok(ResolvedAsset {
            layer_id,
            resolved_path: Arc::from(name),
            layer: Some(layer),
        })
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

fn emit_layer(
    source: &str,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut MemoryResolver<'_>,
) -> Layer {
    let cst = parse_cst(source);
    assert!(cst.diagnostics.is_empty(), "{:?}", cst.diagnostics);
    let ast = lower::lower(&cst.tree, source);
    assert!(ast.diagnostics.is_empty(), "{:?}", ast.diagnostics);
    let result = emit::emit(&ast.layer, layer_id, tokens, paths, resolver);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    resolver.pending.extend(result.resolved_layers);
    result.layer
}

/// A composed stage over one case's layers, rooted at `root`.
struct Composed {
    store: InMemoryStore,
    stage: Stage,
}

impl Composed {
    fn new(sources: &BTreeMap<String, String>, root: &str) -> Self {
        let mut store = InMemoryStore::default();
        let mut resolver = MemoryResolver {
            sources,
            by_name: BTreeMap::from([(root.to_string(), LayerId(1))]),
            next_layer_id: 2,
            pending: Vec::new(),
        };
        let layer = emit_layer(
            &sources[root],
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
            &mut resolver,
        );
        store.insert_layer(layer);
        for layer in resolver.pending.drain(..) {
            store.insert_layer(layer);
        }
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        Self { store, stage }
    }

    fn resolve(&mut self, query: &Query) -> Option<Resolved> {
        let path = PropertyPath::parse(&query.attr, &mut self.store.tokens, &mut self.store.paths)
            .expect("attribute path");
        let value = match query.time {
            Some(time) => {
                self.stage
                    .resolve_property_path_at_time(path, time, query.interpolation())?
                    .value
            }
            None => self.stage.resolve_field_path(path)?.value,
        };
        Some(Resolved::from_value(&value))
    }
}

fn vectors() -> Vectors {
    serde_json::from_str(VECTORS).expect("valid temporal_sparse.json")
}

fn query_label(case: &Case, query: &Query) -> String {
    let time = query
        .time
        .map_or_else(|| "default".to_string(), |t| t.to_string());
    format!("{} {} t={time} {}", case.name, query.attr, query.interp)
}

#[test]
fn composed_resolution_matches_openusd() {
    let vectors = vectors();
    let mut failures = String::new();
    let mut checked = 0;
    for case in &vectors.cases {
        let mut composed = Composed::new(&case.layers, "root.usda");
        for query in &case.queries {
            checked += 1;
            let expected = query.expected();
            let actual = composed.resolve(query);
            if !same_within(actual.as_ref(), expected.as_ref(), case.tolerance()) {
                let _ = writeln!(
                    failures,
                    "{}: layerstack {} != expected {}{}",
                    query_label(case, query),
                    show(actual.as_ref()),
                    show(expected.as_ref()),
                    query
                        .divergence
                        .as_ref()
                        .map(|d| format!(" (divergence `{d}`)"))
                        .unwrap_or_default(),
                );
            }
        }
    }
    assert!(checked > 0, "no queries");
    assert!(
        failures.is_empty(),
        "differential mismatches against OpenUSD {}:\n{failures}",
        vectors.openusd_version
    );
}

/// The vectors pin exactly the named divergences, each confirmed against
/// the recorded OpenUSD release and each shown by some query whose OpenUSD
/// value differs from Layerstack's expected one.
#[test]
fn divergences_are_exactly_the_named_ones() {
    let vectors = vectors();
    let recorded: Vec<(&str, &str)> = vectors
        .divergences
        .iter()
        .map(|(name, divergence)| (name.as_str(), divergence.openusd.as_str()))
        .collect();
    let mut named = DIVERGENCES.to_vec();
    named.sort_unstable();
    assert_eq!(recorded, named, "recorded divergences and versions");

    let mut shown = BTreeMap::new();
    for case in &vectors.cases {
        for query in &case.queries {
            let Some(name) = &query.divergence else {
                continue;
            };
            assert!(
                DIVERGENCES.iter().any(|(known, _)| known == name),
                "{}: unnamed divergence `{name}`",
                query_label(case, query)
            );
            assert!(
                !same(
                    decode(query.openusd.as_ref()).as_ref(),
                    query.expected().as_ref()
                ),
                "{}: OpenUSD no longer differs for `{name}`",
                query_label(case, query)
            );
            *shown.entry(name.as_str()).or_insert(0) += 1;
        }
    }
    for (name, _) in DIVERGENCES {
        assert!(shown.contains_key(name), "no query shows `{name}`");
    }
}

#[test]
fn composed_resolution_matches_flattened_resolution() {
    let vectors = vectors();
    let mut failures = String::new();
    let mut checked = 0;
    for case in &vectors.cases {
        let Some(flattened) = &case.flattened_layer else {
            continue;
        };
        let mut composed = Composed::new(&case.layers, "root.usda");
        let flat_sources = BTreeMap::from([("flat.usda".to_string(), flattened.clone())]);
        let mut flat = Composed::new(&flat_sources, "flat.usda");
        for query in &case.queries {
            checked += 1;
            let from_composed = composed.resolve(query);
            let from_flat = flat.resolve(query);
            if !same(from_composed.as_ref(), from_flat.as_ref()) {
                let _ = writeln!(
                    failures,
                    "{}: composed {} != flattened {}",
                    query_label(case, query),
                    show(from_composed.as_ref()),
                    show(from_flat.as_ref()),
                );
            }
        }
    }
    assert!(checked > 0, "no flattened cases");
    assert!(
        failures.is_empty(),
        "composed and flattened resolution disagree:\n{failures}"
    );
}
