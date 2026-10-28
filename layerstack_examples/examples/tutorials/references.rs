// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! References remap one authored asset into independent stage occurrences.
//! <https://openusd.org/release/tut_referencing_layers.html>
mod support;
use layerstack::{
    EditTarget, LayerOffset, ListPosition, Reference, ReferenceTarget, Specifier, Transaction,
};
use layerstack_schemas::{SchemaEdit, usd_geom::SphereEdit};
fn main() {
    let directory = support::directory();
    support::hello(&directory);
    let mut document = support::create(&directory, "RefExample.usda");
    let root = document.root_layer();
    let at = EditTarget::for_layer(root);
    let store = document.store_mut();
    let first = store.path("/refSphere");
    let second = store.path("/refSphere2");
    // Two `over` prims, each referencing the asset's default prim, as
    // `UsdStage::OverridePrim` and `UsdReferences::AddReference` author
    // them. The reference names the asset as authored, anchored to the root
    // layer; the document loads it when the edit asks for it.
    let mut arcs = Transaction::new();
    for path in [first, second] {
        arcs.create_prim(at.prim(path), Specifier::Over, None);
        let hello = Reference::deferred(
            root,
            "./HelloWorld.usda",
            ReferenceTarget::DefaultPrim,
            LayerOffset::IDENTITY,
        );
        arcs.add_reference(at.prim(path), hello, ListPosition::BackOfPrepend);
    }
    let (applied, loaded) = document.apply(&arcs).unwrap();
    assert_eq!(applied.asset_requests.len(), 2, "each reference asks");
    assert_eq!(loaded.layers.len(), 1, "for the one asset, now loaded");
    // Override the color of one occurrence: the referenced sphere is a
    // `Sphere` on the stage, so its typed edit handle is had by path.
    let (store, live) = document.parts_mut();
    let red = store.path("/refSphere2/world");
    let mut edit = SchemaEdit::new(live.stage(), store, at);
    SphereEdit::new(&edit, red)
        .expect("outside any instance")
        .expect("a Sphere, through the reference")
        .set_display_color(&mut edit, &[[1.0, 0.0, 0.0]]);
    let transaction = edit.finish();
    document.apply(&transaction).unwrap();
    assert!(
        document.stage().stage().composition_errors().is_empty(),
        "authored composition must be valid"
    );
    let report = document.save();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    println!("{}", directory.join("RefExample.usda").display());
}
