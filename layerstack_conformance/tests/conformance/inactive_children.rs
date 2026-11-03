// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Edits that change which children a prim has, or their order, with
//! inactive children among them, as OpenUSD 26.11 composes and announces
//! them.
//!
//! `scripts/inactive_children_oracle.py` records, per case, a stage on
//! `fixtures/inactive_children/layers/root.usda` taking edits of one prim
//! spec each: `primOrder` (`reorder nameChildren`) moving or naming
//! inactive children, an inactive child added or removed, a child
//! deactivated or activated, through a reference and inside a variant.
//! After each step it records the children of the watched prims
//! (`GetAllChildren`, `GetChildren`,
//! `GetFilteredChildren(Usd.PrimAllPrimsPredicate)`) and the notices.
//!
//! Here each case replays against a `StageDocument` on the fixture's
//! files, one transaction a step. After each step the live stage's
//! children must be OpenUSD's, its notices OpenUSD's (merged as one change
//! block merges them, as `sublayer_changes.rs` merges them), and the live
//! stage must compose what a stage composed afresh on the same layers
//! composes: the same prims, each with the same children, active and
//! inactive, and the same prim stack.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use layerstack::{
    EditTarget, FieldValue, LayerId, Path, PrimPredicate, SpecPath, Specifier, Stage, StageOptions,
    Transaction, Value,
};
use layerstack_conformance::workspace_root;
use layerstack_io::{Filesystem, StageDocument};
use serde_json::{Value as Json, json};

use crate::support::notices;

const ORACLE: &str = include_str!("../../fixtures/inactive_children/oracle.json");

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
            .join("fixtures/inactive_children/layers");
    }
    let canonical = workspace_root()
        .join("layerstack_conformance/fixtures/inactive_children/layers")
        .canonicalize()
        .unwrap();
    match canonical.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => canonical,
    }
}

/// The paths of one notice: the resynced ones, and the info-only ones with
/// their changed fields.
type Notice = (BTreeSet<String>, BTreeMap<String, BTreeSet<String>>);

/// The notices a stage sent.
type Heard = Arc<Mutex<Vec<Notice>>>;

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
            let resynced = notices::resynced_paths(notice.changes, paths, tokens);
            let info = notices::notice_info_fields(&notice);
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

    /// The edit target and stage path of the spec at `spec` (a variant
    /// path, `/V{look=one}N`, for a spec inside a variant) of `layer`.
    fn target(&mut self, layer: LayerId, spec: &str) -> (EditTarget, layerstack::PathId) {
        let store = self.doc.store_mut();
        let Some(open) = spec.rfind('{') else {
            return (EditTarget::for_layer(layer), store.path(spec));
        };
        let close = open + spec[open..].find('}').unwrap();
        let variant = SpecPath::parse(&spec[..=close], store.tokens(), store.paths()).unwrap();
        let rest = &spec[close + 1..];
        let host = &spec[..open];
        let prim = if rest.is_empty() {
            host.to_string()
        } else {
            format!("{host}/{rest}")
        };
        (
            EditTarget::for_local_variant(layer, &variant),
            store.path(&prim),
        )
    }

    fn take_step(&mut self, step: &Json) {
        let kind = step[0].as_str().unwrap();
        let layer = self.layer(step[1].as_str().unwrap());
        let (target, prim) = self.target(layer, step[2].as_str().unwrap());
        let at = target.prim(prim);
        let tokens = self.doc.store().tokens();
        let mut edit = Transaction::new();
        match kind {
            "active" => match step[3].as_bool() {
                Some(active) => {
                    edit.set_active(at, active);
                }
                None => {
                    edit.clear_active(at);
                }
            },
            "order" => match step[3].as_array() {
                Some(names) => {
                    let order = names
                        .iter()
                        .map(|name| tokens.intern(name.as_str().unwrap()))
                        .collect();
                    edit.set_prim_order(at, order);
                }
                None => {
                    edit.clear_prim_order(at);
                }
            },
            "def" => {
                edit.create_prim(at.clone(), Specifier::Def, None);
                if let Some(active) = step[3].as_bool() {
                    edit.set_active(at, active);
                }
            }
            "remove" => {
                edit.remove_spec(at);
            }
            "doc" => {
                let text = step[3].as_str().unwrap();
                edit.set_metadata(
                    at,
                    tokens.intern("documentation"),
                    FieldValue::Value(Value::string(text)),
                );
            }
            "select" => {
                let set = tokens.intern(step[3].as_str().unwrap());
                let variant = tokens.intern(step[4].as_str().unwrap());
                edit.set_variant_selection(at, set, Some(variant));
            }
            other => panic!("unknown step {other}"),
        }
        self.doc.apply(&edit).unwrap();
    }

    /// The children of the prims `watched` names that `stage` composes, as
    /// the oracle records them.
    fn children(&self, stage: &Stage, watched: &serde_json::Map<String, Json>) -> Json {
        let store = self.doc.store();
        let tokens = store.tokens();
        let mut found = serde_json::Map::new();
        for path in watched.keys() {
            let Some(id) = Path::parse_absolute(path, tokens)
                .ok()
                .and_then(|path| store.paths().lookup(&path))
            else {
                continue;
            };
            let Some(prim) = stage.prim(id, store) else {
                continue;
            };
            let names = |children: &mut dyn Iterator<Item = layerstack::Prim<'_>>| -> Vec<String> {
                children.map(|child| child.name().to_string()).collect()
            };
            found.insert(
                path.clone(),
                json!({
                    "all": names(&mut prim.all_children()),
                    "active": names(&mut prim.children()),
                    "filtered": names(&mut prim.filtered_children(PrimPredicate::ALL)),
                }),
            );
        }
        Json::Object(found)
    }

    /// Every prim `stage` composes, inactive ones included, with its
    /// children, active and inactive, and its prim stack.
    fn composed(&self, stage: &Stage) -> Vec<String> {
        let store = self.doc.store();
        let (paths, tokens) = (store.paths(), store.tokens());
        let root = paths.lookup(&Path::root()).unwrap();
        stage
            .traverse_all(root)
            .map(|path| {
                let prim = stage.prim(path, store).unwrap();
                let all: Vec<String> = prim
                    .all_children()
                    .map(|child| child.name().to_string())
                    .collect();
                let active: Vec<String> = prim
                    .children()
                    .map(|child| child.name().to_string())
                    .collect();
                format!(
                    "{} active={} all={all:?} children={active:?} stack={:?}",
                    paths.display(path, tokens),
                    prim.is_active(),
                    prim.prim_stack(),
                )
            })
            .collect()
    }

    /// What a stage composed afresh on the document's layers composes.
    fn fresh(&mut self) -> Vec<String> {
        let root = self.doc.root_layer();
        let fresh = Stage::compose(self.doc.store_mut(), root, StageOptions::default());
        self.composed(&fresh)
    }
}

/// `heard`'s notices merged as one change block merges them: a path
/// resynced in one is not info-only in another, nor is a path beneath a
/// resynced one, and an info-only path's fields are those of all.
fn merged(heard: impl Iterator<Item = Notice>) -> Json {
    let mut resynced = BTreeSet::new();
    let mut info: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (r, i) in heard {
        resynced.extend(r);
        for (path, fields) in i {
            info.entry(path).or_default().extend(fields);
        }
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
    info.retain(|path, _| !beneath(path, &resynced));
    json!([{"resynced": resynced, "info": info}])
}

/// The oracle's notices of a step, as [`merged`] merges them. The fields
/// of resynced paths are not compared: `Changes` lists fields of info-only
/// paths alone.
fn expected_heard(step: &Json) -> Json {
    let strings = |json: &Json| -> BTreeSet<String> {
        json.as_array()
            .unwrap()
            .iter()
            .map(|path| path.as_str().unwrap().to_owned())
            .collect()
    };
    merged(step["heard"].as_array().unwrap().iter().map(|notice| {
        let fields = notice["info"]
            .as_array()
            .unwrap()
            .iter()
            .map(|path| {
                let path = path.as_str().unwrap();
                (path.to_owned(), strings(&notice["fields"][path]))
            })
            .collect();
        (strings(&notice["resynced"]), fields)
    }))
}

#[test]
fn inactive_children_compose_and_announce_as_openusd() {
    let oracle = oracle();
    let mut failures = Vec::new();
    for case in oracle["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut run = Case::open();
        for step in case["steps"].as_array().unwrap() {
            let call = &step["step"];
            if call[0] != "open" {
                run.take_step(call);
            }
            let watched = step["children"].as_object().unwrap();
            let got = json!({
                "children": run.children(run.doc.stage().stage(), watched),
                "heard": run.take_heard(),
            });
            let want = json!({
                "children": step["children"],
                "heard": expected_heard(step),
            });
            if got != want {
                failures.push(format!("{name}: {call}\n  got  {got}\n  want {want}"));
            }
            let live = run.composed(run.doc.stage().stage());
            let fresh = run.fresh();
            if live != fresh {
                failures.push(format!(
                    "{name}: {call} composes otherwise than a fresh stage\n  live  {live:#?}\n  fresh {fresh:#?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
