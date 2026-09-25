// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for the arc path of each source in a prim's graph.
//!
//! `fixtures/prim_index_graph/oracle.json` records what OpenUSD 26.08
//! composes from `fixtures/prim_index_graph/root.usda`
//! (`scripts/prim_index_graph_oracle.py`, which also writes those layers):
//! for every prim, each source of its prim stack with the arc path of the
//! node that provides it, from the root node down. Layerstack must compose
//! the same sources, and each source must name a node of
//! [`Stage::explain_prim_graph`] with the same arc path.
//!
//! The fixture authors variant branches where they change the arc path: a
//! variant set nested in another branch, references and payloads authored on
//! a branch, a child authored only inside an ancestor's branch, and such a
//! child reached through inherits, specializes, a reference and a payload.
//! Each branch is a node beneath the node whose site hosts its variant set,
//! and what the branch authors sits beneath the branch's node.
//!
//! Spec: AOUSD Core §10.3.2.5 (variants), §10.4 (an arc's target ranks
//! beneath the site that authors it). OpenUSD: `PcpPrimIndex::GetGraph()`;
//! `_EvalNodeVariantSets` and `_AddArc` in `pxr/usd/pcp/primIndex.cpp`.

#![allow(missing_docs, reason = "integration tests")]

use layerstack::{NodeId, PrimIndexGraph, SpecPath, Stage, StageOptions};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/prim_index_graph/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    prims: Vec<Prim>,
}

#[derive(Deserialize)]
struct Prim {
    path: String,
    sources: Vec<Source>,
}

/// One source of a prim stack and the arc path of its node.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Source {
    layer: String,
    site: String,
    arc_path: Vec<(String, String)>,
}

fn oracle() -> Oracle {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert!(
        oracle.openusd_version.starts_with("0.26."),
        "oracle from OpenUSD {}",
        oracle.openusd_version
    );
    oracle
}

fn compose(oracle: &Oracle) -> (LoadedStage, Stage) {
    let mut loaded = load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/prim_index_graph")
            .join(&oracle.root),
    );
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );
    (loaded, stage)
}

/// A layer's file name without its directory.
fn layer_name(loaded: &LoadedStage, layer: layerstack::LayerId) -> String {
    let name = &loaded.layer_names[&layer];
    name.rsplit('/').next().unwrap_or(name).to_string()
}

fn display(loaded: &LoadedStage, site: &SpecPath) -> String {
    site.display(&loaded.store.tokens)
}

/// `(arc, site)` pairs from the graph's root down to `node`.
fn arc_path(loaded: &LoadedStage, graph: &PrimIndexGraph, node: NodeId) -> Vec<(String, String)> {
    let mut path = Vec::new();
    let mut cursor = Some(node);
    while let Some(id) = cursor {
        let node = graph.node(id).expect("node of the graph");
        path.push((
            format!("{:?}", node.arc_kind()),
            display(loaded, node.site()),
        ));
        cursor = node.parent();
    }
    path.reverse();
    path
}

#[test]
fn sources_name_the_nodes_openusd_provides_them_from() {
    let oracle = oracle();
    let (mut loaded, stage) = compose(&oracle);
    let mut mismatches = Vec::new();
    for prim in &oracle.prims {
        let id = loaded.store.path(&prim.path);
        let graph = stage.explain_prim_graph(id).expect("composed prim");
        let mut sources: Vec<Source> = Vec::new();
        for key in stage.explain_prim(id).expect("composed prim") {
            let source = Source {
                layer: layer_name(&loaded, key.layer_id),
                site: display(&loaded, &key.spec_path),
                arc_path: arc_path(&loaded, graph, key.node),
            };
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
        if sources != prim.sources {
            mismatches.push(format!(
                "{}\n    expected {:?}\n    actual   {sources:?}",
                prim.path, prim.sources
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "sources differ from OpenUSD's prim index:\n{}",
        mismatches.join("\n")
    );
}
