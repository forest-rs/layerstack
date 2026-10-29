// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Instance proxies report what OpenUSD 26.11 reports for them, and a live
//! stage reports what a new composition does, through a run of edits.
//!
//! `scripts/instance_proxies_oracle.py` keeps two OpenUSD stages open, on
//! `fixtures/instance_proxies/scene.usda` and on `dependent.usda`, which
//! references the scene's instances, and edits `scene.usda` step by step;
//! after each step it records every prim, instance proxies included:
//! prim stacks, property stacks, values and prim index nodes. See the
//! script for what OpenUSD reports for a proxy's prim index: the prototype
//! source's, whichever instance that is, which an open stage and a new one
//! can pick differently.
//!
//! Here two `LiveStage`s over one store take the same edits, as
//! transactions and namespace edits. After each step, each must report
//! what a stage composed anew reports, its graphs node for node; and
//! OpenUSD's prim stacks, property stacks and values. A prim's
//! composition graph must have OpenUSD's nodes; an instance proxy's is the
//! one its own path composes: OpenUSD's, read from the source instance's,
//! with the sites that name the source moved to its own instance, which
//! is checked where the source and the instance are reached alike (for a
//! proxy whose instance is itself a proxy, OpenUSD reports another
//! instance's arcs).
//!
//! Spec: AOUSD Core §11.3.3 (scene graph instancing).

use std::collections::BTreeMap;

use layerstack::{
    ArcKind, EditTarget, InMemoryStore, LayerId, ListPosition, LiveStage, NamespaceEdit, Path,
    PropertyPath, PropertySpec, PropertyType, Reference, Specifier, Stage, StageOptions,
    TargetPath, Time, Transaction, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/instance_proxies/oracle.json");

/// The stages the oracle records, by root layer.
const STAGES: [&str; 2] = ["scene.usda", "dependent.usda"];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn load() -> LoadedStage {
    let loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/instance_proxies/dependent.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

fn layer_named(loaded: &LoadedStage, name: &str) -> LayerId {
    *loaded
        .layer_names
        .iter()
        .find(|(_, n)| n.ends_with(&format!("/{name}")) || n.as_str() == name)
        .unwrap_or_else(|| panic!("no layer {name}"))
        .0
}

fn file_name(loaded: &LoadedStage, layer: LayerId) -> String {
    let name = &loaded.layer_names[&layer];
    name.rsplit('/').next().unwrap().to_string()
}

fn arc_name(kind: ArcKind, root: bool) -> &'static str {
    match kind {
        _ if root => "root",
        ArcKind::Inherits => "inherit",
        ArcKind::Variants => "variant",
        ArcKind::Relocates => "relocate",
        ArcKind::References => "reference",
        ArcKind::Payloads => "payload",
        ArcKind::Specializes => "specialize",
        ArcKind::Local => "local",
    }
}

/// One prim as the oracle records it, but for the nodes' inertness, which
/// layerstack does not report; `depth` adds each node's namespace depth,
/// which OpenUSD does not, for comparing two layerstack stages.
fn prim_record(loaded: &LoadedStage, stage: &Stage, prim: layerstack::PathId, depth: bool) -> Json {
    let store = &loaded.store;
    let object = stage.prim(prim, store).unwrap();
    let display = |path: &layerstack::SpecPath| path.display(store.paths(), store.tokens());
    let kind = if object.is_instance_proxy() {
        "proxy"
    } else if object.is_instance() {
        "instance"
    } else {
        "prim"
    };
    let prim_stack: Vec<Json> = object
        .prim_stack()
        .iter()
        .map(|(layer, spec)| json!([file_name(loaded, *layer), display(spec)]))
        .collect();
    let mut properties = BTreeMap::new();
    let mut values = BTreeMap::new();
    for property in object.properties() {
        let stack: Vec<Json> = property
            .opinions()
            .iter()
            .map(|o| json!([file_name(loaded, o.layer_id()), display(&o.spec_path())]))
            .collect();
        properties.insert(property.name().to_string(), stack);
        if let Some(attribute) = property.as_attribute() {
            values.insert(
                property.name().to_string(),
                json!(attribute.value::<f64>(Time::Default)),
            );
        }
    }
    let graph = object.composition_graph();
    let index: Vec<Json> = graph
        .nodes()
        .map(|(_, node)| {
            let mut entry = json!({
                "arc": arc_name(node.arc_kind(), node.parent().is_none()),
                "layer": file_name(loaded, node.layer_stack()),
                "site": display(node.site()),
            });
            if depth {
                entry["depth"] = json!(node.namespace_depth());
            }
            entry
        })
        .collect();
    json!({
        "kind": kind,
        "prim_stack": prim_stack,
        "properties": properties,
        "values": values,
        "index": index,
    })
}

/// Every prim of `stage`, instance proxies included, by path.
fn stage_record(loaded: &LoadedStage, stage: &Stage, depth: bool) -> BTreeMap<String, Json> {
    let store = &loaded.store;
    let root = store.paths().lookup(&Path::root()).unwrap();
    stage
        .traverse_all(root)
        .filter(|p| *p != root)
        .map(|p| {
            (
                store.paths().display(p, store.tokens()),
                prim_record(loaded, stage, p, depth),
            )
        })
        .collect()
}

/// The oracle's nodes of `record`, without their inertness.
fn nodes(record: &Json) -> Vec<Json> {
    record["index"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| json!({"arc": node["arc"], "layer": node["layer"], "site": node["site"]}))
        .collect()
}

/// `path` with its prefix `from` replaced by `to`, if it has it.
fn reroot(path: &str, from: &str, to: &str) -> Option<String> {
    let rest = path.strip_prefix(from)?;
    (rest.is_empty() || rest.starts_with('/')).then(|| format!("{to}{rest}"))
}

/// The oracle's nodes of the proxy `record`, read from its source
/// instance's, with each site beneath a site of the source instance's own
/// index moved beneath its instance's site in the same layer: `None` when
/// either instance is not one the oracle records an own index for, or
/// their indexes have other arcs.
fn rerooted(oracle: &Map, record: &Json) -> Option<Vec<Json>> {
    let source = oracle.get(record["source"].as_str()?)?;
    let instance = oracle.get(record["instance"].as_str()?)?;
    if source["kind"] != "instance" || instance["kind"] != "instance" {
        return None;
    }
    let (from, to) = (nodes(source), nodes(instance));
    let arcs = |nodes: &[Json]| -> Vec<(Json, Json)> {
        nodes
            .iter()
            .map(|n| (n["arc"].clone(), n["layer"].clone()))
            .collect()
    };
    if arcs(&from) != arcs(&to) {
        return None;
    }
    let moves: Vec<(&str, &str, &str)> = from
        .iter()
        .zip(&to)
        .filter(|(a, b)| a["site"] != b["site"])
        .map(|(a, b)| {
            (
                a["layer"].as_str().unwrap(),
                a["site"].as_str().unwrap(),
                b["site"].as_str().unwrap(),
            )
        })
        .collect();
    Some(
        nodes(record)
            .into_iter()
            .map(|mut node| {
                let site = node["site"].as_str().unwrap().to_string();
                let moved = moves
                    .iter()
                    .filter(|(layer, ..)| node["layer"] == *layer)
                    .filter_map(|(_, from, to)| Some((from.len(), reroot(&site, from, to)?)))
                    .max_by_key(|(len, _)| *len);
                if let Some((_, site)) = moved {
                    node["site"] = json!(site);
                }
                node
            })
            .collect(),
    )
}

type Map = serde_json::Map<String, Json>;

/// The step's edit, as layerstack authors it in `scene`.
fn edit(loaded: &mut LoadedStage, live: &LiveStage, scene: LayerId, step: &Json) -> Transaction {
    let store: &mut InMemoryStore = &mut loaded.store;
    let target = EditTarget::for_layer(scene);
    let path = store.path(step["path"].as_str().unwrap());
    let mut transaction = Transaction::new();
    match step["edit"].as_str().unwrap() {
        "move" => {
            let to = store.path(step["argument"].as_str().unwrap());
            let stage = live.stage();
            let edit = NamespaceEdit::prepare(
                stage,
                store,
                &target,
                TargetPath::Prim(path),
                TargetPath::Prim(to),
            );
            transaction = edit.expect("moves").into_transaction();
        }
        "delete" => {
            let stage = live.stage();
            let edit = NamespaceEdit::prepare_delete(stage, store, &target, TargetPath::Prim(path));
            transaction = edit.expect("deletes").into_transaction();
        }
        "size" => {
            let size = store.tokens().intern("size");
            let spec = PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
                .with_default(Value::Double(step["argument"].as_f64().unwrap()));
            transaction.create_property(target.property(PropertyPath::new(path, size)), spec);
        }
        "instance" => {
            let proto = store.path(step["argument"].as_str().unwrap());
            let at = target.prim(path);
            transaction
                .create_prim(at.clone(), Specifier::Def, None)
                .set_instanceable(at.clone(), true)
                .add_reference(
                    at,
                    Reference::new(scene, proto),
                    ListPosition::FrontOfPrepend,
                );
            // First among its siblings.
            let parent = store.paths().parent(path).unwrap();
            let stage = live.stage();
            let name = *store.paths().resolve(path).segments().last().unwrap();
            let siblings: Vec<String> = stage
                .prim(parent, &*store)
                .unwrap()
                .all_children()
                .map(|child| child.name().to_string())
                .collect();
            let mut order = vec![name];
            order.extend(siblings.iter().map(|s| store.tokens().intern(s)));
            transaction.set_prim_order(target.prim(parent), order);
        }
        "instanceable" => {
            transaction.set_instanceable(target.prim(path), step["argument"].as_bool().unwrap());
        }
        "active" => {
            transaction.set_active(target.prim(path), step["argument"].as_bool().unwrap());
        }
        other => panic!("no edit {other}"),
    }
    transaction
}

/// Each stage's records against a new composition's and the oracle's.
fn check(loaded: &mut LoadedStage, lives: &[LiveStage], step: &Json, compared: &mut usize) {
    let name = step["name"].as_str().unwrap();
    for (live, stage_name) in lives.iter().zip(STAGES) {
        let root = layer_named(loaded, stage_name);
        let fresh = Stage::compose(&mut loaded.store, root, StageOptions::default());
        let ours = stage_record(loaded, live.stage(), true);
        let theirs = stage_record(loaded, &fresh, true);
        for (path, record) in &theirs {
            assert_eq!(
                ours.get(path),
                Some(record),
                "{name}, {stage_name}: {path} on the live stage and a new one"
            );
        }
        assert_eq!(
            ours.keys().collect::<Vec<_>>(),
            theirs.keys().collect::<Vec<_>>(),
            "{name}, {stage_name}: prims"
        );
        let want = step["stages"][stage_name]["prims"].as_object().unwrap();
        let got = stage_record(loaded, live.stage(), false);
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            want.keys().collect::<Vec<_>>(),
            "{name}, {stage_name}: prims as OpenUSD composes them"
        );
        for (path, record) in want {
            let ours = &got[path];
            for key in ["kind", "prim_stack", "properties", "values"] {
                assert_eq!(
                    ours[key], record[key],
                    "{name}, {stage_name}: {path} {key} as OpenUSD reports it"
                );
            }
            let nodes = if record["kind"] == "proxy" {
                rerooted(want, record)
            } else {
                Some(nodes(record))
            };
            if let Some(nodes) = nodes {
                assert_eq!(
                    ours["index"],
                    json!(nodes),
                    "{name}, {stage_name}: {path} nodes as OpenUSD reports them"
                );
                *compared += 1;
            }
        }
    }
}

#[test]
fn instance_proxies_compose_as_openusd_and_a_new_stage_through_edits() {
    let oracle = oracle();
    let mut loaded = load();
    let scene = layer_named(&loaded, "scene.usda");
    let mut lives: Vec<LiveStage> = STAGES
        .iter()
        .map(|name| {
            let root = layer_named(&loaded, name);
            LiveStage::compose(&mut loaded.store, root, StageOptions::default())
        })
        .collect();
    let steps = oracle["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 7, "every step is recorded");
    let mut compared = 0;
    check(&mut loaded, &lives, &steps[0], &mut compared);
    for step in &steps[1..] {
        let transaction = edit(&mut loaded, &lives[0], scene, step);
        lives[0]
            .apply(&mut loaded.store, &transaction)
            .unwrap_or_else(|e| panic!("{}: {e:?}", step["name"]));
        for live in &mut lives[1..] {
            live.synchronize(&mut loaded.store);
        }
        check(&mut loaded, &lives, step, &mut compared);
    }
    assert!(compared > 100, "{compared} graphs compared");
}

#[test]
fn an_open_openusd_stage_keeps_prototype_sources_a_new_one_does_not_pick() {
    // The oracle's evidence that which instance a proxy's prim index is
    // read from is OpenUSD's history, not composition: after an instance
    // ordered first is added, the open stage keeps its source.
    let oracle = oracle();
    let step = oracle["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "add_first_instance")
        .unwrap();
    let differs = step["stages"]["scene.usda"]["fresh_differs"]
        .as_array()
        .unwrap();
    assert!(differs.contains(&json!("/Group/I2/Leaf")), "{differs:?}");
    let before = &oracle["steps"][0]["stages"]["scene.usda"]["fresh_differs"];
    assert_eq!(before, &json!([]));
}
