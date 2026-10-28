// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A layer whose namespace is far deeper than the USDA reader reads,
//! authored through the layer API and saved as USDA, as OpenUSD 26.11
//! writes it.
//!
//! `scripts/usda_namespace_depth_oracle.py` builds the layer with `Sdf`
//! and records the text `ExportToString` writes for it, by length, line
//! count and FNV-1a hash (`fixtures/usda_namespace_depth/oracle.json`).
//! Here the same layer is built with [`Layer::define_prim`] and variant
//! specs and saved with [`save_usda`] on a thread whose stack is far too
//! small for one frame per namespace level, so a writer that recursed
//! through the namespace would overflow it.
//!
//! Spec: AOUSD Core §16.2 (USDA), §7.3.5–§7.3.6 (prim, variant set and
//! variant specs).

use std::thread;

use layerstack::doc::{Layer, PrimSpec};
use layerstack::path::{Path, PathId};
use layerstack::property::{PropertySpec, PropertyType};
use layerstack::spec_path::VariantSelectionSite;
use layerstack::{InMemoryStore, LayerId, NoAssets, PathInterner, TokenInterner, Value};
use layerstack_usda::read_usda;
use layerstack_usda::save::save_usda;
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/usda_namespace_depth/oracle.json");

/// The stack the save runs on: a few frames for each of the hundreds of
/// namespace levels would not fit.
const STACK_BYTES: usize = 128 * 1024;

/// What OpenUSD wrote for the layer.
struct Expected {
    depth: usize,
    variant_depth: usize,
    child_depth: usize,
    bytes: usize,
    lines: usize,
    fnv1a64: String,
}

fn expected() -> Expected {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    assert_eq!(
        oracle["reads_back"],
        Json::Bool(true),
        "OpenUSD reads its text back"
    );
    let count = |key: &str| usize::try_from(oracle[key].as_u64().unwrap()).unwrap();
    Expected {
        depth: count("depth"),
        variant_depth: count("variant_depth"),
        child_depth: count("child_depth"),
        bytes: count("bytes"),
        lines: count("lines"),
        fnv1a64: String::from(oracle["fnv1a64"].as_str().unwrap()),
    }
}

/// The oracle's layer: `depth` prims `def "A"`, the last holding
/// `int x = 1` and `variant_depth` variant sets `v0`, `v1`, ... nested in
/// each other's variant `x`, the innermost holding `child_depth` prims
/// `def "C"`.
fn deep_layer(expected: &Expected, tokens: &TokenInterner, paths: &PathInterner) -> Layer {
    let mut layer = Layer::new(LayerId(1));
    let a = tokens.intern("A");
    let mut leaf = paths.intern(Path::root());
    for _ in 0..expected.depth {
        leaf = paths.intern_child(leaf, a);
        layer.define_prim(leaf, PrimSpec::def(), paths);
    }
    let int = PropertyType::parse("int").unwrap();
    let spec = layer.prim_mut(leaf).unwrap();
    spec.set_property(
        tokens.intern("x"),
        PropertySpec::typed_attribute(int).with_default(Value::Int(1)),
    );

    let x = tokens.intern("x");
    let c = tokens.intern("C");
    let mut sites = Vec::new();
    let mut sets = &mut spec.rare_mut().variant_sets;
    for i in 0..expected.variant_depth {
        let set = tokens.intern(format!("v{i}"));
        sites.push(VariantSelectionSite {
            host_path: leaf,
            set,
            variant: x,
        });
        let variant = sets
            .get_or_insert_default(set)
            .variants
            .get_or_insert_default(x);
        if i + 1 == expected.variant_depth {
            variant.authored_children.push(c);
        }
        sets = &mut variant.variant_sets;
    }
    let mut child: PathId = leaf;
    for depth in 0..expected.child_depth {
        child = paths.intern_child(child, c);
        let mut spec = PrimSpec::def().with_outer_variant_sites(sites.clone());
        if depth + 1 < expected.child_depth {
            spec.authored_children.push(c);
        }
        layer.insert_prim(child, spec);
    }
    layer
}

fn fnv1a64(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// The deep layer saves on a small stack, and its text is OpenUSD's.
#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
fn deep_namespace_saves_as_openusd_writes_it() {
    let expected = expected();
    let saved = thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let tokens = TokenInterner::default();
            let paths = PathInterner::default();
            let layer = deep_layer(&expected, &tokens, &paths);
            let text = save_usda(&layer, &tokens, &paths).map_err(|e| format!("{e:?}"));
            (expected, text)
        })
        .unwrap()
        .join();
    let (expected, text) = saved.expect("the save fits the small stack");
    let text = text.expect("saves");
    assert_eq!(text.len(), expected.bytes, "length");
    assert_eq!(text.split('\n').count(), expected.lines, "lines");
    assert_eq!(fnv1a64(text.as_bytes()), expected.fnv1a64, "hash");
    // The reader's nesting limit is the reader's alone: it refuses the
    // text OpenUSD reads back.
    let store = InMemoryStore::default();
    let read = read_usda(
        &text,
        LayerId(2),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    );
    assert!(
        read.parse_diagnostics
            .iter()
            .any(|d| d.message.contains("deeper than 256 levels")),
        "{:?}",
        read.parse_diagnostics
    );
}
