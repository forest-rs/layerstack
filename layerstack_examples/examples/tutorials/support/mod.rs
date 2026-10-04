// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared setup keeps each runnable tutorial focused on its workflow.
use layerstack::{EditTarget, InMemoryStore, StageOptions};
use layerstack_io::{Filesystem, StageDocument};
use layerstack_schemas::{
    SchemaEdit, XformOpPrecision,
    usd_geom::{Sphere, Xform},
};
use std::{path::PathBuf, sync::Arc};

pub(crate) type Document = StageDocument<Filesystem>;
pub(crate) fn directory() -> PathBuf {
    let directory = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("layerstack-tutorial-{}", std::process::id()))
        });
    std::fs::create_dir_all(&directory).expect("tutorial output directory");
    directory
}
pub(crate) fn create(directory: &std::path::Path, name: &str) -> Document {
    let mut store = InMemoryStore::default();
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    StageDocument::create_in(
        Filesystem::new(directory, []).unwrap(),
        store,
        name,
        StageOptions {
            schemas: Some(schemas),
            with_provenance: true,
            ..Default::default()
        },
    )
    .unwrap()
}
pub(crate) fn hello(directory: &std::path::Path) -> Document {
    let mut document = create(directory, "HelloWorld.usda");
    let (store, live) = document.parts_mut();
    let root = live.stage().root_layer().unwrap();
    let hello = store.path("/hello");
    let world = store.path("/hello/world");
    let mut edit = SchemaEdit::new(live.stage(), store, EditTarget::for_layer(root));
    let xform = Xform::define(&mut edit, hello);
    xform
        .add_translate_op(&mut edit, XformOpPrecision::Double)
        .unwrap()
        .set(&mut edit, [4.0, 5.0, 6.0])
        .unwrap();
    let sphere = Sphere::define(&mut edit, world);
    sphere.set_radius(&mut edit, 2.0);
    sphere.set_extent(&mut edit, &[[-2.0, -2.0, -2.0], [2.0, 2.0, 2.0]]);
    sphere.set_display_color(&mut edit, &[[0.0, 0.0, 1.0]]);
    let transaction = edit.finish();
    live.apply(store, &transaction).unwrap();
    let name = store.tokens.intern("hello");
    live.set_default_prim(store, Some(name)).unwrap();
    let report = document.save();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.saved, vec![root], "save the newly authored root");
    document
}
