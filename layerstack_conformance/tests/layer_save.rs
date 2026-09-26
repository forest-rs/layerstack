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
//!   unchanged;
//! - both saved files read back as the saved layer, spec by spec.
//!
//! A case about composition, saved in either format and composed with its
//! assets, composes as its expected layer does.
//!
//! Every `save_corpus::unsupported_cases` encoding is rejected by both
//! formats with the same error, naming the source path.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use layerstack::doc::{Layer, LayerId, PrimSpec, Reference, SublayerEntry, VariantSpec};
use layerstack::listop::ListOp;
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

/// A `delete` in `variantSets` is kept apart from the declared sets, and
/// saves and reads back from both formats.
///
/// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops).
#[test]
fn deleted_variant_sets_round_trip() {
    const SOURCE: &str = r#"#usda 1.0

over "Loam" (
    delete variantSets = "grain"
)
{
}

def "Clay" (
    prepend variantSets = "grain"
)
{
}
"#;
    let mut layer = Imported::usda(SOURCE);
    let grain = layer.tokens.intern("grain");
    let mut path = |text: &str| {
        let parsed = layerstack::Path::parse_absolute(text, &mut layer.tokens).expect("prim path");
        layer.paths.intern(parsed)
    };
    let (loam, clay) = (path("/Loam"), path("/Clay"));
    let spec = |layer: &Layer, path| layer.prims.get(&path).expect("prim").clone();
    assert_eq!(spec(&layer.layer, loam).deleted_variant_sets, [grain]);
    assert!(spec(&layer.layer, loam).variant_set_order.is_empty());
    assert_eq!(spec(&layer.layer, clay).variant_set_order, [grain]);
    let want = structure(&layer.layer);
    let usda = layer.save_usda().unwrap();
    assert!(usda.contains("delete variantSets = [\"grain\"]"), "{usda}");
    let usdc = layer.save_usdc().unwrap();
    let from_usda = reimport_usda(&mut layer, &usda);
    assert_eq!(structure(&from_usda), want, "layer → USDA → layer");
    let from_usdc = reimport_usdc(&mut layer, &usdc);
    assert_eq!(structure(&from_usdc), want, "layer → USDC → layer");
}

#[test]
fn corpus_edits_change_what_they_edit() {
    for case in cases() {
        let unedited = Imported::usda(case.source).save_usda().unwrap();
        let expected = Imported::usda(case.expected).save_usda().unwrap();
        assert_ne!(unedited, expected, "{}: the edit is visible", case.name);
    }
}

/// Imports USDA text into `layer`'s interners, so token and path ids
/// compare with its own.
fn reimport_usda(layer: &mut Imported, text: &str) -> Layer {
    let parsed = layerstack_usda::parser::parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut layer.tokens,
        &mut layer.paths,
        &mut AnyAsset::default(),
    );
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    result.layer
}

/// Imports a USDC file into `layer`'s interners.
fn reimport_usdc(layer: &mut Imported, bytes: &[u8]) -> Layer {
    let result = layerstack_usdc::read_usdc(
        bytes,
        LayerId(1),
        &mut layer.tokens,
        &mut layer.paths,
        &mut AnyAsset::default(),
    )
    .expect("USDC reads");
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    result.layer
}

/// An arc list op by what was authored: the asset path, target and
/// offset of each arc, not the layer id its resolution assigned.
fn arc_list(op: &ListOp<Reference>) -> String {
    let show = |items: &[Reference]| -> Vec<String> {
        items
            .iter()
            .map(|r| {
                format!(
                    "{:?} {:?} {:?} unresolved={}",
                    r.asset,
                    r.target,
                    r.layer_offset,
                    r.is_unresolved()
                )
            })
            .collect()
    };
    format!(
        "explicit {:?} delete {:?} prepend {:?} append {:?}",
        op.explicit.as_deref().map(show),
        show(&op.delete),
        show(&op.prepend),
        show(&op.append)
    )
}

/// A variant spec's content, with its selections sorted.
fn variant_structure(variant: &VariantSpec) -> String {
    let VariantSpec {
        fields,
        properties,
        authored_children,
        references,
        inherits,
        specializes,
        payloads,
        variant_selections,
        // Walked by `PrimSpec::variant_branches`.
        variant_sets: _,
        variant_set_order,
        property_order,
    } = variant;
    let selections: BTreeMap<_, _> = variant_selections.iter().collect();
    format!(
        "fields {fields:?}\n      properties {properties:?}\n      children \
         {authored_children:?}\n      references {}\n      payloads {}\n      inherits \
         {inherits:?}\n      specializes {specializes:?}\n      selections {selections:?}\n      \
         variant sets {variant_set_order:?}\n      property order {property_order:?}",
        arc_list(references),
        arc_list(payloads),
    )
}

/// A layer's authored content as canonical text, for layers whose ids come
/// from the same interners: every prim spec by path and branch context,
/// each field in its authored order, maps sorted, and arcs by what was
/// authored.
fn structure(layer: &Layer) -> String {
    let Layer {
        id: _,
        sublayers,
        default_prim,
        metadata,
        prims,
        variant_prims,
        relocates,
        // The edit counters are not content.
        ..
    } = layer;
    let sublayers: Vec<_> = sublayers
        .iter()
        .map(|s| (s.asset.clone(), s.offset, s.is_unresolved()))
        .collect();
    let mut out = format!(
        "sublayers {sublayers:?}\ndefault prim {default_prim:?}\nmetadata {metadata:?}\n\
         relocates {relocates:?}\n"
    );
    let specs = prims.iter().chain(
        variant_prims
            .iter()
            .flat_map(|(path, specs)| specs.iter().map(move |spec| (path, spec))),
    );
    let mut sorted = BTreeMap::new();
    for (path, spec) in specs {
        let PrimSpec {
            specifier,
            type_name,
            fields,
            properties,
            property_order,
            outer_variant_sites,
            authored_children,
            variant_selections,
            // Walked by `PrimSpec::variant_branches` below.
            variant_sets: _,
            variant_set_order,
            deleted_variant_sets,
            references,
            inherits,
            specializes,
            payloads,
            prim_order,
            instanceable,
            active,
        } = spec;
        let selections: BTreeMap<_, _> = variant_selections.iter().collect();
        let mut text = format!(
            "  {specifier:?} {type_name:?}\n  fields {fields:?}\n  properties {properties:?}\n  \
             property order {property_order:?}\n  children {authored_children:?}\n  prim order \
             {prim_order:?}\n  selections {selections:?}\n  variant sets {variant_set_order:?} deleted {deleted_variant_sets:?}\n  \
             references {}\n  payloads {}\n  inherits {inherits:?}\n  specializes \
             {specializes:?}\n  instanceable {instanceable:?} active {active:?}\n",
            arc_list(references),
            arc_list(payloads),
        );
        // Every variant spec by its path on the prim spec, nested ones
        // included.
        let variants: BTreeMap<Vec<_>, _> = spec
            .variant_branches()
            .map(|branch| (branch.chain().collect(), branch.spec))
            .collect();
        for (chain, variant) in variants {
            let _ = writeln!(
                text,
                "  variant {chain:?}\n      {}",
                variant_structure(variant)
            );
        }
        sorted.insert((*path, outer_variant_sites.clone()), text);
    }
    for ((path, sites), text) in sorted {
        let _ = write!(out, "prim {path:?} in {sites:?}\n{text}");
    }
    out
}

/// Every corpus layer, edited, saves as USDA and as USDC that read back as
/// the same layer, compared spec by spec: variant sets, branch prim specs
/// and nested sets included.
///
/// Spec: AOUSD Core §7 (scene description), §16.2 (USDA), §16.3 (crate).
#[test]
fn saved_layers_read_back_as_the_layer() {
    for case in cases() {
        let name = case.name;
        let mut layer = Imported::usda(case.source);
        (case.edit)(&mut layer);
        let want = structure(&layer.layer);
        let usda = layer.save_usda().unwrap();
        let usdc = layer.save_usdc().unwrap();
        let from_usda = reimport_usda(&mut layer, &usda);
        assert_eq!(structure(&from_usda), want, "{name}: layer → USDA → layer");
        let from_usdc = reimport_usdc(&mut layer, &usdc);
        assert_eq!(structure(&from_usdc), want, "{name}: layer → USDC → layer");
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
