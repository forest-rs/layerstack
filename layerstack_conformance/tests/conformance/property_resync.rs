// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Change reports for property spec additions, removals and renames, as
//! OpenUSD 26.11 sends them.
//!
//! `scripts/property_resync_oracle.py` takes a fixed sequence of edits on
//! `fixtures/property_resync/scene.usda` and records, call by call, the
//! `UsdNotice::ObjectsChanged` notices each sent and the composed property
//! names after it. Here each edit is one transaction on a `LiveStage`, and
//! its [`Changes`] must report what the edit's notices do, merged as one
//! `SdfChangeBlock` merges them (`create_with_default_in_block` records
//! that): a path resynced by one notice and info-only in another is
//! resynced, and a path beneath a resync is not reported.
//!
//! A C++ property path maps to [`Changes::resynced_properties`] when
//! resynced, and to its prim in [`Changes::changed_info_only`] with the
//! property's fields in [`Changes::properties_for`] when info-only.
//!
//! Differences, deliberate:
//!
//! - A property renamed with `UsdNamespaceEditor::RenameProperty` is
//!   reported by OpenUSD as an info-only change of the new path with no
//!   fields, and the old path not at all (`UsdStage::_HandleLayersDidChange`
//!   resyncs only property entries that add or remove specs, and a rename's
//!   `SdfChangeList` entry has `didRename` alone), so a client caching the
//!   old property is not told it is gone. A [`NamespaceEdit`] rewrites the
//!   prim's spec, which resyncs the prim here: conservative, and it covers
//!   both property paths.
//! - layerstack names no prototype prims; it composes an instance's
//!   descendants per instance. An edit OpenUSD reports at
//!   `/__Prototype_1/Ball.radius`, the prototype of the one instance
//!   `/Robot`, is reported at `/Robot/Ball.radius`.
//! - Creating the attribute `/Lamp.weight` through a layer that has no
//!   `/Lamp` spec creates the `over` too. OpenUSD reports the inert spec as
//!   an info-only change of `/Lamp`; here a prim spec created resyncs the
//!   prim, which covers the property.
//! - A prim spec created is reported with an info-only change of the
//!   pseudo-root, whose spec lists the new child; OpenUSD reports only
//!   the resync.

use std::collections::BTreeMap;

use layerstack::{
    Changes, EditTarget, FieldValue, LayerId, ListPosition, LiveStage, NamespaceEdit, PathId,
    PropertyField, PropertyPath, PropertySpec, PropertyType, Specifier, StageOptions, TargetPath,
    Transaction, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/property_resync/oracle.json");

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

/// The notices of one oracle record merged as one change block merges
/// them, prototype paths at their instance (see the module docs).
fn expected(record: &Json) -> Report {
    let mut resynced: Vec<String> = Vec::new();
    let mut info: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for notice in record["notices"].as_array().unwrap() {
        let fields = &notice["fields"];
        for path in notice["resynced"].as_array().unwrap() {
            resynced.push(path.as_str().unwrap().to_string());
        }
        for path in notice["info"].as_array().unwrap() {
            let path = path.as_str().unwrap();
            let entry = info.entry(path.to_string()).or_default();
            for field in fields[path].as_array().unwrap() {
                entry.push(field.as_str().unwrap().to_string());
            }
        }
    }
    let instance = |path: &str| path.replace("/__Prototype_1", "/Robot");
    let mut resynced: Vec<String> = resynced.iter().map(|path| instance(path)).collect();
    let mut info: BTreeMap<String, Vec<String>> = info
        .into_iter()
        .map(|(path, fields)| (instance(&path), fields))
        .collect();
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

/// OpenUSD's name for a changed property field.
fn field_name(field: PropertyField, is_relationship: bool, loaded: &LoadedStage) -> String {
    match field {
        PropertyField::Default => "default".into(),
        PropertyField::TimeSamples | PropertyField::Animation => "timeSamples".into(),
        PropertyField::Targets if is_relationship => "targetPaths".into(),
        PropertyField::Targets => "connectionPaths".into(),
        PropertyField::Metadata(key) => loaded.store.tokens().resolve(key).to_string(),
        other => panic!("unexpected field {other:?}"),
    }
}

fn actual(changes: &Changes, live: &LiveStage, loaded: &LoadedStage) -> Report {
    let paths = loaded.store.paths();
    let tokens = loaded.store.tokens();
    let prim = |id: PathId| paths.display(id, tokens);
    let property = |path: PropertyPath| {
        format!(
            "{}.{}",
            prim(path.prim_path()),
            tokens.resolve(path.property())
        )
    };
    let mut resynced: Vec<String> = changes.resynced.iter().map(|id| prim(*id)).collect();
    resynced.extend(
        changes
            .resynced_properties
            .iter()
            .map(|path| property(*path)),
    );
    resynced.sort();
    let mut info = BTreeMap::new();
    let view = live.view(&loaded.store);
    for &path in &changes.changed_info_only {
        match changes.properties_for(path) {
            Some(fields) if !fields.is_empty() => {
                for change in fields {
                    let at = PropertyPath::new(path, change.name);
                    let is_relationship = view
                        .prim(path)
                        .and_then(|p| p.relationship(change.name))
                        .is_some();
                    info.entry(property(at))
                        .or_insert_with(Vec::new)
                        .push(field_name(change.field, is_relationship, loaded));
                }
            }
            _ => {
                info.insert(prim(path), Vec::new());
            }
        }
    }
    for fields in info.values_mut() {
        fields.sort();
        fields.dedup();
    }
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
    let property = |path: &str| store.property_path(path);
    let rock_visibility = property("/Rock.visibility");
    let rock_roughness = property("/Rock.roughness");
    let rock_proxy = property("/Rock.proxyPrim");
    let rock_size = property("/Rock.size");
    let rock_weight = property("/Rock.weight");
    let lamp_weight = property("/Lamp.weight");
    let intensity = property("/Asset.intensity");
    let radius = property("/Asset/Ball.radius");
    let rotate = property("/Rock.xformOp:rotateZ");
    let translate = property("/Rock.xformOp:translate");
    let order = property("/Rock.xformOpOrder");
    let lift = property("/Rock.lift");
    let gloss = property("/Rock.gloss");
    let pebble_size = property("/Pebble.size");
    let looks = store.path("/Looks");
    let pebble = store.path("/Pebble");
    let tokens = store.tokens();
    let token = |text: &str| Value::Token(tokens.intern(text));
    let at = |layer: LayerId, path: PropertyPath| EditTarget::for_layer(layer).property(path);
    let typed = |name: &str| PropertySpec::typed_attribute(PropertyType::parse(name).unwrap());
    let mut txn = Transaction::new();
    match name {
        "set_visibility" => {
            txn.set_default(at(scene, rock_visibility), token("invisible"));
        }
        "change_visibility" => {
            txn.set_default(at(scene, rock_visibility), token("inherited"));
        }
        "remove_visibility" => {
            txn.remove_spec(at(scene, rock_visibility));
        }
        "create_required_only" => {
            txn.create_property(at(scene, rock_roughness), typed("float").custom());
        }
        "set_required_default" => {
            txn.set_default(at(scene, rock_roughness), Value::Float(0.5));
        }
        "create_relationship" => {
            txn.create_property(at(scene, rock_proxy), PropertySpec::relationship());
        }
        "add_relationship_target" => {
            txn.add_target(
                at(scene, rock_proxy),
                TargetPath::Prim(looks),
                ListPosition::BackOfPrepend,
            );
        }
        "remove_relationship" => {
            txn.remove_spec(at(scene, rock_proxy));
        }
        "set_stronger" => {
            txn.set_default(at(scene, rock_size), Value::Double(2.0));
        }
        "remove_stronger" => {
            txn.remove_spec(at(scene, rock_size));
        }
        "remove_last" => {
            txn.remove_spec(at(weak, rock_size));
        }
        "create_in_sublayer" => {
            txn.create_property(
                at(weak, rock_weight),
                typed("double").custom().with_default(Value::Double(3.0)),
            );
        }
        "create_with_new_over" => {
            txn.create_prim(
                EditTarget::for_layer(weak).prim(lamp_weight.prim_path()),
                Specifier::Over,
                None,
            );
            txn.create_property(
                at(weak, lamp_weight),
                typed("double").custom().with_default(Value::Double(1.0)),
            );
        }
        "create_in_reference" => {
            txn.create_property(
                at(asset, intensity),
                typed("float").with_default(Value::Float(2.0)),
            );
        }
        "change_in_reference" => {
            txn.set_default(at(asset, intensity), Value::Float(4.0));
        }
        "create_in_prototype_source" => {
            txn.create_property(
                at(asset, radius),
                typed("double").with_default(Value::Double(2.0)),
            );
        }
        "remove_in_reference" => {
            txn.remove_spec(at(asset, intensity));
        }
        "add_rotate_op" => {
            txn.create_property(at(scene, rotate), typed("float"));
            txn.set_default(
                at(scene, order),
                Value::array(vec![token("xformOp:translate"), token("xformOp:rotateZ")]),
            );
            txn.set_default(at(scene, rotate), Value::Float(45.0));
        }
        "change_translate" => {
            txn.set_default(at(scene, translate), Value::Vec3d([4.0, 5.0, 6.0]));
        }
        "rename_property" => {
            let renamed = NamespaceEdit::prepare(
                live.stage(),
                store,
                &EditTarget::for_layer(scene),
                TargetPath::Property(rock_roughness),
                TargetPath::Property(gloss),
            )
            .expect("rename");
            txn = renamed.into_transaction();
        }
        "create_prim_and_property" => {
            txn.create_prim(
                EditTarget::for_layer(scene).prim(pebble),
                Specifier::Def,
                None,
            );
            txn.create_property(
                at(scene, pebble_size),
                typed("double").with_default(Value::Double(1.0)),
            );
        }
        "create_with_default_in_block" => {
            txn.create_property(
                at(scene, lift),
                typed("double").with_default(Value::Double(0.5)),
            );
        }
        "set_metadata" => {
            let key = tokens.intern("documentation");
            txn.set_metadata(
                at(scene, gloss),
                key,
                FieldValue::Value(Value::String("polish".into())),
            );
        }
        other => panic!("no edit for {other}"),
    }
    txn
}

/// The oracle's report of `name` with the differences of the module docs.
fn corrected(name: &str, mut report: Report) -> Report {
    match name {
        "rename_property" => {
            report.info.remove("/Rock.gloss");
            report.resynced = vec!["/Rock".into()];
        }
        "create_with_new_over" => {
            report.info.remove("/Lamp");
            report.resynced = vec!["/Lamp".into()];
            report.info.insert("/".into(), Vec::new());
        }
        "create_prim_and_property" => {
            report.info.insert("/".into(), Vec::new());
        }
        _ => {}
    }
    report
}

#[test]
fn property_spec_edits_resync_property_paths() {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(
        version.starts_with("0.26."),
        "oracle from OpenUSD {version}"
    );
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/property_resync/scene.usda"),
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
        let found = actual(&applied.changes, &live, &loaded);
        let want = corrected(name, expected(record));
        if found != want {
            failures.push(format!("{name}:\n  want {want:?}\n  got  {found:?}"));
        }
        for (path, names) in record["properties"].as_object().unwrap() {
            let prim = loaded.store.path(path);
            let view = live.view(&loaded.store);
            let prim = view.prim(prim).expect(path);
            let mut found: Vec<String> = prim
                .property_names()
                .into_iter()
                .map(str::to_string)
                .collect();
            found.sort();
            let want: Vec<String> = names
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap().to_string())
                .collect();
            if found != want {
                failures.push(format!(
                    "{name} {path} properties:\n  want {want:?}\n  got  {found:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
