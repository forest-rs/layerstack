// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `NodeDefAPI` source queries and setters against OpenUSD 26.11, call by
//! call: each setter's acceptance and the specs it leaves, then the reads.
use crate::support::schema_scene as support;
use layerstack::{
    InMemoryStore, LayerId, PathId, PropertyKind, Value, Variability, edit::EditTarget,
};
use layerstack_schemas::{
    PrimView, Scene, SchemaEdit,
    shading::NodeDefError,
    usd_shade::{NodeDefApi, NodeDefApiEdit},
};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    scene_reads: Vec<BTreeMap<String, serde_json::Value>>,
    calls: Vec<Call>,
    edit_reads: Vec<BTreeMap<String, serde_json::Value>>,
}
#[derive(Deserialize)]
struct Call {
    call: String,
    prim: String,
    value: String,
    source_type: String,
    accepted: bool,
    specs: BTreeMap<String, Spec>,
}
#[derive(Debug, Deserialize, PartialEq)]
struct Spec {
    #[serde(rename = "type")]
    ty: String,
    custom: bool,
    uniform: bool,
    default: Option<String>,
}

fn oracle() -> Oracle {
    serde_json::from_str(include_str!("../../fixtures/shade_node_def/oracle.json")).unwrap()
}

/// The reads the oracle records for each prim, as layerstack answers them.
fn reads(
    scene: &Scene<'_>,
    store: &InMemoryStore,
    path: &str,
) -> BTreeMap<String, serde_json::Value> {
    let view = NodeDefApi::from_prim_unchecked(PrimView::new(*scene, store.path(path)));
    let mut row = BTreeMap::new();
    row.insert("path".into(), path.into());
    row.insert(
        "effective".into(),
        view.effective_implementation_source().as_str().into(),
    );
    row.insert("shader_id".into(), view.shader_id().into());
    row.insert("source_types".into(), view.source_types().into());
    for source_type in ["", "osl", "mdl", "glslfx", "ri"] {
        row.insert(
            format!("source_asset:{source_type}"),
            view.source_asset(source_type)
                .map(|asset| asset.asset_path)
                .into(),
        );
        row.insert(
            format!("subidentifier:{source_type}"),
            view.source_asset_subidentifier(source_type).into(),
        );
        row.insert(
            format!("source_code:{source_type}"),
            view.source_code(source_type)
                .map(|code| code.to_string())
                .into(),
        );
    }
    row
}

#[test]
fn source_queries_match_cpp() {
    let oracle = oracle();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(include_str!("../../fixtures/shade_node_def/scene.usda"));
    let scene = Scene::new(live.stage(), &store);
    for want in &oracle.scene_reads {
        let got = reads(&scene, &store, want["path"].as_str().unwrap());
        assert_eq!(&got, want);
    }
}

/// Every attribute spec of the root layer's prim at `path`.
fn specs(store: &InMemoryStore, path: PathId) -> BTreeMap<String, Spec> {
    let Some(prim) = store.layer(LayerId(1)).unwrap().prim(path) else {
        return BTreeMap::new();
    };
    prim.properties
        .iter()
        .filter(|entry| entry.spec.kind == PropertyKind::Attribute)
        .map(|entry| {
            let spec = &entry.spec;
            let default = spec.default.as_ref().map(|value| match value {
                Value::Token(token) => store.tokens().resolve(*token).to_string(),
                Value::String(text) | Value::Asset(text) => text.to_string(),
                other => panic!("unexpected default {other:?}"),
            });
            let rendered = Spec {
                ty: spec
                    .type_name
                    .as_ref()
                    .map(|ty| ty.type_name().to_string())
                    .unwrap_or_default(),
                custom: spec.custom,
                uniform: spec.variability == Variability::Uniform,
                default,
            };
            (store.tokens().resolve(entry.name).to_string(), rendered)
        })
        .collect()
}

#[test]
fn setters_match_cpp_call_by_call() {
    let oracle = oracle();
    let (mut store, mut live) = support::scene(concat!(
        "#usda 1.0\n",
        "def Material \"Mat\" {\n",
        "    def Shader \"S\" {}\n",
        "    def Shader \"S3\" { uniform token info:implementationSource = \"sourceAsset\"; }\n",
        "    def Shader \"S4\" {}\n",
        "    def Shader \"S5\" {}\n",
        "    def Shader \"S6\" {}\n",
        "    def Shader \"S7\" { uniform token info:implementationSource = \"sourceAsset\"; }\n",
        "    def \"U\" {}\n",
        "}\n",
        "def PluginLight \"Light\" {}\n",
    ));
    for call in &oracle.calls {
        let prim = store.path(&call.prim);
        let before = store.layer(LayerId(1)).unwrap().clone();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let handle = NodeDefApiEdit::new(&edit, prim).unwrap().unwrap();
        let (value, source_type) = (call.value.as_str(), call.source_type.as_str());
        let result = match call.call.as_str() {
            "set_shader_id" => handle.set_shader_id(&mut edit, value).map(|_| ()),
            "set_id" => {
                handle.set_id(&mut edit, value);
                Ok(())
            }
            "set_source_asset" => handle
                .set_source_asset(&mut edit, value, source_type)
                .map(|_| ()),
            "set_source_code" => handle
                .set_source_code(&mut edit, value, source_type)
                .map(|_| ()),
            "set_source_asset_subidentifier" => handle
                .set_source_asset_subidentifier(&mut edit, value, source_type)
                .map(|_| ()),
            other => panic!("unknown call {other}"),
        };
        let transaction = edit.finish();
        assert_eq!(result.is_ok(), call.accepted, "{} {}", call.call, call.prim);
        if call.accepted {
            live.apply(&mut store, &transaction).unwrap();
            assert_eq!(
                specs(&store, prim),
                call.specs,
                "{} {}",
                call.call,
                call.prim
            );
        } else {
            // C++ `SetShaderId` authors `info:implementationSource` and then
            // fails at the missing `info:id`; a refused edit authors nothing.
            assert_eq!(result, Err(NodeDefError::MissingIdAttribute(prim)));
            assert!(transaction.is_empty());
            assert_eq!(*store.layer(LayerId(1)).unwrap(), before);
        }
    }
    let scene = Scene::new(live.stage(), &store);
    for want in &oracle.edit_reads {
        let got = reads(&scene, &store, want["path"].as_str().unwrap());
        assert_eq!(&got, want);
    }
}

#[test]
fn a_mistyped_source_attribute_is_refused_without_edits() {
    let (mut store, live) = support::scene(concat!(
        "#usda 1.0\n",
        "def Shader \"S\" { uniform token info:osl:sourceAsset = \"a.osl\"; }\n",
    ));
    let prim = store.path("/S");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let handle = NodeDefApiEdit::new(&edit, prim).unwrap().unwrap();
    assert_eq!(
        handle.set_source_asset(&mut edit, "a.osl", "osl").err(),
        Some(NodeDefError::WrongPropertyType {
            prim,
            property: "info:osl:sourceAsset".into()
        })
    );
    assert!(edit.finish().is_empty());
}
