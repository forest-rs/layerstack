// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Winning asset sources and host anchors remain explicit without global provenance.
#![allow(missing_docs, reason = "integration tests")]
#[path = "support/schema_scene.rs"]
mod support;
use layerstack::{
    AssetResolveError, AssetResolver, EditTarget, Layer, LayerId, LayerOffset, LayerStore,
    LiveStage, PathInterner, PrimSpec, PropertyPath, PropertySpec, PropertyType, ResolvedAsset,
    StageOptions, SublayerEntry, TokenInterner, Transaction, Value,
};
use layerstack_schemas::{
    Scene, Time,
    assets::{AssetAnchorError, AssetReadError, AssetReference},
    light::{LightCache, LightInputs},
};
struct Locations;
impl AssetResolver for Locations {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        match id.0 {
            1 => Some("/root/scene.usda"),
            2 => Some("/library/lights.usda"),
            _ => None,
        }
    }
}
#[test]
fn weak_layer_and_retimed_assets_keep_their_actual_anchor() {
    let (mut store, _) = support::scene("#usda 1.0\ndef DomeLight \"Dome\" {}\n");
    let path = store.path("/Dome");
    let name = store.tokens.intern("inputs:texture:file");
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(
        path,
        PrimSpec::over().with_property(
            name,
            PropertySpec::attribute()
                .with_type(PropertyType::new("asset", false, Value::Asset("".into())))
                .with_default(Value::Asset("./default.exr".into()))
                .with_time_samples(vec![
                    (0., Value::Asset("./early.exr".into())),
                    (2., Value::Asset("./late.exr".into())),
                ]),
        ),
    );
    store.insert_layer(weak);
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .sublayers
        .push(SublayerEntry::with_offset(
            LayerId(2),
            LayerOffset {
                offset: 10.,
                scale: 2.,
            },
        ));
    let schemas = std::sync::Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let scene = Scene::new(live.stage(), &store);
    let property = PropertyPath::new(path, name);
    for (time, file) in [
        (Time::Default, "default.exr"),
        (Time::at(10.), "early.exr"),
        (Time::at(14.), "late.exr"),
    ] {
        let raw = live
            .stage()
            .read_property(property, time, |v| match v {
                Value::Asset(p) => Some(p.clone()),
                _ => None,
            })
            .unwrap();
        assert!(raw.provenance.is_none());
        let asset = AssetReference::read(&scene, property, time)
            .unwrap()
            .unwrap();
        assert_eq!(asset.source.as_ref().unwrap().layer, LayerId(2));
        assert_eq!(
            asset.anchor(&Locations).unwrap().identifier,
            format!("/library/{file}")
        );
        let capture = LightInputs::read(&scene, path, time, &[]).unwrap();
        let asset = capture
            .input("texture:file")
            .unwrap()
            .constant()
            .unwrap()
            .asset_reference()
            .unwrap();
        assert_eq!(asset.source.unwrap().layer, LayerId(2));
    }
    // Typed default reads skip incompatible storage, but numeric reads select
    // the strongest contributing value before attempting the typed conversion.
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prims
        .get_mut(&path)
        .unwrap()
        .set_property(
            name,
            PropertySpec::attribute()
                .with_type(PropertyType::new("float", false, Value::Float(0.)))
                .with_default(Value::Float(2.)),
        );
    let schemas = std::sync::Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let scene = Scene::new(live.stage(), &store);
    let asset = AssetReference::read(&scene, property, Time::Default)
        .unwrap()
        .unwrap();
    assert_eq!(asset.authored_path, "./default.exr");
    assert_eq!(asset.source.unwrap().layer, LayerId(2));
    assert_eq!(
        AssetReference::read(&scene, property, Time::at(10.)),
        Err(AssetReadError::InvalidValue(property))
    );
    let intensity = PropertyPath::new(path, store.tokens.lookup("inputs:intensity").unwrap());
    let fallback = live
        .stage()
        .read_property_with_provenance(intensity, Time::Default, |v| match v {
            Value::Float(v) => Some(*v),
            _ => None,
        })
        .unwrap();
    assert_eq!(fallback.value, 1.);
    assert!(fallback.provenance.is_none());
}
#[test]
fn blocks_wrong_storage_and_absent_attributes_are_distinct() {
    let (mut store, live) = support::scene(
        r#"#usda 1.0
def DomeLight "Dome" { asset inputs:texture:file = None }
def Scope "Bad" { custom float texture = 2; rel relation }
"#,
    );
    let dome = store.path("/Dome");
    let bad = store.path("/Bad");
    let texture = store.tokens.intern("inputs:texture:file");
    let custom = store.tokens.intern("texture");
    let missing = store.tokens.intern("missing");
    let relation = store.tokens.intern("relation");
    let scene = Scene::new(live.stage(), &store);
    assert_eq!(
        AssetReference::read(&scene, PropertyPath::new(dome, texture), Time::Default),
        Ok(None)
    );
    assert_eq!(
        AssetReference::read(&scene, PropertyPath::new(bad, custom), Time::Default),
        Err(AssetReadError::InvalidValue(PropertyPath::new(bad, custom)))
    );
    for name in [missing, relation] {
        assert_eq!(
            AssetReference::read(&scene, PropertyPath::new(bad, name), Time::Default),
            Err(AssetReadError::MissingAttribute(PropertyPath::new(
                bad, name
            )))
        );
    }
}
#[test]
fn anchoring_reports_missing_context_and_preserves_host_search_policy() {
    for (path, error) in [
        ("", AssetAnchorError::EmptyPath),
        ("./a.exr", AssetAnchorError::MissingSource),
        ("`${MAP}`", AssetAnchorError::ExpressionRequiresEvaluation),
    ] {
        assert_eq!(
            AssetReference {
                authored_path: path.into(),
                source: None
            }
            .anchor(&Locations),
            Err(error)
        );
    }
    for path in ["/absolute/a.exr", "https://example.org/a.exr"] {
        assert_eq!(
            AssetReference {
                authored_path: path.into(),
                source: None
            }
            .anchor(&Locations)
            .unwrap()
            .identifier,
            path
        );
    }
    let (mut store, live) = support::scene(
        "#usda 1.0\ndef DomeLight \"Dome\" {\n asset inputs:texture:file = @textures/a.exr@\n}\n",
    );
    let path = store.path("/Dome");
    let name = store.tokens.intern("inputs:texture:file");
    let asset = AssetReference::read(
        &Scene::new(live.stage(), &store),
        PropertyPath::new(path, name),
        Time::Default,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        asset.anchor(&Locations).unwrap().identifier,
        "textures/a.exr"
    );
    let mut missing = asset;
    missing.source.as_mut().unwrap().layer = LayerId(99);
    missing.authored_path = "./a.exr".into();
    assert_eq!(
        missing.anchor(&Locations),
        Err(AssetAnchorError::CannotAnchor)
    );
}

#[test]
fn source_only_asset_edits_change_anchors_and_parameter_upload_revisions() {
    let (mut store, _) = support::scene("#usda 1.0\ndef DomeLight \"Dome\" {}\n");
    let path = store.path("/Dome");
    let name = store.tokens.intern("inputs:texture:file");
    let property = PropertyPath::new(path, name);
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(
        path,
        PrimSpec::over().with_property(
            name,
            PropertySpec::attribute()
                .with_type(PropertyType::new("asset", false, Value::Asset("".into())))
                .with_default(Value::Asset("./sky.exr".into())),
        ),
    );
    store.insert_layer(weak);
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .sublayers
        .push(SublayerEntry::new(LayerId(2)));
    let schemas = std::sync::Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let texture_asset = |inputs: &LightInputs| {
        inputs
            .input("texture:file")
            .unwrap()
            .constant()
            .unwrap()
            .asset_reference()
            .unwrap()
    };
    let mut cache = LightCache::new(Time::Default, &[]);
    let first = cache
        .capture(&Scene::new(live.stage(), &store), path)
        .unwrap();
    let before = first.revisions;
    let before_asset = texture_asset(first.inputs);
    assert_eq!(before_asset.source.as_ref().unwrap().layer, LayerId(2));
    assert_eq!(
        before_asset.anchor(&Locations).unwrap().identifier,
        "/library/sky.exr"
    );

    // Identical spelling, now authored in the root layer. The engine must
    // recreate its asset identifier and update the parameter resource binding.
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(LayerId(1)).property(property),
        Value::Asset("./sky.exr".into()),
    );
    let applied = live.apply(&mut store, &edit).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &applied.changes);
    let next = cache.capture(&scene, path).unwrap();
    let asset = texture_asset(next.inputs);
    assert!(next.evaluated);
    assert_eq!(asset.authored_path, before_asset.authored_path);
    assert_eq!(asset.source.as_ref().unwrap().layer, LayerId(1));
    assert_eq!(
        asset.anchor(&Locations).unwrap().identifier,
        "/root/sky.exr"
    );
    assert_ne!(next.revisions.parameters, before.parameters);
    assert_eq!(next.revisions.transform, before.transform);
    assert_eq!(next.revisions.shader, before.shader);
    assert_eq!(next.revisions.relationships, before.relationships);
    let updated = next.revisions.parameters;

    let undone = live.apply(&mut store, &applied.inverse).unwrap();
    let scene = Scene::new(live.stage(), &store);
    cache.apply_changes(&scene, &undone.changes);
    let restored = cache.capture(&scene, path).unwrap();
    assert_eq!(texture_asset(restored.inputs), before_asset);
    assert_eq!(
        texture_asset(restored.inputs)
            .anchor(&Locations)
            .unwrap()
            .identifier,
        "/library/sky.exr"
    );
    assert_ne!(restored.revisions.parameters, updated);
}
