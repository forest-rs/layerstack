// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Target explanations say which authored paths composition dropped, and
//! why.
//!
//! In `fixtures/target_explanations`, `/Place` and `/Override` reference
//! `lib.usda`'s `/Lib/Item`, whose `r` targets `</Lib/Item/Part>` and
//! `</Lib/Other>`, and whose `x` connects to `</Lib/Other.y>`. The
//! reference maps only `/Lib/Item` and what lies beneath it, so OpenUSD
//! 26.08 composes `/Place.r` as `[/Place/Part]` and `/Place.x` with no
//! connections, without saying why. `/Override` authors its own explicit
//! `r = </Place>`, which replaces the referenced opinion.
//!
//! Pcp's target index (`Pcp.Cache.ComputeRelationshipTargetPaths` and
//! `ComputeAttributeConnectionPaths`, outside USD mode) reports an
//! `ErrorInvalidExternalTargetPath` for `/Place.r`, `/Place.x` and
//! `/Override.x`, and none for `/Override.r`: the replaced opinion's paths
//! are never checked. `Stage::composition_errors` reports the same three.
//!
//! Spec: AOUSD Core §10.3.2, §12.4.

use layerstack::{ArcKind, DropReason, PropertyPath, Stage, StageOptions, TargetPath};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};

#[test]
fn dropped_targets_are_explained_with_their_opinion() {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/target_explanations/scene.usda");
    let mut loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let store = &mut loaded.store;
    let (place, part) = (store.path("/Place"), store.path("/Place/Part"));
    let (lib_other, lib_part) = (store.path("/Lib/Other"), store.path("/Lib/Item/Part"));
    let other_y = store.property_path("/Lib/Other.y");
    let item = store.path("/Lib/Item");

    let errors: Vec<_> = stage
        .composition_errors()
        .iter()
        .filter_map(|e| match e {
            layerstack::CompositionError::InvalidExternalTargetPath(e) => {
                Some(PropertyPath::new(e.prim, e.property).display(store.paths(), store.tokens()))
            }
            _ => None,
        })
        .collect();
    let mut errors = errors;
    errors.sort();
    assert_eq!(errors, ["/Override.x", "/Place.r", "/Place.x"]);

    let prim = stage.prim(place, &*store).unwrap();
    let targets = prim.relationship("r").unwrap().explain_targets().unwrap();
    assert_eq!(targets.targets, vec![TargetPath::Prim(part)]);
    let [opinion] = targets.opinions.as_slice() else {
        panic!("one opinion: {targets:?}")
    };
    assert_eq!(opinion.arc, Some(ArcKind::References));
    assert_eq!(opinion.spec.prim_path(), item);
    assert!(opinion.contributes);
    assert_eq!(
        opinion.authored.explicit,
        Some(vec![
            TargetPath::Prim(lib_part),
            TargetPath::Prim(lib_other)
        ])
    );
    assert_eq!(
        opinion.kept,
        vec![(TargetPath::Prim(lib_part), TargetPath::Prim(part))]
    );
    assert_eq!(opinion.dropped.len(), 1);
    assert_eq!(opinion.dropped[0].authored, TargetPath::Prim(lib_other));
    assert_eq!(
        opinion.dropped[0].reason,
        DropReason::OutsideArc(ArcKind::References)
    );

    let connections = prim.attribute("x").unwrap().explain_connections().unwrap();
    assert!(connections.targets.is_empty());
    assert_eq!(
        connections.opinions[0].dropped[0].authored,
        TargetPath::Property(other_y)
    );

    // `/Override`'s own explicit list replaces the referenced opinion,
    // whose paths are then never mapped.
    let over = store.path("/Override");
    let targets = stage
        .prim(over, &*store)
        .unwrap()
        .relationship("r")
        .unwrap()
        .explain_targets()
        .unwrap();
    assert_eq!(targets.targets, vec![TargetPath::Prim(place)]);
    let [local, referenced] = targets.opinions.as_slice() else {
        panic!("two opinions: {targets:?}")
    };
    assert_eq!(local.arc, None);
    assert!(local.contributes);
    assert!(!referenced.contributes);
    assert!(referenced.kept.is_empty() && referenced.dropped.is_empty());
    assert_eq!(referenced.authored.explicit.as_ref().map(Vec::len), Some(2));
    // Its connection has no stronger opinion, so the drop is reported.
    let connections = stage
        .prim(over, &*store)
        .unwrap()
        .attribute("x")
        .unwrap()
        .explain_connections()
        .unwrap();
    assert_eq!(
        connections.opinions[0].dropped[0].reason,
        DropReason::OutsideArc(ArcKind::References)
    );
}
