// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asset paths anchored as OpenUSD anchors them when it flattens a stage
//! read from files, and the asset paths of stage metadata anchored to the
//! layer that authors each, replayed from `flatten_anchoring_oracle.py`
//! (see the script for the scene and what it records).
//!
//! The root case flattens through the document's own resolver
//! ([`StageDocument::flatten`], [`StageDocument::layer_locations`]). A
//! session layer the host reads itself has no location in the document, so
//! the session cases flatten through a resolver that knows each layer's
//! file, anchoring as the document's does
//! ([`layerstack::asset::create_identifier`]).
//!
//! OpenUSD: `UsdStage::Flatten`, `SdfAnchorAssetPaths` and
//! `ArDefaultResolver::_CreateIdentifier`; `UsdStage::GetMetadata`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use layerstack::stage::flatten::{AssetPaths, FlattenRequirements};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, LayerStore, NoAssets, PathInterner,
    ResolvedAsset, Stage, StageOptions, TokenInterner, Value,
};
use layerstack_io::{Filesystem, OpenOptions, StageDocument};
use serde_json::{Map, Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/flatten_anchoring/oracle.json");

/// The session layer of each case, by its file in the fixture.
const CASES: [(&str, Option<&str>); 3] = [
    ("root", None),
    ("session", Some("sess/session.usda")),
    ("rate", Some("rate/rate.usda")),
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

/// The id a host-owned session layer takes.
const SESSION: LayerId = LayerId(1);

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

/// The fixture's stage, opened from `root.usda` with `session`, read by
/// the host, as its session layer.
fn open(session: Option<&str>) -> StageDocument<Filesystem> {
    let directory = directory();
    let storage = Filesystem::new(&directory, []).unwrap();
    let root = directory.join("root.usda");
    let mut store = InMemoryStore::default();
    if let Some(session) = session {
        let text = std::fs::read_to_string(directory.join(session)).unwrap();
        let read = layerstack_usda::read_usda(
            &text,
            SESSION,
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        assert!(read.parse_diagnostics.is_empty(), "{session}");
        store.insert_layer(read.emitted.layer);
    }
    let options = OpenOptions {
        stage: StageOptions {
            session_layer: session.map(|_| SESSION),
            ..StageOptions::default()
        },
        store: Some(store),
        ..OpenOptions::default()
    };
    StageDocument::open(storage, root.to_str().unwrap(), options).unwrap()
}

/// Knows the file of each layer of `document`, and of its session layer
/// `session`, and anchors as the document's resolver does.
struct Located {
    layers: HashMap<LayerId, String>,
}

impl Located {
    fn new(document: &StageDocument<Filesystem>, session: Option<&str>) -> Self {
        let mut layers: HashMap<LayerId, String> = document
            .store()
            .layer_ids()
            .into_iter()
            .filter_map(|id| Some((id, document.identifier(id)?.to_owned())))
            .collect();
        if let Some(session) = session {
            layers.insert(SESSION, slashed(&directory().join(session)));
        }
        Self { layers }
    }
}

impl AssetResolver for Located {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::Unavailable)
    }

    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.layers.get(&id).map(String::as_str)
    }

    fn anchor_asset_path(&self, asset_path: &str, anchor: LayerId) -> Option<String> {
        let location = self.resolved_path(anchor)?;
        layerstack::asset::create_identifier(asset_path, location, |identifier| {
            Path::new(identifier).is_file()
        })
    }
}

/// `layer` saved as USDA, with the fixture directory spelled `<dir>`.
fn text(layer: &layerstack::Layer, store: &InMemoryStore) -> String {
    let text = layerstack_usda::save_usda(layer, store.tokens(), store.paths()).expect("saves");
    text.replace(&format!("{}/", slashed(&directory())), "<dir>/")
}

/// The case's stage flattened under `requirements`: through the
/// document's resolver when it has no session layer.
fn flatten(case: Option<&str>, requirements: &FlattenRequirements) -> String {
    let mut document = open(case);
    if case.is_none() {
        let flat = document.flatten(requirements).expect("flattens");
        return text(&flat.layer, document.store());
    }
    let located = Located::new(&document, case);
    let root = document.root_layer();
    let (store, live) = document.parts_mut();
    let flat = live
        .stage()
        .flatten(store, &located, root, LayerId(999_999), requirements)
        .expect("flattens");
    text(&flat.layer, store)
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
        let located = Located::new(&document, session);
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
            if session.is_none() {
                // The document's own resolver anchors the same.
                assert_eq!(
                    stage.layer_metadata_anchored(key, store, &document.layer_locations()),
                    Some(anchored),
                    "{name} {field}"
                );
            }
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
