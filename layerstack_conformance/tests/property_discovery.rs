// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Attribute presence and delimiter-aware namespace discovery.
#![allow(missing_docs, reason = "integration tests")]
use layerstack_schemas::{PrimView, Scene};
#[path = "support/schema_scene.rs"]
mod support;
#[test]
fn presence_distinguishes_fallbacks_animation_blocks_and_authored_specs() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
        def Mesh "M" {
            float primvars:x = 1 (interpolation = "vertex")
            float primvars:x:nested = 2
            float primvars:xy = 3
            float primvars:blocked = None
            float primvars:empty
            float primvars:anim.timeSamples = {1: 1, 2: 2}
            rel primvars:x:relationship = </M>
        }
    "#,
    );
    let path = store.path("/M");
    let prim = PrimView::new(Scene::new(live.stage(), &store), path);
    assert!(prim.has_attribute("subdivisionScheme"));
    assert!(prim.has_value("subdivisionScheme"));
    assert!(!prim.has_authored_value("subdivisionScheme"));
    assert!(prim.has_value("primvars:anim"));
    assert!(prim.has_authored_value("primvars:anim"));
    assert!(!prim.has_value("primvars:blocked"));
    assert!(!prim.has_value("primvars:empty"));
    assert!(!prim.has_attribute("primvars:x:relationship"));
    assert!(!prim.has_value("missing"));
    let names = |properties: Vec<layerstack::PropertyPath>| {
        properties
            .into_iter()
            .map(|p| store.tokens.resolve(p.property()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(prim.attributes_in_namespace("primvars:x")),
        vec!["primvars:x", "primvars:x:nested"]
    );
    assert_eq!(
        names(prim.attributes_in_namespace("primvars:x:")),
        names(prim.attributes_in_namespace("primvars:x"))
    );
    assert!(prim.attributes_in_namespace("primvar").is_empty());
    assert_eq!(
        names(prim.attributes_in_namespace("subdivisionScheme")),
        vec!["subdivisionScheme"]
    );
    assert!(
        prim.authored_attributes_in_namespace("subdivisionScheme")
            .is_empty()
    );
    let authored = prim.property_metadata("primvars:x").unwrap();
    assert!(authored.has_authored("interpolation"));
    assert!(authored.has_authored("typeName"));
    assert!(authored.has_authored("variability"));
    assert!(authored.has_authored("default"));
    assert!(!authored.has_authored("elementSize"));
    assert!(
        !prim
            .property_metadata("subdivisionScheme")
            .unwrap()
            .has_authored("interpolation")
    );
}
