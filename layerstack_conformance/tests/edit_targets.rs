// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Edit targets built from prim index nodes write where the node's
//! opinions come from.
//!
//! For every composed prim of every composition and value resolution
//! fixture of the supplemental suite, and every property opinion of it,
//! an `EditTarget` built from the opinion's node and layer
//! (`EditTarget::for_node_layer`) must have the opinion's layer offset, so
//! a time authored through it lands where that opinion's samples are read
//! from. For opinions authored on the prim's own site, the target must map
//! the prim to the opinion's spec path, so an edit through it rewrites the
//! spec that authored the opinion.
//!
//! Spec: AOUSD Core §10.3.1.1 and §12.3.2.1 (layer offsets), §10.4 (the
//! prim index). OpenUSD: `UsdEditTarget(layer, node)`.

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeSet;
use std::path::PathBuf;

use layerstack::{
    EditTarget, InMemoryStore, LayerStack, Opinion, PathId, PropertyPath, Stage, StageOptions,
    TokenId,
};
use layerstack_conformance::{pcp_txt::load_pcp_txt, usda_real::load_entry_usda, workspace_root};

fn spec_assets(area: &str) -> PathBuf {
    workspace_root()
        .join("core-spec-supplemental-release_dec2025")
        .join(area)
        .join("tests")
        .join("assets")
}

fn sorted_dirs(dir: &PathBuf) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("assets dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

#[derive(Default)]
struct Tally {
    offsets: usize,
    sites: usize,
    failures: Vec<String>,
}

fn check_file(label: &str, entry: &std::path::Path, tally: &mut Tally) {
    let mut loaded = load_entry_usda(entry);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let store = &mut loaded.store;
    let specs = || {
        store.layers.values().flat_map(|layer| {
            layer
                .prims
                .values()
                .chain(layer.variant_prims.values().flatten())
        })
    };
    let properties: BTreeSet<TokenId> = specs()
        .flat_map(|spec| spec.properties.iter().map(|entry| entry.name))
        .collect();
    let fields: BTreeSet<TokenId> = specs()
        .flat_map(|spec| spec.fields.iter().map(|entry| entry.name))
        .collect();
    let root = store
        .paths
        .lookup(&layerstack::Path::root())
        .expect("root path");
    let prims: Vec<PathId> = stage.traverse(root).collect();
    for prim in prims {
        let named = properties
            .iter()
            .map(|name| (*name, true))
            .chain(fields.iter().map(|name| (*name, false)));
        for (name, is_property) in named {
            let opinions = if is_property {
                stage.explain_property_path(PropertyPath::new(prim, name))
            } else {
                stage.explain_field(prim, name)
            };
            for opinion in opinions.unwrap_or_default() {
                check_opinion(label, &stage, store, prim, name, opinion, tally);
            }
        }
    }
}

/// Checks the target of one opinion's node and layer.
fn check_opinion(
    label: &str,
    stage: &Stage,
    store: &mut InMemoryStore,
    prim: PathId,
    name: TokenId,
    opinion: &Opinion,
    tally: &mut Tally,
) {
    let key = &opinion.key;
    let context = format!(
        "{label} {} {} from layer {} at {}",
        store.paths.display(prim, &store.tokens),
        store.tokens.resolve(name),
        key.layer_id.0,
        key.spec_path.display(&store.tokens)
    );
    let node = stage
        .explain_prim_graph(prim)
        .and_then(|graph| graph.node(key.node))
        .expect("opinion node");
    let stack = LayerStack::gather(&*store, node.layer_stack());
    if stack
        .layers
        .iter()
        .filter(|id| **id == key.layer_id)
        .count()
        != 1
    {
        // A layer repeated in its stack is not one target.
        return;
    }
    let Some(target) = EditTarget::for_node_layer(stage, &*store, prim, key.node, key.layer_id)
    else {
        tally.failures.push(format!("{context}: no target"));
        return;
    };
    tally.offsets += 1;
    if target.layer_offset() != opinion.layer_offset {
        tally.failures.push(format!(
            "{context}: target offset {:?}, opinion offset {:?}",
            target.layer_offset(),
            opinion.layer_offset
        ));
    }
    // Only opinions the node's own site authors, rather than ones copied
    // from an ancestor's or a class's namespace, are written through it.
    // Metadata opinions name their field the way properties do.
    let site = node.site().with_property(name);
    if key.spec_path.prim_path() != key.lookup_path || site != key.spec_path {
        return;
    }
    tally.sites += 1;
    let mapped = target.map_property_to_spec_path(PropertyPath::new(prim, name), &mut store.paths);
    if mapped.as_ref() != Some(&key.spec_path) {
        tally.failures.push(format!(
            "{context}: target maps to {:?}",
            mapped.map(|p| p.display(&store.tokens))
        ));
    }
}

#[test]
fn node_targets_write_where_their_opinions_come_from() {
    let mut tally = Tally::default();
    for dir in sorted_dirs(&spec_assets("composition")) {
        if !dir.join("pcp.txt").is_file() {
            continue;
        }
        let name = dir
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let oracle = load_pcp_txt(&dir.join("pcp.txt"));
        check_file(&name, &dir.join("usda").join(&oracle.entry), &mut tally);
    }
    for dir in sorted_dirs(&spec_assets("value_resolution")) {
        let name = dir
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let usda = dir.join("usda");
        let entry = ["entry.usda", "root.usda"]
            .iter()
            .map(|file| usda.join(file))
            .find(|path| path.is_file())
            .expect("entry layer");
        check_file(&name, &entry, &mut tally);
    }
    assert!(
        tally.failures.is_empty(),
        "{} of {} offsets and {} sites failed:\n{}",
        tally.failures.len(),
        tally.offsets,
        tally.sites,
        tally.failures.join("\n")
    );
    assert!(
        tally.offsets > 400 && tally.sites > 400,
        "{} offsets, {} sites",
        tally.offsets,
        tally.sites
    );
}
