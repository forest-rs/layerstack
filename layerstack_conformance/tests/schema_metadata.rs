// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Native OpenUSD 26.08 schema metadata and `ColorAPI` precedence fixture.
#![allow(missing_docs, reason = "integration tests")]

use layerstack::schema::{SchemaDeclaration, read_generated_schema};
use layerstack::{
    AssetResolveError, AssetResolver, FieldValue, InMemoryStore, LayerId, PathInterner,
    PropertyPath, ResolvedAsset, ResolvedValue, SchemaKind, SchemaRegistry, Stage, StageOptions,
    TokenInterner, Value,
};
use layerstack_schemas::Scene;
use serde_json::Value as Json;
use std::sync::Arc;

struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

fn parse(text: &str, id: LayerId, store: &mut InMemoryStore) -> layerstack::Layer {
    let parsed = layerstack_usda::parser::parse(text);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        id,
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
    emitted.layer
}

fn json(value: &Value, tokens: &TokenInterner) -> Json {
    match value {
        Value::Token(token) => tokens.resolve(*token).into(),
        Value::String(text) => text.as_ref().into(),
        Value::Int(value) => (*value).into(),
        Value::Dictionary(entries) => entries
            .iter()
            .map(|(key, value)| (key.to_string(), json(value, tokens)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        other => panic!("unexpected fixture value {other:?}"),
    }
}

#[test]
fn property_metadata_and_color_precedence_match_native_fixture() {
    let mut store = InMemoryStore::default();
    let generated = parse(
        include_str!("../fixtures/schema_metadata/generatedSchema.usda"),
        LayerId(2),
        &mut store,
    );
    let declared = [
        ("MetadataColor", SchemaKind::ConcreteTyped),
        ("MetadataWeakAPI", SchemaKind::SingleApplyApi),
        ("MetadataSlotAPI", SchemaKind::MultipleApplyApi),
    ]
    .map(|(name, kind)| SchemaDeclaration::new(store.tokens.intern(name), kind));
    let definitions =
        read_generated_schema(&generated, &declared, &mut store.tokens, &store.paths).unwrap();
    let native = layerstack_schemas::openusd(&mut store.tokens);
    let mut builder = SchemaRegistry::builder();
    for definition in native.schemas().chain(definitions.iter()) {
        builder.register(definition.clone());
    }
    let registry = builder.build(&mut store.tokens);
    let layer = parse(
        include_str!("../fixtures/schema_metadata/scene.usda"),
        LayerId(1),
        &mut store,
    );
    store.insert_layer(layer);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(Arc::new(registry)),
            ..StageOptions::default()
        },
    );
    let oracle: Json =
        serde_json::from_str(include_str!("../fixtures/schema_metadata/oracle.json")).unwrap();
    for row in oracle["properties"].as_array().unwrap() {
        let prim = store.path(row["prim"].as_str().unwrap());
        let property = store
            .tokens
            .lookup(row["property"].as_str().unwrap())
            .unwrap();
        for (field, oracle_field) in [
            ("colorSpace", "metadata_color_space"),
            ("displayName", "display_name"),
            ("displayGroup", "display_group"),
            ("settings", "settings"),
        ] {
            let key = store.tokens.lookup(field).unwrap();
            let actual = stage
                .resolve_property_metadata(prim, property, key)
                .map(|resolved| match resolved.value {
                    ResolvedValue::Scalar(value) => json(&value, &store.tokens),
                    ResolvedValue::Dictionary(entries) => {
                        json(&Value::Dictionary(entries), &store.tokens)
                    }
                    other => panic!("unexpected fixture resolution {other:?}"),
                })
                .unwrap_or(Json::Null);
            assert_eq!(
                actual, row[oracle_field],
                "{} {} {field}",
                row["prim"], row["property"]
            );
        }
        for (field, oracle_field) in [
            ("colorSpace", "schema_color_space"),
            ("documentation", "documentation"),
            ("customData", "custom_data"),
            ("settings", "schema_settings"),
        ] {
            let key = store.tokens.lookup(field).unwrap();
            let actual = stage
                .property_definition_ref(prim, property)
                .unwrap()
                .metadata(key)
                .map(|value| match value {
                    FieldValue::Value(value) => json(value, &store.tokens),
                    other => panic!("unexpected fixture field {other:?}"),
                })
                .unwrap_or(Json::Null);
            assert_eq!(actual, row[oracle_field]);
        }
        let assignment = Scene::new(&stage, &store)
            .compute_attribute_color_space_name(PropertyPath::new(prim, property))
            .unwrap();
        assert_eq!(
            Json::from(assignment.name.as_ref()),
            row["effective_color_space"]
        );
    }
}

#[test]
fn bundled_schema_metadata_is_visible_without_an_authored_property() {
    let (mut store, live) = support_scene::scene("#usda 1.0\ndef Mesh \"Mesh\" {}");
    let prim = store.path("/Mesh");
    let name = store.tokens.lookup("subdivisionScheme").unwrap();
    let key = store.tokens.lookup("allowedTokens").unwrap();
    assert!(
        live.stage()
            .resolve_authored_property_metadata(prim, name, key)
            .is_none()
    );
    let resolved = live
        .stage()
        .resolve_property_metadata(prim, name, key)
        .unwrap();
    assert!(resolved.provenance.is_none());
    assert!(
        matches!(resolved.value, ResolvedValue::Scalar(Value::Array(ref values)) if values.len() == 4)
    );
}

#[path = "support/schema_scene.rs"]
mod support_scene;
