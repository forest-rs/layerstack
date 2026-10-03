// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed standard shader nodes preserve ordinary USD composition and authoring.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    InMemoryStore, Layer, LayerId, LiveStage, PropertyType, StageOptions, Time, Value,
    edit::EditTarget,
};
use layerstack_schemas::{
    Scene, SchemaEdit,
    shading::nodes::{PreviewSurface, UvTexture},
    usd_shade::{NodeDefApiImplementationSource, Shader},
};
use std::sync::Arc;

#[path = "generated/shader_nodes.rs"]
mod generated;

fn scene() -> (InMemoryStore, LiveStage) {
    let mut store = InMemoryStore::default();
    store.insert_layer(Layer::new(LayerId(1)));
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    (store, live)
}

#[test]
fn every_node_default_and_port_type_matches_composed_cpp_definitions() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/shader_nodes.json")).unwrap();
    assert_eq!(oracle["version"], layerstack_schemas::OPENUSD_VERSION);
    assert_eq!(
        generated::IDS.len(),
        oracle["nodes"].as_object().unwrap().len()
    );
    let (mut store, mut live) = scene();
    let paths: Vec<_> = generated::IDS
        .iter()
        .map(|id| store.path(&format!("/{id}")))
        .collect();
    let before = store.layers[&LayerId(1)].clone();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    generated::author(&mut edit, &paths);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    generated::compare(&Scene::new(live.stage(), &store), &paths, &oracle["nodes"]);
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(store.layers[&LayerId(1)], before);
}

#[test]
fn identifier_checks_defaults_connections_samples_and_undo_are_explicit() {
    let (mut store, mut live) = scene();
    let surface = store.path("/Surface");
    let texture = store.path("/Texture");
    let missing = store.path("/Missing");
    let before = store.layers[&LayerId(1)].clone();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    PreviewSurface::define(&mut edit, surface);
    UvTexture::define(&mut edit, texture);
    let transaction = edit.finish();
    let applied = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let view = PreviewSurface::new(&scene, surface).unwrap();
    assert!(view.roughness().is_none());
    assert!(view.roughness_input().is_none());
    assert_eq!(PreviewSurface::roughness_default(), 0.5);
    assert!(PreviewSurface::new(&scene, texture).is_none());
    assert!(PreviewSurface::new(&scene, missing).is_none());
    let surface_edit = view.edit();
    let texture_edit = UvTexture::new(&scene, texture).unwrap().edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    surface_edit.set_roughness_at(&mut edit, 1.0, 0.2).unwrap();
    surface_edit.set_roughness_at(&mut edit, 3.0, 0.8).unwrap();
    let input = surface_edit.create_diffuse_color_input(&mut edit).unwrap();
    let output = texture_edit.create_rgb_output(&mut edit).unwrap();
    input.set_sources(&mut edit, &[output]).unwrap();
    let transaction = edit.finish();
    let edits = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    let view = PreviewSurface::new(&scene, surface).unwrap();
    assert!(
        view.roughness().is_none(),
        "samples do not become default values"
    );
    assert!(
        (view
            .roughness_at(2.0, layerstack::InterpolationType::Linear)
            .unwrap()
            - 0.5)
            .abs()
            < 1e-6
    );
    assert!(
        view.diffuse_color().is_none(),
        "reads do not follow connections"
    );
    let sources = view.diffuse_color_input().unwrap().shader_sources();
    assert_eq!(sources.sources.len(), 1);
    assert_eq!(sources.sources[0].output.prim_path(), texture);
    let handle = view.edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    handle
        .create_roughness_input(&mut edit)
        .unwrap()
        .set(&mut edit, Value::Blocked)
        .unwrap();
    handle
        .node_def_api()
        .set_implementation_source(&mut edit, NodeDefApiImplementationSource::SourceAsset);
    let transaction = edit.finish();
    let blocked = live.apply(&mut store, &transaction).unwrap();
    let scene = Scene::new(live.stage(), &store);
    assert!(
        PreviewSurface::new(&scene, surface).is_none(),
        "source assets are not identifier-based nodes"
    );
    assert!(
        Shader::new(&scene, surface)
            .unwrap()
            .input("roughness")
            .unwrap()
            .value(Time::Default)
            .is_none()
    );
    live.apply(&mut store, &blocked.inverse).unwrap();
    live.apply(&mut store, &edits.inverse).unwrap();
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(store.layers[&LayerId(1)], before);
}

#[test]
fn incompatible_existing_port_is_rejected_without_appending_edits() {
    let (mut store, mut live) = scene();
    let path = store.path("/Surface");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let node = PreviewSurface::define(&mut edit, path);
    node.create_input(
        &mut edit,
        "roughness",
        PropertyType::new("string", false, Value::string("")),
    )
    .unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let handle = PreviewSurface::new(&Scene::new(live.stage(), &store), path)
        .unwrap()
        .edit();
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    assert!(handle.set_roughness(&mut edit, 0.3).is_err());
    assert!(edit.finish().is_empty());
}

#[test]
fn defining_a_node_overrides_a_weaker_source_asset_implementation() {
    let (mut store, _) = scene();
    let path = store.path("/Surface");
    let shader = store.tokens.intern("Shader");
    let implementation = store.tokens.intern("info:implementationSource");
    let source_asset = store.tokens.intern("sourceAsset");
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(path, layerstack::PrimSpec::def().with_type_name(shader));
    weak.prims.get_mut(&path).unwrap().set_property(
        implementation,
        layerstack::PropertySpec::attribute().with_default(Value::Token(source_asset)),
    );
    store.insert_layer(weak);
    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .sublayers
        .push(layerstack::SublayerEntry::new(LayerId(2)));
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    PreviewSurface::define(&mut edit, path);
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    assert!(
        PreviewSurface::new(&Scene::new(live.stage(), &store), path).is_some(),
        "node definition selects its identifier implementation over weaker opinions"
    );
}

#[test]
fn legacy_primvar_names_are_consumable_without_retyping_authored_ports() {
    use layerstack::{
        AssetResolveError, AssetResolver, PathInterner, PropertyPath, ResolvedAsset, Stage,
        TokenInterner,
    };
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
    use layerstack_schemas::shading::nodes::{PrimvarReaderFloat2, PrimvarReaderString};
    let mut store = InMemoryStore::default();
    let source = r#"#usda 1.0
def NodeGraph "Graph" {
    token inputs:frame:st = "st"
    def Shader "Connected" {
        uniform token info:id = "UsdPrimvarReader_float2"
        string inputs:varname.connect = </Graph.inputs:frame:st>
    }
    def Shader "Legacy" {
        uniform token info:id = "UsdPrimvarReader_float2"
        token inputs:varname = "uv"
    }
    def Shader "Modern" {
        uniform token info:id = "UsdPrimvarReader_string"
        string inputs:varname = "label"
        token inputs:fallback = "not-a-string"
    }
    def Shader "Bad" {
        uniform token info:id = "UsdPrimvarReader_float2"
        int inputs:varname = 42
    }
}
"#;
    let parsed = layerstack_usda::parser::parse(source);
    assert!(parsed.diagnostics.is_empty());
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(emitted.diagnostics.is_empty());
    store.insert_layer(emitted.layer);
    let options = StageOptions {
        with_provenance: true,
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let legacy = store.path("/Graph/Legacy");
    let connected = store.path("/Graph/Connected");
    let modern = store.path("/Graph/Modern");
    let bad = store.path("/Graph/Bad");
    let input = store.tokens.intern("inputs:varname");
    let stage = Stage::compose(&mut store, LayerId(1), options);
    let scene = Scene::new(&stage, &store);
    let legacy = PrimvarReaderFloat2::new(&scene, legacy).unwrap();
    assert_eq!(legacy.varname().as_deref(), Some("uv"));
    assert_eq!(
        legacy
            .varname_input()
            .unwrap()
            .property_type()
            .unwrap()
            .type_name
            .as_ref(),
        "token"
    );
    let modern = PrimvarReaderString::new(&scene, modern).unwrap();
    assert_eq!(modern.varname().as_deref(), Some("label"));
    assert_eq!(modern.fallback(), None, "general strings remain strict");
    assert_eq!(
        PrimvarReaderFloat2::new(&scene, bad).unwrap().varname(),
        None
    );
    assert_eq!(
        PrimvarReaderFloat2::new(&scene, connected)
            .unwrap()
            .varname(),
        None,
        "direct accessors do not evaluate connections"
    );
    let sources = scene.value_sources(PropertyPath::new(connected, input));
    assert!(sources.issues.is_empty());
    assert_eq!(sources.sources.len(), 1);
    let name = sources.sources[0].primvar_name(&scene).unwrap();
    assert_eq!(&*name.value, "st");
    assert!(name.provenance.is_some());
    assert_eq!(sources.sources[0].chain.len(), 2);
    let invalid = scene.value_sources(PropertyPath::new(bad, input));
    assert!(invalid.sources[0].primvar_name(&scene).is_none());
    assert!(matches!(
        stage
            .resolve_field_path(sources.sources[0].attribute)
            .unwrap()
            .value,
        Value::Token(_)
    ));
}
