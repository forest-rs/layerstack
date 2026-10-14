// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Numeric import checks exact integer boundaries before lossy coercion.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, ResolvedAsset,
    TokenInterner, Value,
};
use layerstack_usda::{emit, parser, read_usda};

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

#[test]
fn numeric_import_preserves_integer_minimum_and_rejects_rounded_overflow() {
    // Native OpenUSD 26.8: GfNumericCast checks floating (min-1, max+1)
    // before truncation. Large rounded doubles must not impersonate exact
    // integer boundary literals. AOUSD Core §6.2 (numeric types and ranges).
    for (ty, literal, expected) in [
        (
            "int64",
            "-9223372036854775808",
            Some(Value::Int64(i64::MIN)),
        ),
        ("int64", "-9223372036854775809", None),
        ("int64", "-9223372036854775808.0", None),
        (
            "int64",
            "-9223372036854774784.0",
            Some(Value::Int64(-9_223_372_036_854_774_784)),
        ),
        (
            "uint64",
            "18446744073709551615",
            Some(Value::UInt64(u64::MAX)),
        ),
        ("uint64", "18446744073709551615.0", None),
        ("int", "-2147483648.5", Some(Value::Int(i32::MIN))),
    ] {
        let source = format!(
            "#usda 1.0\ndef \"P\" {{\n {ty} x = {literal}\n {ty}[] array = [{literal}]\n {ty}[] samples.timeSamples = {{0: [{literal}]}}\n}}\n"
        );
        let mut store = InMemoryStore::default();
        let result = read_usda(
            &source,
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
            &mut NoAssets,
        );
        let path = store.path("/P");
        assert_eq!(
            result.emitted.layer.prims[&path].properties[0].spec.default, expected,
            "{ty} {literal}"
        );
        assert_eq!(
            result.stats.numeric_arrays, 2,
            "both array defaults and samples use compact conversion"
        );
        let prim = &result.emitted.layer.prims[&path];
        for (name, sample) in [("array", false), ("samples", true)] {
            let property = prim
                .properties
                .iter()
                .find(|entry| store.tokens.resolve(entry.name) == name)
                .unwrap();
            let value = if sample {
                property
                    .spec
                    .time_samples
                    .as_ref()
                    .and_then(|samples| samples.first())
                    .map(|(_, value)| value)
            } else {
                property.spec.default.as_ref()
            };
            assert_eq!(
                value
                    .and_then(Value::array_ref)
                    .and_then(|values| values.get(0))
                    .map(|value| value.into_owned()),
                expected,
                "{ty} {literal} {name}"
            );
        }
        let parsed = parser::parse(&source);
        assert!(parsed.diagnostics.is_empty());
        let ast = emit::emit(
            &parsed.layer,
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
            &mut NoAssets,
        );
        assert_eq!(
            ast.layer, result.emitted.layer,
            "AST and compact import agree for {ty} {literal}"
        );
        assert_eq!(
            result.emitted.diagnostics.is_empty(),
            expected.is_some(),
            "{ty} {literal}"
        );
    }
}
