// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `sdrMetadata` API of shaders, inputs and outputs against OpenUSD.
//!
//! `scripts/shade_sdr_metadata_oracle.py` records C++ `GetSdrMetadata`,
//! `GetSdrMetadataByKey`, `HasSdrMetadata` and `HasSdrMetadataByKey` on
//! `fixtures/shade_sdr_metadata/scene.usda`, values of many types
//! stringified, and then calls the setters and clearers one at a time on a
//! new stage, recording the root layer's `sdrMetadata` dictionaries after
//! each. The test reads the scene through `Shader` and `Port` and replays
//! the calls through `ShaderEdit` and `PortEdit`.

use crate::support::schema_scene as support;

use std::collections::BTreeMap;
use std::sync::Arc;

use layerstack::{
    EditTarget, FieldValue, InMemoryStore, LayerId, PropertyPath, Stage, StageOptions, Value,
};
use layerstack_conformance::{usdc::load_entry_usdc, workspace_root};
use layerstack_schemas::shading::{Port, PortEdit};
use layerstack_schemas::usd_shade::{Shader, ShaderEdit};
use layerstack_schemas::{MetadataError, Scene, SchemaEdit};
use serde::Deserialize;
use serde_json::{Map, Value as Json};

const ORACLE: &str = include_str!("../../fixtures/shade_sdr_metadata/oracle.json");
const SCENE: &str = include_str!("../../fixtures/shade_sdr_metadata/scene.usda");

const AUTHORING: &str = r#"#usda 1.0
def Material "Look"
{
    def Shader "S"
    {
        float inputs:x
        token outputs:out
    }
}
"#;

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    reads: BTreeMap<String, Record>,
    calls: Vec<Call>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Record {
    map: BTreeMap<String, String>,
    has: bool,
    by_key: BTreeMap<String, String>,
    has_by_key: BTreeMap<String, bool>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Call {
    call: String,
    dictionaries: BTreeMap<String, Json>,
}

fn shader_record(shader: &Shader<'_>, keys: &BTreeMap<String, String>) -> Record {
    Record {
        map: shader
            .sdr_metadata_strings()
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        has: shader.has_sdr_metadata(),
        by_key: keys
            .keys()
            .map(|key| (key.clone(), shader.sdr_metadata_by_key(key)))
            .collect(),
        has_by_key: keys
            .keys()
            .map(|key| (key.clone(), shader.has_sdr_metadata_by_key(key)))
            .collect(),
    }
}

fn port_record(port: &Port<'_>, keys: &BTreeMap<String, String>) -> Record {
    Record {
        map: port
            .sdr_metadata_strings()
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        has: port.has_sdr_metadata(),
        by_key: keys
            .keys()
            .map(|key| (key.clone(), port.sdr_metadata_by_key(key)))
            .collect(),
        has_by_key: keys
            .keys()
            .map(|key| (key.clone(), port.has_sdr_metadata_by_key(key)))
            .collect(),
    }
}

/// Each read of `expected` against the shader or port at `path`.
fn read(scene: &Scene<'_>, store: &InMemoryStore, path: &str, expected: &Record) -> Record {
    match path.split_once('.') {
        Some(_) => {
            let port = Port::get(scene, store.property_path(path)).expect("a port");
            port_record(&port, &expected.by_key)
        }
        None => {
            let shader = Shader::get(scene, path).expect("a shader");
            shader_record(&shader, &expected.by_key)
        }
    }
}

#[test]
fn sdr_metadata_reads_match_openusd() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, layerstack_schemas::OPENUSD_VERSION);
    let (store, live) = support::scene(SCENE);
    let scene = Scene::new(live.stage(), &store);
    for (path, expected) in oracle.reads.iter().filter(|(path, _)| *path != "/Ops") {
        assert_eq!(&read(&scene, &store, path, expected), expected, "{path}");
    }
}

/// List ops in `sdrMetadata`, which only the binary format holds.
#[test]
fn sdr_metadata_list_ops_read_as_openusd_spells_them() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let mut loaded = load_entry_usdc(
        &workspace_root().join("layerstack_conformance/fixtures/shade_sdr_metadata/listops.usdc"),
    );
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(loaded.store.tokens()))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut loaded.store, loaded.root_layer, options);
    let scene = Scene::new(&stage, &loaded.store);
    let expected = &oracle.reads["/Ops"];
    assert_eq!(&read(&scene, &loaded.store, "/Ops", expected), expected);
}

fn json(value: &Value) -> Json {
    match value {
        Value::String(text) => Json::String(text.to_string()),
        Value::Dictionary(entries) => Json::Object(
            entries
                .iter()
                .map(|(key, value)| {
                    let FieldValue::Value(value) = value else {
                        panic!("{key}: a list op");
                    };
                    (key.to_string(), json(value))
                })
                .collect::<Map<_, _>>(),
        ),
        other => panic!("unexpected {other:?}"),
    }
}

/// The `sdrMetadata` the root layer authors on `/Look/S` and its
/// properties.
fn dictionaries(store: &InMemoryStore) -> BTreeMap<String, Json> {
    let layer = store.layer(LayerId(1)).unwrap();
    let key = store.tokens().lookup("sdrMetadata").unwrap();
    let path = store.path("/Look/S");
    let spec = layer.prim(path).unwrap();
    let mut found = BTreeMap::new();
    if let Some(FieldValue::Value(value)) = layerstack::get_field(&spec.fields, &key) {
        found.insert("/Look/S".to_owned(), json(value));
    }
    for entry in &spec.properties {
        if let Some(FieldValue::Value(value)) = entry.spec.metadata(key) {
            let name = store.tokens().resolve(entry.name);
            found.insert(format!("/Look/S.{name}"), json(value));
        }
    }
    found
}

/// One authoring call, on the shader or one of its ports.
enum Author {
    ShaderSet(&'static [(&'static str, &'static str)]),
    ShaderClearKey(&'static str),
    ShaderClear,
    PortSet(&'static str, &'static [(&'static str, &'static str)]),
    PortClearKey(&'static str, &'static str),
    PortClear(&'static str),
}

#[test]
fn sdr_metadata_authoring_matches_openusd_call_by_call() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    let (mut store, mut live) = support::scene(AUTHORING);
    let shader = store.path("/Look/S");
    use Author::{PortClear, PortClearKey, PortSet, ShaderClear, ShaderClearKey, ShaderSet};
    let calls = [
        ("shader set role", ShaderSet(&[("role", "surface")])),
        ("shader set nested", ShaderSet(&[("page:name", "Base")])),
        (
            "shader set map",
            ShaderSet(&[("role", "texture"), ("page:open", "1")]),
        ),
        ("shader clear nested", ShaderClearKey("page:name")),
        ("shader clear last nested", ShaderClearKey("page:open")),
        ("shader clear missing", ShaderClearKey("missing")),
        ("shader set a:", ShaderSet(&[("a:", "e")])),
        ("shader set x::y", ShaderSet(&[("x::y", "v")])),
        ("shader set :b", ShaderSet(&[(":b", "w")])),
        (
            "shader set map with empty parts",
            ShaderSet(&[(":", "z"), ("page:", "p")]),
        ),
        ("shader clear x::y", ShaderClearKey("x::y")),
        ("shader clear :", ShaderClearKey(":")),
        ("shader clear a:", ShaderClearKey("a:")),
        ("input set label", PortSet("inputs:x", &[("label", "Tint")])),
        (
            "input set map",
            PortSet("inputs:x", &[("help", "A tint"), ("widget:kind", "color")]),
        ),
        (
            "output set",
            PortSet("outputs:out", &[("renderType", "terminal")]),
        ),
        (
            "input clear widget",
            PortClearKey("inputs:x", "widget:kind"),
        ),
        ("output clear", PortClear("outputs:out")),
        ("shader clear", ShaderClear),
        ("input clear label", PortClearKey("inputs:x", "label")),
        ("input clear help", PortClearKey("inputs:x", "help")),
        ("input set :k", PortSet("inputs:x", &[(":k", "1")])),
        ("shader clear empty key", ShaderClearKey("")),
        ("input clear empty key", PortClearKey("inputs:x", "")),
    ];
    assert_eq!(calls.len(), oracle.calls.len());
    for ((label, author), expected) in calls.into_iter().zip(&oracle.calls) {
        assert_eq!(label, expected.call);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let api = ShaderEdit::new(&edit, shader)
            .expect("outside any instance")
            .unwrap();
        let port = |edit: &mut SchemaEdit<'_>, name: &str| {
            let name = edit.tokens().intern(name);
            PortEdit::get(edit, PropertyPath::new(shader, name))
                .expect("outside any instance")
                .expect("a port")
        };
        let result: Result<(), MetadataError> = match author {
            ShaderSet(entries) => api.set_sdr_metadata(&mut edit, entries).map(drop),
            ShaderClearKey(key) => api.clear_sdr_metadata_by_key(&mut edit, key).map(drop),
            ShaderClear => api.clear_sdr_metadata(&mut edit).map(drop),
            PortSet(name, entries) => {
                let port = port(&mut edit, name);
                port.set_sdr_metadata(&mut edit, entries).map(drop)
            }
            PortClearKey(name, key) => {
                let port = port(&mut edit, name);
                port.clear_sdr_metadata_by_key(&mut edit, key).map(drop)
            }
            PortClear(name) => {
                let port = port(&mut edit, name);
                port.clear_sdr_metadata(&mut edit).map(drop)
            }
        };
        result.unwrap_or_else(|error| panic!("{label}: {error}"));
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        assert_eq!(dictionaries(&store), expected.dictionaries, "{label}");
    }
}

/// The setters refuse the empty key before anything is authored: C++
/// replaces the whole dictionary with the string there.
#[test]
fn sdr_metadata_setters_refuse_the_empty_key() {
    let (mut store, live) = support::scene(AUTHORING);
    let shader = store.path("/Look/S");
    let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
    let api = ShaderEdit::new(&edit, shader)
        .expect("outside any instance")
        .unwrap();
    assert!(
        matches!(
            api.set_sdr_metadata_by_key(&mut edit, "", "v"),
            Err(MetadataError::InvalidKeyPath(_))
        ),
        "the empty key"
    );
    assert!(
        api.set_sdr_metadata(&mut edit, &[("role", "surface"), ("", "v")])
            .is_err(),
        "one empty key refuses the map"
    );
    let input = edit.tokens().intern("inputs:x");
    let port = PortEdit::get(&mut edit, PropertyPath::new(shader, input))
        .expect("outside any instance")
        .expect("a port");
    assert!(
        port.set_sdr_metadata_by_key(&mut edit, "", "v").is_err(),
        "the empty key on a port"
    );
    assert!(edit.transaction().is_empty(), "refusals author nothing");
}
