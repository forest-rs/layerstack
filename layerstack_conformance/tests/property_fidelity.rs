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
    PropertyPath, ResolvedAsset, ResolvedValue, Stage, StageOptions, Value, Variability,
};
use layerstack_conformance::authored::{Names, dump_layer, render_value};
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

/// Lines only in `left` (`-`) or only in `right` (`+`).
fn line_diff(left: &[String], right: &[String]) -> String {
    let only = |a: &[String], b: &[String], mark: char| {
        a.iter()
            .filter(|line| !b.contains(line))
            .map(|line| format!("{mark} {line}"))
            .collect::<Vec<_>>()
    };
    let mut out = only(left, right, '-');
    out.extend(only(right, left, '+'));
    out.join("\n")
}

/// Loads one USDC layer as `LayerId(1)`, asserting a diagnostic-free read.
fn load_usdc(path: &Path) -> InMemoryStore {
    let data =
        std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let mut store = InMemoryStore::default();
    let result = layerstack_usdc::read_usdc(
        &data,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    assert!(
        result.diagnostics.is_empty(),
        "diagnostics for {}: {:?}",
        path.display(),
        result.diagnostics
    );
    store.insert_layer(result.layer);
    store
}

/// Asserts that the fixture and OpenUSD's rewrite of it author the same
/// content, and returns the fixture's store.
///
/// Both of OpenUSD's rewrites are checked: USDA text (`*.usdcat.usda`) and
/// the crate binary (`*.usdc`, written by `usdcat -o`).
fn assert_matches_usdcat(name: &str) -> InMemoryStore {
    let original = load_usda(&assets_dir().join(format!("{name}.usda")));
    let binary = load_usdc(&assets_dir().join(format!("{name}.usdc")));
    let (original_dump, binary_dump) = (dump(&original), dump(&binary));
    assert!(
        original_dump == binary_dump,
        "{name}: USDC content differs from the USDA it was written from\n{}",
        line_diff(&original_dump, &binary_dump)
    );
    assert_matches_usdcat_text(name)
}

/// Asserts that the fixture and OpenUSD's text rewrite of it author the same
/// content, and returns the fixture's store.
fn assert_matches_usdcat_text(name: &str) -> InMemoryStore {
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

/// Relative inherits, specializes, connection and relationship paths are
/// anchored to the authoring prim; `usdcat` writes them absolute.
///
/// Spec: AOUSD Core §8 (paths).
#[test]
fn relative_paths_match_usdcat() {
    assert_matches_usdcat("relative_paths");
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

/// Nested UI-hint `limits` metadata composes by dictionary combining: a
/// stronger soft minimum keeps the weaker soft maximum and hard limits.
///
/// Expected values come from `ui_hints.flattened.usda`, Apple `usdcat
/// --flatten` of `ui_hints.usda` (only its generated `doc` was shortened).
///
/// Spec: AOUSD Core §12.2.5 (dictionaries combine);
/// `OpenUSD-proposals/proposals/ui-hints/README.md` (weaker entries
/// survive).
#[test]
fn ui_hint_limits_combine_like_usdcat_flatten() {
    let mut loaded =
        layerstack_conformance::usda_real::load_entry_usda(&assets_dir().join("ui_hints.usda"));
    let prim = loaded.store.path("/Root");
    let softness = loaded.store.tokens.intern("softness");
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );

    let mut flattened = load_usda(&assets_dir().join("ui_hints.flattened.usda"));
    let flat_softness = flattened.property_path("/Root.softness");

    for key_name in ["limits", "customData"] {
        let key = loaded.store.tokens.intern(key_name);
        let resolved = stage
            .resolve_property_metadata(prim, softness, key)
            .unwrap_or_else(|| panic!("{key_name} resolves"));
        let ResolvedValue::Dictionary(entries) = resolved.value else {
            panic!("{key_name} is a dictionary");
        };
        let flat_key = flattened.tokens.intern(key_name);
        let Some(FieldValue::Value(expected)) = flattened.layers[&LayerId(1)]
            .property(flat_softness)
            .and_then(|spec| spec.metadata(flat_key))
        else {
            panic!("flattened {key_name}");
        };
        let actual = render_value(
            &Value::Dictionary(entries),
            Names {
                tokens: &loaded.store.tokens,
                paths: &loaded.store.paths,
            },
        );
        let expected = render_value(
            expected,
            Names {
                tokens: &flattened.tokens,
                paths: &flattened.paths,
            },
        );
        assert_eq!(actual, expected, "{key_name} differs from usdcat --flatten");
    }
    assert_eq!(
        stage
            .resolve_field_path(PropertyPath::new(prim, softness))
            .map(|r| r.value),
        Some(Value::Float(0.5)),
        "the weaker default survives the stronger declaration-only spec"
    );
}

/// Unknown applied schemas and namespaced uniform properties survive, and
/// declarations resolve per Core §12.2.3–§12.2.4.
#[test]
fn unknown_schemas_and_uniform_namespaced_properties_survive() {
    let mut store = assert_matches_usdcat("metadata");
    let prim = store.path("/Root");
    let api = store.tokens.intern("apiSchemas");
    let review = store.tokens.intern("StudioReviewAPI:main");
    let tool = store.tokens.intern("authorship:main:tool");
    let inputs = store.tokens.intern("authorship:main:inputs");
    let softness = store.tokens.intern("softness");
    let color = store.tokens.intern("primvars:displayColor");
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let schemas = stage
        .resolve_token_list(prim, api)
        .expect("apiSchemas")
        .value;
    assert!(schemas.contains(&review), "unknown applied schema kept");

    let tool = stage
        .resolve_property_declaration(prim, tool)
        .expect("tool");
    assert_eq!(tool.variability, Variability::Uniform);
    assert!(!tool.custom);
    let inputs = stage
        .resolve_property_declaration(prim, inputs)
        .expect("inputs");
    assert_eq!(inputs.variability, Variability::Uniform);
    assert!(inputs.custom);
    assert_eq!(inputs.type_name.map(|t| t.is_array), Some(true));

    assert_eq!(
        stage.resolve_property_order(prim, &store),
        Some(vec![softness, color])
    );
}

/// Two variant branches author the same descendant paths; each branch keeps
/// its own specs, in USDA, in OpenUSD's rewrite and in its USDC, and only
/// the selected branch contributes (checked against `usdcat --flatten`:
/// `detail = 1`, `purpose = "proxy"`).
///
/// Spec: AOUSD Core §7.3.6 (variant specs contain prim specs), §10.5.
#[test]
fn variant_branch_specs_at_the_same_path_are_kept() {
    let mut store = assert_matches_usdcat("variant_branches");
    let geom = store.path("/Model/Geom");
    let mesh = store.path("/Model/Geom/Mesh");
    let detail = store.tokens.intern("detail");
    let purpose = store.tokens.intern("purpose");
    let proxy = store.tokens.intern("proxy");

    let layer = &store.layers[&LayerId(1)];
    assert_eq!(layer.prim_specs(geom).count(), 2, "one `Geom` per branch");
    assert_eq!(layer.prim_specs(mesh).count(), 2, "one `Mesh` per branch");

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        stage
            .resolve_field_path(PropertyPath::new(geom, detail))
            .map(|r| r.value),
        Some(Value::Int(1))
    );
    assert_eq!(
        stage
            .resolve_field_path(PropertyPath::new(mesh, purpose))
            .map(|r| r.value),
        Some(Value::Token(proxy))
    );
    assert_eq!(
        stage
            .explain_property_path(PropertyPath::new(mesh, purpose))
            .map(<[layerstack::Opinion]>::len),
        Some(1),
        "the unselected branch contributes nothing"
    );
}

/// The same fixture read from OpenUSD's USDC composes the same way.
#[test]
fn variant_branch_specs_compose_from_usdc() {
    let mut store = load_usdc(&assets_dir().join("variant_branches.usdc"));
    let mesh = store.path("/Model/Geom/Mesh");
    let purpose = store.tokens.intern("purpose");
    let proxy = store.tokens.intern("proxy");
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        stage
            .resolve_field_path(PropertyPath::new(mesh, purpose))
            .map(|r| r.value),
        Some(Value::Token(proxy))
    );
}

/// Declarations of a prim authored in several variant branches come from
/// the selected branch's spec, including through nested selections.
///
/// Expected values from OpenUSD 26.08 (`GetTypeName`, `GetSpecifier`,
/// `GetPropertyOrder`, `GetPrimStack` of `/P/C`):
/// `variant_declarations.usda` → `Xform`, class, `[b]`, `/P{v=b}C`;
/// `variant_declarations_nested.usda` → `Xform`, class, `[y]`,
/// `/P{v=b}{w=y}C`.
///
/// Spec: AOUSD Core §10.5 (only the selected variant contributes), §12.2.1
/// (specifier), §12.2.2 (type name).
#[test]
fn declarations_come_from_the_selected_variant_branch() {
    for (fixture, order) in [
        ("variant_declarations.usda", "b"),
        ("variant_declarations_nested.usda", "y"),
    ] {
        let mut store = load_usda(&assets_dir().join(fixture));
        let c = store.path("/P/C");
        let xform = store.tokens.intern("Xform");
        let order = store.tokens.intern(order);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(stage.resolve_type_name(c, &store), Some(xform), "{fixture}");
        assert_eq!(
            stage.resolve_specifier(c, &store),
            Some(layerstack::Specifier::Class),
            "{fixture}"
        );
        assert_eq!(
            stage.resolve_property_order(c, &store),
            Some(vec![order]),
            "{fixture}"
        );
    }
}
