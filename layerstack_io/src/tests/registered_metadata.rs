// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage options govern registered USDA metadata through every reader lifecycle.

use super::*;
use layerstack::{FieldValue, MetadataDefinition, MetadataTarget, Value};

fn setup(files: &[(&str, &str)]) -> (Memory, InMemoryStore, StageOptions) {
    let store = InMemoryStore::default();
    let mut builder = layerstack::SchemaRegistry::builder();
    for (name, ty) in [("hostScale", "float"), ("hostAsset", "asset")] {
        builder
            .register_metadata(
                MetadataDefinition::new(
                    store.tokens().intern(name),
                    layerstack::MetadataType::parse(ty).unwrap(),
                    [MetadataTarget::Prim],
                ),
                store.tokens(),
            )
            .unwrap();
    }
    let registry = Arc::new(builder.build(store.tokens()));
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
        .map(|name| store.tokens().lookup(name).unwrap())
        .collect();
    let path = layerstack::Path::root().join(&segments);
    let path = store.paths().lookup(&path).unwrap();
    layerstack::doc::get_field(
        &layer.prims()[&path].fields,
        &store.tokens().lookup("hostScale").unwrap(),
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
    let mut document = StageDocument::open(
        memory,
        "root.usda",
        OpenOptions {
            stage: options,
            store: Some(store),
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let base = document.catalog.ids["base.usda"];
    let asset = document.catalog.ids["asset.usda"];
    assert_eq!(
        scale(document.store.layer(base).unwrap(), &document.store, "/P"),
        FieldValue::Value(Value::Float(1.))
    );
    assert_eq!(
        scale(
            document.store.layer(asset).unwrap(),
            &document.store,
            "/Source"
        ),
        FieldValue::Value(Value::Float(2.))
    );
    let (extra, report) = document.load_asset("extra.usda", None).unwrap();
    assert!(report.diagnostics.is_empty());
    assert_eq!(
        scale(
            document.store.layer(extra).unwrap(),
            &document.store,
            "/Extra"
        ),
        FieldValue::Value(Value::Float(4.))
    );
    document.storage.files.insert(
        "base.usda".into(),
        b"#usda 1.0\nover \"P\" (hostScale = 7) {}\n".to_vec(),
    );
    document.reload(ReloadPolicy::DiscardDirty).unwrap();
    assert_eq!(
        scale(document.store.layer(base).unwrap(), &document.store, "/P"),
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
    let before: Vec<_> = document.store.layers().cloned().collect();
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
        document.store.layers().cloned().collect::<Vec<_>>(),
        before,
        "invalid metadata in a reload publishes no partial layer"
    );
}

#[test]
fn strict_import_keeps_a_field_on_a_spec_it_is_not_registered_for_untyped() {
    // `hostScale` is registered for prims: on a layer it is a field the
    // layer's spec definition does not register, which OpenUSD keeps as it
    // reads it rather than rejecting the layer.
    let (memory, store, options) =
        setup(&[("root.usda", "#usda 1.0\n(hostScale = 1)\ndef \"P\" {}\n")]);
    let document = StageDocument::open(
        memory,
        "root.usda",
        OpenOptions {
            stage: options,
            store: Some(store),
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let key = document.store.tokens().lookup("hostScale").unwrap();
    let layer = document.store.layers().next().unwrap();
    assert_eq!(
        layer.metadata(key),
        Some(&FieldValue::Value(Value::Int64(1)))
    );
}

#[test]
fn opening_with_a_registry_of_another_store_fails() {
    let files = [(
        "rocks.usda",
        "#usda 1.0\ndef \"Boulder\" (hostScale = 2) {}\n",
    )];
    let rejected = |result: Result<StageDocument<Memory>, IoError>| {
        let error = result.expect_err("a foreign registry is rejected");
        assert_eq!(error.kind, IoErrorKind::ForeignSchemaRegistry);
        assert!(
            error.to_string().contains("another token interner"),
            "{error}"
        );
    };
    // `open` makes a store no registry built beforehand belongs to.
    let (memory, _, options) = setup(&files);
    rejected(StageDocument::open(memory, "rocks.usda", options));
    let (memory, _, options) = setup(&files);
    rejected(StageDocument::open(
        memory,
        "rocks.usda",
        OpenOptions {
            stage: options,
            store: Some(InMemoryStore::default()),
            ..OpenOptions::default()
        },
    ));
    let (memory, _, options) = setup(&files);
    rejected(StageDocument::create(
        memory,
        "lights.usda",
        OpenOptions {
            stage: options,
            store: Some(InMemoryStore::default()),
            ..OpenOptions::default()
        },
    ));
    let (memory, store, options) = setup(&files);
    let doc = StageDocument::open(
        memory,
        "rocks.usda",
        OpenOptions {
            stage: options,
            store: Some(store),
            ..OpenOptions::default()
        },
    )
    .expect("the registry belongs to the store it was built on");
    assert!(doc.stage.stage().composition_errors().is_empty());
    assert!(doc.stage.stage().schemas().is_some());
}
