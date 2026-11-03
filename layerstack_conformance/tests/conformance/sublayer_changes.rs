// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A layer's sublayers inserted, removed, reordered or retimed, and
//! sublayers muted and unmuted, as OpenUSD 26.11 announces and composes
//! them.
//!
//! `scripts/sublayer_changes_oracle.py` records, per case, a stage on
//! `fixtures/sublayer_changes/layers/root.usda` whose root layer, one of
//! its sublayers or the layer a reference reaches has its sublayers
//! edited, or which mutes and unmutes layers. After each step it records
//! the composed values, the stage's prims and layer stack, and the
//! notices. See the script for the 26.11 code each shows.
//!
//! Here each case replays against a `StageDocument` on the fixture's
//! files, through transactions (`insert_sublayer`, `remove_sublayer`,
//! `set_sublayers`, `set_sublayer_offset`) and
//! `mute_and_unmute_layers`. After each step the live stage must compose
//! what a stage composed afresh on the same layers composes.
//!
//! Notices map as `layer_reload.rs` maps them: resynced prims and
//! properties are resynced paths; an info-only prim with a property
//! inventory is its properties' paths, one without is its own path. The
//! notices of one step are merged, as one change block merges them, and a
//! notice naming nothing is no notice.
//!
//! Three differences are deliberate:
//!
//! - `muted_insert` (OpenUSD bug B038): inserting a sublayer that is muted
//!   composes nothing and announces only the `subLayers` change at `/`,
//!   where OpenUSD opens the muted layer to compute the change
//!   (`PcpChanges::_LoadSublayerForChange` does not ask whether it is
//!   muted) and resyncs `/N`, a prim it defines that composes neither
//!   before nor after.
//! - `referenced_as_sublayer`, reorder: a layer that `/R` references and
//!   the root layer stack holds as a sublayer too. Reordering the root
//!   layer's sublayers resyncs `/R` as well, as layerstack counts a
//!   reordered sublayer's specs as removed and added wherever they compose
//!   (its spec stacks do not record the layer stack an entry came
//!   through), where OpenUSD counts them in the reordered layer stack
//!   alone. Coarser, and covering OpenUSD's.
//! - `referenced_as_sublayer`, removal: removing that sublayer does not
//!   resync `/R`, which composes the same, where OpenUSD processes the
//!   removed layer's specs in every layer stack holding the layer, `/R`'s
//!   referenced one among them, and resyncs it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use layerstack::{LayerId, LayerOffset, Stage, StageOptions, SublayerEntry, Time, Transaction};
use layerstack_conformance::workspace_root;
use layerstack_io::{Filesystem, StageDocument};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/sublayer_changes/oracle.json");

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

/// The fixture's layers' directory, absolute, as the filesystem storage
/// names the layers it reads (see `layer_muting.rs`).
fn layers_directory() -> PathBuf {
    if cfg!(target_os = "wasi") {
        return std::env::current_dir()
            .unwrap()
            .join("fixtures/sublayer_changes/layers");
    }
    let canonical = workspace_root()
        .join("layerstack_conformance/fixtures/sublayer_changes/layers")
        .canonicalize()
        .unwrap();
    match canonical.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => canonical,
    }
}

/// `identifier` relative to `directory`, `/` separated.
fn relative(directory: &Path, identifier: &str) -> String {
    let identifier = identifier.replace('\\', "/");
    let prefix = format!("{}/", directory.to_str().unwrap().replace('\\', "/"));
    identifier
        .strip_prefix(&prefix)
        .map_or(identifier.clone(), str::to_string)
}

/// The notices a stage sent: resynced and info-only paths of each.
type Heard = Arc<Mutex<Vec<(BTreeSet<String>, BTreeSet<String>)>>>;

struct Case {
    directory: PathBuf,
    doc: StageDocument<Filesystem>,
    heard: Heard,
}

impl Case {
    fn open() -> Self {
        let directory = layers_directory();
        let storage = Filesystem::new(&directory, []).unwrap();
        let root = directory.join("root.usda");
        let mut doc =
            StageDocument::open(storage, root.to_str().unwrap(), StageOptions::default()).unwrap();
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
        Self {
            directory,
            doc,
            heard,
        }
    }

    /// The step's notices, merged as one change block merges them.
    fn take_heard(&self) -> Json {
        let heard = std::mem::take(&mut *self.heard.lock().unwrap());
        merged(heard.into_iter())
    }

    fn layer(&self, name: &str) -> LayerId {
        let identifier = self.directory.join(name);
        self.doc
            .find_layer_named(identifier.to_str().unwrap())
            .unwrap()
            .unwrap_or_else(|| panic!("no layer {name}"))
    }

    fn sublayers(&self, layer: LayerId) -> Vec<SublayerEntry> {
        self.doc.store().layer(layer).unwrap().sublayers().to_vec()
    }

    fn take_step(&mut self, step: &Json) {
        let kind = step[0].as_str().unwrap();
        let name = step[1].as_str().unwrap();
        match kind {
            "mute" | "unmute" => {
                let identifier = self.directory.join(name);
                let layer = self.doc.layer_named(identifier.to_str().unwrap()).unwrap();
                let (mute, unmute) = if kind == "mute" {
                    (vec![layer], vec![])
                } else {
                    (vec![], vec![layer])
                };
                self.doc.mute_and_unmute_layers(&mute, &unmute).unwrap();
                self.doc.load_requested_assets().unwrap();
                return;
            }
            _ => {}
        }
        let layer = self.layer(name);
        let index = |at: usize| usize::try_from(step[at].as_u64().unwrap()).unwrap();
        let mut edit = Transaction::new();
        match kind {
            "insert" => {
                let asset = step[3].as_str().unwrap();
                edit.insert_sublayer(
                    layer,
                    index(2),
                    SublayerEntry::unresolved(asset, LayerOffset::IDENTITY),
                );
            }
            "remove" => {
                edit.remove_sublayer(layer, index(2));
            }
            "reorder" => {
                let sublayers = self.sublayers(layer);
                let order = step[2].as_array().unwrap();
                let reordered = order
                    .iter()
                    .map(|at| sublayers[usize::try_from(at.as_u64().unwrap()).unwrap()].clone())
                    .collect();
                edit.set_sublayers(layer, reordered);
            }
            "offset" => {
                let offset = LayerOffset {
                    offset: step[3].as_f64().unwrap(),
                    scale: step[4].as_f64().unwrap(),
                };
                edit.set_sublayer_offset(layer, index(2), offset);
            }
            other => panic!("unknown step {other}"),
        }
        self.doc.apply(&edit).unwrap();
    }

    fn value(&mut self, path: &str, sampled: &Json) -> Json {
        let time = sampled
            .get(path)
            .and_then(Json::as_f64)
            .map_or(Time::Default, Time::at);
        let path = self.doc.store_mut().property_path(path);
        self.doc
            .view()
            .attribute(path)
            .and_then(|attribute| attribute.value::<f64>(time))
            .map_or(Json::Null, |value| json!(value))
    }

    fn prims(&self, stage: &Stage) -> Json {
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

    fn layer_stack(&self, stage: &Stage) -> Json {
        json!(
            stage
                .layer_stack()
                .iter()
                .map(|&id| relative(&self.directory, self.doc.identifier(id).unwrap()))
                .collect::<Vec<_>>()
        )
    }

    /// What a stage composed afresh on the document's layers composes.
    fn fresh(&mut self) -> (Json, Json) {
        let root = self.doc.root_layer();
        let muted = self.doc.stage().options().muted_layers.clone();
        let options = StageOptions {
            muted_layers: muted,
            ..StageOptions::default()
        };
        let fresh = Stage::compose(self.doc.store_mut(), root, options);
        (self.prims(&fresh), self.layer_stack(&fresh))
    }
}

/// `heard`'s notices merged as one change block merges them: a path
/// resynced in one is not info-only in another, nor is a path beneath a
/// resynced one, and an info-only prim covers its properties.
fn merged(heard: impl Iterator<Item = (BTreeSet<String>, BTreeSet<String>)>) -> Json {
    let mut resynced = BTreeSet::new();
    let mut info = BTreeSet::new();
    for (r, i) in heard {
        resynced.extend(r);
        info.extend(i);
    }
    if resynced.is_empty() && info.is_empty() {
        return json!([]);
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

/// `info` without the properties of the prims it lists.
fn covered(info: BTreeSet<String>) -> BTreeSet<String> {
    info.iter()
        .filter(|path| {
            path.rsplit_once('.')
                .is_none_or(|(prim, _)| prim.is_empty() || !info.contains(prim))
        })
        .cloned()
        .collect()
}

/// The oracle's notices of a step, as [`merged`] merges them.
fn expected_heard(step: &Json) -> Json {
    let strings = |json: &Json| -> BTreeSet<String> {
        json.as_array()
            .unwrap()
            .iter()
            .map(|path| path.as_str().unwrap().to_owned())
            .collect()
    };
    merged(
        step["heard"]
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| (strings(&notice["resynced"]), strings(&notice["info"]))),
    )
}

/// The deliberate differences of the module docs: what layerstack
/// announces for a case's step where OpenUSD announces otherwise.
fn deliberate(name: &str, step: usize) -> Option<Json> {
    match (name, step) {
        // OpenUSD bug B038: OpenUSD resyncs `/N` too.
        ("muted_insert", 2) => Some(json!([{"resynced": [], "info": ["/"]}])),
        // The reorder resyncs `/R` too, which OpenUSD does not.
        ("referenced_as_sublayer", 2) => Some(json!([{
            "resynced": ["/A.y", "/Asset", "/R", "/S", "/W"],
            "info": ["/", "/A"],
        }])),
        // The removal leaves `/R`, which OpenUSD resyncs.
        ("referenced_as_sublayer", 3) => Some(json!([{"resynced": ["/Asset"], "info": ["/"]}])),
        _ => None,
    }
}

#[test]
fn sublayer_changes_announce_and_compose_as_openusd() {
    let oracle = oracle();
    let sampled = &oracle["sampled"];
    let mut failures = Vec::new();
    for case in oracle["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut run = Case::open();
        for (at, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let call = &step["step"];
            if call[0] != "open" {
                run.take_step(call);
            }
            let values: serde_json::Map<String, Json> = step["values"]
                .as_object()
                .unwrap()
                .keys()
                .map(|path| (path.clone(), run.value(path, sampled)))
                .collect();
            let stage = run.doc.stage().stage();
            let got = json!({
                "values": values,
                "prims": run.prims(stage),
                "layer_stack": run.layer_stack(stage),
                "heard": run.take_heard(),
            });
            let want = json!({
                "values": step["values"],
                "prims": step["prims"],
                "layer_stack": step["layer_stack"],
                "heard": deliberate(name, at).unwrap_or_else(|| expected_heard(step)),
            });
            if got != want {
                failures.push(format!("{name}: {call}\n  got  {got}\n  want {want}"));
            }
            let (prims, layer_stack) = run.fresh();
            assert_eq!(
                (&got["prims"], &got["layer_stack"]),
                (&prims, &layer_stack),
                "{name}: {call} composes as a fresh stage"
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
