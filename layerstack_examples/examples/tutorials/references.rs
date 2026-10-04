// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! References remap one authored asset into independent stage occurrences.
//! <https://openusd.org/release/tut_referencing_layers.html>
mod support;
use layerstack::{EditTarget, Reference, Specifier, Transaction};
use layerstack_schemas::{SchemaEdit, usd_geom::SphereEdit};
fn main() {
    let directory = support::directory();
    support::hello(&directory);
    let mut document = support::create(&directory, "RefExample.usda");
    let (asset, _) = document.load_asset("./HelloWorld.usda", None).unwrap();
    let (store, live) = document.parts_mut();
    let root = live.stage().root_layer().unwrap();
    let first = store.path("/refSphere");
    let second = store.path("/refSphere2");
    let mut create = Transaction::new();
    for path in [first, second] {
        create.create_prim(
            EditTarget::for_layer(root).prim(path),
            Specifier::Over,
            None,
        );
    }
    live.apply(store, &create).unwrap();
    // Layer APIs author composition arcs. Preserve each created spec and its
    // hierarchy bookkeeping; direct layer edits synchronize explicitly.
    let layer = store.layers.get_mut(&root).unwrap();
    for path in [first, second] {
        let spec = layer.prims[&path]
            .clone()
            .with_reference(Reference::with_asset_default_prim(
                asset,
                "./HelloWorld.usda",
            ));
        layer.insert_prim(path, spec);
    }
    live.synchronize(store);
    let red = store.path("/refSphere2/world");
    let mut edit = SchemaEdit::new(live.stage(), store, EditTarget::for_layer(root));
    SphereEdit::new(&edit, red)
        .unwrap()
        .set_display_color(&mut edit, &[[1.0, 0.0, 0.0]]);
    let transaction = edit.finish();
    live.apply(store, &transaction).unwrap();
    assert!(
        live.stage().composition_errors().is_empty(),
        "authored composition must be valid"
    );
    let report = document.save();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    println!("{}", directory.join("RefExample.usda").display());
}
