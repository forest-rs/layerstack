// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for prospective writes to sites no spec is at yet.
//!
//! `fixtures/authoring_targets/oracle.json` records, for edit targets at a
//! reference node of a prim and properties of its descendants, whether the
//! descendant's OpenUSD 26.08 prim index holds a node at the target's site
//! and the value a resolve target "stronger than the edit target" finds
//! (`scripts/authoring_targets_oracle.py`, which also writes the layers).
//!
//! Composition culls a node whose site no layer of its stack has a spec at,
//! so the asset a prim references has no node where a write would go until
//! the write authors one. `Stage::inspect_property_authoring` must still
//! place the site with its strength, as OpenUSD places it in the prim's
//! index composed without culling: a write there is weaker than the
//! referencing layer's opinion (`/Model/Local`, `/Model/Deep/Leaf`), wins
//! where nothing stronger has one (`/Model/Bare`, `/Model/Deep`), wins over
//! a weaker reference (`/Pair/Spot`), and loses to a stronger one
//! (`/Pair/Spot2`).
//!
//! Spec: AOUSD Core §10.4 (strength ordering); OpenUSD
//! `UsdPrim::MakeResolveTargetStrongerThanEditTarget`,
//! `UsdPrim::ComputeExpandedPrimIndex`.

#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    ArcKind, AuthoringQuery, PropertyPath, Stage, StageOptions, Value, edit::EditTarget,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde::Deserialize;

const ORACLE: &str = include_str!("../fixtures/authoring_targets/oracle.json");

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    root: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    holder: String,
    source: String,
    layer: String,
    property: String,
    site: String,
    culled: bool,
    stronger_value: Option<f64>,
}

#[test]
fn writes_to_culled_sites_are_placed_with_their_strength() {
    let oracle: Oracle = serde_json::from_str(ORACLE).expect("oracle.json");
    assert_eq!(oracle.openusd_version, "26.8");
    let mut loaded = load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/authoring_targets")
            .join(&oracle.root),
    );
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    for case in &oracle.cases {
        let store = &mut loaded.store;
        let holder = store.path(&case.holder);
        let source = store.path(&case.source);
        let graph = stage.prim(holder, store).unwrap().composition_graph();
        let node = graph
            .nodes()
            .find(|(_, node)| {
                node.arc_kind() == ArcKind::References && node.site().prim_path() == source
            })
            .map(|(id, _)| id)
            .expect("reference node");
        let target = EditTarget::for_node(&stage, holder, node).expect("edit target");
        let name = &loaded.layer_names[&target.layer()];
        assert!(
            name.ends_with(&case.layer),
            "{}: target layer {name}",
            case.property
        );
        let property = store.property_path(&case.property);
        let inspection = stage
            .inspect_property_authoring(store, property, &target, AuthoringQuery::default_value())
            .unwrap();
        let context = &case.property;
        let site = inspection.target.as_ref().expect("the target is placed");
        assert_eq!(site.lookup_path, store.path(&case.site), "{context}");
        assert_eq!(inspection.target_culled, case.culled, "{context}");
        // A culled site's node is one of the expanded graph's.
        if case.culled {
            let expanded = stage
                .expanded_prim_graph(store, property.prim_path())
                .unwrap();
            let node = expanded.node(site.node).expect("expanded node");
            assert_eq!(node.site().prim_path(), site.lookup_path, "{context}");
        }
        let stronger = inspection.stronger_sources.first().map(|source| {
            let spec = store
                .layer(source.key.layer_id)
                .and_then(|layer| {
                    layer.property(PropertyPath::new(
                        source.key.spec_path.prim_path(),
                        property.property(),
                    ))
                })
                .expect("stronger spec");
            match spec.default {
                Some(Value::Double(value)) => value,
                ref other => panic!("{context}: stronger default {other:?}"),
            }
        });
        assert_eq!(stronger, case.stronger_value, "{context}");
        assert_eq!(
            inspection.is_strongest(),
            case.stronger_value.is_none(),
            "{context}"
        );
    }
}
