// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored-property fidelity: everything a USD layer authors survives
//! ingestion.
//!
//! The fixtures under `tests/assets/property_fidelity/` were also passed
//! through Apple's `/usr/bin/usdcat` (Apple USD Tools 0.25.11) to produce the
//! `*.usdcat.usda` files. Ingesting the original and OpenUSD's rewrite must
//! yield the same authored content, so this checks Layerstack against
//! OpenUSD's reading of the same text rather than against its own round
//! trip.
//!
//! Spec: AOUSD Core §7.3–§7.6 (specs and their fields), §12.3 (attribute
//! value resolution), §12.4 (connections).

use std::path::{Path, PathBuf};

use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{
    AssetResolveError, AssetResolver, FieldValue, InMemoryStore, InterpolationType, LayerId,
    PropertyPath, ResolvedAsset, Stage, StageOptions, Value,
};
use layerstack_conformance::authored::{Names, dump_layer};
use layerstack_conformance::workspace_root;
use layerstack_usda::emit::emit;
use layerstack_usda::lower::lower;
use layerstack_usda::parser::parse_cst;

fn assets_dir() -> PathBuf {
    workspace_root()
        .join("layerstack_conformance")
        .join("tests")
        .join("assets")
        .join("property_fidelity")
}

/// Rejects every asset path: the fixtures are single layers.
struct NoAssets;

impl AssetResolver for NoAssets {
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

/// Loads one USDA layer as `LayerId(1)`, asserting a diagnostic-free read.
fn load_usda(path: &Path) -> InMemoryStore {
    let source = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let cst = parse_cst(&source);
    let ast = lower(&cst.tree, &source);
    let mut store = InMemoryStore::default();
    let result = emit(
        &ast.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    let diagnostics: Vec<_> = cst
        .diagnostics
        .iter()
        .chain(&ast.diagnostics)
        .chain(&result.diagnostics)
        .collect();
    assert!(
        diagnostics.is_empty(),
        "diagnostics for {}: {diagnostics:?}",
        path.display()
    );
    store.insert_layer(result.layer);
    store
}

fn dump(store: &InMemoryStore) -> Vec<String> {
    dump_layer(
        &store.layers[&LayerId(1)],
        Names {
            tokens: &store.tokens,
            paths: &store.paths,
        },
    )
}

/// Asserts that the fixture and OpenUSD's rewrite of it author the same
/// content, and returns the fixture's store.
fn assert_matches_usdcat(name: &str) -> InMemoryStore {
    let original = load_usda(&assets_dir().join(format!("{name}.usda")));
    let rewritten = load_usda(&assets_dir().join(format!("{name}.usdcat.usda")));
    let (original_dump, rewritten_dump) = (dump(&original), dump(&rewritten));
    assert_eq!(
        original_dump,
        rewritten_dump,
        "{name}: authored content differs from usdcat's rewrite\n{}",
        original_dump.join("\n")
    );
    original
}

#[test]
fn probe_matches_usdcat() {
    assert_matches_usdcat("probe");
}

#[test]
fn metadata_matches_usdcat() {
    assert_matches_usdcat("metadata");
}

/// The roadmap probe: a default next to time samples, and a default next to
/// a connection.
///
/// Spec: AOUSD Core §7.6.4.2.3 (a value, a connection, or both), §12.3.1
/// (default-time queries read defaults only), §12.3.2 (numeric-time
/// queries), §12.4 (connections resolve separately from values).
#[test]
fn probe_keeps_every_authored_slot() {
    let mut store = assert_matches_usdcat("probe");
    let a = store.property_path("/Root.a");
    let b = store.property_path("/Root.b");
    let st = store.property_path("/Root.primvars:st");
    let interpolation = store.tokens.intern("interpolation");
    let constant = store.tokens.intern("constant");
    let root_a = store.target_path("/Root.a");

    let layer = &store.layers[&LayerId(1)];
    let a_spec = layer.property(a).expect("a authored");
    assert!(a_spec.custom);
    assert_eq!(a_spec.default, Some(Value::Float(1.0)));
    assert_eq!(
        a_spec.time_samples.as_deref(),
        Some(&[(0.0, Value::Float(2.0)), (1.0, Value::Float(3.0))][..])
    );
    let b_spec = layer.property(b).expect("b authored");
    assert_eq!(b_spec.default, Some(Value::Float(4.0)));
    assert_eq!(
        b_spec.targets.as_ref().and_then(|t| t.explicit.clone()),
        Some(vec![root_a])
    );
    assert_eq!(
        layer
            .property(st)
            .expect("primvars:st authored")
            .metadata(interpolation),
        Some(&FieldValue::Value(Value::Token(constant)))
    );
    let meters = store.tokens.intern("metersPerUnit");
    assert_eq!(
        store.layers[&LayerId(1)].metadata(meters),
        Some(&FieldValue::Value(Value::Double(1.0)))
    );

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let default_of = |path: PropertyPath| stage.resolve_field_path(path).map(|r| r.value);
    assert_eq!(default_of(a), Some(Value::Float(1.0)));
    assert_eq!(default_of(b), Some(Value::Float(4.0)));
    assert_eq!(
        stage
            .resolve_property_path_at_time(a, 1.0, InterpolationType::Held)
            .map(|r| r.value),
        Some(Value::Float(3.0))
    );
    assert_eq!(
        stage
            .resolve_property_path_at_time(b, 1.0, InterpolationType::Held)
            .map(|r| r.value),
        Some(Value::Float(4.0)),
        "a connection does not replace the attribute's value"
    );
    assert_eq!(
        stage.resolve_target_list_path(b).map(|r| r.value),
        Some(vec![root_a])
    );
    assert_eq!(
        stage.resolve_target_list_path(a),
        None,
        "`a` authors no connections"
    );

    // Removing one slot leaves the others in place.
    let layer = store.layers.get_mut(&LayerId(1)).expect("layer");
    layer.property_mut(a).expect("a").default = None;
    layer.property_mut(b).expect("b").targets = None;
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(stage.resolve_field_path(a), None, "no default left on `a`");
    assert_eq!(
        stage
            .resolve_property_path_at_time(a, 0.0, InterpolationType::Held)
            .map(|r| r.value),
        Some(Value::Float(2.0)),
        "the samples survive removing the default"
    );
    assert_eq!(
        stage.resolve_field_path(b).map(|r| r.value),
        Some(Value::Float(4.0)),
        "the default survives removing the connection"
    );
    assert_eq!(stage.resolve_target_list_path(b), None);
}
