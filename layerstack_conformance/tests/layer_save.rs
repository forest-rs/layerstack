// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored-layer save over the preservation corpus, within the workspace.
//!
//! OpenUSD's reading of the saved files is checked by the `export_interop`
//! test when a Python with OpenUSD's `pxr` is available; this test needs no
//! external tool. For every case of `save_corpus::cases`:
//!
//! - the edited layer saves as USDA that this workspace reads as the hand
//!   written expected layer (both saved again give the same text);
//! - its USDC reads back as the same layer as its USDA;
//! - saving is stable: the saved USDA, imported and saved again, is
//!   unchanged.
//!
//! A case about composition, saved in either format and composed with its
//! assets, composes as its expected layer does.
//!
//! Every `save_corpus::unsupported_cases` encoding is rejected by both
//! formats with the same error, naming the source path.

use layerstack::doc::{Layer, LayerId, SublayerEntry};
use layerstack::{InMemoryStore, Stage, StageOptions};
use layerstack_conformance::save_corpus::{
    AnyAsset, Imported, MISSING, Root, cases, composed, unsupported_cases,
};
use layerstack_usdc::writer::UsdcWriteError;

/// Emits USDA text into `store` as layer `id`, sharing its interners.
fn emit_into(store: &mut InMemoryStore, id: u64, text: &str) {
    let parsed = layerstack_usda::parser::parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(id),
        &mut store.tokens,
        &mut store.paths,
        &mut AnyAsset::default(),
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    store.insert_layer(result.layer);
}

/// Saved explicit-empty target and connection lists still block a weaker
/// layer's opinions when composed, from both formats; a bare declaration
/// does not, and a deleted target is removed from the weaker list.
///
/// Spec: AOUSD Core §12.4 (targets and connections compose as list ops; an
/// explicit list replaces weaker opinions).
#[test]
fn saved_explicit_empty_lists_block_weaker_opinions() {
    let case = cases()
        .into_iter()
        .find(|c| c.name == "explicit_empty_lists")
        .expect("case");
    let mut layer = Imported::usda(case.source);
    (case.edit)(&mut layer);
    let from_usda = layer.save_usda().unwrap();
    let from_usdc = Imported::usdc(&layer.save_usdc().unwrap())
        .save_usda()
        .unwrap();
    for (format, saved) in [("USDA", from_usda), ("USDC", from_usdc)] {
        let mut store = InMemoryStore::default();
        emit_into(&mut store, 1, &saved);
        emit_into(&mut store, 2, case.weaker.expect("weaker layer"));
        let mut root = Layer::new(LayerId(3));
        root.sublayers = vec![
            SublayerEntry::new(LayerId(1)),
            SublayerEntry::new(LayerId(2)),
        ];
        store.insert_layer(root);
        let properties = [
            "/A.declared",
            "/A.blocked",
            "/A.emptied",
            "/A.blockedInput",
            "/A.emptiedInput",
            "/A.pruned",
        ]
        .map(|p| store.property_path(p));
        let stage = Stage::compose(&mut store, LayerId(3), StageOptions::default());
        let targets: Vec<Vec<String>> = properties
            .iter()
            .map(|p| {
                stage
                    .resolve_target_list_path(*p)
                    .map(|r| r.value)
                    .unwrap_or_default()
                    .iter()
                    .map(|t| t.display(&store.paths, &store.tokens))
                    .collect()
            })
            .collect();
        let none: Vec<String> = Vec::new();
        assert_eq!(
            targets,
            [
                vec!["/Elsewhere".to_string()],
                none.clone(),
                none.clone(),
                none.clone(),
                none,
                vec!["/Elsewhere/Kept".to_string()],
            ],
            "{format}: composed targets and connections"
        );
    }
}

#[test]
fn corpus_saves_as_expected_in_both_formats() {
    for case in cases() {
        let name = case.name;
        let mut layer = Imported::usda(case.source);
        (case.edit)(&mut layer);
        let usda = layer.save_usda().unwrap_or_else(|e| panic!("{name}: {e}"));
        let expected = Imported::usda(case.expected)
            .save_usda()
            .unwrap_or_else(|e| panic!("{name} expected: {e}"));
        assert_eq!(usda, expected, "{name}: saved USDA");

        let usdc = layer.save_usdc().unwrap_or_else(|e| panic!("{name}: {e}"));
        let from_usdc = Imported::usdc(&usdc).save_usda().unwrap();
        assert_eq!(from_usdc, usda, "{name}: USDC reads back as the USDA");

        let again = Imported::usda(&usda).save_usda().unwrap();
        assert_eq!(again, usda, "{name}: stable");
    }
}

/// Spec: AOUSD Core §10.3 (composition arcs), §12.3.2.1 (layer offsets).
#[test]
fn saved_arcs_compose_as_expected() {
    for case in cases() {
        let Some(assets) = case.composition else {
            continue;
        };
        let name = case.name;
        let want = composed(Root::Usda(case.expected), assets);
        let mut layer = Imported::usda(case.source);
        (case.edit)(&mut layer);
        let usda = layer.save_usda().unwrap();
        assert_eq!(
            composed(Root::Usda(&usda), assets),
            want,
            "{name}: saved USDA"
        );
        let usdc = layer.save_usdc().unwrap();
        assert_eq!(
            composed(Root::Usdc(&usdc), assets),
            want,
            "{name}: saved USDC"
        );
        let unedited = composed(Root::Usda(case.source), assets);
        assert_ne!(unedited, want, "{name}: the edit changes what composes");
    }
}

/// An arc whose asset does not resolve is imported as an unresolved arc
/// from either format, and saved with its authored asset path, prim path
/// and offset.
#[test]
fn unresolved_arcs_are_kept_as_authored() {
    let case = cases()
        .into_iter()
        .find(|c| c.name == "unresolved_arcs")
        .expect("case");
    let from_usda = Imported::usda(case.source);
    let from_usdc = Imported::usdc(&from_usda.save_usdc().unwrap());
    for (format, layer) in [("USDA", from_usda), ("USDC", from_usdc)] {
        let sublayers: Vec<_> = layer
            .layer
            .sublayers
            .iter()
            .filter(|s| s.is_unresolved())
            .map(|s| (s.asset.clone(), s.offset.offset))
            .collect();
        assert_eq!(
            sublayers,
            [(Some(format!("{MISSING}notes.usda")), 5.0)],
            "{format}: unresolved sublayer"
        );
        let references: Vec<_> = layer
            .layer
            .prims
            .values()
            .flat_map(|p| &p.references.prepend)
            .filter(|r| r.is_unresolved())
            .map(|r| (r.asset.clone(), r.layer_offset.offset))
            .collect();
        assert_eq!(
            references,
            [(Some(format!("{MISSING}anchor.usda")), 2.0)],
            "{format}: unresolved reference"
        );
        let saved = layer.save_usda().unwrap();
        assert!(
            saved.contains("@./missing/notes.usda@ (offset = 5)")
                && saved.contains("@./missing/anchor.usda@</Anchor> (offset = 2)"),
            "{format}: saved as authored\n{saved}"
        );
    }
}

#[test]
fn corpus_edits_change_what_they_edit() {
    for case in cases() {
        let unedited = Imported::usda(case.source).save_usda().unwrap();
        let expected = Imported::usda(case.expected).save_usda().unwrap();
        assert_ne!(unedited, expected, "{}: the edit is visible", case.name);
    }
}

#[test]
fn unsupported_encodings_are_rejected_before_output() {
    for (name, source, error) in unsupported_cases() {
        let layer = Imported::usda(source);
        assert_eq!(layer.save_usda(), Err(error.clone()), "{name}: USDA");
        assert_eq!(
            layer.save_usdc(),
            Err(UsdcWriteError::Save(error)),
            "{name}: USDC"
        );
    }
}
