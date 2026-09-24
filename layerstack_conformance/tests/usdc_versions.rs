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
    AssetResolveError, AssetResolver, ResolvedAsset, ResolvedValue, Stage, StageOptions,
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

            let default =
                stage
                    .resolve_value(prim, field)
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
                    .resolve_value_at_time(prim, field, time, InterpolationType::Linear)
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
