// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Adding and removing relationship targets and attribute connections one
//! at a time agrees with OpenUSD 26.08, call by call, and a stack of
//! sublayers, each appending or deleting through the generated schema
//! handles, composes the target order OpenUSD composes.
//!
//! `scripts/target_list_edits_oracle.py` writes the layers of
//! `fixtures/target_list_edits` and records what OpenUSD does with them
//! (see the script for every case):
//!
//! - `Transaction::add_target` and `remove_target`, and the generated
//!   handles' `add_to_<rel>` and `remove_from_<rel>` (`CollectionAPI`'s
//!   `includes` and `excludes`, `LightAPI`'s `filters`), accept or reject
//!   each call as `UsdRelationship::AddTarget` / `RemoveTarget`,
//!   `UsdAttribute::AddConnection` / `RemoveConnection` and the schemas'
//!   `Create*Rel()` do, and leave the same property spec after it: its
//!   kind, `custom`, type name and list op. Each call is one transaction
//!   applied through a `LiveStage`; undoing every call restores the
//!   layers.
//! - `scopes.usda` sublayers three scopes, strongest first. The two weaker
//!   append to a light-link collection and the light's filters and the
//!   strongest deletes, each through its own edit target; the composed
//!   targets are OpenUSD's, in its order, live and freshly composed.
//!
//! Spec: AOUSD Core §7.6.4.2.3 (`connectionPaths`), §7.6.5.1.1
//! (`targetPaths`), §12.4 (list ops), §12.2.4 (properties).

#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::{
    ArcKind, EditTarget, InMemoryStore, Layer, LayerId, ListOp, ListPosition, LiveStage,
    PathInterner, PropertyKind, PropertyPath, PropertySpec, SpecPath, Stage, StageOptions,
    TargetPath, TokenInterner, Transaction,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::usd::{CollectionApi, CollectionApiEdit};
use layerstack_schemas::usd_lux::{SphereLight, SphereLightEdit};
use layerstack_schemas::{Scene, SchemaEdit};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../fixtures/target_list_edits/oracle.json");

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn load(name: &str) -> LoadedStage {
    let loaded = load_entry_usda(
        &workspace_root()
            .join("layerstack_conformance/fixtures/target_list_edits")
            .join(name),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

/// A stage composed with OpenUSD's schemas, which the generated handles
/// author through.
fn live_of(loaded: &mut LoadedStage) -> LiveStage {
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&loaded.store.tokens))),
        ..StageOptions::default()
    };
    LiveStage::compose(&mut loaded.store, loaded.root_layer, options)
}

fn layer_named(loaded: &LoadedStage, name: &str) -> LayerId {
    *loaded
        .layer_names
        .iter()
        .find(|(_, n)| n.ends_with(name))
        .unwrap_or_else(|| panic!("no layer {name}"))
        .0
}

fn edit_target(loaded: &mut LoadedStage, live: &LiveStage, target: &Json) -> EditTarget {
    let kind = match target["kind"].as_str().unwrap() {
        "root" => return EditTarget::for_layer(loaded.root_layer),
        "sublayer" => return EditTarget::for_layer(layer_named(loaded, "edits_sub.usda")),
        "variant" => ArcKind::Variants,
        "reference" => ArcKind::References,
        other => panic!("no target {other}"),
    };
    let prim = loaded.store.path(target["prim"].as_str().unwrap());
    let graph = live.stage().explain_prim_graph(prim).expect("a graph");
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| graph.node(*id).is_some_and(|n| n.arc_kind() == kind))
        .expect("a node of the arc");
    EditTarget::for_node(live.stage(), prim, node).expect("an edit target")
}

fn position(step: &Json) -> ListPosition {
    match step["position"].as_str().unwrap_or("back_of_prepend") {
        "front_of_prepend" => ListPosition::FrontOfPrepend,
        "back_of_prepend" => ListPosition::BackOfPrepend,
        "front_of_append" => ListPosition::FrontOfAppend,
        "back_of_append" => ListPosition::BackOfAppend,
        other => panic!("no position {other}"),
    }
}

fn list_op(op: &ListOp<TargetPath>, paths: &PathInterner, tokens: &TokenInterner) -> Json {
    let items = |list: &[TargetPath]| -> Vec<String> {
        list.iter().map(|t| t.display(paths, tokens)).collect()
    };
    json!({
        "explicit": op.explicit.as_deref().map(items),
        "prepend": items(&op.prepend),
        "append": items(&op.append),
        "delete": items(&op.delete),
        "add": items(&op.add),
        "reorder": items(&op.reorder),
    })
}

/// The property spec as the oracle records it.
fn spec_record(spec: &PropertySpec, paths: &PathInterner, tokens: &TokenInterner) -> Json {
    let (kind, type_name) = match spec.kind {
        PropertyKind::Relationship => ("relationship", None),
        PropertyKind::Attribute => (
            "attribute",
            spec.type_name.as_ref().map(|ty| ty.type_name().to_string()),
        ),
    };
    json!({
        "kind": kind,
        "custom": spec.custom,
        "type": type_name,
        "targets": spec.targets.as_ref().map(|op| list_op(op, paths, tokens)),
    })
}

/// The property `name` of the prim or variant spec at `path` in `layer`.
fn property_at<'a>(
    layer: &'a Layer,
    path: &SpecPath,
    name: &str,
    paths: &PathInterner,
    tokens: &TokenInterner,
) -> Option<&'a PropertySpec> {
    let properties = match layer.variant_spec_at(path, paths) {
        Some(variant) => &variant.properties,
        None if path.variant_chain().is_empty() => {
            let sites: Vec<_> = path.variant_selection_sites().collect();
            &layer.prim_spec_in(path.prim_path(), &sites)?.properties
        }
        None => return None,
    };
    properties
        .iter()
        .find(|entry| tokens.resolve(entry.name) == name)
        .map(|entry| &*entry.spec)
}

/// The property spec `target` addresses for `prim`'s `name`, recorded.
fn addressed(
    loaded: &mut LoadedStage,
    target: &EditTarget,
    prim: layerstack::PathId,
    name: &str,
) -> Json {
    let store = &mut loaded.store;
    let Some(path) = target.map_to_spec_path(prim, &mut store.paths) else {
        return Json::Null;
    };
    property_at(
        &store.layers[&target.layer()],
        &path,
        name,
        &store.paths,
        &store.tokens,
    )
    .map_or(Json::Null, |spec| {
        spec_record(spec, &store.paths, &store.tokens)
    })
}

/// Makes the step's call on `prim` through `target`: a `Transaction` edit
/// for a relationship or attribute, else the generated handle's method.
fn transaction(
    loaded: &mut LoadedStage,
    live: &LiveStage,
    target: &EditTarget,
    prim: layerstack::PathId,
    step: &Json,
) -> Transaction {
    let store = &mut loaded.store;
    let item = TargetPath::parse(
        step["target"].as_str().unwrap(),
        &store.tokens,
        &store.paths,
    )
    .expect("a target path");
    let call = step["call"].as_str().unwrap();
    match call {
        "add_target" | "remove_target" | "add_connection" | "remove_connection" => {
            let name = store.tokens.intern(step["property"].as_str().unwrap());
            let at = target.property(PropertyPath::new(prim, name));
            let mut txn = Transaction::new();
            if call.starts_with("add_") {
                txn.add_target(at, item, position(step));
            } else {
                txn.remove_target(at, item);
            }
            txn
        }
        _ => {
            let mut edit = SchemaEdit::new(live.stage(), store, target.clone());
            let collection = CollectionApiEdit::new(&edit, prim, "lightLink").expect("the prim");
            let light = SphereLightEdit::new(&edit, prim)
                .expect("a sphere light")
                .light_api();
            let position = position(step);
            match call {
                "add_to_includes" => {
                    collection.add_to_includes(&mut edit, item, position);
                }
                "remove_from_includes" => {
                    collection.remove_from_includes(&mut edit, item);
                }
                "add_to_excludes" => {
                    collection.add_to_excludes(&mut edit, item, position);
                }
                "remove_from_excludes" => {
                    collection.remove_from_excludes(&mut edit, item);
                }
                "add_to_filters" => {
                    light.add_to_filters(&mut edit, item, position);
                }
                "remove_from_filters" => {
                    light.remove_from_filters(&mut edit, item);
                }
                other => panic!("no call {other}"),
            }
            edit.finish()
        }
    }
}

#[test]
fn target_edits_agree_with_openusd_call_by_call() {
    let oracle = oracle();
    let cases = oracle["edits"].as_array().unwrap();
    assert!(cases.len() >= 17);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load("edits.usda");
        let original = loaded.store.layers.clone();
        let mut live = live_of(&mut loaded);
        let target = edit_target(&mut loaded, &live, &case["target"]);
        let prim = loaded.store.path(case["prim"].as_str().unwrap());
        let mut accepted = Vec::new();
        let mut inverses = Vec::new();
        for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let txn = transaction(&mut loaded, &live, &target, prim, step);
            match live.apply(&mut loaded.store, &txn) {
                Ok(applied) => {
                    accepted.push(true);
                    inverses.push(applied.inverse);
                }
                Err(_) => accepted.push(false),
            }
            let property = step["property"].as_str().unwrap();
            assert_eq!(
                addressed(&mut loaded, &target, prim, property),
                case["after"][index],
                "{name}: spec after step {index}"
            );
        }
        let want: Vec<bool> = serde_json::from_value(case["accepted"].clone()).unwrap();
        assert_eq!(accepted, want, "{name}: accepted calls");

        for inverse in inverses.iter().rev() {
            live.apply(&mut loaded.store, inverse)
                .unwrap_or_else(|e| panic!("{name}: undo: {e}"));
        }
        for (id, layer) in &original {
            let now = &loaded.store.layers[id];
            assert_eq!(now.prims(), layer.prims(), "{name}: undo restores {id:?}");
            assert_eq!(
                now.variant_prims(),
                layer.variant_prims(),
                "{name}: undo restores {id:?}"
            );
        }
    }
}

/// The targets of the light's collection and filters, as composed.
fn composed(stage: &Stage, store: &InMemoryStore, prim: layerstack::PathId) -> Json {
    let scene = Scene::new(stage, store);
    let collection =
        CollectionApi::get(&scene, prim, "lightLink").expect("the light-link collection");
    let light = SphereLight::new(&scene, prim).expect("a sphere light");
    let names = |targets: Vec<TargetPath>| -> Vec<String> {
        targets
            .iter()
            .map(|t| t.display(&store.paths, &store.tokens))
            .collect()
    };
    json!({
        "includes": names(collection.includes()),
        "excludes": names(collection.excludes()),
        "filters": names(light.light_api().filters()),
    })
}

#[test]
fn scopes_compose_appends_and_deletes_in_openusd_order() {
    let oracle = oracle();
    let scopes = &oracle["scopes"];
    let mut loaded = load("scopes.usda");
    let mut live = live_of(&mut loaded);
    let prim = loaded.store.path(scopes["prim"].as_str().unwrap());
    for (index, step) in scopes["steps"].as_array().unwrap().iter().enumerate() {
        let target = EditTarget::for_layer(layer_named(&loaded, step["layer"].as_str().unwrap()));
        let txn = transaction(&mut loaded, &live, &target, prim, step);
        let accepted = live.apply(&mut loaded.store, &txn).is_ok();
        assert_eq!(json!(accepted), step["accepted"], "step {index}");
        let property = step["property"].as_str().unwrap();
        assert_eq!(
            addressed(&mut loaded, &target, prim, property),
            step["after"],
            "spec after step {index}"
        );
    }
    assert_eq!(
        composed(live.stage(), &loaded.store, prim),
        scopes["composed"],
        "live"
    );
    let fresh = live_of(&mut loaded);
    let fresh = fresh.stage();
    assert_eq!(
        composed(fresh, &loaded.store, prim),
        scopes["composed"],
        "fresh"
    );
}
