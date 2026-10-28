// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A layer whose namespace is hundreds of levels deep, authored through
//! the layer API, saved as USDC and read back, as OpenUSD 26.11 writes and
//! reads it.
//!
//! `scripts/usdc_namespace_depth_oracle.py` builds the layer with `Sdf`
//! and records the crate file `SdfLayer::Export` writes for it
//! (`fixtures/usdc_namespace_depth/deep.usdc`, and its length and FNV-1a
//! hash in `oracle.json`) with the text of the layer OpenUSD reads back.
//! The saved file must be OpenUSD's but for its LZ4 blocks
//! ([`crate_sections`]).
//! Here the same layer is built with [`Layer::define_prim`] and variant
//! specs, saved with [`save_usdc`], and both files are read back with
//! [`read_usdc`], each on a thread whose stack is far too small for one
//! frame per namespace level, so a writer or reader that recursed through
//! the namespace would overflow it.
//!
//! Spec: AOUSD Core §16.3 (USDC), §7.3.5–§7.3.6 (prim, variant set and
//! variant specs).

use std::sync::Arc;
use std::thread;

use layerstack::doc::{Layer, PrimSpec};
use layerstack::path::{Path, PathId, TargetPath};
use layerstack::property::{PropertySpec, PropertyType};
use layerstack::spec_path::VariantSelectionSite;
use layerstack::{LayerId, ListOp, NoAssets, PathInterner, TokenInterner, Value};
use layerstack_conformance::usdc::crate_sections;
use layerstack_conformance::workspace_root;
use layerstack_usda::save::save_usda;
use layerstack_usdc::{AssembleResult, read_usdc, read_usdc_lazy, save_usdc};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/usdc_namespace_depth/oracle.json");

/// The stack each save and read runs on: a few frames for each of the
/// hundreds of namespace levels would not fit.
const STACK_BYTES: usize = 128 * 1024;

/// What OpenUSD wrote for the layer, and read back from it.
struct Expected {
    depth: usize,
    variant_depth: usize,
    child_depth: usize,
    usda_bytes: usize,
    usda_fnv1a64: String,
}

fn expected() -> Expected {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    assert_eq!(
        oracle["reads_back"],
        Json::Bool(true),
        "OpenUSD reads its file back"
    );
    let count = |key: &str| usize::try_from(oracle[key].as_u64().unwrap()).unwrap();
    let hash = |key: &str| String::from(oracle[key].as_str().unwrap());
    Expected {
        depth: count("depth"),
        variant_depth: count("variant_depth"),
        child_depth: count("child_depth"),
        usda_bytes: count("usda_bytes"),
        usda_fnv1a64: hash("usda_fnv1a64"),
    }
}

/// The oracle's layer: `depth` prims `def "A"`, the last holding
/// `int x = 1` and `variant_depth` variant sets `v0`, `v1`, ... nested in
/// each other's variant `x`, the innermost holding `child_depth` prims
/// `def "C"`, and the first holding `rel deep = </A/A/.../A>`, targeting
/// the last `A`.
fn deep_layer(expected: &Expected, tokens: &TokenInterner, paths: &PathInterner) -> Layer {
    let mut layer = Layer::new(LayerId(1));
    let a = tokens.intern("A");
    let mut leaf = paths.intern(Path::root());
    for _ in 0..expected.depth {
        leaf = paths.intern_child(leaf, a);
        layer.define_prim(leaf, PrimSpec::def(), paths);
    }
    let first = paths.intern_child(paths.intern(Path::root()), a);
    layer.prim_mut(first).unwrap().set_property(
        tokens.intern("deep"),
        PropertySpec::relationship().with_targets(ListOp::explicit(vec![TargetPath::Prim(leaf)])),
    );
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

/// The file OpenUSD writes for the layer.
fn openusd_file() -> Vec<u8> {
    std::fs::read(
        workspace_root().join("layerstack_conformance/fixtures/usdc_namespace_depth/deep.usdc"),
    )
    .unwrap()
}

/// Runs `f` on a thread with a [`STACK_BYTES`] stack.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(f)
        .unwrap()
        .join()
        .expect("fits the small stack")
}

/// Reads `data` back, eagerly and retaining its values, and saves what
/// each read as USDA, on a small stack, with the readers' diagnostics.
fn read_back_as_usda(data: Vec<u8>) -> [(String, Vec<String>); 2] {
    on_small_stack(move || {
        let save = |read: AssembleResult, tokens: &TokenInterner, paths: &PathInterner| {
            let diagnostics = read.diagnostics.iter().map(|d| format!("{d:?}")).collect();
            let text = save_usda(&read.layer, tokens, paths).expect("saves as USDA");
            (text, diagnostics)
        };
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let eager = read_usdc(&data, LayerId(2), &tokens, &paths, &mut NoAssets).expect("reads");
        let eager = save(eager, &tokens, &paths);
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let lazy = read_usdc_lazy(Arc::from(data), LayerId(3), &tokens, &paths, &mut NoAssets)
            .expect("reads");
        let lazy = save(lazy.assembled, &tokens, &paths);
        [eager, lazy]
    })
}

/// Asserts that both reads of a file of the deep layer saved as the text
/// of the layer OpenUSD reads from it.
fn assert_reads_as_openusd_reads(expected: &Expected, data: Vec<u8>) {
    for (text, diagnostics) in read_back_as_usda(data) {
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(text.len(), expected.usda_bytes, "length");
        assert_eq!(fnv1a64(text.as_bytes()), expected.usda_fnv1a64, "hash");
    }
}

/// The deep layer saves on a small stack as the file OpenUSD writes, and
/// reads back as the layer OpenUSD reads from that file.
#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
fn deep_namespace_saves_as_openusd_writes_it() {
    let expected = expected();
    let (expected, data) = on_small_stack(move || {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let layer = deep_layer(&expected, &tokens, &paths);
        let data = save_usdc(&layer, &tokens, &paths).map_err(|e| format!("{e:?}"));
        (expected, data)
    });
    let data = data.expect("saves");
    let openusd = openusd_file();
    assert!(
        crate_sections(&data).unwrap() == crate_sections(&openusd).unwrap(),
        "the file OpenUSD writes"
    );
    assert_reads_as_openusd_reads(&expected, data);
}

/// OpenUSD's file of the deep layer reads back on a small stack, eagerly
/// and retaining its values, as the layer OpenUSD reads from it.
#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
fn deep_namespace_reads_as_openusd_reads_it() {
    let expected = expected();
    let data = openusd_file();
    assert_reads_as_openusd_reads(&expected, data);
}
