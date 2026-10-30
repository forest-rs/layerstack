// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Change reports for prim spec additions, removals and child reorders,
//! and for the arcs, metadata and layers above them, as OpenUSD 26.11
//! sends them.
//!
//! `scripts/prim_resync_oracle.py` takes a fixed sequence of edits on
//! `fixtures/prim_resync/scene.usda` and records, call by call, the
//! `UsdNotice::ObjectsChanged` notices each sent and the composed child
//! names after it. Here each edit is one transaction on a `LiveStage` (a
//! layer muted or unmuted is a stage control, recomposed), and its
//! [`Changes`] must report what the edit's notices do, merged as one
//! `SdfChangeBlock` merges them: a path resynced by one notice and
//! info-only in another is resynced, and a path beneath a resync is not
//! reported.
//!
//! The points it pins: a prim created or removed resyncs that prim alone.
//! Its parent, whose spec lists one child more or fewer, is not reported
//! (OpenUSD records no field change for `primChildren`), so a client
//! caching the parent's other inputs keeps them. A reorder (`primOrder`)
//! resyncs the parent. An inert `over` added or removed for a prim that
//! composes either way, in any layer of the root layer stack, the session
//! layer or a referenced asset, changes that prim info-only, and each
//! prim composing from it (`/Echo` inherits `/World/Lamp`); one holding an
//! attribute spec resyncs the attribute too, and one holding metadata
//! composing nothing is info-only. `kind` resyncs its prim, and a field
//! of the root layer composing nothing changes `/` info-only. A sublayer
//! inserted or removed resyncs the prims whose specs it adds or takes
//! away, changes those its `over`s hold properties of info-only, with the
//! properties resynced, and changes `/` info-only.
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
//! - An `over` of `/Thing` added to the weaker layer moves `Thing` before
//!   `Robot` and `Drone` (child names compose weakest layer first).
//!   OpenUSD reports `/Thing` info-only and keeps the old order, which a
//!   stage opened afresh does not compose (the oracle records the fresh
//!   order); layerstack composes the new order and resyncs `/Thing`, so
//!   that a client reads the root prims again.
//! - OpenUSD reports `/Echo` info-only with every change of the root
//!   layer's own fields, which compose nothing into it; layerstack does
//!   not.
//! - A layer of the root layer stack muted or unmuted recomposes the stage
//!   whole here, and resyncs `/`. OpenUSD resyncs the prims and properties
//!   whose specs came or went, and changes the rest of the prims whose spec
//!   stacks changed, and `/`, info-only, as both do for a sublayer
//!   inserted or removed.

use std::collections::BTreeMap;

use layerstack::{
    Changes, EditTarget, FieldValue, Layer, LayerId, ListPosition, LiveStage, NamespaceEdit,
    PathId, PropertyField, PropertySpec, PropertyType, Reference, Specifier, StageOptions,
    SublayerEntry, TargetPath, Transaction, Value,
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
    // A prim with a complete property inventory is reported at the
    // properties it names, with their fields, as OpenUSD reports them.
    let mut info = BTreeMap::new();
    for &path in &changes.changed_info_only {
        match changes.properties_for(path) {
            Some(fields) if !fields.is_empty() => {
                for change in fields {
                    let field = match change.field {
                        PropertyField::Targets => "targetPaths".to_string(),
                        PropertyField::Default => "default".to_string(),
                        PropertyField::Metadata(key) => tokens.resolve(key).to_string(),
                        other => format!("{other:?}"),
                    };
                    info.entry(format!("{}.{}", prim(path), tokens.resolve(change.name)))
                        .or_insert_with(Vec::new)
                        .push(field);
                }
            }
            _ => {
                info.insert(prim(path), Vec::new());
            }
        }
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

/// The layers the replay adds to the loaded store: the session layer (an
/// anonymous one, as `UsdStage::Open` makes), and the sublayers the
/// sequence inserts.
struct Extra {
    session: LayerId,
    empty: LayerId,
    comet: LayerId,
    heavy: LayerId,
}

impl Extra {
    fn add(loaded: &mut LoadedStage) -> Self {
        let next = loaded.store.layer_ids().into_iter().max().unwrap().0 + 1;
        let [session, empty, comet, heavy] = [0, 1, 2, 3].map(|at| LayerId(next + at));
        for id in [session, empty, comet, heavy] {
            loaded.store.insert_layer(Layer::new(id));
        }
        let mut txn = Transaction::new();
        let path = loaded.store.path("/Comet");
        txn.create_prim(
            EditTarget::for_layer(comet).prim(path),
            Specifier::Def,
            None,
        );
        let rock = loaded.store.path("/World/Rock");
        txn.create_prim(
            EditTarget::for_layer(heavy).prim(rock),
            Specifier::Over,
            None,
        );
        let mass = loaded.store.property_path("/World/Rock.mass");
        let double = PropertySpec::typed_attribute(PropertyType::parse("double").unwrap());
        txn.create_property(
            EditTarget::for_layer(heavy).property(mass),
            double.with_default(Value::Double(2.0)),
        );
        txn.apply(&mut loaded.store)
            .expect("comet.usda, heavy.usda");
        Self {
            session,
            empty,
            comet,
            heavy,
        }
    }
}

/// The transaction that makes the oracle's edit `name`.
fn edit(name: &str, loaded: &mut LoadedStage, live: &LiveStage, added: &Extra) -> Transaction {
    let scene = layer(loaded, "scene.usda");
    let weak = layer(loaded, "weak.usda");
    let asset = layer(loaded, "asset.usda");
    let session = added.session;
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
    let moon = path("/Moon");
    let thing = path("/Thing");
    let thing_ball = path("/Thing/Ball");
    let asset_root = path("/Asset");
    let asset_ball = path("/Asset/Ball");
    let sky = path("/Sky");
    let robot = path("/Robot");
    let switch = path("/Switch");
    let rover = path("/Rover");
    let rock = path("/World/Rock");
    let roughness = store.property_path("/World/Rock.roughness");
    let weight = store.property_path("/Moon.weight");
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
        "rename_prim" | "reparent_prim" | "rename_target" => {
            let (from, to) = match name {
                "rename_prim" => (ghost, spirit),
                "reparent_prim" => (spirit, moved),
                _ => (path("/World/Robot"), path("/World/Rover")),
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
        "override_in_root" => {
            txn.create_prim(prim(scene, moon), Specifier::Over, None);
        }
        "remove_override_in_root" => {
            txn.remove_spec(prim(scene, moon));
        }
        "remove_override_in_sublayer" => {
            txn.remove_spec(prim(weak, lamp));
        }
        "override_reference_child" => {
            txn.create_prim(prim(scene, thing_ball), Specifier::Over, None);
        }
        "override_in_asset" => {
            txn.create_prim(prim(asset, asset_ball), Specifier::Over, None);
        }
        "remove_override_in_asset" => {
            txn.remove_spec(prim(asset, asset_ball));
        }
        "override_with_attribute" => {
            txn.create_prim(prim(scene, moon), Specifier::Over, None);
            let float = PropertySpec::typed_attribute(PropertyType::parse("float").unwrap());
            txn.create_property(EditTarget::for_layer(scene).property(weight), float);
        }
        "override_with_metadata" => {
            txn.create_prim(prim(session, sky), Specifier::Over, None);
            txn.set_metadata(
                prim(session, sky),
                tokens.intern("documentation"),
                FieldValue::Value(Value::string("clear")),
            );
        }
        "override_in_session" => {
            txn.create_prim(prim(session, lamp), Specifier::Over, None);
        }
        "override_instance_root" => {
            txn.create_prim(prim(weak, robot), Specifier::Over, None);
        }
        "remove_override_instance_root" => {
            txn.remove_spec(prim(weak, robot));
        }
        "select_variant" | "clear_variant" => {
            let night = (name == "select_variant").then(|| tokens.intern("night"));
            txn.set_variant_selection(prim(scene, switch), tokens.intern("look"), night);
        }
        "add_reference" => {
            txn.add_reference(
                prim(scene, rover),
                Reference::new(asset, asset_root),
                ListPosition::FrontOfPrepend,
            );
        }
        "clear_references" => {
            txn.clear_references(prim(scene, rover));
        }
        "set_kind" => {
            let component = tokens.intern("component");
            txn.set_metadata(
                prim(scene, rock),
                tokens.intern("kind"),
                FieldValue::Value(Value::Token(component)),
            );
        }
        "set_documentation" => {
            txn.set_metadata(
                prim(scene, rock),
                tokens.intern("documentation"),
                FieldValue::Value(Value::string("granite")),
            );
        }
        "deactivate" | "activate" => {
            txn.set_active(prim(scene, lamp), name == "activate");
        }
        "set_type_name" => {
            txn.set_type_name(prim(scene, lamp), tokens.intern("Scope"));
        }
        "set_specifier" => {
            txn.set_specifier(prim(scene, rover), Specifier::Class);
        }
        "override_reorders_children" => {
            txn.create_prim(prim(weak, thing), Specifier::Over, None);
        }
        "set_layer_documentation" => {
            txn.set_layer_metadata(
                scene,
                tokens.intern("documentation"),
                FieldValue::Value(Value::string("a scene")),
            );
        }
        "insert_empty_sublayer" | "insert_prim_sublayer" | "insert_opinion_sublayer" => {
            let sublayer = match name {
                "insert_empty_sublayer" => added.empty,
                "insert_prim_sublayer" => added.comet,
                _ => added.heavy,
            };
            txn.insert_sublayer(scene, 1, SublayerEntry::new(sublayer));
        }
        "remove_empty_sublayer" | "remove_prim_sublayer" | "remove_opinion_sublayer" => {
            txn.remove_sublayer(scene, 1);
        }
        other => panic!("no edit for {other}"),
    }
    txn
}

/// The oracle's report of `name` with the differences of the module docs.
fn corrected(name: &str, mut report: Report) -> Report {
    // The root layer's own fields compose nothing into `/Echo`.
    if report.info.contains_key("/") {
        report.info.remove("/Echo");
    }
    match name {
        "define_under_instance" => {
            report.resynced = vec!["/Robot/Extra".into()];
        }
        "override_reorders_children" => {
            report.info.remove("/Thing");
            report.resynced = vec!["/Thing".into()];
        }
        "mute_sublayer" | "unmute_sublayer" => {
            report = Report {
                resynced: vec!["/".into()],
                ..Report::default()
            };
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
    let extra = Extra::add(&mut loaded);
    let options = StageOptions {
        schemas: Some(std::sync::Arc::new(layerstack_schemas::openusd(
            loaded.store.tokens(),
        ))),
        session_layer: Some(extra.session),
        ..StageOptions::default()
    };
    let mut live = LiveStage::compose(&mut loaded.store, loaded.root_layer, options);
    let mut failures = Vec::new();
    for record in oracle["records"].as_array().unwrap() {
        let name = record["name"].as_str().unwrap();
        let changes = if let Some(mute) = name
            .strip_suffix("_sublayer")
            .filter(|verb| *verb == "mute" || *verb == "unmute")
        {
            let weak = layer(&loaded, "weak.usda");
            if mute == "mute" {
                live.mute_layer(weak).expect(name);
            } else {
                live.unmute_layer(weak);
            }
            live.recompose_changes(&mut loaded.store)
        } else {
            let txn = edit(name, &mut loaded, &live, &extra);
            live.apply(&mut loaded.store, &txn).expect(name).changes
        };
        let found = actual(&changes, &loaded);
        let mut want = corrected(name, expected(record));
        // Fields are compared for info-only properties: a prim spec edit
        // reports none for its prim, and an info-only prim here has no
        // inventory.
        for (path, fields) in want.info.iter_mut() {
            if !path.contains('.') {
                fields.clear();
            }
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
