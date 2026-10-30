// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset paths anchored as OpenUSD anchors them when it flattens a stage
//! read from files, and the asset paths of stage metadata anchored to the
//! layer that authors each, replayed from `flatten_anchoring_oracle.py`
//! (see the script for the scene and what it records).
//!
//! Every case opens its session layer from the fixture
//! ([`OpenOptions::session_layer`]), as `UsdStage::Open(rootLayer,
//! sessionLayer)` does, and flattens through the document's own resolver
//! ([`StageDocument::flatten`], [`StageDocument::layer_locations`]).
//!
//! OpenUSD: `UsdStage::Flatten`, `SdfAnchorAssetPaths` and
//! `ArDefaultResolver::_CreateIdentifier`; `UsdStage::GetMetadata`.

use std::path::{Path, PathBuf};

use layerstack::stage::flatten::{AssetPaths, FlattenRequirements};
use layerstack::{InMemoryStore, LayerStore, Stage, Value};
use layerstack_io::{Filesystem, OpenOptions, StageDocument};
use serde_json::{Map, Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/flatten_anchoring/oracle.json");

/// The session layer of each case, by its file in the fixture.
const CASES: [(&str, Option<&str>); 5] = [
    ("root", None),
    ("session", Some("sess/session.usda")),
    ("rate", Some("rate/rate.usda")),
    ("session_sublayers", Some("stack/session.usda")),
    ("package", Some("pkg/session.usda")),
];

/// The stage metadata fields the oracle reads.
const FIELDS: [&str; 7] = [
    "customLayerData",
    "defaultPrim",
    "upAxis",
    "startTimeCode",
    "endTimeCode",
    "framesPerSecond",
    "timeCodesPerSecond",
];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

/// The fixture directory, absolute, as the filesystem storage names the
/// layers it reads. Canonical elsewhere, without the verbatim prefix
/// `canonicalize` adds on Windows (`\\?\`), as OpenUSD's paths have none.
/// Under WASI, whose preopened directories have no canonical path, the
/// current directory is the preopened crate directory.
fn directory() -> PathBuf {
    if cfg!(target_os = "wasi") {
        return std::env::current_dir()
            .unwrap()
            .join("fixtures/flatten_anchoring");
    }
    let canonical = layerstack_conformance::workspace_root()
        .join("layerstack_conformance/fixtures/flatten_anchoring")
        .canonicalize()
        .unwrap();
    match canonical.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => canonical,
    }
}

/// `path` with `/` separators, as anchored asset paths spell it.
fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The fixture's stage, opened from `root.usda` with `session` read from
/// the fixture as its session layer.
fn open(session: Option<&str>) -> StageDocument<Filesystem> {
    let directory = directory();
    let storage = Filesystem::new(&directory, []).unwrap();
    let root = directory.join("root.usda");
    let options = OpenOptions {
        session_layer: session.map(|session| slashed(&directory.join(session))),
        ..OpenOptions::default()
    };
    StageDocument::open(storage, root.to_str().unwrap(), options).unwrap()
}

/// `layer` saved as USDA, with the fixture directory spelled `<dir>`.
fn text(layer: &layerstack::Layer, store: &InMemoryStore) -> String {
    let text = layerstack_usda::save_usda(layer, store.tokens(), store.paths()).expect("saves");
    text.replace(&format!("{}/", slashed(&directory())), "<dir>/")
}

/// The case's stage flattened under `requirements`, through the
/// document's resolver.
fn flatten(case: Option<&str>, requirements: &FlattenRequirements) -> String {
    let mut document = open(case);
    let flat = document.flatten(requirements).expect("flattens");
    text(&flat.layer, document.store())
}

/// Each case flattens to OpenUSD's text, every asset path anchored as
/// OpenUSD anchors it: a search path only when its file is beside the
/// layer that authors it (`textures/beside.png` beside the root, but not
/// beside the asset that also authors it), and the stage metadata only
/// where the root or session layer authors it (no `timeCodesPerSecond`
/// unless one does; nothing from the root's sublayer).
#[test]
fn flattening_anchors_the_asset_paths_openusd_anchors() {
    let oracle = oracle();
    for (name, session) in CASES {
        let text = flatten(session, &FlattenRequirements::default());
        let expected = oracle[name]["flatten"].as_str().unwrap();
        assert_eq!(text, expected, "{name}");
    }
}

/// Keeping asset paths as authored is still an explicit choice.
#[test]
fn flattening_keeps_asset_paths_as_authored_on_request() {
    let requirements = FlattenRequirements {
        asset_paths: AssetPaths::AsAuthored,
        ..FlattenRequirements::default()
    };
    let text = flatten(None, &requirements);
    assert!(!text.contains("<dir>"), "{text}");
    for authored in [
        "asset beside = @textures/beside.png@",
        "asset own = @./root.png@",
        "asset tex = @./tex.png@",
        "asset rootTex = @./root.png@",
    ] {
        assert!(text.contains(authored), "{authored} in {text}");
    }
}

/// `value` as the oracle encodes it, each asset path as its authored
/// spelling and `anchored`'s at the same place.
fn encode(value: &Value, anchored: &Value, store: &dyn LayerStore) -> Json {
    match (value, anchored) {
        (Value::Asset(authored), Value::Asset(anchored)) => {
            let anchored = anchored.replace(&format!("{}/", slashed(&directory())), "<dir>/");
            json!({"authored": &**authored, "resolved": anchored})
        }
        (Value::Dictionary(entries), Value::Dictionary(anchored)) => {
            let encoded: Map<String, Json> = entries
                .iter()
                .zip(anchored)
                .map(|((key, value), (_, anchored))| {
                    let (value, anchored) =
                        (value.as_value().unwrap(), anchored.as_value().unwrap());
                    (key.to_string(), encode(value, anchored, store))
                })
                .collect();
            json!({ "dictionary": encoded })
        }
        (Value::Double(value), _) => json!(value),
        (Value::Token(token), _) => json!(store.tokens().resolve(*token)),
        (Value::String(text), _) => json!(&**text),
        (value, anchored) => {
            let (items, anchored) = (value.array_ref().unwrap(), anchored.array_ref().unwrap());
            Json::Array(
                items
                    .iter()
                    .zip(anchored.iter())
                    .map(|(value, anchored)| encode(value.as_ref(), anchored.as_ref(), store))
                    .collect(),
            )
        }
    }
}

/// The oracle's `value`, with each asset path OpenUSD did not find (an
/// empty `resolvedPath`) given the identifier OpenUSD's flatten of the
/// same stage writes for it, from `flatten`: an expression that evaluates
/// to nothing as authored.
fn identifiers(value: &Json, key: &str, flatten: &str) -> Json {
    match value {
        Json::Object(object) if object.contains_key("authored") => {
            let mut object = object.clone();
            // An expression's evaluation shows in the identifier.
            object.remove("evaluated");
            if object["resolved"] == "" {
                let line = format!("asset {key} = @");
                let start = flatten.find(&line).expect(key) + line.len();
                let end = start + flatten[start..].find('@').unwrap();
                object.insert("resolved".into(), json!(&flatten[start..end]));
            }
            Json::Object(object)
        }
        Json::Object(object) => {
            let entries = object["dictionary"].as_object().unwrap();
            json!({
                "dictionary": entries
                    .iter()
                    .map(|(key, value)| (key.clone(), identifiers(value, key, flatten)))
                    .collect::<Map<String, Json>>()
            })
        }
        Json::Array(items) => Json::Array(
            items
                .iter()
                .map(|item| identifiers(item, key, flatten))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Each stage metadata field reads as `UsdStage::GetMetadata` reads it:
/// from the session layer over the root, never from a sublayer, with a
/// fallback where neither authors it, and each asset path anchored to the
/// layer that authors it (the root's beneath the session's in a merged
/// dictionary), as OpenUSD resolves it. Where OpenUSD finds no file it
/// resolves to nothing; the identifier is the one its flatten writes.
#[test]
fn stage_metadata_asset_paths_anchor_to_the_layer_authoring_them() {
    let oracle = oracle();
    for (name, session) in CASES {
        let document = open(session);
        let located = document.layer_locations();
        let stage: &Stage = document.stage().stage();
        let store = document.store();
        let flatten = oracle[name]["flatten"].as_str().unwrap();
        for field in FIELDS {
            let expected = &oracle[name]["metadata"][field];
            let key = store.tokens().intern(field);
            assert_eq!(
                stage.has_authored_metadata(key, store),
                expected["authored"].as_bool().unwrap(),
                "{name} {field}"
            );
            let authored = stage.layer_metadata(key, store).expect(field);
            let anchored = stage
                .layer_metadata_anchored(key, store, &located)
                .expect(field);
            assert_eq!(
                encode(&authored, &anchored, store),
                identifiers(&expected["value"], field, flatten),
                "{name} {field}"
            );
        }
        let default_prim = document
            .view()
            .default_prim()
            .map(|prim| store.paths().display(prim.path(), store.tokens()));
        assert_eq!(
            default_prim.as_deref(),
            oracle[name]["default_prim"].as_str(),
            "{name}"
        );
    }
}
