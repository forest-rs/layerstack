// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Differential tests for loading and unloading payloads on a stage.
//!
//! `fixtures/payload_loading/oracle.json` records what OpenUSD 26.11's
//! stage answers, call by call (`scripts/payload_loading_oracle.py`): a
//! stage opened with every payload loaded or none, then `UsdStage::Load`,
//! `Unload`, `LoadAndUnload` and `SetLoadRules`, some naming inactive
//! paths, paths not on the stage and prototype paths, which OpenUSD skips
//! with an error while it applies the rest; each followed by the paths it
//! refused, the literal load rules, `GetLoadSet`, `FindLoadable`, which payload layers
//! the stage uses, and for each path whether a prim is there,
//! `UsdPrim::IsLoaded` and `UsdPrim::HasAuthoredPayloads`. A document
//! opened with the same rules, making the same calls through
//! [`LiveStage`](layerstack::LiveStage) and composing as the document
//! does, must answer the same; and it reads a payload's layer only once
//! the rules include the payload, as OpenUSD opens it.
//!
//! Spec: AOUSD Core §10.3.2.2 (payloads); OpenUSD `UsdStage::LoadAndUnload`
//! (`pxr/usd/usd/stage.cpp`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use layerstack::{
    LoadOutcome, LoadPolicy, Path, PathId, PayloadLoadRules, PayloadRule, StageOptions,
};
use layerstack_conformance::workspace_root;
use layerstack_io::{Filesystem, OpenOptions, StageDocument};
use serde::Deserialize;

const ORACLE: &str = include_str!("../../fixtures/payload_loading/oracle.json");
const PAYLOAD_LAYERS: [&str; 3] = ["rock.usda", "tree.usda", "leaf.usda"];

#[derive(Deserialize)]
struct Oracle {
    openusd_version: String,
    paths: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    load: String,
    opened: State,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    call: Vec<serde_json::Value>,
    result: Option<String>,
    refused: Vec<(String, String)>,
    after: State,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct State {
    rules: Vec<(String, String)>,
    load_set: Vec<String>,
    loadable: Vec<String>,
    used_payload_layers: Vec<String>,
    prims: BTreeMap<String, (bool, bool, bool)>,
}

fn directory() -> PathBuf {
    if cfg!(target_os = "wasi") {
        return std::env::current_dir()
            .unwrap()
            .join("fixtures/payload_loading");
    }
    let canonical = workspace_root()
        .join("layerstack_conformance/fixtures/payload_loading")
        .canonicalize()
        .unwrap();
    match canonical.to_string_lossy().strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => canonical,
    }
}

fn open(load_none: bool) -> StageDocument<Filesystem> {
    let directory = directory();
    let storage = Filesystem::new(&directory, []).unwrap();
    let root = directory.join("scene.usda");
    let options = OpenOptions {
        stage: StageOptions {
            load_rules: if load_none {
                PayloadLoadRules::load_none()
            } else {
                PayloadLoadRules::default()
            },
            ..StageOptions::default()
        },
        ..OpenOptions::default()
    };
    StageDocument::open(storage, root.to_str().unwrap(), options).unwrap()
}

fn rule_name(rule: PayloadRule) -> &'static str {
    match rule {
        PayloadRule::All => "all",
        PayloadRule::Only => "only",
        PayloadRule::None => "none",
    }
}

fn path(doc: &StageDocument<Filesystem>, text: &str) -> PathId {
    let store = doc.store();
    store
        .paths()
        .intern(Path::parse_absolute(text, store.tokens()).unwrap())
}

/// The layer file names `ids` names, of the payloads' layers.
fn payload_layers(
    doc: &StageDocument<Filesystem>,
    ids: impl IntoIterator<Item = layerstack::LayerId>,
) -> Vec<String> {
    let names: Vec<String> = ids
        .into_iter()
        .filter_map(|id| doc.identifier(id))
        .filter_map(|identifier| identifier.rsplit(['/', '\\']).next().map(str::to_owned))
        .collect();
    PAYLOAD_LAYERS
        .iter()
        .filter(|name| names.iter().any(|n| n == *name))
        .map(|name| (*name).to_owned())
        .collect()
}

fn state(doc: &StageDocument<Filesystem>, paths: &[String]) -> State {
    let store = doc.store();
    let stage = doc.stage().stage();
    let display = |id: PathId| store.paths().display(id, store.tokens());
    let mut rules: Vec<(String, String)> = stage
        .load_rules()
        .rules()
        .map(|(path, rule)| (path.display(store.tokens()), rule_name(rule).to_owned()))
        .collect();
    rules.sort();
    let root = store.paths().intern(Path::root());
    let prims = paths
        .iter()
        .map(|text| {
            let prim = doc.view().prim(text);
            let row = prim.map_or((false, false, false), |prim| {
                (true, prim.is_loaded(), prim.has_authored_payloads())
            });
            (text.clone(), row)
        })
        .collect();
    State {
        rules,
        load_set: stage
            .loaded_payload_paths(store)
            .into_iter()
            .map(display)
            .collect(),
        loadable: stage
            .loadable_paths(root, store)
            .into_iter()
            .map(display)
            .collect(),
        used_payload_layers: payload_layers(doc, stage.used_layers(false)),
        prims,
    }
}

fn policy(name: &serde_json::Value) -> LoadPolicy {
    match name.as_str().unwrap() {
        "withDescendants" => LoadPolicy::WithDescendants,
        "withoutDescendants" => LoadPolicy::WithoutDescendants,
        other => panic!("policy {other}"),
    }
}

fn paths_of(doc: &StageDocument<Filesystem>, list: &serde_json::Value) -> Vec<PathId> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|p| path(doc, p.as_str().unwrap()))
        .collect()
}

/// Makes the step's call as the document's live stage, composes as the
/// document does, and returns what OpenUSD's call returned (`prim` or
/// `invalid` for `Load`: whether a prim is at the path after it) and the
/// paths it refused, each with its code, sorted.
fn call(
    doc: &mut StageDocument<Filesystem>,
    step: &[serde_json::Value],
) -> (Option<String>, Vec<(String, String)>) {
    let kind = step[0].as_str().unwrap();
    let outcome = match kind {
        "load" => {
            let target = path(doc, step[1].as_str().unwrap());
            let (store, live) = doc.parts_mut();
            live.load(store, target, policy(&step[2]))
        }
        "unload" => {
            let target = path(doc, step[1].as_str().unwrap());
            let (store, live) = doc.parts_mut();
            live.unload(store, target)
        }
        "loadAndUnload" => {
            let load = paths_of(doc, &step[1]);
            let unload = paths_of(doc, &step[2]);
            let (store, live) = doc.parts_mut();
            live.load_and_unload(store, &load, &unload, policy(&step[3]))
        }
        "setLoadRules" => {
            let mut rules = PayloadLoadRules::load_none();
            for rule in step[1].as_array().unwrap() {
                let target = doc
                    .store()
                    .paths()
                    .resolve(path(doc, rule[0].as_str().unwrap()))
                    .clone();
                let rule = match rule[1].as_str().unwrap() {
                    "all" => PayloadRule::All,
                    "only" => PayloadRule::Only,
                    _ => PayloadRule::None,
                };
                rules.add_rule(target, rule);
            }
            let (_, live) = doc.parts_mut();
            live.set_load_rules(rules);
            LoadOutcome::default()
        }
        other => panic!("call {other}"),
    };
    doc.synchronize();
    doc.load_requested_assets().unwrap();
    let store = doc.store();
    let mut refused: Vec<(String, String)> = outcome
        .refused
        .iter()
        .map(|(path, reason)| {
            (
                store.paths().display(*path, store.tokens()),
                reason.code().to_owned(),
            )
        })
        .collect();
    refused.sort();
    let result = (kind == "load").then(|| {
        let there = doc.view().prim(step[1].as_str().unwrap()).is_some();
        if there { "prim" } else { "invalid" }.to_owned()
    });
    (result, refused)
}

#[test]
fn loading_and_unloading_payloads_matches_openusd_call_by_call() {
    let oracle: Oracle = serde_json::from_str(ORACLE).unwrap();
    assert_eq!(oracle.openusd_version, "0.26.11");
    for case in &oracle.cases {
        let load_none = case.load == "none";
        let mut doc = open(load_none);
        assert_eq!(
            state(&doc, &oracle.paths),
            case.opened,
            "open {}",
            case.load
        );
        if load_none {
            assert_eq!(
                payload_layers(&doc, doc.store().layer_ids()),
                Vec::<String>::new(),
                "an unloaded payload's layer is not read"
            );
            assert!(doc.unavailable_assets().is_empty());
        }
        for step in &case.steps {
            let (result, refused) = call(&mut doc, &step.call);
            assert_eq!(result, step.result, "{} {:?}", case.load, step.call);
            assert_eq!(refused, step.refused, "{} {:?}", case.load, step.call);
            assert_eq!(
                state(&doc, &oracle.paths),
                step.after,
                "{} after {:?}",
                case.load,
                step.call
            );
        }
    }
}
