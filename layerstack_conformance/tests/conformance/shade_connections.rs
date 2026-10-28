// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Connection list edits on shading ports against OpenUSD 26.11, call by
//! call: `ConnectToSource` with Replace, Prepend and Append (creating a
//! missing source with the consumer's type) and single-source
//! `DisconnectSource`, authored in a root layer over a weaker one.
use crate::support::schema_scene as support;
use layerstack::{InMemoryStore, LayerId, ListOp, PropertyPath, TargetPath, edit::EditTarget};
use layerstack_schemas::{
    Scene, SchemaEdit,
    shading::{ConnectionModification, PortEdit},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Oracle {
    version: String,
    calls: Vec<Call>,
}
#[derive(Deserialize)]
struct Call {
    call: String,
    consumer: String,
    prim: String,
    name: String,
    mode: Option<String>,
    accepted: bool,
    consumer_spec: Option<Spec>,
    source_spec: Option<Spec>,
    composed: Vec<String>,
}
#[derive(Debug, Deserialize, PartialEq)]
struct Spec {
    #[serde(rename = "type")]
    ty: String,
    custom: bool,
    connections: Option<Connections>,
}
#[derive(Debug, Deserialize, PartialEq)]
struct Connections {
    explicit: bool,
    explicit_items: Vec<String>,
    prepended: Vec<String>,
    appended: Vec<String>,
    deleted: Vec<String>,
}

fn target_text(store: &InMemoryStore, target: &TargetPath) -> String {
    target.display(store.paths(), store.tokens())
}

/// The root layer's spec of `path`, as the oracle records it.
fn spec(store: &InMemoryStore, path: PropertyPath) -> Option<Spec> {
    let prim = store.layer(LayerId(1)).unwrap().prim(path.prim_path())?;
    let entry = prim
        .properties
        .iter()
        .find(|entry| entry.name == path.property())?;
    let texts = |items: &[TargetPath]| items.iter().map(|t| target_text(store, t)).collect();
    let connections = entry
        .spec
        .targets
        .as_deref()
        .map(|op: &ListOp<TargetPath>| Connections {
            explicit: op.explicit.is_some(),
            explicit_items: texts(op.explicit.as_deref().unwrap_or_default()),
            prepended: texts(&op.prepend),
            appended: texts(&op.append),
            deleted: texts(&op.delete),
        });
    Some(Spec {
        ty: entry
            .spec
            .type_name
            .as_ref()
            .map(|ty| ty.type_name().to_string())
            .unwrap_or_default(),
        custom: entry.spec.custom,
        connections,
    })
}

#[test]
fn connection_edits_match_cpp_call_by_call() {
    let oracle: Oracle =
        serde_json::from_str(include_str!("../../fixtures/shade_connections/oracle.json")).unwrap();
    assert_eq!(oracle.version, layerstack_schemas::OPENUSD_VERSION);
    let (mut store, mut live) =
        support::layered_scene(include_str!("../../fixtures/shade_connections/weak.usda"));
    for call in &oracle.calls {
        let consumer = store.property_path(&call.consumer);
        let prim = store.path(&call.prim);
        let source = store.property_path(&format!("{}.{}", call.prim, call.name));
        let before = store.layer(LayerId(1)).unwrap().clone();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let port = PortEdit::get(&mut edit, consumer).unwrap().unwrap();
        let accepted = match call.call.as_str() {
            "connect" => {
                let modification = match call.mode.as_deref() {
                    Some("replace") => ConnectionModification::Replace,
                    Some("prepend") => ConnectionModification::Prepend,
                    Some("append") => ConnectionModification::Append,
                    other => panic!("unknown mode {other:?}"),
                };
                port.connect_to_source(&mut edit, prim, &call.name, modification)
                    .is_ok()
            }
            "disconnect" => {
                let source = PortEdit::get(&mut edit, source).unwrap().unwrap();
                port.disconnect_source(&mut edit, &source).is_ok()
            }
            other => panic!("unknown call {other}"),
        };
        let label = format!("{} {} {}", call.call, call.consumer, call.prim);
        assert_eq!(accepted, call.accepted, "{label}");
        let transaction = edit.finish();
        if accepted {
            live.apply(&mut store, &transaction).unwrap();
        } else {
            assert!(transaction.is_empty(), "{label}");
            assert_eq!(*store.layer(LayerId(1)).unwrap(), before, "{label}");
        }
        assert_eq!(spec(&store, consumer), call.consumer_spec, "{label}");
        assert_eq!(spec(&store, source), call.source_spec, "{label}");
        let scene = Scene::new(live.stage(), &store);
        let composed: Vec<String> = scene
            .connected_sources(consumer)
            .sources
            .iter()
            .map(|path| path.display(store.paths(), store.tokens()))
            .collect();
        assert_eq!(composed, call.composed, "{label}");
    }
}
