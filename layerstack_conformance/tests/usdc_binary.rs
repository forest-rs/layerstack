// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Integration tests for USDC binary file parsing.
//!
//! These tests read the supplemental spec's `gen_*.usdc` reference files
//! through `layerstack_usdc::read_usdc` and verify the assembled layers
//! contain the expected structure and values.
//!
//! Spec: AOUSD Core §16.3.

// The test fixture stores 3.1415 (not std::f64::consts::PI), so we suppress
// the approx_constant lint for this module.
#![allow(
    clippy::approx_constant,
    reason = "test fixtures store 3.1415, not std PI"
)]

use std::path::{Path, PathBuf};

use layerstack::doc::{LayerId, Value};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, InMemoryStore, PropertySpec, ResolvedAsset};
use layerstack_conformance::workspace_root;

/// Path to the binary test assets relative to the workspace root.
fn binary_assets_dir() -> PathBuf {
    workspace_root().join("core-spec-supplemental-release_dec2025/file_formats/tests/assets/binary")
}

/// Reads a gen_*.usdc file and returns the assembled layer plus store.
fn read_gen(name: &str) -> ParsedUsdc {
    let path = binary_assets_dir().join(format!("gen_{name}.usdc"));
    read_usdc_file(&path)
}

/// Reads a .usdc file and returns the assembled layer plus store.
fn read_usdc_file(path: &Path) -> ParsedUsdc {
    let data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

    let mut store = InMemoryStore::default();
    let layer_id = LayerId(1);
    let mut resolver = StubResolver;

    let result = layerstack_usdc::read_usdc(
        &data,
        layer_id,
        &mut store.tokens,
        &mut store.paths,
        &mut resolver,
    )
    .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));

    store.insert_layer(result.layer);

    ParsedUsdc {
        store,
        layer_id,
        diagnostics: result.diagnostics,
    }
}

struct ParsedUsdc {
    store: InMemoryStore,
    layer_id: LayerId,
    diagnostics: Vec<layerstack_usdc::assemble::AssembleDiagnostic>,
}

impl ParsedUsdc {
    /// Returns the prim spec at a given path, panicking when absent.
    fn prim(&mut self, prim_path: &str) -> layerstack::PrimSpec {
        let path = layerstack::path::Path::parse_absolute(prim_path, &mut self.store.tokens)
            .expect("invalid path");
        let path_id = self.store.paths.lookup(&path).expect("path interned");
        self.store.layers[&self.layer_id].prims[&path_id].clone()
    }

    /// Returns the authored property spec at a given prim path and name.
    fn property(&mut self, prim_path: &str, name: &str) -> Option<PropertySpec> {
        let path = layerstack::path::Path::parse_absolute(prim_path, &mut self.store.tokens)
            .expect("invalid path");
        let path_id = self.store.paths.lookup(&path)?;
        let name_tok = self.store.tokens.intern(name);
        let layer = self.store.layers.get(&self.layer_id)?;
        layer.prims.get(&path_id)?.property(name_tok).cloned()
    }

    /// Returns the property spec at a given prim path and name, panicking on
    /// None.
    fn expect_property(&mut self, prim_path: &str, name: &str) -> PropertySpec {
        self.property(prim_path, name)
            .unwrap_or_else(|| panic!("expected property {name:?} on prim {prim_path:?}"))
    }

    /// Returns the authored default value of an attribute.
    fn expect_value(&mut self, prim_path: &str, name: &str) -> Value {
        self.expect_property(prim_path, name)
            .default
            .unwrap_or_else(|| panic!("expected a default at {prim_path}.{name}"))
    }

    /// Helper to check if a prim exists.
    fn has_prim(&mut self, prim_path: &str) -> bool {
        let path = layerstack::path::Path::parse_absolute(prim_path, &mut self.store.tokens)
            .expect("invalid path");
        let Some(path_id) = self.store.paths.lookup(&path) else {
            return false;
        };
        let Some(layer) = self.store.layers.get(&self.layer_id) else {
            return false;
        };
        layer.prims.contains_key(&path_id)
    }
}

/// Stub resolver that doesn't resolve any assets (for simple test files).
struct StubResolver;

impl AssetResolver for StubResolver {
    fn resolve(
        &mut self,
        asset_path: &str,
        _anchor: Option<LayerId>,
        _tokens: &mut TokenInterner,
        _paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let _ = asset_path;
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

// ---------------------------------------------------------------------------
// Header / structural tests
// ---------------------------------------------------------------------------

#[test]
fn invalid_magic_rejected() {
    let data = vec![0_u8; 64];
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdc::read_usdc(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err());
}

#[test]
fn truncated_file_rejected() {
    let data = b"PXR-USDC";
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdc::read_usdc(data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Scalar / array value tests
// ---------------------------------------------------------------------------

#[test]
fn gen_bool_parses() {
    let mut parsed = read_gen("bool");
    assert!(parsed.has_prim("/root"));

    // single = true
    let single = parsed.expect_value("/root", "single");
    assert_eq!(single, Value::Bool(true));

    // array = [false, false, true, false, false]
    let array = parsed.expect_value("/root", "array");
    match array {
        Value::Array(items) => {
            assert_eq!(items.len(), 5);
            assert_eq!(items[0], Value::Bool(false));
            assert_eq!(items[2], Value::Bool(true));
        }
        _ => panic!("expected array, got {array:?}"),
    }
}

#[test]
fn gen_int_parses() {
    let mut parsed = read_gen("int");
    assert!(parsed.has_prim("/root"));

    let single = parsed.expect_value("/root", "single");
    assert_eq!(single, Value::Int(-2_147_483_647));

    let array = parsed.expect_value("/root", "array");
    match array {
        Value::Array(items) => {
            assert_eq!(items.len(), 7);
            assert_eq!(items[0], Value::Int(-2_147_483_647));
            assert_eq!(items[6], Value::Int(2_147_483_647));
        }
        _ => panic!("expected array, got {array:?}"),
    }
}

#[test]
fn gen_uint_parses() {
    let mut parsed = read_gen("uint");
    let single = parsed.expect_value("/root", "single");
    assert_eq!(single, Value::UInt(4_294_967_295));
}

#[test]
fn gen_int64_parses() {
    let mut parsed = read_gen("int64");
    let single = parsed.expect_value("/root", "single");
    assert_eq!(single, Value::Int64(-9_223_372_036_854_775_807));
}

#[test]
fn gen_uint64_parses() {
    let mut parsed = read_gen("uint64");
    let single = parsed.expect_value("/root", "single");
    assert_eq!(single, Value::UInt64(18_446_744_073_709_551_615));
}

#[test]
fn gen_uchar_parses() {
    let mut parsed = read_gen("uchar");
    let single = parsed.expect_value("/root", "single");
    assert_eq!(single, Value::UChar(255));
}

#[test]
fn gen_float_parses() {
    let mut parsed = read_gen("float");
    let single = parsed.expect_value("/root", "single");
    match single {
        Value::Float(v) => assert!((v - 3.1415).abs() < 0.001, "float single = {v}"),
        _ => panic!("expected Float, got {single:?}"),
    }
}

#[test]
fn gen_double_parses() {
    let mut parsed = read_gen("double");
    let single = parsed.expect_value("/root", "single");
    match single {
        Value::Double(v) => assert!((v - 3.1415).abs() < 0.001, "double single = {v}"),
        _ => panic!("expected Double, got {single:?}"),
    }
}

#[test]
fn gen_half_parses() {
    let mut parsed = read_gen("half");
    let single = parsed.expect_value("/root", "single");
    // Half is stored as u16 bits. Convert to f32 manually.
    match single {
        Value::Half(bits) => {
            let f = half_bits_to_f32(bits);
            assert!((f - 3.1415).abs() < 0.02, "half single = {f}");
        }
        _ => panic!("expected Half, got {single:?}"),
    }
}

/// Convert IEEE 754 half-precision bits to f32.
fn half_bits_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1F) as u32;
    let frac = (bits & 0x3FF) as u32;

    if exp == 0 {
        // Subnormal or zero.
        let f = (frac as f32) / 1024.0 * 2.0_f32.powi(-14);
        if sign == 1 { -f } else { f }
    } else if exp == 31 {
        // Inf or NaN.
        if frac == 0 {
            if sign == 1 {
                f32::NEG_INFINITY
            } else {
                f32::INFINITY
            }
        } else {
            f32::NAN
        }
    } else {
        // Normal.
        let f32_bits = (sign << 31) | ((exp + 112) << 23) | (frac << 13);
        f32::from_bits(f32_bits)
    }
}

#[test]
fn gen_string_parses() {
    let mut parsed = read_gen("string");
    let single = parsed.expect_value("/root", "single");
    match single {
        Value::String(s) => assert_eq!(&*s, "Hello/World"),
        _ => panic!("expected String, got {single:?}"),
    }
}

#[test]
fn gen_token_parses() {
    let mut parsed = read_gen("token");
    let single = parsed.expect_value("/root", "single");
    match single {
        Value::Token(tok) => {
            let name = parsed.store.tokens.resolve(tok);
            assert_eq!(name, "Hello/World");
        }
        _ => panic!("expected Token, got {single:?}"),
    }
}

#[test]
fn gen_assetpath_parses() {
    let mut parsed = read_gen("assetpath");
    let single = parsed.expect_value("/root", "single");
    match single {
        Value::Asset(a) => assert_eq!(&*a, "Hello/World"),
        _ => panic!("expected Asset, got {single:?}"),
    }
}

// ---------------------------------------------------------------------------
// Variant tests
// ---------------------------------------------------------------------------

#[test]
fn gen_variants_parses() {
    // `usdcat`: `def "root" (variants = { string foo = "eggs" } prepend
    // variantSets = "foo") { variantSet "foo" = { "eggs" {} "spam" {} } }`.
    // Crate paths spell the variant specs `/root{foo=eggs}`.
    let mut parsed = read_gen("variants");
    let root = parsed.prim("/root");
    let foo = parsed.store.tokens.intern("foo");
    let eggs = parsed.store.tokens.intern("eggs");
    let spam = parsed.store.tokens.intern("spam");
    assert_eq!(root.variant_selections.get(&foo), Some(&eggs));
    let set = root.variant_sets.get(&foo).expect("variant set `foo`");
    assert!(set.variants.contains_key(&eggs));
    assert!(set.variants.contains_key(&spam));
}

// ---------------------------------------------------------------------------
// Time samples
// ---------------------------------------------------------------------------

#[test]
fn gen_timesamples_parses() {
    let mut parsed = read_gen("timesamples");
    assert!(parsed.has_prim("/root"));

    // `usdcat` shows `float animated.timeSamples = { 0: 0, 1: 1, …, 9: 9,
    // 11: None }`: each frame's value equals its time, and frame 11 blocks.
    // Crate files store the times as a `DoubleVector`.
    let property = parsed.expect_property("/root", "animated");
    let mut expected: Vec<(f64, Value)> = (0_u8..10)
        .map(|frame| (f64::from(frame), Value::Float(f32::from(frame))))
        .collect();
    expected.push((11.0, Value::Blocked));
    assert_eq!(property.time_samples, Some(expected));
}

// ---------------------------------------------------------------------------
// ListOp tests
// ---------------------------------------------------------------------------

#[test]
fn gen_pathexpression_is_typed() {
    // `usdcat`: `pathExpression[] array = ["/root/Spam", "/root/Eggs"]` and
    // `pathExpression single = "/root/Foo"`. Crate files store them like
    // asset paths (Core §16.3.10.14); they must not read as assets.
    let mut parsed = read_gen("pathexpression");
    assert_eq!(
        parsed.expect_value("/root", "single"),
        Value::PathExpression("/root/Foo".into())
    );
    assert_eq!(
        parsed.expect_value("/root", "array"),
        Value::Array(vec![
            Value::PathExpression("/root/Spam".into()),
            Value::PathExpression("/root/Eggs".into()),
        ])
    );
}

#[test]
fn gen_relocates_reports_unsupported() {
    // Relocates are not modelled; assembly says so instead of dropping them.
    let parsed = read_gen("relocates");
    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.spec_path, "/");
    assert_eq!(diagnostic.field.as_deref(), Some("layerRelocates"));
}

#[test]
fn gen_permissions_reads_permission_as_token() {
    // `permission` is a deprecated Sdf enum field (Core §7.6.2.7); it is kept
    // as the token USDA spells it with.
    let mut parsed = read_gen("permissions");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let specialize = parsed.prim("/Specialize");
    let permission = parsed.store.tokens.intern("permission");
    let private = parsed.store.tokens.intern("private");
    assert_eq!(
        specialize.field(permission),
        Some(&layerstack::FieldValue::Value(Value::Token(private)))
    );
}

#[test]
fn gen_listops_parses() {
    // `usdcat`: `apiSchemas = ["MaterialBindingAPI"]`, `append payload =
    // @eggs.usda@`, `prepend references = @./ref.usda@</Model> (offset =
    // 10)` and `rel foo = [</eggs>, </spam>]`. Crate files name the fields
    // `payload` and `inheritPaths` (`pxr/usd/sdf/schema.h`).
    let mut parsed = read_gen("listops");
    let root = parsed.prim("/root");
    let api = parsed.store.tokens.intern("apiSchemas");
    let binding = parsed.store.tokens.intern("MaterialBindingAPI");
    assert_eq!(
        root.field(api),
        Some(&layerstack::FieldValue::TokenListOp(layerstack::ListOp {
            explicit: Some(vec![binding]),
            ..layerstack::ListOp::default()
        }))
    );
    assert_eq!(root.payloads.append.len(), 1, "{:?}", root.payloads);
    assert_eq!(root.payloads.append[0].asset.as_deref(), Some("eggs.usda"));
    assert_eq!(root.references.prepend.len(), 1);
    assert_eq!(root.references.prepend[0].layer_offset.offset, 10.0);
    let foo = parsed.expect_property("/root", "foo");
    assert!(foo.is_relationship());
    assert_eq!(
        foo.targets.and_then(|t| t.explicit).map(|t| t.len()),
        Some(2)
    );
}

// ---------------------------------------------------------------------------
// Real-world scene tests
// ---------------------------------------------------------------------------

#[test]
fn ball_maya_usdc_parses() {
    let path = binary_assets_dir().join("ball.maya.usdc");
    let parsed = read_usdc_file(&path);

    // The file should have a populated layer with prims.
    let layer = parsed.store.layers.get(&parsed.layer_id).expect("layer");
    assert!(!layer.prims.is_empty(), "expected prims in ball.maya.usdc");
}

#[test]
fn fender_stratocaster_usdc_parses() {
    let path = binary_assets_dir().join("fender_stratocaster.usdc");
    let parsed = read_usdc_file(&path);

    let layer = parsed.store.layers.get(&parsed.layer_id).expect("layer");
    // The Python reference expects 432 specs (prims + attributes + etc.).
    // Only Prim specs become entries in layer.prims, so the count is lower.
    assert!(
        layer.prims.len() > 50,
        "expected many prims in fender_stratocaster.usdc, got {}",
        layer.prims.len()
    );
}

#[test]
fn toy_biplane_idle_usdc_parses() {
    let path = binary_assets_dir().join("toy_biplane_idle.usdc");
    let parsed = read_usdc_file(&path);

    let layer = parsed.store.layers.get(&parsed.layer_id).expect("layer");
    assert!(
        layer.prims.len() > 30,
        "expected many prims in toy_biplane_idle.usdc, got {}",
        layer.prims.len()
    );
}

// ---------------------------------------------------------------------------
// Dictionary tests
// ---------------------------------------------------------------------------

#[test]
fn gen_dict_parses() {
    let _parsed = read_gen("dict");
    // If parsing doesn't panic, the structure was decoded correctly.
}

// ---------------------------------------------------------------------------
// Batch parse tests (ensures all gen_* files parse without error)
// ---------------------------------------------------------------------------

#[test]
fn all_gen_files_parse_without_error() {
    let dir = binary_assets_dir();
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).expect("binary assets dir missing") {
        let entry = entry.expect("dir entry error");
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with("gen_")
            && name.ends_with(".usdc")
        {
            // Parse each file; panic on error.
            let data = std::fs::read(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
            let mut tokens = TokenInterner::default();
            let mut paths = PathInterner::default();
            let mut resolver = StubResolver;
            let result = layerstack_usdc::read_usdc(
                &data,
                LayerId(1),
                &mut tokens,
                &mut paths,
                &mut resolver,
            );
            assert!(result.is_ok(), "failed to parse {name}: {:?}", result.err());
            count += 1;
        }
    }
    assert!(count >= 30, "expected ≥30 gen_* files, found {count}");
}
