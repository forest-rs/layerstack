// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage options govern registered USDA metadata through every reader lifecycle.

use super::*;
use layerstack::{FieldValue, MetadataDefinition, MetadataTarget};

fn setup(files: &[(&str, &str)]) -> (Memory, InMemoryStore, StageOptions) {
    let mut store = InMemoryStore::default();
    let mut builder = layerstack::SchemaRegistry::builder();
    for (name, ty) in [("hostScale", "float"), ("hostAsset", "asset")] {
        builder
            .register_metadata(MetadataDefinition {
                name: store.tokens.intern(name),
                type_name: ty.into(),
                targets: alloc::vec![MetadataTarget::Prim],
                default: None,
                documentation: "".into(),
            })
            .unwrap();
    }
    let registry = Arc::new(builder.build(&mut store.tokens));
    (
        Memory::with(files),
        store,
        StageOptions {
            schemas: Some(registry),
            ..Default::default()
        },
    )
}

fn scale(layer: &Layer, store: &InMemoryStore, path: &str) -> FieldValue {
    let segments: Vec<_> = path
        .split('/')
        .filter(|name| !name.is_empty())
        .map(|name| store.tokens.lookup(name).unwrap())
        .collect();
    let path = layerstack::Path::root().join(&segments);
    let path = store.paths.lookup(&path).unwrap();
    layerstack::doc::get_field(
        &layer.prims()[&path].fields,
        &store.tokens.lookup("hostScale").unwrap(),
    )
    .unwrap()
    .clone()
}

#[test]
fn stage_registry_types_initial_recursive_explicit_reload_and_package_reads() {
    let (memory, store, options) = setup(&[
        (
            "root.usda",
            "#usda 1.0\n(subLayers = [@base.usda@])\ndef \"P\" (references = @asset.usda@</Source>; hostAsset = @metadata.usda@) {}\n",
        ),
        ("base.usda", "#usda 1.0\nover \"P\" (hostScale = 1) {}\n"),
        (
            "asset.usda",
            "#usda 1.0\ndef \"Source\" (hostScale = 2) {}\n",
        ),
        (
            "metadata.usda",
            "#usda 1.0\ndef \"Metadata\" (hostScale = 3) {}\n",
        ),
        (
            "extra.usda",
            "#usda 1.0\ndef \"Extra\" (hostScale = 4) {}\n",
        ),
    ]);
    let mut document =
        StageDocument::open_in(memory, store, "root.usda", options, ImportPolicy::Strict).unwrap();
    let base = document.catalog.ids["base.usda"];
    let asset = document.catalog.ids["asset.usda"];
    assert_eq!(
        scale(&document.store.layers[&base], &document.store, "/P"),
        FieldValue::Value(Value::Float(1.))
    );
    assert_eq!(
        scale(&document.store.layers[&asset], &document.store, "/Source"),
        FieldValue::Value(Value::Float(2.))
    );
    let (extra, report) = document.load_asset("extra.usda", None).unwrap();
    assert!(report.diagnostics.is_empty());
    assert_eq!(
        scale(&document.store.layers[&extra], &document.store, "/Extra"),
        FieldValue::Value(Value::Float(4.))
    );
    document.storage.files.insert(
        "base.usda".into(),
        b"#usda 1.0\nover \"P\" (hostScale = 7) {}\n".to_vec(),
    );
    document.reload(ReloadPolicy::DiscardDirty).unwrap();
    assert_eq!(
        scale(&document.store.layers[&base], &document.store, "/P"),
        FieldValue::Value(Value::Float(7.))
    );

    // A USD layer referenced only by asset metadata is first imported by the
    // isolated packaging reader, which must use the same schema registry.
    assert!(!document.catalog.ids.contains_key("metadata.usda"));
    let root = document.stage.stage().root_layer().unwrap();
    let package = document
        .prepare_package(
            root,
            "scene.usda",
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )
        .unwrap();
    assert!(package.report().is_complete(), "{:?}", package.report());
    let metadata = package
        .plan()
        .unwrap()
        .layers
        .iter()
        .find(|layer| {
            layer.layer.prims().values().any(|prim| {
                prim.fields
                    .iter()
                    .any(|field| field.value == FieldValue::Value(Value::Float(3.)))
            })
        })
        .unwrap();
    assert!(
        metadata
            .layer
            .prims()
            .values()
            .flat_map(|prim| &prim.fields)
            .any(|field| field.value == FieldValue::Value(Value::Float(3.)))
    );
    assert!(
        !document.catalog.ids.contains_key("metadata.usda"),
        "package preparation keeps import state isolated"
    );
    let before = document.store.layers.clone();
    document.storage.files.insert(
        "base.usda".into(),
        b"#usda 1.0\nover \"P\" (hostScale = \"bad\") {}\n".to_vec(),
    );
    assert_eq!(
        document
            .reload(ReloadPolicy::DiscardDirty)
            .unwrap_err()
            .kind,
        IoErrorKind::Rejected
    );
    assert_eq!(
        document.store.layers, before,
        "invalid metadata in a reload publishes no partial layer"
    );
}

#[test]
fn strict_import_rejects_registered_metadata_on_invalid_spec_targets() {
    let (memory, store, options) =
        setup(&[("root.usda", "#usda 1.0\n(hostScale = 1)\ndef \"P\" {}\n")]);
    assert_eq!(
        StageDocument::open_in(memory, store, "root.usda", options, ImportPolicy::Strict)
            .unwrap_err()
            .kind,
        IoErrorKind::Rejected
    );
}
