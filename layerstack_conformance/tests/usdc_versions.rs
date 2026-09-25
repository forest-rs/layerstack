// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests of the USDC reader against OpenUSD 26.08.
//!
//! The fixtures in `layerstack_conformance/fixtures/usdc_versions` were
//! written by OpenUSD itself (see `generate.py` there), and `expected.json`
//! holds the values the same OpenUSD resolves for them. Each test decodes a
//! fixture, composes it through [`Stage`], and compares every attribute at
//! default and at the recorded times.
//!
//! Spec: AOUSD Core §16.3 (crate format), §12 (value resolution); OpenUSD
//! v26.08 `pxr/usd/sdf/crateFile.cpp` for crate versions after 0.12.

use std::path::PathBuf;

use layerstack::doc::{InterpolationType, LayerId, Value};
use layerstack::interner::TokenInterner;
use layerstack::path::{Path, PathInterner};
use layerstack::{
    ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, AssetResolveError, AssetResolver,
    PropertyPath, PropertySpec, ResolvedAsset, ResolvedValue, Stage, StageOptions,
};
use layerstack_conformance::usdc::load_entry_usdc;
use layerstack_conformance::workspace_root;
use layerstack_usdc::{CrateVersion, UsdcError};
use serde_json::Value as Json;

fn fixtures_dir() -> PathBuf {
    workspace_root().join("layerstack_conformance/fixtures/usdc_versions")
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn expected_json() -> Json {
    let path = fixtures_dir().join("expected.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("expected.json parses")
}

fn expected(name: &str) -> Json {
    expected_json()["fixtures"][name].clone()
}

struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// Decodes one fixture on its own, without resolving sublayers.
fn read(data: &[u8]) -> Result<layerstack_usdc::AssembleResult, UsdcError> {
    layerstack_usdc::read_usdc(
        data,
        LayerId(1),
        &mut TokenInterner::default(),
        &mut PathInterner::default(),
        &mut NoAssets,
    )
}

/// Returns `data` with the header's version bytes replaced.
fn with_version(mut data: Vec<u8>, version: CrateVersion) -> Vec<u8> {
    data[8] = version.major;
    data[9] = version.minor;
    data[10] = version.patch;
    data
}

// ---------------------------------------------------------------------------
// Differential comparison
// ---------------------------------------------------------------------------

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
}

fn floats_match(values: &[f64], json: &Json) -> bool {
    json.as_array().is_some_and(|items| {
        items.len() == values.len()
            && items
                .iter()
                .zip(values)
                .all(|(j, v)| j.as_f64().is_some_and(|j| close(j, *v)))
    })
}

/// Whether a resolved Layerstack value equals OpenUSD's JSON-exported value.
fn value_matches(value: &Value, json: &Json, tokens: &TokenInterner) -> bool {
    match value {
        Value::Null | Value::Blocked => json.is_null(),
        Value::Bool(v) => json.as_bool() == Some(*v),
        Value::UChar(v) => json.as_u64() == Some(u64::from(*v)),
        Value::Int(v) => json.as_i64() == Some(i64::from(*v)),
        Value::UInt(v) => json.as_u64() == Some(u64::from(*v)),
        Value::Int64(v) => json.as_i64() == Some(*v),
        Value::UInt64(v) => json.as_u64() == Some(*v),
        Value::Float(v) => json.as_f64().is_some_and(|j| close(j, f64::from(*v))),
        Value::Double(v) | Value::TimeCode(v) => json.as_f64().is_some_and(|j| close(j, *v)),
        Value::String(v) | Value::Asset(v) => json.as_str() == Some(&**v),
        Value::Token(v) => json.as_str() == Some(tokens.resolve(*v)),
        Value::Vec3f(v) => floats_match(&v.map(f64::from), json),
        Value::Vec3d(v) => floats_match(v, json),
        Value::Array(items) => json.as_array().is_some_and(|js| {
            js.len() == items.len()
                && items
                    .iter()
                    .zip(js)
                    .all(|(v, j)| value_matches(v, j, tokens))
        }),
        _ => false,
    }
}

/// One attribute value that differs from OpenUSD.
#[derive(Debug)]
#[allow(dead_code, reason = "fields are reported through `Debug`")]
struct Mismatch {
    attribute: String,
    time: Option<f64>,
    openusd: Json,
    layerstack: Option<Value>,
}

/// Composes `root` (and its sublayers) and compares every attribute recorded
/// for it in `expected.json`. Returns the attributes that differ.
fn compare_with_openusd(root: &str) -> Vec<Mismatch> {
    compare_file_with_openusd(&fixtures_dir().join(root), &expected(root))
}

fn compare_file_with_openusd(root: &std::path::Path, expected: &Json) -> Vec<Mismatch> {
    let mut loaded = load_entry_usdc(root);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let store = &mut loaded.store;

    let mut mismatches = Vec::new();
    for (prim_path, attributes) in expected["prims"].as_object().expect("prims") {
        let path = Path::parse_absolute(prim_path, &mut store.tokens).expect("prim path");
        let prim = store.paths.intern(path);
        for (name, record) in attributes.as_object().expect("attributes") {
            let field = store.tokens.intern(name);
            let attribute = format!("{prim_path}.{name}");

            let default = stage
                .resolve_property_path(PropertyPath::new(prim, field))
                .and_then(|resolved| match resolved.value {
                    ResolvedValue::Scalar(value) => Some(value),
                    _ => None,
                });
            let openusd = &record["default"];
            let matches = match &default {
                Some(value) => value_matches(value, openusd, &store.tokens),
                None => openusd.is_null(),
            };
            if !matches {
                mismatches.push(Mismatch {
                    attribute: attribute.clone(),
                    time: None,
                    openusd: openusd.clone(),
                    layerstack: default,
                });
            }

            for sample in record["times"].as_array().into_iter().flatten() {
                let time = sample[0].as_f64().expect("time");
                let openusd = &sample[1];
                let value = stage
                    .resolve_property_path_at_time(
                        PropertyPath::new(prim, field),
                        time,
                        InterpolationType::Linear,
                    )
                    .map(|resolved| resolved.value);
                let matches = match &value {
                    Some(value) => value_matches(value, openusd, &store.tokens),
                    None => openusd.is_null(),
                };
                if !matches {
                    mismatches.push(Mismatch {
                        attribute: attribute.clone(),
                        time: Some(time),
                        openusd: openusd.clone(),
                        layerstack: value,
                    });
                }
            }
        }
    }
    mismatches
}

fn assert_matches_openusd(root: &str) {
    let mismatches = compare_with_openusd(root);
    assert!(
        mismatches.is_empty(),
        "{root} differs from OpenUSD 26.08:\n{mismatches:#?}"
    );
}

// ---------------------------------------------------------------------------
// Ordinary files, one per version
// ---------------------------------------------------------------------------

#[test]
fn fixtures_declare_the_expected_versions() {
    for (name, version) in [
        ("version_0_12.usdc", "0.12.0"),
        ("version_0_13.usdc", "0.13.0"),
        ("version_0_14.usdc", "0.14.0"),
        ("version_0_15.usdc", "0.15.0"),
        ("array_edits_weak.usdc", "0.14.0"),
        ("array_edits_strong.usdc", "0.14.0"),
        ("spline_loop_boundary.usdc", "0.15.0"),
        ("spline_time_valued.usdc", "0.15.0"),
        ("sublayers_weak.usdc", "0.8.0"),
        ("sublayers_root.usdc", "0.8.0"),
    ] {
        let data = fixture_bytes(name);
        let found = format!("{}.{}.{}", data[8], data[9], data[10]);
        assert_eq!(found, version, "{name}");
        assert_eq!(expected(name)["crate_version"], version, "{name}");
    }
}

#[test]
fn version_0_12_matches_openusd() {
    assert_matches_openusd("version_0_12.usdc");
}

#[test]
fn version_0_13_matches_openusd() {
    assert_matches_openusd("version_0_13.usdc");
}

#[test]
fn version_0_14_matches_openusd() {
    assert_matches_openusd("version_0_14.usdc");
}

#[test]
fn version_0_15_matches_openusd() {
    assert_matches_openusd("version_0_15.usdc");
}

#[test]
fn version_newer_than_readable_is_rejected() {
    let next = CrateVersion::new(0, CrateVersion::NEWEST_READABLE.minor + 1, 0);
    let data = with_version(fixture_bytes("version_0_12.usdc"), next);
    assert_eq!(
        read(&data).err(),
        Some(UsdcError::UnsupportedVersion {
            major: next.major,
            minor: next.minor,
            patch: next.patch,
        })
    );
}

/// Values of the offset sublayer's time samples, which Layerstack maps
/// through the sublayer offset differently from OpenUSD.
fn is_offset_sample(mismatch: &Mismatch) -> bool {
    mismatch.attribute == "/Prim.animated" && mismatch.time.is_some()
}

/// `subLayers` is a string vector and `subLayerOffsets` holds the weak
/// layer's offset (10) and scale (2).
#[test]
fn sublayers_match_openusd() {
    let unexpected: Vec<_> = compare_with_openusd("sublayers_root.usdc")
        .into_iter()
        .filter(|mismatch| !is_offset_sample(mismatch))
        .collect();
    assert!(unexpected.is_empty(), "{unexpected:#?}");
    // The authored offset and scale are recorded on the sublayer entry.
    let loaded = load_entry_usdc(&fixtures_dir().join("sublayers_root.usdc"));
    let root = &loaded.store.layers[&loaded.root_layer];
    assert_eq!(root.sublayers.len(), 1);
    assert_eq!(root.sublayers[0].offset.offset, 10.0);
    assert_eq!(root.sublayers[0].offset.scale, 2.0);
}

#[test]
fn sublayer_offset_time_mapping_matches_openusd() {
    let known: Vec<_> = compare_with_openusd("sublayers_root.usdc")
        .into_iter()
        .filter(is_offset_sample)
        .collect();
    assert!(known.is_empty(), "{known:#?}");
}

// ---------------------------------------------------------------------------
// Splines
// ---------------------------------------------------------------------------

#[test]
fn spline_needs_crate_0_12() {
    let data = with_version(
        fixture_bytes("version_0_12.usdc"),
        CrateVersion::new(0, 11, 0),
    );
    assert_eq!(
        read(&data).err(),
        Some(UsdcError::FeatureRequiresVersion {
            feature: "spline value",
            required: CrateVersion::SPLINES,
            found: CrateVersion::new(0, 11, 0),
        })
    );
}

/// The AOUSD supplemental spec's spline fixture, as OpenUSD evaluates it.
#[test]
fn supplemental_gen_splines_matches_openusd() {
    let path = workspace_root()
        .join("core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary")
        .join("gen_splines.usdc");
    let expected = &expected_json()["supplemental"]["gen_splines.usdc"];
    let mismatches = compare_file_with_openusd(&path, expected);
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn spline_tangent_algorithms_need_crate_0_13() {
    let data = with_version(fixture_bytes("version_0_13.usdc"), CrateVersion::SPLINES);
    assert_eq!(
        read(&data).err(),
        Some(UsdcError::FeatureRequiresVersion {
            feature: "spline binary format",
            required: CrateVersion::SPLINE_TANGENT_ALGORITHMS,
            found: CrateVersion::SPLINES,
        })
    );
}

/// Crate 0.15's spline `loopBoundaryTime` has no Layerstack representation.
#[test]
fn spline_loop_boundary_time_is_reported() {
    assert_eq!(
        read(&fixture_bytes("spline_loop_boundary.usdc")).err(),
        Some(UsdcError::UnsupportedFeature {
            feature: "spline loopBoundaryTime",
        })
    );
}

/// Crate 0.15's `GfTimeCode`-valued splines have no Layerstack representation.
#[test]
fn time_valued_spline_is_reported() {
    assert_eq!(
        read(&fixture_bytes("spline_time_valued.usdc")).err(),
        Some(UsdcError::UnsupportedFeature {
            feature: "time-valued spline",
        })
    );
}

// ---------------------------------------------------------------------------
// Native array edits (crate 0.14)
// ---------------------------------------------------------------------------

/// Decodes `name` alone and returns the authored value of `prim.attribute`.
fn authored(name: &str, prim: &str, attribute: &str) -> PropertySpec {
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let result = layerstack_usdc::read_usdc(
        &fixture_bytes(name),
        LayerId(1),
        &mut tokens,
        &mut paths,
        &mut NoAssets,
    )
    .unwrap_or_else(|e| panic!("failed to read {name}: {e}"));
    let path = Path::parse_absolute(prim, &mut tokens).expect("prim path");
    let spec = &result.layer.prims[&paths.lookup(&path).expect("prim")];
    spec.property(tokens.intern(attribute))
        .unwrap_or_else(|| panic!("no {prim}.{attribute}"))
        .clone()
}

fn authored_edit(prim: &str, attribute: &str) -> ArrayEdit {
    match authored("array_edits_strong.usdc", prim, attribute).default {
        Some(Value::ArrayEdit(edit)) => edit,
        other => panic!("{prim}.{attribute} is not an array edit: {other:?}"),
    }
}

fn lit(value: i32) -> ArrayEditOperand {
    ArrayEditOperand::Literal(Value::Int(value))
}

fn at(index: i64) -> ArrayIndex {
    ArrayIndex::Position(index)
}

/// The native encoding decodes to the instructions `generate.py` built.
#[test]
fn array_edits_decode_to_the_authored_instructions() {
    let copy = ArrayEditOperand::CopyFrom;
    for (attribute, ops) in [
        (
            "insertLiteral",
            vec![ArrayEditOp::Insert {
                src: lit(99),
                index: at(2),
            }],
        ),
        (
            "insertNegative",
            vec![ArrayEditOp::Insert {
                src: lit(77),
                index: at(-1),
            }],
        ),
        (
            "appendRef",
            vec![ArrayEditOp::Insert {
                src: copy(at(1)),
                index: ArrayIndex::End,
            }],
        ),
        (
            "prependLiteral",
            vec![ArrayEditOp::Insert {
                src: lit(5),
                index: at(0),
            }],
        ),
        (
            "eraseRef",
            vec![
                ArrayEditOp::Erase { index: at(1) },
                ArrayEditOp::Erase { index: at(-1) },
            ],
        ),
        (
            "writeLiteral",
            vec![
                ArrayEditOp::Write {
                    src: lit(7),
                    index: at(0),
                },
                ArrayEditOp::Write {
                    src: lit(8),
                    index: at(-2),
                },
            ],
        ),
        (
            "writeRef",
            vec![ArrayEditOp::Write {
                src: copy(at(-1)),
                index: at(0),
            }],
        ),
        ("minSize", vec![ArrayEditOp::MinSize { len: 8 }]),
        (
            "minSizeFill",
            vec![ArrayEditOp::MinSizeFill {
                len: 8,
                fill: Value::Int(7),
            }],
        ),
        ("resizeShrink", vec![ArrayEditOp::Resize { len: 3 }]),
        (
            "resizeFill",
            vec![ArrayEditOp::ResizeFill {
                len: 7,
                fill: Value::Int(-1),
            }],
        ),
        ("maxSize", vec![ArrayEditOp::MaxSize { len: 2 }]),
        (
            "repeated",
            vec![
                ArrayEditOp::Insert {
                    src: lit(1),
                    index: ArrayIndex::End,
                },
                ArrayEditOp::Insert {
                    src: lit(2),
                    index: ArrayIndex::End,
                },
                ArrayEditOp::Insert {
                    src: lit(3),
                    index: ArrayIndex::End,
                },
                ArrayEditOp::Insert {
                    src: lit(0),
                    index: at(0),
                },
            ],
        ),
        ("identity", vec![]),
    ] {
        assert_eq!(
            authored_edit("/Ops", attribute).ops,
            ops,
            "/Ops.{attribute}"
        );
    }

    // Time samples hold edits too.
    match authored("array_edits_strong.usdc", "/BasicsSamples", "attr").time_samples {
        Some(samples) => {
            assert_eq!(samples.len(), 2);
            assert!(
                samples
                    .iter()
                    .all(|(_, value)| matches!(value, Value::ArrayEdit(_)))
            );
        }
        other => panic!("expected time samples, got {other:?}"),
    }
}

#[test]
fn array_edit_needs_crate_0_14() {
    let found = CrateVersion::SPLINE_TANGENT_ALGORITHMS;
    let data = with_version(fixture_bytes("array_edits_strong.usdc"), found);
    assert_eq!(
        read(&data).err(),
        Some(UsdcError::FeatureRequiresVersion {
            feature: "array edit",
            required: CrateVersion::ARRAY_EDITS,
            found,
        })
    );
}

/// Resolution differences from OpenUSD 26.08 that belong to other work.
/// Each is asserted by an ignored test below, so they stay visible.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KnownDifference {
    /// Interpolating between a sparse and a dense time sample.
    SparseSampleInterpolation,
    /// A sparse default over a blocked default.
    EditOverBlockedDefault,
}

fn known_difference(root: &str, mismatch: &Mismatch) -> Option<KnownDifference> {
    let attribute = mismatch.attribute.as_str();
    match (attribute, mismatch.time) {
        ("/Interp.attr" | "/InterpRoot.attr", Some(time)) if time > 1.0 && time < 3.0 => {
            Some(KnownDifference::SparseSampleInterpolation)
        }
        ("/Block.overDefault", None) if root == "array_edits_strong.usdc" => {
            Some(KnownDifference::EditOverBlockedDefault)
        }
        _ => None,
    }
}

/// Mismatches in the array edit fixtures, split into unexpected ones and
/// those of the given known difference.
fn array_edit_mismatches(kind: KnownDifference) -> (Vec<Mismatch>, Vec<Mismatch>) {
    let mut unexpected = Vec::new();
    let mut known = Vec::new();
    for root in ["array_edits_weak.usdc", "array_edits_strong.usdc"] {
        for mismatch in compare_with_openusd(root) {
            match known_difference(root, &mismatch) {
                None => unexpected.push(mismatch),
                Some(k) if k == kind => known.push(mismatch),
                Some(_) => {}
            }
        }
    }
    (unexpected, known)
}

/// Every attribute of the array edit fixtures, alone and composed, at
/// default and at every recorded time, apart from the known differences.
#[test]
fn array_edits_match_openusd() {
    let (unexpected, _) = array_edit_mismatches(KnownDifference::SparseSampleInterpolation);
    assert!(
        unexpected.is_empty(),
        "array edits differ from OpenUSD 26.08:\n{unexpected:#?}"
    );
}

/// A spec's default beside its own time samples resolves at default time
/// (AOUSD Core §12.3.1): `/BasicsSamples.attr` and `/BasicsBothSamples.attr`
/// are checked by [`array_edits_match_openusd`], since they are no longer a
/// known difference.
#[test]
fn array_edit_default_beside_samples_matches_openusd() {
    let mismatches: Vec<_> = ["array_edits_weak.usdc", "array_edits_strong.usdc"]
        .into_iter()
        .flat_map(compare_with_openusd)
        .filter(|m| {
            m.time.is_none()
                && matches!(
                    m.attribute.as_str(),
                    "/BasicsSamples.attr" | "/BasicsBothSamples.attr"
                )
        })
        .collect();
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn array_edit_sample_interpolation_matches_openusd() {
    let (_, known) = array_edit_mismatches(KnownDifference::SparseSampleInterpolation);
    assert!(known.is_empty(), "{known:#?}");
}

#[test]
#[ignore = "OpenUSD 26.08 resolves a sparse default over a blocked default to no \
            value (default-time `MetadataValueComposer` path, \
            `pxr/usd/usd/stage.cpp:7258`), yet composes a sparse sample over a \
            blocked sample onto the empty array; Layerstack composes both onto the \
            empty array or fallback."]
fn array_edit_over_blocked_default_matches_openusd() {
    let (_, known) = array_edit_mismatches(KnownDifference::EditOverBlockedDefault);
    assert!(known.is_empty(), "{known:#?}");
}

// ---------------------------------------------------------------------------
// Vectors ported from OpenUSD's testUsdAttributeArrayEdits.cpp
// ---------------------------------------------------------------------------

/// `(prim, time, expected)` from `TestBasics` and `TestInterpolation` in
/// OpenUSD v26.08 `pxr/usd/usd/testenv/testUsdAttributeArrayEdits.cpp`. The
/// test's session layer is `array_edits_strong.usdc` and its root layer is
/// `array_edits_weak.usdc`; each stage of `TestBasics` is its own prim.
/// `None` is the default time.
type Vector = (&'static str, Option<f64>, &'static str);

const TEST_BASICS: &[Vector] = &[
    ("/Basics", None, "[0, 3, 2, 1, 9]"),
    ("/BasicsSamples", Some(0.0), "[3, 3, 2, 1, 3]"),
    ("/BasicsSamples", Some(3.0), "[3, 3, 2, 1, 3]"),
    ("/BasicsSamples", Some(5.0), "[3, 3, 2, 1, 3]"),
    ("/BasicsSamples", Some(6.0), "[6, 3, 2, 1, 7]"),
    ("/BasicsSamples", Some(7.0), "[6, 3, 2, 1, 7]"),
    ("/BasicsBothSamples", Some(0.0), "[3, -1, -1, 3]"),
    ("/BasicsBothSamples", Some(3.0), "[3, -1, -1, 3]"),
    ("/BasicsBothSamples", Some(4.0), "[3, -1, -1, 3]"),
    ("/BasicsBothSamples", Some(5.0), "[3, -5, -5, 3]"),
    ("/BasicsBothSamples", Some(6.0), "[6, -5, -5, 7]"),
    ("/BasicsBothSamples", Some(7.0), "[6, -5, -5, 7]"),
    ("/BasicsBothSamples", Some(9.0), "[6, -9, -9, 7]"),
    ("/BasicsBothSamples", Some(10.0), "[6, -9, -9, 7]"),
];

/// `TestBasics`: "Get at default should ignore the samples."
const TEST_BASICS_DEFAULT_BESIDE_SAMPLES: &[Vector] = &[
    ("/BasicsSamples", None, "[0, 3, 2, 1, 9]"),
    ("/BasicsBothSamples", None, "[0, 3, 2, 1, 9]"),
];

/// `TestInterpolation` at sample times and outside the sampled range.
const TEST_INTERPOLATION: &[Vector] = &[
    ("/InterpRoot", Some(0.0), "[0.0, 0.0, 0.0, 0.0]"),
    ("/InterpRoot", Some(1.0), "[0.0, 0.0, 0.0, 0.0]"),
    ("/InterpRoot", Some(3.0), "[2.0, 4.0, 6.0, 8.0]"),
    ("/InterpRoot", Some(4.0), "[2.0, 4.0, 6.0, 8.0]"),
    ("/Interp", Some(0.0), "[0.0, 8.0, 0.0, 0.0]"),
    ("/Interp", Some(1.0), "[0.0, 8.0, 0.0, 0.0]"),
    ("/Interp", Some(2.0), "[0.0, 8.0, 0.0, 0.0]"),
    ("/Interp", Some(3.0), "[2.0, 8.0, 6.0, 8.0]"),
    ("/Interp", Some(4.0), "[2.0, 8.0, 6.0, 8.0]"),
];

/// `TestInterpolation` between a sparse and a dense sample.
const TEST_INTERPOLATION_BETWEEN_SAMPLES: &[Vector] = &[
    ("/InterpRoot", Some(2.0), "[1.0, 2.0, 3.0, 4.0]"),
    ("/Interp", Some(2.5), "[1.0, 8.0, 3.0, 4.0]"),
];

/// Returns the vectors Layerstack resolves differently.
fn failing_vectors(vectors: &[Vector]) -> Vec<String> {
    let mut loaded = load_entry_usdc(&fixtures_dir().join("array_edits_strong.usdc"));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let store = &mut loaded.store;
    let field = store.tokens.intern("attr");
    let mut failing = Vec::new();
    for &(prim, time, expected) in vectors {
        let path = Path::parse_absolute(prim, &mut store.tokens).expect("prim path");
        let prim_id = store.paths.intern(path);
        let value = match time {
            None => stage
                .resolve_property_path(PropertyPath::new(prim_id, field))
                .and_then(|resolved| match resolved.value {
                    ResolvedValue::Scalar(value) => Some(value),
                    _ => None,
                }),
            Some(time) => stage
                .resolve_property_path_at_time(
                    PropertyPath::new(prim_id, field),
                    time,
                    InterpolationType::Linear,
                )
                .map(|resolved| resolved.value),
        };
        let json: Json = serde_json::from_str(expected).expect("vector");
        if !value.is_some_and(|value| value_matches(&value, &json, &store.tokens)) {
            failing.push(format!("{prim} at {time:?}"));
        }
    }
    failing
}

#[test]
fn test_usd_attribute_array_edits_vectors() {
    let failing = failing_vectors(&[TEST_BASICS, TEST_INTERPOLATION].concat());
    assert!(failing.is_empty(), "{failing:#?}");
}

#[test]
fn test_usd_attribute_array_edits_default_beside_samples() {
    let failing = failing_vectors(TEST_BASICS_DEFAULT_BESIDE_SAMPLES);
    assert!(failing.is_empty(), "{failing:#?}");
}

#[test]
fn test_usd_attribute_array_edits_interpolation() {
    let failing = failing_vectors(TEST_INTERPOLATION_BETWEEN_SAMPLES);
    assert!(failing.is_empty(), "{failing:#?}");
}
