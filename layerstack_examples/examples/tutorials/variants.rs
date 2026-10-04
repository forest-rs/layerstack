// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Local opinions beat variants; an explicit variant edit target authors within
//! the branch without changing the stage's current selection.
//! <https://openusd.org/release/tut_authoring_variants.html>
mod support;
use layerstack::{EditTarget, SpecPath, Transaction};
use layerstack_schemas::{SchemaEdit, usd_geom::SphereEdit};
fn main() {
    let directory = support::directory();
    let mut document = support::hello(&directory);
    let (store, live) = document.parts_mut();
    let root = live.stage().root_layer().unwrap();
    let hello = store.path("/hello");
    let world = store.path("/hello/world");
    let color = store.property_path("/hello/world.primvars:displayColor");
    let mut clear = Transaction::new();
    clear.clear_default(EditTarget::for_layer(root).property(color));
    live.apply(store, &clear).unwrap();
    for (name, rgb) in [("red", [1.0, 0.0, 0.0]), ("green", [0.0, 1.0, 0.0])] {
        let branch = SpecPath::parse(
            &format!("/hello{{shadingVariant={name}}}"),
            &mut store.tokens,
            &mut store.paths,
        )
        .unwrap();
        let mut edit = SchemaEdit::new(
            live.stage(),
            store,
            EditTarget::for_local_variant(root, &branch),
        );
        SphereEdit::new(&edit, world)
            .unwrap()
            .set_display_color(&mut edit, &[rgb]);
        let transaction = edit.finish();
        live.apply(store, &transaction).unwrap();
    }
    let set = store.tokens.intern("shadingVariant");
    let selected = store.tokens.intern("green");
    let mut edit = Transaction::new();
    edit.set_variant_selection(EditTarget::for_layer(root).prim(hello), set, Some(selected));
    live.apply(store, &edit).unwrap();
    assert!(
        live.stage().composition_errors().is_empty(),
        "authored composition must be valid"
    );
    let report = document.save();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    println!(
        "{} (green variant selected)",
        directory.join("HelloWorld.usda").display()
    );
}
