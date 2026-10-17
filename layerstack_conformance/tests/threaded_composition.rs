// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composing on several threads composes the same stage as on one.
//!
//! Every fixture of the supplemental composition suite is loaded twice and
//! composed once with [`StageOptions::threads`] at 8 and once at 1. Each
//! load interns its own paths, in an order the threads decide for the
//! first, so the stages are compared by what they say, not by id: every
//! prim in traversal order with its children, prim stack and property
//! names, and the composition errors in order, each by kind and the prim
//! it names.

use std::fmt::Write as _;
use std::path::PathBuf;

use layerstack::{LayerStore, Stage, StageOptions};
use layerstack_conformance::{
    pcp_txt::load_pcp_txt,
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};

fn assets_dir() -> PathBuf {
    workspace_root()
        .join("core-spec-supplemental-release_dec2025")
        .join("composition")
        .join("tests")
        .join("assets")
}

/// `loaded`'s stage composed on `threads` threads, rendered without ids.
fn render(mut loaded: LoadedStage, threads: usize) -> String {
    let standin = loaded.store.tokens.intern("standin");
    let render = loaded.store.tokens.intern("render");
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            with_provenance: true,
            variant_fallbacks: [(standin, vec![render])].into_iter().collect(),
            threads,
            ..StageOptions::default()
        },
    );
    let store = &loaded.store;
    let (paths, tokens) = (store.paths(), store.tokens());
    let path = |id| paths.display(id, tokens);
    let mut out = String::new();
    let root = paths.lookup(&layerstack::Path::root()).expect("root");
    for prim in stage.traverse_all(root) {
        writeln!(out, "{}", path(prim)).unwrap();
        for child in stage.children_of(prim).unwrap_or(&[]) {
            writeln!(out, "  child {}", path(*child)).unwrap();
        }
        for key in stage.explain_prim(prim).unwrap_or(&[]) {
            let layer = loaded.layer_names.get(&key.layer_id).cloned();
            let site = key.spec_path.display(paths, tokens);
            writeln!(out, "  site {} {site}", layer.unwrap_or_default()).unwrap();
        }
        for name in stage.property_names(prim, store) {
            writeln!(out, "  property {}", tokens.resolve(name)).unwrap();
        }
    }
    for error in stage.composition_errors() {
        let prim = error.prim().map(path).unwrap_or_default();
        writeln!(out, "error {:?} {prim}", core::mem::discriminant(error)).unwrap();
    }
    out
}

#[test]
fn eight_threads_compose_the_stage_one_does() {
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(assets_dir())
        .expect("composition assets")
        .map(|entry| entry.expect("fixture").path())
        .filter(|dir| dir.join("pcp.txt").is_file())
        .collect();
    fixtures.sort();
    assert!(fixtures.len() > 100, "found {} fixtures", fixtures.len());
    for dir in fixtures {
        let entry = dir
            .join("usda")
            .join(load_pcp_txt(&dir.join("pcp.txt")).entry);
        let threaded = render(load_entry_usda(&entry), 8);
        let single = render(load_entry_usda(&entry), 1);
        assert_eq!(threaded, single, "{}", dir.display());
    }
}
