// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `AssetPreviewsAPI` default thumbnails and an asset's default previews
//! against OpenUSD 26.11.
use crate::support::schema_scene as support;
use layerstack::{FieldValue, InMemoryStore, LayerId, PathId, Value, edit::EditTarget};
use layerstack_schemas::{
    PrimView, Scene, SchemaEdit,
    asset_previews::asset_default_previews,
    usd_media::{AssetPreviewsApi, AssetPreviewsApiEdit},
};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use std::sync::Arc;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    reads: Vec<Read>,
    edits: Vec<Edit>,
    asset_default_previews: Previews,
}
#[derive(Deserialize)]
struct Read {
    prim: String,
    default_image: Option<String>,
}
#[derive(Deserialize)]
struct Edit {
    call: String,
    prim: String,
    image: Option<String>,
    local_asset_info: Option<Json>,
    composed_default: Json,
    default_image: Option<String>,
}
#[derive(Deserialize)]
struct Previews {
    prim: String,
    default_image: Option<String>,
    populated: Vec<String>,
}

fn oracle() -> Oracle {
    serde_json::from_str(include_str!("../../fixtures/asset_previews/oracle.json")).unwrap()
}
const SCENE: &str = include_str!("../../fixtures/asset_previews/scene.usda");

fn to_json(value: &Value) -> Json {
    match value {
        Value::Dictionary(entries) => Json::Object(
            entries
                .iter()
                .map(|(key, value)| (key.to_string(), to_json(value)))
                .collect::<Map<_, _>>(),
        ),
        Value::Int(v) => json!(v),
        Value::String(text) => json!(&**text),
        Value::Asset(path) => json!({ "asset": &**path }),
        other => panic!("no JSON rendering for {other:?}"),
    }
}

fn thumbnail(scene: Scene<'_>, prim: PathId) -> Option<String> {
    AssetPreviewsApi::from_prim_unchecked(PrimView::new(scene, prim))
        .default_thumbnails()
        .map(|thumbnails| thumbnails.default_image.authored_path)
}

fn local_asset_info(store: &InMemoryStore, prim: PathId) -> Option<Json> {
    let spec = store.layer(LayerId(1)).unwrap().prim(prim)?;
    match layerstack::get_field(&spec.fields, &store.tokens.lookup("assetInfo")?)? {
        FieldValue::Value(value) => Some(to_json(value)),
        other => panic!("unexpected assetInfo {other:?}"),
    }
}

#[test]
fn default_thumbnails_read_as_cpp_reads_them() {
    let oracle = oracle();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for row in &oracle.reads {
        assert_eq!(
            thumbnail(scene, store.path(&row.prim)),
            row.default_image,
            "{}",
            row.prim
        );
    }
}

#[test]
fn default_thumbnails_edit_as_cpp_edits_them() {
    let oracle = oracle();
    let (mut store, mut live) = support::layered_scene(SCENE);
    for row in &oracle.edits {
        let prim = store.path(&row.prim);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let handle = AssetPreviewsApiEdit::new(&edit, prim).unwrap().unwrap();
        match (row.call.as_str(), row.image.as_deref()) {
            ("set", Some(image)) => handle.set_default_thumbnails(&mut edit, image).map(|_| ()),
            ("clear", None) => handle.clear_default_thumbnails(&mut edit).map(|_| ()),
            other => panic!("unknown call {other:?}"),
        }
        .unwrap();
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let label = format!("{} {}", row.call, row.prim);
        assert_eq!(
            local_asset_info(&store, prim),
            row.local_asset_info,
            "{label}"
        );
        let scene = Scene::new(live.stage(), &store);
        let view = PrimView::new(scene, prim);
        assert_eq!(
            view.metadata_by_key("assetInfo", "previews:thumbnails:default")
                .map(|value| to_json(&value)),
            Some(row.composed_default.clone()),
            "{label}"
        );
        let thumbnails = AssetPreviewsApi::from_prim_unchecked(view).default_thumbnails();
        assert_eq!(
            thumbnails
                .as_ref()
                .map(|t| t.default_image.authored_path.clone()),
            row.default_image,
            "{label}"
        );
        // The image anchors at the layer whose opinion authors it.
        if let Some(thumbnails) = thumbnails {
            let layer = if row.call == "set" {
                LayerId(1)
            } else {
                LayerId(2)
            };
            assert_eq!(
                thumbnails.default_image.source.map(|source| source.layer),
                Some(layer),
                "{label}"
            );
        }
    }
}

#[test]
fn asset_default_previews_populate_only_the_default_prim() {
    let oracle = oracle();
    let want = &oracle.asset_default_previews;
    let (mut store, _) = support::scene(SCENE);
    let schemas = Arc::new(layerstack_schemas::openusd(&store.tokens));
    let previews = asset_default_previews(&mut store, LayerId(1), schemas).unwrap();
    assert_eq!(previews.prim, store.path(&want.prim));
    assert_eq!(
        previews
            .default_thumbnails(&store)
            .map(|t| t.default_image.authored_path),
        want.default_image
    );
    for path in [
        "/Asset",
        "/Asset/Child",
        "/WeakAsset",
        "/Unapplied",
        "/StringImage",
    ] {
        assert_eq!(
            previews.stage.prim(store.path(path), &store).is_some(),
            want.populated.iter().any(|p| p == path),
            "{path}"
        );
    }
}
