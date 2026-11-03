// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reloading a stage or its layers after their files change, as OpenUSD
//! 26.11 reloads them: unchanged layers are not read, changed ones publish
//! what they changed, and the stage's notices name only that.
//!
//! `scripts/layer_reload_oracle.py` records, per case, a stage on
//! `fixtures/layer_reload/layers/root.usda` whose files are rewritten or
//! created, whose layers are edited in memory, muted or unmuted, and which
//! is reloaded (`UsdStage::Reload`) or has one layer reloaded
//! (`SdfLayer::Reload`, forced or not). After each step it records the
//! composed values, the stage's prims, the layers it uses (dirty or not,
//! and where they resolve) and the notices. See the script for the 26.11
//! code each shows.
//!
//! Here each case replays against a `StageDocument` on in-memory files
//! with a search path (`first/`, then `second/`), twice: once with storage
//! reporting versions ([`Storage::version`], moved by every write, as a
//! file's modification time is), once with storage reporting none, where
//! the document compares the bytes it reads instead. `UsdStage::Reload` is
//! `StageDocument::reload` with `ReloadPolicy::DiscardDirty`, OpenUSD's
//! only behaviour; `SdfLayer::Reload` is `reload_layers`, forced
//! `force_reload_layers`. A layer reload "returns true" when it read the
//! layer ([`LoadReport::layers`]).
//!
//! Notices map as `property_resync.rs` maps them: resynced prims and
//! properties are resynced paths; an info-only prim with a property
//! inventory is its properties' paths, one without is its own path. The
//! notices of one step are merged, as one `SdfChangeBlock` merges them:
//! layerstack publishes a reload in one notice, but a deferred arc whose
//! missing asset now resolves in a second.
//!
//! A layer that does not parse keeps its content while the others reload,
//! as in OpenUSD, which raises the parse error after the reload; layerstack
//! returns it among the report's `asset_failures` (`parse_failure`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use layerstack::{EditTarget, LayerId, Transaction, Value};
use layerstack_io::{
    IoError, IoErrorKind, LoadReport, ReloadPolicy, SourceVersion, StageDocument, Storage,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/layer_reload/oracle.json");

/// The fixture's layers, as the oracle wrote them.
const LAYERS: [(&str, &str); 6] = [
    (
        "root.usda",
        include_str!("../../fixtures/layer_reload/layers/root.usda"),
    ),
    (
        "sub.usda",
        include_str!("../../fixtures/layer_reload/layers/sub.usda"),
    ),
    (
        "ref.usda",
        include_str!("../../fixtures/layer_reload/layers/ref.usda"),
    ),
    (
        "other.usda",
        include_str!("../../fixtures/layer_reload/layers/other.usda"),
    ),
    (
        "extra.usda",
        include_str!("../../fixtures/layer_reload/layers/extra.usda"),
    ),
    (
        "second/asset.usda",
        include_str!("../../fixtures/layer_reload/layers/second/asset.usda"),
    ),
];

/// The search path, as the oracle sets it.
const SEARCH: [&str; 2] = ["first", "second"];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    assert_eq!(oracle["search_path"], json!(SEARCH), "the search path");
    oracle
}

/// The case's files, in memory: each with a revision that every write
/// moves, reported as its version when `versions` is set, and the
/// identifiers read, in order.
#[derive(Debug, Default)]
struct Files {
    files: BTreeMap<String, (Vec<u8>, u64)>,
    versions: bool,
    reads: Mutex<Vec<String>>,
}

impl Files {
    fn exists(&self, identifier: &str) -> bool {
        self.files.contains_key(identifier)
    }
}

impl Storage for Files {
    /// An asset relative to its anchor's directory when it is there, as
    /// `ArDefaultResolver` looks first, then in the search path's
    /// directories, in order; `./` and `../` assets only relative.
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        let directory = anchor
            .and_then(|anchor| anchor.rsplit_once('/'))
            .map(|(directory, _)| directory);
        let join = |directory: Option<&str>, asset: &str| match directory {
            Some(directory) => format!("{directory}/{asset}"),
            None => asset.to_owned(),
        };
        if let Some(relative) = asset.strip_prefix("./") {
            return Ok(join(directory, relative));
        }
        let anchored = join(directory, asset);
        if self.exists(&anchored) {
            return Ok(anchored);
        }
        Ok(SEARCH
            .iter()
            .map(|search| format!("{search}/{asset}"))
            .find(|candidate| self.exists(candidate))
            .unwrap_or(anchored))
    }

    fn version(&self, identifier: &str) -> Result<Option<SourceVersion>, IoError> {
        let file = self
            .files
            .get(identifier)
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, "no such file"))?;
        Ok(self
            .versions
            .then(|| SourceVersion::new(file.1.to_string())))
    }

    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.reads.lock().unwrap().push(identifier.to_owned());
        self.files
            .get(identifier)
            .map(|(bytes, _)| bytes.clone())
            .ok_or_else(|| IoError::new(IoErrorKind::NotFound, "no such file"))
    }

    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        let revision = self.files.get(identifier).map_or(0, |(_, r)| r + 1);
        self.files
            .insert(identifier.to_owned(), (bytes.to_vec(), revision));
        Ok(())
    }
}

/// The notices a stage sent, as the oracle records them.
type Heard = Arc<Mutex<Vec<(BTreeSet<String>, BTreeSet<String>)>>>;

struct Case {
    doc: StageDocument<Files>,
    heard: Heard,
}

impl Case {
    fn open(versions: bool) -> Self {
        let files = Files {
            files: LAYERS
                .iter()
                .map(|(name, text)| ((*name).to_owned(), (text.as_bytes().to_vec(), 0)))
                .collect(),
            versions,
            reads: Mutex::default(),
        };
        let mut doc =
            StageDocument::open(files, "root.usda", layerstack::StageOptions::default()).unwrap();
        let heard: Heard = Arc::default();
        let sink = Arc::clone(&heard);
        let (_, live) = doc.parts_mut();
        live.subscribe_changes(move |notice| {
            let (paths, tokens) = (notice.store.paths(), notice.store.tokens());
            let changes = notice.changes;
            let resynced: BTreeSet<String> = changes
                .resynced
                .iter()
                .map(|p| paths.display(*p, tokens))
                .chain(
                    changes
                        .resynced_properties
                        .iter()
                        .map(|p| p.display(paths, tokens)),
                )
                .collect();
            let mut info = BTreeSet::new();
            for &prim in &changes.changed_info_only {
                match changes.properties_for(prim) {
                    Some(fields) if !fields.is_empty() => {
                        for field in fields {
                            info.insert(format!(
                                "{}.{}",
                                paths.display(prim, tokens),
                                tokens.resolve(field.name)
                            ));
                        }
                    }
                    _ => {
                        info.insert(paths.display(prim, tokens));
                    }
                }
            }
            if !resynced.is_empty() || !info.is_empty() {
                sink.lock().unwrap().push((resynced, info));
            }
        });
        Self { doc, heard }
    }

    /// The step's notices, merged as one change block merges them: a path
    /// resynced in one is not info-only in another, nor is a path beneath
    /// a resynced one.
    fn take_heard(&self) -> Json {
        let heard = std::mem::take(&mut *self.heard.lock().unwrap());
        if heard.is_empty() {
            return json!([]);
        }
        let mut resynced = BTreeSet::new();
        let mut info = BTreeSet::new();
        for (r, i) in heard {
            resynced.extend(r);
            info.extend(i);
        }
        let beneath = |path: &str, roots: &BTreeSet<String>| {
            roots.iter().any(|root| {
                path == root
                    || (root != "/"
                        && (path.starts_with(&format!("{root}/"))
                            || path.starts_with(&format!("{root}."))))
            })
        };
        info.retain(|path| !beneath(path, &resynced));
        json!([{"resynced": resynced, "info": covered(info)}])
    }

    fn value(&mut self, path: &str) -> Json {
        let path = self.doc.store_mut().property_path(path);
        self.doc
            .view()
            .attribute(path)
            .and_then(|attribute| attribute.value::<f64>(layerstack::Time::Default))
            .map_or(Json::Null, |value| json!(value))
    }

    fn prims(&self) -> Json {
        let stage = self.doc.stage().stage();
        let (paths, tokens) = (self.doc.store().paths(), self.doc.store().tokens());
        let root = paths.lookup(&layerstack::Path::root()).unwrap();
        let mut prims: Vec<String> = stage
            .traverse_all(root)
            .filter(|path| *path != root)
            .map(|path| paths.display(path, tokens))
            .collect();
        prims.sort();
        json!(prims)
    }

    /// The file layers the stage uses, by where they resolve, with whether
    /// each is dirty.
    fn layers(&self) -> Json {
        let mut layers = BTreeMap::new();
        for id in self.doc.stage().stage().used_layers(false) {
            if let Some(identifier) = self.doc.identifier(id) {
                layers.insert(identifier.to_owned(), self.doc.is_dirty(id));
            }
        }
        json!(layers)
    }

    fn layer(&self, name: &str) -> LayerId {
        self.doc
            .find_layer_named(name)
            .unwrap()
            .or_else(|| {
                // A search-path asset, by the name it was asked for.
                let resolved = self.doc.storage().identify(name, Some("root.usda")).ok()?;
                self.doc.find_layer_named(&resolved).ok().flatten()
            })
            .or_else(|| {
                SEARCH.iter().find_map(|search| {
                    self.doc
                        .find_layer_named(&format!("{search}/{name}"))
                        .ok()
                        .flatten()
                })
            })
            .unwrap_or_else(|| panic!("no layer {name}"))
    }

    /// Takes `step`; `Some(read)` for a layer reload, whether it read the
    /// layer, and whether it failed.
    fn take_step(&mut self, step: &[&str], rewrites: &Json) -> (Option<bool>, bool) {
        let reloaded = |report: &LoadReport, layer| report.layers.contains(&layer);
        match *step {
            ["rewrite" | "create", file, key] => {
                let text = rewrites[key].as_str().unwrap();
                self.doc.storage_mut().write(file, text.as_bytes()).unwrap();
            }
            ["create_search", directory, file, key] => {
                let text = rewrites[key].as_str().unwrap();
                let path = format!("{directory}/{file}");
                self.doc
                    .storage_mut()
                    .write(&path, text.as_bytes())
                    .unwrap();
            }
            ["reload"] => {
                let report = self.doc.reload(ReloadPolicy::DiscardDirty).unwrap();
                // An arc to a missing asset, read again, is reported too.
                let failed = report.asset_failures.iter().any(|f| !f.is_missing());
                return (None, failed);
            }
            ["layer_reload", name] => {
                let layer = self.layer(name);
                let report = self
                    .doc
                    .reload_layers(&[layer], ReloadPolicy::DiscardDirty)
                    .unwrap();
                return (Some(reloaded(&report, layer)), false);
            }
            ["layer_reload_force", name] => {
                let layer = self.layer(name);
                let report = self
                    .doc
                    .force_reload_layers(&[layer], ReloadPolicy::DiscardDirty)
                    .unwrap();
                return (Some(reloaded(&report, layer)), false);
            }
            ["mute", name] => {
                let layer = self.doc.layer_named(name).unwrap();
                self.doc.mute_and_unmute_layers(&[layer], &[]).unwrap();
            }
            ["unmute", name] => {
                let layer = self.doc.layer_named(name).unwrap();
                self.doc.mute_and_unmute_layers(&[], &[layer]).unwrap();
                self.doc.load_requested_assets().unwrap();
            }
            _ => panic!("unknown step {step:?}"),
        }
        (None, false)
    }

    fn set(&mut self, name: &str, attribute: &str, value: f64) {
        let layer = self.layer(name);
        let property = self.doc.store_mut().property_path(attribute);
        let mut edit = Transaction::new();
        edit.set_default(
            EditTarget::for_layer(layer).property(property),
            Value::Double(value),
        );
        self.doc.apply(&edit).unwrap();
    }
}

/// `info` without the properties of the prims it lists: a prim's
/// info-only entry with no property inventory covers its properties. A
/// prim composing a reloaded layer (`/R` for `ref.usda`) is such an entry
/// here, where OpenUSD lists its changed properties beside it.
fn covered(info: BTreeSet<String>) -> BTreeSet<String> {
    info.iter()
        .filter(|path| {
            path.rsplit_once('.')
                .is_none_or(|(prim, _)| prim.is_empty() || !info.contains(prim))
        })
        .cloned()
        .collect()
}

/// The oracle's record of a step, in the shape [`Case`] records it: the
/// layers by where they resolve, with whether each is dirty.
fn expected(step: &Json) -> Json {
    let layers: BTreeMap<String, bool> = step["layers"]
        .as_object()
        .unwrap()
        .values()
        .map(|layer| {
            (
                layer["resolved"].as_str().unwrap().to_owned(),
                layer["dirty"].as_bool().unwrap(),
            )
        })
        .collect();
    let mut expected = json!({
        "values": step["values"],
        "prims": step["prims"],
        "layers": layers,
        "heard": step["heard"]
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| {
                let info: BTreeSet<String> = notice["info"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|path| path.as_str().unwrap().to_owned())
                    .collect();
                json!({"resynced": notice["resynced"], "info": covered(info)})
            })
            .collect::<Vec<_>>(),
    });
    if let Some(returned) = step.get("returned") {
        expected["returned"] = returned.clone();
    }
    expected
}

/// What layerstack announces for a case's step where OpenUSD announces
/// otherwise: each a deliberate difference.
///
/// `sublayer_added` (OpenUSD bug B037): the sublayer `extra.usda` holds an `over`
/// of `/A`, which moves `/A` before `/B` and `/S` among the pseudo-root's
/// children (children compose weakest layer first). OpenUSD changes `/A`
/// info-only and resyncs `/A.e`, and its stage keeps the old order
/// (`UsdStage` composes a parent's children again only for a resynced
/// child), so a stage opened afresh on the same layers orders its root
/// prims otherwise; layerstack composes the new order and resyncs the
/// child it moved, as it reports any child order a change moves
/// (`prim_resync.rs`, `override_reorders_children`).
fn deliberate(name: &str, call: &[&str]) -> Option<Json> {
    match (name, call) {
        ("sublayer_added", ["reload"]) => Some(json!([{"resynced": ["/A"], "info": ["/"]}])),
        _ => None,
    }
}

fn replay(versions: bool) {
    let oracle = oracle();
    let rewrites = &oracle["rewrites"];
    for case in oracle["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut run = Case::open(versions);
        for step in case["steps"].as_array().unwrap() {
            let call: Vec<&str> = step["step"]
                .as_array()
                .unwrap()
                .iter()
                .map(|part| part.as_str().unwrap_or_default())
                .collect();
            let (returned, failed) = match call[..] {
                ["open"] => (None, false),
                ["set", layer, attribute, _] => {
                    let value = step["step"][3].as_f64().unwrap();
                    run.set(layer, attribute, value);
                    (None, false)
                }
                _ => run.take_step(&call, rewrites),
            };
            let attributes: Vec<String> = step["values"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let values: serde_json::Map<String, Json> = attributes
                .iter()
                .map(|path| (path.clone(), run.value(path)))
                .collect();
            let mut got = json!({
                "values": values,
                "prims": run.prims(),
                "layers": run.layers(),
                "heard": run.take_heard(),
            });
            if let Some(returned) = returned {
                got["returned"] = json!(returned);
            }
            let mode = if versions { "versions" } else { "digests" };
            assert_eq!(
                failed,
                step["raised"].as_bool().unwrap(),
                "{name} ({mode}): {call:?} reports a failure as OpenUSD raises one"
            );
            let mut want = expected(step);
            if let Some(heard) = deliberate(name, &call) {
                want["heard"] = heard;
            }
            assert_eq!(got, want, "{name} ({mode}): {call:?}");
        }
    }
}

#[test]
fn reloads_publish_what_changed_with_storage_versions() {
    replay(true);
}

#[test]
fn reloads_publish_what_changed_comparing_bytes() {
    replay(false);
}

/// A reload with nothing changed reads no layer's file when storage
/// reports versions, and reads each once, decoding none, when it does not.
#[test]
fn an_unchanged_reload_reads_nothing_it_need_not() {
    for versions in [true, false] {
        let mut run = Case::open(versions);
        run.doc.storage().reads.lock().unwrap().clear();
        let report = run.doc.reload(ReloadPolicy::DiscardDirty).unwrap();
        assert!(report.layers.is_empty(), "no layer read again");
        assert!(report.reads.is_empty(), "nothing decoded");
        assert_eq!(run.take_heard(), json!([]), "no notice");
        let reads = run.doc.storage().reads.lock().unwrap().clone();
        // The missing asset is asked for again, as `PcpCache::Reload` does.
        let mut reads = reads;
        reads.sort();
        if versions {
            assert_eq!(reads, ["missing.usda"], "only the missing asset");
        } else {
            assert_eq!(
                reads,
                [
                    "missing.usda",
                    "ref.usda",
                    "root.usda",
                    "second/asset.usda",
                    "sub.usda"
                ],
                "each source once, to compare"
            );
        }
    }
}
