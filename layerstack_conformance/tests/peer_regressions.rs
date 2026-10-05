// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Native OpenUSD 26.8 expectations for composition regressions identified in
//! the peer's October 2026 changes. Fixtures keep introduced sites separate from
//! the stage occurrences that compose them. AOUSD Core §10.3.2, §10.6.

use layerstack::{CompositionError, Layer, LayerId, LiveStage, SpecPath, Stage, StageOptions};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use std::collections::BTreeSet;

fn fixture(name: &str) -> std::path::PathBuf {
    workspace_root()
        .join("layerstack_conformance/fixtures/peer_regressions")
        .join(name)
}

#[test]
fn missing_internal_targets_retain_introducing_sites_and_selected_variants_resolve() {
    let mut loaded = load_entry_usda(&fixture("variants.usda"));
    assert!(
        loaded.invalid.is_empty(),
        "fixture loads without parser errors"
    );
    let session = LayerId(1_000);
    loaded.store.insert_layer(Layer::new(session));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions {
            session_layer: Some(session),
            ..Default::default()
        },
    );
    let mut found = BTreeSet::new();
    for error in stage.composition_errors() {
        let CompositionError::UnresolvedPrimPath(error) = error else {
            panic!("unexpected diagnostic: {error:?}");
        };
        assert_eq!(
            error.layer, loaded.root_layer,
            "internal target identity is the root, excluding the session"
        );
        assert_eq!(
            error.introducing_layer,
            Some(loaded.root_layer),
            "the authored layer remains explicit"
        );
        let name = loaded.store.paths.display(error.prim, &loaded.store.tokens);
        let spec = error.introducing_spec.display(&loaded.store.tokens);
        found.insert((name, spec));
    }
    assert_eq!(
        found,
        BTreeSet::from([
            ("/RootMissing".into(), "/RootMissing".into()),
            ("/SubMissing".into(), "/SubMissing".into()),
            ("/PayloadMissing".into(), "/PayloadMissing".into()),
            ("/Outer/ToAbsent".into(), "/Outer/ToAbsent".into()),
            ("/NestedMissing".into(), "/Outer/ToAbsent".into()),
        ]),
        "nested diagnostics identify the original authored source"
    );
    for name in ["/FromVariantRef", "/NestedVariantRef", "/SubPresent"] {
        let path = loaded.store.path(name);
        assert!(stage.has_prim(path), "selected target {name} composes");
    }
}

#[test]
fn missing_external_root_and_subroot_targets_name_the_target_and_introducing_layers() {
    let mut loaded = load_entry_usda(&fixture("external.usda"));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert_eq!(
        stage.composition_errors().len(),
        4,
        "one diagnostic per unresolved reference/payload"
    );
    for diagnostic in stage.composition_errors() {
        let CompositionError::UnresolvedPrimPath(error) = diagnostic else {
            panic!("unexpected diagnostic: {diagnostic:?}");
        };
        assert!(
            loaded.layer_names[&error.layer].ends_with("content.usda"),
            "target identity names the external asset"
        );
        assert_eq!(
            error.introducing_layer,
            Some(loaded.root_layer),
            "root layer authors the arc"
        );
        assert_eq!(
            error.introducing_spec,
            SpecPath::from_prim_path(error.prim, &loaded.store.paths),
            "direct arc keeps its authored spec"
        );
    }
}

#[test]
fn an_explicit_identity_offset_deletes_the_omitted_identity_payload() {
    let mut loaded = load_entry_usda(&fixture("payload.usda"));
    let mut stage = LiveStage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let root = loaded.store.path("/Root");
    let child = loaded.store.path("/Root/Child");
    assert!(
        stage.stage().has_prim(root),
        "the locally defined root remains"
    );
    assert!(
        !stage.stage().has_prim(child),
        "identity payload deletion removes its contribution"
    );
    assert!(
        stage.stage().composition_errors().is_empty(),
        "deleting the payload is valid"
    );
    // Empty pseudo-root attribute queries are safe through explicit controls.
    let pseudo_root = loaded.store.path("/");
    for name in ["purpose", "visibility"] {
        let name = loaded.store.tokens.intern(name);
        let mut query =
            layerstack::AttributeQuery::new(layerstack::PropertyPath::new(pseudo_root, name));
        assert!(
            query
                .try_get(stage.stage(), layerstack::Time::Default)
                .unwrap()
                .is_none(),
            "pseudo-root has no imageable attributes"
        );
    }
    let weak = *loaded
        .layer_names
        .iter()
        .find(|(_, name)| name.ends_with("weak.usda"))
        .unwrap()
        .0;
    stage.mute_layer(weak).unwrap();
    stage.synchronize(&mut loaded.store);
    stage.unmute_layer(weak);
    stage.synchronize(&mut loaded.store);
    assert!(
        !stage.stage().has_prim(child),
        "mute/unmute retains the identity deletion without a cache panic"
    );
}

#[test]
fn nested_variant_arc_errors_keep_the_variant_qualified_authored_spec() {
    let mut loaded = load_entry_usda(&fixture("variant_source.usda"));
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert_eq!(
        stage.composition_errors().len(),
        2,
        "authored and referenced occurrences report the missing arc"
    );
    for diagnostic in stage.composition_errors() {
        let CompositionError::UnresolvedPrimPath(error) = diagnostic else {
            panic!("unexpected diagnostic: {diagnostic:?}");
        };
        assert_eq!(
            error.introducing_layer,
            Some(loaded.root_layer),
            "root authors the variant arc"
        );
        assert_eq!(
            error.introducing_spec.display(&loaded.store.tokens),
            "/Template{shape=on}Part",
            "source evidence preserves variant selection and authored namespace"
        );
    }
}
