// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A sample set through an edit target of a variant in an offset sublayer
//! lands where C++ OpenUSD 26.11 writes it, call by call.
//!
//! `scripts/variant_target_offsets_oracle.py` opens
//! `fixtures/variant_target_offsets/scene.usda`, whose sublayer `sub.usda`
//! is authored with `(offset = 10)` and holds `/Robot`'s variant branches,
//! and sets `/Robot.spin` at stage time 15 through one edit target per
//! case. `UsdEditTarget::ForLocalDirectVariant` has no time offset, so the
//! sample lands at layer time 15 (read at 25); the variant node's target
//! and `GetEditTargetForLocalLayer` carry the sublayer's offset and write
//! layer time 5. Here each call is a `Transaction` through the matching
//! layerstack target: [`EditTarget::for_local_variant`],
//! [`EditTarget::for_node_layer`] of the variant node and
//! [`EditTarget::for_local_layer`].
//!
//! Spec: AOUSD Core §10.3.1.1 (sublayer offsets), §12.3.2.1 (layer
//! offsets). OpenUSD: `pxr/usd/usd/editTarget.cpp`.

use layerstack::{
    ArcKind, EditTarget, LiveStage, PropertySpec, PropertyType, SpecPath, StageOptions,
    Transaction, Value,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/variant_target_offsets/oracle.json");

#[test]
fn variant_targets_in_an_offset_sublayer_write_where_openusd_does() {
    let oracle: Json = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 4);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let path = workspace_root()
            .join("layerstack_conformance/fixtures/variant_target_offsets/scene.usda");
        let mut loaded = load_entry_usda(&path);
        assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
        let sub = *loaded
            .layer_names
            .iter()
            .find(|(_, n)| n.ends_with("sub.usda"))
            .unwrap()
            .0;
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let robot = loaded.store.path("/Robot");
        let target = match case["variant"].as_str() {
            Some(variant) => {
                let variant =
                    SpecPath::parse(variant, loaded.store.tokens(), loaded.store.paths()).unwrap();
                EditTarget::for_local_variant(sub, &variant)
            }
            None if name == "local_layer" => {
                EditTarget::for_local_layer(live.stage(), &loaded.store, sub).unwrap()
            }
            None => {
                let graph = live
                    .stage()
                    .prim(robot, &loaded.store)
                    .unwrap()
                    .composition_graph();
                let node = graph
                    .nodes()
                    .find(|(_, node)| node.arc_kind() == ArcKind::Variants)
                    .unwrap()
                    .0;
                EditTarget::for_node_layer(live.stage(), &loaded.store, robot, node, sub).unwrap()
            }
        };
        let spin = loaded.store.property_path("/Robot.spin");
        let spec_path = target
            .map_property_to_spec_path(spin, loaded.store.paths())
            .unwrap();
        assert_eq!(
            spec_path.display(loaded.store.paths(), loaded.store.tokens()),
            case["spec_path"].as_str().unwrap(),
            "{name}: spec path"
        );
        let mut txn = Transaction::new();
        txn.create_property(
            target.property(spin),
            PropertySpec::typed_attribute(PropertyType::parse("double").unwrap()),
        )
        .set_time_sample(target.property(spin), 15.0, Value::Double(70.0));
        let accepted = live.apply(&mut loaded.store, &txn).is_ok();
        assert_eq!(
            accepted,
            case["accepted"].as_bool().unwrap(),
            "{name}: accepted"
        );
        let layer_times: Vec<f64> = loaded
            .store
            .layer(sub)
            .unwrap()
            .property_at(&spec_path)
            .and_then(|spec| {
                spec.time_samples
                    .get()
                    .map(|samples| samples.iter().map(|(t, _)| *t).collect())
            })
            .unwrap_or_default();
        let want = |key: &str| -> Vec<f64> {
            case[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_f64().unwrap())
                .collect()
        };
        assert_eq!(layer_times, want("layer_times"), "{name}: layer times");
        let stage_times = live
            .stage()
            .prim(robot, &loaded.store)
            .and_then(|prim| prim.attribute("spin"))
            .map(|attribute| attribute.sample_times())
            .unwrap_or_default();
        assert_eq!(stage_times, want("stage_times"), "{name}: stage times");
    }
}
