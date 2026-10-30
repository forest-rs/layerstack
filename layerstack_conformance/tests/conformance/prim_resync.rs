// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Change reports for prim spec additions, removals and child reorders, as
//! OpenUSD 26.11 sends them.
//!
//! `scripts/prim_resync_oracle.py` takes a fixed sequence of edits on
//! `fixtures/prim_resync/scene.usda` and records, call by call, the
//! `UsdNotice::ObjectsChanged` notices each sent and the composed child
//! names after it. Here each edit is one transaction on a `LiveStage`, and
//! its [`Changes`] must report what the edit's notices do, merged as one
//! `SdfChangeBlock` merges them: a path resynced by one notice and
//! info-only in another is resynced, and a path beneath a resync is not
//! reported.
//!
//! The point it pins: a prim created or removed resyncs that prim alone.
//! Its parent, whose spec lists one child more or fewer, is not reported
//! (OpenUSD records no field change for `primChildren`), so a client
//! caching the parent's other inputs keeps them. A reorder (`primOrder`)
//! resyncs the parent.
//!
//! Differences, deliberate:
//!
//! - layerstack names no prototype prims; it composes an instance's
//!   descendants per instance. An edit OpenUSD reports at
//!   `/__Prototype_1/Antenna`, the prototype `/Robot` and `/Drone` share,
//!   is reported at `/Drone/Antenna` and `/Robot/Antenna`, and the
//!   prototype's children are compared with `/Robot`'s.
//! - A spec authored beneath the instance root `/Robot` composes nothing
//!   in either. OpenUSD resyncs the prototype's path when the prototype
//!   was built from `/Robot`, and sends nothing when from `/Drone`, which
//!   varies run to run (the oracle records none); layerstack resyncs the
//!   path authored.
//! - An inert `over` added through a sublayer for a prim already composed
//!   changes only that prim's spec stack, which OpenUSD reports info-only.
//!   layerstack recomposes such a prim with its namespace, and resyncs it:
//!   conservative, and it covers a new child the `over` lists.

use std::collections::BTreeMap;

use layerstack::{
    Changes, EditTarget, LayerId, LiveStage, NamespaceEdit, PathId, PropertySpec, PropertyType,
    Specifier, StageOptions, TargetPath, Transaction,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/prim_resync/oracle.json");

/// The instances of the fixture's one prototype.
const INSTANCES: [&str; 2] = ["/Drone", "/Robot"];

/// The resynced and info-only paths of one edit, and the fields of the
/// info-only ones, as strings.
#[derive(Debug, Default, PartialEq)]
struct Report {
    resynced: Vec<String>,
    info: BTreeMap<String, Vec<String>>,
}

/// Whether `path` is `root` or beneath it, by text.
fn beneath(path: &str, root: &str) -> bool {
    path == root
        || root == "/"
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('.'))
}

/// `path` at each instance when it is in the prototype, else itself.
fn at_instances(path: &str) -> Vec<String> {
    match path.strip_prefix("/__Prototype_1") {
        Some(rest) => INSTANCES
            .iter()
            .map(|instance| format!("{instance}{rest}"))
            .collect(),
        None => vec![path.to_string()],
    }
}

/// The notices of one oracle record merged as one change block merges
/// them, prototype paths at their instances (see the module docs).
fn expected(record: &Json) -> Report {
    let mut resynced: Vec<String> = Vec::new();
    let mut info: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for notice in record["notices"].as_array().unwrap() {
        let fields = &notice["fields"];
        for path in notice["resynced"].as_array().unwrap() {
            resynced.extend(at_instances(path.as_str().unwrap()));
        }
        for path in notice["info"].as_array().unwrap() {
            let path = path.as_str().unwrap();
            for at in at_instances(path) {
                let entry = info.entry(at).or_default();
                for field in fields[path].as_array().unwrap() {
                    entry.push(field.as_str().unwrap().to_string());
                }
            }
        }
    }
    resynced.sort();
    resynced.dedup();
    let roots = resynced.clone();
    resynced.retain(|path| !roots.iter().any(|root| root != path && beneath(path, root)));
    info.retain(|path, _| !roots.iter().any(|root| beneath(path, root)));
    for fields in info.values_mut() {
        fields.sort();
        fields.dedup();
    }
    Report { resynced, info }
}

/// What `changes` reports, in the oracle's terms. An info-only prim's
/// fields are not compared: a prim spec edit reports none here.
fn actual(changes: &Changes, loaded: &LoadedStage) -> Report {
    let paths = loaded.store.paths();
    let tokens = loaded.store.tokens();
    let prim = |id: PathId| paths.display(id, tokens);
    let mut resynced: Vec<String> = changes.resynced.iter().map(|id| prim(*id)).collect();
    resynced.extend(changes.resynced_properties.iter().map(|path| {
        format!(
            "{}.{}",
            prim(path.prim_path()),
            tokens.resolve(path.property())
        )
    }));
    resynced.sort();
    let info = changes
        .changed_info_only
        .iter()
        .map(|&path| (prim(path), Vec::new()))
        .collect();
    Report { resynced, info }
}

fn layer(loaded: &LoadedStage, name: &str) -> LayerId {
    *loaded
        .layer_names
        .iter()
        .find(|(_, found)| found.rsplit('/').next() == Some(name))
        .unwrap_or_else(|| panic!("no layer {name}"))
        .0
}

/// The transaction that makes the oracle's edit `name`.
fn edit(name: &str, loaded: &mut LoadedStage, live: &LiveStage) -> Transaction {
    let scene = layer(loaded, "scene.usda");
    let weak = layer(loaded, "weak.usda");
    let asset = layer(loaded, "asset.usda");
    let store = &mut loaded.store;
    let path = |text: &str| store.path(text);
    let pebble = path("/World/Pebble");
    let moss = path("/World/Moss");
    let patch = path("/World/Moss/Patch");
    let ghost = path("/World/Ghost");
    let spirit = path("/World/Spirit");
    let moved = path("/Spirit");
    let base = path("/_Base");
    let ground = path("/Ground");
    let world = path("/World");
    let shrub = path("/World/Shrub");
    let lamp = path("/World/Lamp");
    let cloud = path("/Sky/Cloud");
    let extra = path("/Robot/Extra");
    let antenna = path("/Asset/Antenna");
    let roughness = store.property_path("/World/Rock.roughness");
    let tokens = store.tokens();
    let token = |text: &str| Some(tokens.intern(text));
    let prim = |layer: LayerId, path: PathId| EditTarget::for_layer(layer).prim(path);
    let mut txn = Transaction::new();
    match name {
        "define_child" => {
            txn.create_prim(prim(scene, pebble), Specifier::Def, token("Sphere"));
        }
        "define_with_ancestors" => {
            txn.create_prim(prim(scene, moss), Specifier::Def, None);
            txn.create_prim(prim(scene, patch), Specifier::Def, token("Mesh"));
        }
        "override_prim" => {
            txn.create_prim(prim(scene, ghost), Specifier::Over, None);
        }
        "create_class" => {
            txn.create_prim(prim(scene, base), Specifier::Class, None);
        }
        "define_root" => {
            txn.create_prim(prim(scene, ground), Specifier::Def, token("Xform"));
        }
        "remove_prim" => {
            txn.remove_spec(prim(scene, pebble));
        }
        "remove_with_children" => {
            txn.remove_spec(prim(scene, moss));
        }
        "remove_root" => {
            txn.remove_spec(prim(scene, ground));
        }
        "reorder_children" => {
            let order = vec![tokens.intern("Lamp"), tokens.intern("Rock")];
            txn.set_prim_order(prim(scene, world), order);
        }
        "clear_reorder" => {
            txn.clear_prim_order(prim(scene, world));
        }
        "rename_prim" | "reparent_prim" => {
            let (from, to) = if name == "rename_prim" {
                (ghost, spirit)
            } else {
                (spirit, moved)
            };
            return NamespaceEdit::prepare(
                live.stage(),
                store,
                &EditTarget::for_layer(scene),
                TargetPath::Prim(from),
                TargetPath::Prim(to),
            )
            .expect(name)
            .into_transaction();
        }
        "create_property" => {
            let float = PropertySpec::typed_attribute(PropertyType::parse("float").unwrap());
            txn.create_property(
                EditTarget::for_layer(scene).property(roughness),
                float.custom(),
            );
        }
        "define_in_sublayer" => {
            txn.create_prim(prim(weak, shrub), Specifier::Def, token("Xform"));
        }
        "define_in_sublayer_with_new_over" => {
            txn.create_prim(prim(weak, cloud), Specifier::Def, token("Xform"));
        }
        "override_in_sublayer" => {
            txn.create_prim(prim(weak, lamp), Specifier::Over, None);
        }
        "define_under_instance" => {
            txn.create_prim(prim(scene, extra), Specifier::Def, token("Sphere"));
        }
        "define_in_prototype_source" => {
            txn.create_prim(prim(asset, antenna), Specifier::Def, token("Cylinder"));
        }
        "remove_from_prototype_source" => {
            txn.remove_spec(prim(asset, antenna));
        }
        other => panic!("no edit for {other}"),
    }
    txn
}

/// The oracle's report of `name` with the differences of the module docs.
fn corrected(name: &str, mut report: Report) -> Report {
    match name {
        "define_under_instance" => {
            report.resynced = vec!["/Robot/Extra".into()];
        }
        "define_in_sublayer_with_new_over" => {
            report.info.remove("/Sky");
            report.resynced = vec!["/Sky".into()];
        }
        "override_in_sublayer" => {
            report.info.remove("/World/Lamp");
            report.resynced = vec!["/World/Lamp".into()];
        }
        _ => {}
    }
    report
}

#[test]
fn prim_spec_edits_resync_only_the_prim() {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(
        version.starts_with("0.26."),
        "oracle from OpenUSD {version}"
    );
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/prim_resync/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let options = StageOptions {
        schemas: Some(std::sync::Arc::new(layerstack_schemas::openusd(
            loaded.store.tokens(),
        ))),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options);
    let mut failures = Vec::new();
    for record in oracle["records"].as_array().unwrap() {
        let name = record["name"].as_str().unwrap();
        let txn = edit(name, &mut loaded, &live);
        let applied = live.apply(&mut loaded.store, &txn).expect(name);
        let found = actual(&applied.changes, &loaded);
        let mut want = corrected(name, expected(record));
        // Fields are compared for info-only paths only, which a prim spec
        // edit never reports in the oracle.
        for fields in want.info.values_mut() {
            fields.clear();
        }
        if found != want {
            failures.push(format!("{name}:\n  want {want:?}\n  got  {found:?}"));
        }
        for (path, names) in record["children"].as_object().unwrap() {
            let at = if path == "prototype" { "/Robot" } else { path };
            let id = loaded.store.path(at);
            let view = live.view(&loaded.store);
            let found: Vec<String> = view
                .prim(id)
                .map(|prim| prim.all_children().map(|c| c.name().to_string()).collect())
                .unwrap_or_default();
            let want: Vec<String> = names
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap().to_string())
                .collect();
            // An instance's children come from its prototype, compared
            // under "prototype".
            if path == "/Robot" {
                continue;
            }
            if found != want {
                failures.push(format!(
                    "{name} {path} children:\n  want {want:?}\n  got  {found:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
