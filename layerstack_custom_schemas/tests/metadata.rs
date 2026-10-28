// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The generated metadata views of a schema library of its own
//! (`layerstack_custom_schemas::fields`), authored and read on a stage as a
//! downstream crate would, and saved as OpenUSD writes the same calls: the
//! expected lines are those `layerstack_conformance`'s
//! `scripts/registry_metadata_oracle.py` records for usd-core 26.11
//! (`fixtures/registry_metadata/oracle.json`, `authoring`).

#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::{
    EditTarget, InMemoryStore, LayerId, ListOp, LiveStage, MetadataEditReason, NoAssets,
    StageOptions, TokenInterner, Value,
};
use layerstack_custom_schemas::fields::{
    self, PrimMetadata, PrimMetadataEdit, PropertyMetadata, PropertyMetadataEdit, StageMetadata,
};
use layerstack_schemas::{MetadataError, PrimEdit, PrimView, Scene, SchemaEdit};

fn stage() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    let schemas = fields::registry(store.tokens()).expect("the library's fields register");
    let read = layerstack_usda::read_usda_with_registry(
        "#usda 1.0\ndef \"P\"\n{\n    custom float a\n    custom rel r\n}\n",
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
        &schemas,
    );
    assert!(!read.emitted.rejected, "{:?}", read.emitted.diagnostics);
    store.insert_layer(read.emitted.layer);
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(Arc::new(schemas)),
            ..StageOptions::default()
        },
    );
    (store, live)
}

/// The lines of `field`'s statements in `text`.
fn written(text: &str, field: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| {
            let statement = ["prepend ", "append ", "delete ", "add ", "reorder "]
                .iter()
                .find_map(|op| line.strip_prefix(op))
                .unwrap_or(line);
            statement.starts_with(&format!("{field} = "))
        })
        .map(String::from)
        .collect()
}

#[test]
fn typed_setters_author_what_the_typed_getters_read_and_openusd_writes() {
    let (mut store, mut live) = stage();
    let prim = store.path("/P");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = PrimEdit::new(&edit, prim).unwrap().unwrap();
    PrimMetadataEdit::of(handle)
        .set_meta_bool(&mut edit, true)
        .unwrap()
        .set_meta_float(&mut edit, 0.25)
        .unwrap()
        .set_meta_float3(&mut edit, [1.0, 2.0, 3.0])
        .unwrap()
        .set_meta_tokens(&mut edit, &["a", "b"])
        .unwrap()
        .set_meta_token_list(
            &mut edit,
            ListOp {
                prepend: vec!["a"],
                delete: vec!["b"],
                ..ListOp::default()
            },
        )
        .unwrap()
        .set_meta_int_list(
            &mut edit,
            ListOp {
                append: vec![i32::MIN],
                ..ListOp::default()
            },
        )
        .unwrap();
    PropertyMetadataEdit::of(handle, "r")
        .set_meta_token(&mut edit, "medium")
        .unwrap();
    PropertyMetadataEdit::of(handle, "a")
        .set_meta_half(&mut edit, 0.5)
        .unwrap();
    // A relationship field is refused on an attribute, as OpenUSD refuses
    // it, and the refusal collects nothing.
    let collected = edit.transaction().len();
    let refused = PropertyMetadataEdit::of(handle, "a")
        .set_meta_token(&mut edit, "medium")
        .map(|_| ());
    assert!(
        matches!(
            refused,
            Err(MetadataError::Field(ref error)) if error.reason == MetadataEditReason::NotForSpec
        ),
        "{refused:?}"
    );
    assert_eq!(edit.transaction().len(), collected);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    StageMetadata::set_meta_double(&mut live, &mut store, LayerId(1), 2.5).unwrap();

    let scene = Scene::new(live.stage(), &store);
    let view = PrimView::new(scene, prim);
    let metadata = PrimMetadata::of(view);
    assert_eq!(metadata.meta_bool(), Some(true));
    assert_eq!(metadata.meta_float(), Some(0.25));
    assert_eq!(metadata.meta_float3(), Some([1.0, 2.0, 3.0]));
    assert_eq!(metadata.meta_tokens(), Some(vec!["a", "b"]));
    assert_eq!(metadata.meta_token_list(), Some(vec!["a"]));
    assert_eq!(metadata.meta_int_list(), Some(vec![i32::MIN]));
    assert_eq!(
        PropertyMetadata::of(view, "r").unwrap().meta_token(),
        Some("medium")
    );
    assert_eq!(
        PropertyMetadata::of(view, "a").unwrap().meta_half(),
        Some(0.5)
    );
    let stage = StageMetadata::of(scene);
    assert_eq!(stage.meta_double(), Some(2.5));
    // An unauthored layer field reads as its plugin's default.
    assert_eq!(stage.meta_int(), Some(-7));
    // Prim fields have no fallback read.
    assert_eq!(metadata.meta_u_char(), None);

    let layer = store.layer(LayerId(1)).unwrap();
    let text = layerstack_usda::save_usda(layer, store.tokens(), store.paths()).unwrap();
    for (field, lines) in [
        ("metaBool", &["metaBool = true"][..]),
        ("metaFloat", &["metaFloat = 0.25"]),
        ("metaFloat3", &["metaFloat3 = (1, 2, 3)"]),
        ("metaTokens", &["metaTokens = [\"a\", \"b\"]"]),
        (
            "metaTokenList",
            &[
                "delete metaTokenList = [\"b\"]",
                "prepend metaTokenList = [\"a\"]",
            ],
        ),
        ("metaIntList", &["append metaIntList = [-2147483648]"]),
        ("metaToken", &["metaToken = \"medium\""]),
        ("metaHalf", &["metaHalf = 0.5"]),
        ("metaDouble", &["metaDouble = 2.5"]),
    ] {
        assert_eq!(written(&text, field), lines, "{field}:\n{text}");
    }
    // Clearing leaves no opinion.
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = PrimEdit::new(&edit, prim).unwrap().unwrap();
    PrimMetadataEdit::of(handle)
        .clear_meta_float(&mut edit)
        .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        PrimMetadata::of(PrimView::new(scene, prim)).meta_float(),
        None
    );
    assert_eq!(
        PrimView::new(scene, prim).metadata().value("metaBool"),
        Some(Value::Bool(true))
    );
}

#[test]
fn the_library_fields_conflict_with_no_built_in_field() {
    let tokens = TokenInterner::default();
    let registry = fields::registry(&tokens).unwrap();
    assert!(registry.metadata_definitions().count() > 30);
    // Registering the library twice declares each field alike.
    let mut builder = layerstack::SchemaRegistry::builder();
    fields::register(&mut builder, &tokens).unwrap();
    fields::register(&mut builder, &tokens).unwrap();
}
