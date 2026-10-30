// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Layer muting by identifier, and namespace edits around muted layers,
//! as OpenUSD 26.11 does them.
//!
//! `scripts/layer_muting_oracle.py` records, call by call, a stage on
//! `fixtures/layer_muting/calls/scene.usda` muting and unmuting layers by
//! identifier (some before anything opened them), adding references to
//! them, and the muted set, notices, composed properties and muted-asset
//! errors after each call; and, per case, a `UsdNamespaceEditor` move or
//! delete on `fixtures/layer_muting/namespace/scene.usda` with a layer
//! muted, and every layer's text afterwards. See the script for the 26.11
//! code each shows.
//!
//! Here the calls replay against a `StageDocument` on the filesystem
//! (`StageDocument::layer_named`, `mute_and_unmute_layers`,
//! `muted_identifiers`), and the namespace cases against
//! `NamespaceEdit::prepare` and `prepare_delete` on a `LiveStage` muting
//! the same layers, which must write every layer as OpenUSD does, byte for
//! byte.
//!
//! Two differences are deliberate. A request to mute the root fails as a
//! whole (`LayerMuteError::RootLayer`) and changes nothing, where
//! `PcpCache::RequestLayerMuting` posts a coding error for the root and
//! mutes the request's other layers. And a layer both muted and unmuted
//! in one request under two spellings (`b.usda`, `./b.usda`) ends unmuted
//! in both, but OpenUSD notifies it muted and unmuted, where a document's
//! listeners, which hear the muted set's change, hear nothing.

use std::path::{Path, PathBuf};

use layerstack::{
    CompositionError, EditTarget, LayerId, LayerMuteError, LayerOffset, ListPosition, LiveStage,
    NamespaceEdit, Reference, ReferenceTarget, Specifier, StageOptions, TargetPath, Transaction,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_io::{Filesystem, StageDocument};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/layer_muting/oracle.json");

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn fixtures(directory: &str) -> PathBuf {
    workspace_root()
        .join("layerstack_conformance/fixtures/layer_muting")
        .join(directory)
}

/// `identifier` relative to `directory`, `/` separated, as the oracle
/// records identifiers.
fn relative(directory: &Path, identifier: &str) -> String {
    let identifier = identifier.replace('\\', "/");
    let prefix = format!("{}/", directory.to_str().unwrap().replace('\\', "/"));
    identifier
        .strip_prefix(&prefix)
        .map_or(identifier.clone(), str::to_string)
}

fn strings(json: &Json) -> Vec<String> {
    json.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// The directory of the calls' fixture, absolute, as the filesystem
/// storage names the layers it reads (`Filesystem::new` makes its root
/// absolute against the current directory). Canonical elsewhere, without
/// the verbatim prefix `canonicalize` adds on Windows (`\\?\`), as
/// OpenUSD's paths have none. Under WASI, whose preopened directories
/// have no canonical path, the current directory is the preopened crate
/// directory (see `.cargo/config.toml`), so the fixture is beneath it: an
/// absolute path through `..`, the parent's preopen, would leave both.
fn calls_directory() -> PathBuf {
    if cfg!(target_os = "wasi") {
        return std::env::current_dir()
            .unwrap()
            .join("fixtures/layer_muting/calls");
    }
    let canonical = fixtures("calls").canonicalize().unwrap();
    match canonical.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => canonical,
    }
}

struct Calls {
    directory: PathBuf,
    doc: StageDocument<Filesystem>,
}

impl Calls {
    fn open() -> Self {
        let directory = calls_directory();
        let storage = Filesystem::new(&directory, []).unwrap();
        let root = directory.join("scene.usda");
        let doc =
            StageDocument::open(storage, root.to_str().unwrap(), StageOptions::default()).unwrap();
        Self { directory, doc }
    }

    fn muted(&self) -> Vec<String> {
        self.doc
            .muted_identifiers()
            .into_iter()
            .map(|identifier| relative(&self.directory, identifier))
            .collect()
    }

    fn ids(&mut self, identifiers: &Json) -> Vec<LayerId> {
        strings(identifiers)
            .iter()
            .filter(|identifier| !identifier.is_empty())
            .map(|identifier| self.doc.layer_named(identifier).unwrap())
            .collect()
    }

    /// As `UsdStage::MuteAndUnmuteLayers`: an unmute request spelled as a
    /// mute request is dropped before either is anchored.
    fn mute_and_unmute(&mut self, mute: &Json, unmute: &Json) -> Result<bool, LayerMuteError> {
        let spelled = strings(mute);
        let unmute = Json::Array(
            strings(unmute)
                .into_iter()
                .filter(|identifier| !spelled.contains(identifier))
                .map(Json::String)
                .collect(),
        );
        let mute = self.ids(mute);
        let unmute = self.ids(&unmute);
        // Different spellings of one layer: muted, then unmuted.
        let mute: Vec<LayerId> = mute.into_iter().filter(|id| !unmute.contains(id)).collect();
        let changed = self.doc.mute_and_unmute_layers(&mute, &unmute)?;
        self.doc.load_requested_assets().unwrap();
        Ok(changed)
    }

    fn reference(&mut self, prim: &str, asset: &str, target: &str) {
        let root = self.doc.root_layer();
        let prim = self.doc.store_mut().path(prim);
        let target = self.doc.store_mut().path(target);
        let at = EditTarget::for_layer(root);
        let mut edit = Transaction::new();
        let defined = self
            .doc
            .store()
            .layer(root)
            .is_some_and(|layer| layer.prim(prim).is_some());
        if !defined {
            edit.create_prim(at.prim(prim), Specifier::Def, None);
        }
        edit.add_reference(
            at.prim(prim),
            Reference::deferred(
                root,
                asset,
                ReferenceTarget::Prim(target),
                LayerOffset::IDENTITY,
            ),
            ListPosition::BackOfPrepend,
        );
        self.doc.apply(&edit).unwrap();
    }

    fn properties(&self, path: &str) -> Json {
        let store = self.doc.store();
        let view = self.doc.view();
        match view.prim(store.path(path)) {
            None => Json::Null,
            Some(prim) => {
                let mut names: Vec<String> = prim
                    .property_names()
                    .iter()
                    .map(|name| name.to_string())
                    .collect();
                names.sort();
                json!(names)
            }
        }
    }

    fn muted_assets(&self) -> Json {
        let store = self.doc.store();
        let mut rows: Vec<Json> = self
            .doc
            .stage()
            .stage()
            .composition_errors()
            .iter()
            .filter_map(|error| match error {
                CompositionError::MutedAsset(error) => Some(json!([
                    store.paths().display(error.prim, store.tokens()),
                    relative(&self.directory, self.doc.identifier(error.layer).unwrap()),
                    relative(
                        &self.directory,
                        self.doc.identifier(error.introducing_layer).unwrap()
                    ),
                ])),
                _ => None,
            })
            .collect();
        rows.sort_by_key(ToString::to_string);
        rows.dedup();
        Json::Array(rows)
    }

    fn holds(&self, name: &str) -> bool {
        let identifier = self.directory.join(name);
        self.doc
            .store()
            .layer_ids()
            .into_iter()
            .any(|id| self.doc.identifier(id) == identifier.to_str())
    }
}

#[test]
fn muting_by_identifier_replays_openusd_call_by_call() {
    let oracle = oracle();
    let mut calls = Calls::open();
    for step in oracle["calls"].as_array().unwrap() {
        let call = &step["call"];
        let method = call[0].as_str().unwrap();
        let before = calls.muted();
        match method {
            "is_muted" => {
                let identifier = call[1].as_str().unwrap();
                let id = calls.doc.find_layer_named(identifier).unwrap();
                let muted = id.is_some_and(|id| calls.doc.stage().stage().is_layer_muted(id));
                assert_eq!(json!(muted), step["result"], "{call}");
            }
            "mute" | "unmute" | "mute_and_unmute" => {
                let (mute, unmute) = match method {
                    "mute" => (json!([call[1]]), json!([])),
                    "unmute" => (json!([]), json!([call[1]])),
                    _ => (call[1].clone(), call[2].clone()),
                };
                let result = calls.mute_and_unmute(&mute, &unmute);
                if step["raised"].as_bool().unwrap() {
                    // The root: refused as a whole (see the module docs).
                    assert_eq!(result, Err(LayerMuteError::RootLayer), "{call}");
                    assert_eq!(calls.muted(), before, "{call}: nothing changes");
                    let others: Vec<LayerId> = calls
                        .ids(&mute)
                        .into_iter()
                        .filter(|id| *id != calls.doc.root_layer())
                        .collect();
                    calls.doc.mute_and_unmute_layers(&others, &[]).unwrap();
                } else {
                    result.unwrap();
                }
            }
            "reference" => calls.reference(
                call[1].as_str().unwrap(),
                call[2].as_str().unwrap(),
                call[3].as_str().unwrap(),
            ),
            other => panic!("unknown call {other}"),
        }
        assert_eq!(
            json!(calls.muted()),
            step["muted"],
            "{call}: GetMutedLayers"
        );
        let heard = step["heard"].clone();
        let after = calls.muted();
        let net = json!([
            after
                .iter()
                .filter(|layer| !before.contains(layer))
                .collect::<Vec<_>>(),
            before
                .iter()
                .filter(|layer| !after.contains(layer))
                .collect::<Vec<_>>(),
        ]);
        let spelled_twice = heard[0].as_array().is_some_and(|muted| {
            muted
                .iter()
                .any(|layer| heard[1].as_array().unwrap().contains(layer))
        });
        if heard.is_null() || spelled_twice {
            assert_eq!(net, json!([[], []]), "{call}: no change heard");
        } else {
            assert_eq!(net, heard, "{call}: LayerMutingChanged");
        }
        for path in ["/Ball", "/Lamp", "/Robot"] {
            assert_eq!(
                calls.properties(path),
                step["properties"][path],
                "{call}: {path}"
            );
        }
        assert_eq!(calls.muted_assets(), step["muted_assets"], "{call}");
        let opened = strings(&step["open"]).contains(&"lamp.usda".to_string());
        assert_eq!(
            calls.holds("lamp.usda"),
            opened,
            "{call}: a layer muted before anything read it is not read"
        );
    }
}

#[test]
fn namespace_edits_leave_muted_layers_as_openusd_does() {
    let oracle = oracle();
    let directory = fixtures("namespace");
    for case in oracle["namespace"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load_entry_usda(&directory.join("scene.usda"));
        assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
        let named = |loaded: &layerstack_conformance::usda_real::LoadedStage, file: &str| {
            *loaded
                .layer_names
                .iter()
                .find(|(_, path)| {
                    let path = path.replace('\\', "/");
                    path == file || path.ends_with(&format!("/{file}"))
                })
                .unwrap()
                .0
        };
        let muted = strings(&case["muted"])
            .iter()
            .map(|file| named(&loaded, file))
            .collect();
        let options = StageOptions {
            muted_layers: muted,
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options);
        let ids: Vec<(String, LayerId)> = case["layers"]
            .as_object()
            .unwrap()
            .keys()
            .map(|file| (file.clone(), named(&loaded, file)))
            .collect();
        let store = &mut loaded.store;
        let target = EditTarget::for_layer(loaded.root_layer);
        let source = TargetPath::Prim(store.path(case["path"].as_str().unwrap()));
        let prepared = match case["destination"].as_str() {
            Some(to) => {
                let destination = TargetPath::Prim(store.path(to));
                NamespaceEdit::prepare(live.stage(), store, &target, source, destination)
            }
            None => NamespaceEdit::prepare_delete(live.stage(), store, &target, source),
        };
        if !case["can_apply"].as_bool().unwrap() {
            assert!(prepared.is_err(), "{name}: OpenUSD refuses it");
            continue;
        }
        let edit = prepared.unwrap_or_else(|e| panic!("{name}: {e}"));
        live.apply(store, edit.transaction())
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        for (file, id) in &ids {
            let text = &case["layers"][file];
            let layer = store.layer(*id).unwrap();
            let written =
                layerstack_usda::save_usda(layer, store.tokens(), store.paths()).expect("saves");
            assert_eq!(written, text.as_str().unwrap(), "{name}: {file}");
        }
        let errors = live
            .stage()
            .composition_errors()
            .iter()
            .filter(|error| matches!(error, CompositionError::MutedAsset(_)))
            .count();
        assert_eq!(
            errors,
            case["muted_assets"].as_array().unwrap().len(),
            "{name}: muted assets"
        );
    }
}
