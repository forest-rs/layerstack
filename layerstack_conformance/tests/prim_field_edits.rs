// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim field and composition arc edits agree with OpenUSD 26.08's
//! authoring calls, call by call.
//!
//! `scripts/prim_field_edits_oracle.py` opens
//! `fixtures/prim_field_edits/scene.usda` once per case, points the stage
//! at an edit target (the root layer, its sublayer, `/V`'s variant branch,
//! or `/R`'s reference into `asset.usda`) and makes a sequence of
//! `UsdPrim`, `UsdReferences`, `UsdPayloads`, `UsdInherits` and
//! `UsdSpecializes` calls. It records whether each call was accepted and
//! the fields the addressed spec then authors. Here each call is one
//! `Transaction` applied through a `LiveStage`, which must accept or reject
//! it as OpenUSD does and leave the same spec; applying the inverses in
//! reverse order must then restore every layer.
//!
//! Through `/R`'s reference, OpenUSD drops an inherit path the edit target
//! does not map, and authors an internal reference to an unmapped path
//! unchanged, reporting success either way; layerstack rejects both edits,
//! so a caller learns that nothing was authored (`DELIBERATE`).
//!
//! Spec: AOUSD Core §7.6.2 (prim spec fields), §10 (composition arcs),
//! §12.4 (list ops). OpenUSD: `pxr/usd/usd/listEditImpl.h`,
//! `SdfListEditorProxy`.

#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    ArcKind, EditTarget, InMemoryStore, LayerId, ListOp, ListPosition, LiveStage, Path, PathId,
    PrimSpec, Reference, ReferenceTarget, SpecPath, Specifier, Stage, StageOptions, Transaction,
    VariantSpec,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../fixtures/prim_field_edits/oracle.json");

/// Cases where layerstack deliberately differs, with what it does instead:
/// every call rejected, and the spec as the scene authors it.
const DELIBERATE: &[(&str, &[bool])] = &[("reference_target_rejects_unmapped", &[false; 4])];

fn load() -> LoadedStage {
    let path = workspace_root().join("layerstack_conformance/fixtures/prim_field_edits/scene.usda");
    let loaded = load_entry_usda(&path);
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
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
        "sublayer" => return EditTarget::for_layer(layer_named(loaded, "sub.usda")),
        "variant" => ArcKind::Variants,
        "reference" => ArcKind::References,
        other => panic!("no target {other}"),
    };
    let prim = loaded.store.path(target["prim"].as_str().unwrap());
    let graph = live
        .stage()
        .prim(prim, &loaded.store)
        .map(|p| p.composition_graph())
        .expect("a graph");
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| graph.node(*id).is_some_and(|n| n.arc_kind() == kind))
        .expect("a node of the arc");
    EditTarget::for_node(live.stage(), prim, node).expect("an edit target")
}

fn position(step: &Json) -> ListPosition {
    match step["position"].as_str().unwrap_or("front_of_prepend") {
        "front_of_prepend" => ListPosition::FrontOfPrepend,
        "back_of_prepend" => ListPosition::BackOfPrepend,
        "front_of_append" => ListPosition::FrontOfAppend,
        "back_of_append" => ListPosition::BackOfAppend,
        other => panic!("no position {other}"),
    }
}

/// The reference or payload a step names: internal (no asset, anchored at
/// the target's layer) or into `asset.usda`, to its default prim when the
/// path is empty.
fn arc(loaded: &mut LoadedStage, layer: LayerId, item: &Json) -> Reference {
    let path = item["path"].as_str().unwrap();
    let mut reference = match item["asset"].as_str().unwrap_or("") {
        "" => Reference::new(layer, loaded.store.path(path)),
        asset => {
            let id = layer_named(loaded, asset);
            if path.is_empty() {
                Reference::with_asset_default_prim(id, asset)
            } else {
                Reference::with_asset(id, loaded.store.path(path), asset)
            }
        }
    };
    reference.layer_offset.offset = item["offset"].as_f64().unwrap_or(0.0);
    reference.layer_offset.scale = item["scale"].as_f64().unwrap_or(1.0);
    reference
}

fn paths(loaded: &mut LoadedStage, value: &Json) -> ListOp<PathId> {
    let items = value.as_array().unwrap();
    ListOp::explicit(
        items
            .iter()
            .map(|p| loaded.store.path(p.as_str().unwrap()))
            .collect(),
    )
}

fn tokens(loaded: &mut LoadedStage, value: &Json) -> Vec<layerstack::TokenId> {
    let items = value.as_array().unwrap();
    items
        .iter()
        .map(|t| loaded.store.tokens.intern(t.as_str().unwrap()))
        .collect()
}

/// The transaction making the step's call at `prim` through `target`.
fn transaction(
    loaded: &mut LoadedStage,
    target: &EditTarget,
    prim: PathId,
    step: &Json,
) -> Transaction {
    let at = target.prim(prim);
    let layer = target.layer();
    let value = &step["value"];
    let mut txn = Transaction::new();
    match step["call"].as_str().unwrap() {
        "add_reference" => {
            let r = arc(loaded, layer, step);
            txn.add_reference(at, r, position(step))
        }
        "remove_reference" => {
            let r = arc(loaded, layer, step);
            txn.remove_reference(at, r)
        }
        "set_references" if value.is_null() => txn.clear_references(at),
        "set_references" => {
            let items = value.as_array().unwrap();
            let items = items.iter().map(|i| arc(loaded, layer, i)).collect();
            txn.set_references(at, ListOp::explicit(items))
        }
        "add_payload" => {
            let r = arc(loaded, layer, step);
            txn.add_payload(at, r, position(step))
        }
        "remove_payload" => {
            let r = arc(loaded, layer, step);
            txn.remove_payload(at, r)
        }
        "set_payloads" if value.is_null() => txn.clear_payloads(at),
        "set_payloads" => {
            let items = value.as_array().unwrap();
            let items = items.iter().map(|i| arc(loaded, layer, i)).collect();
            txn.set_payloads(at, ListOp::explicit(items))
        }
        "add_inherit" => {
            let path = loaded.store.path(step["path"].as_str().unwrap());
            txn.add_inherit(at, path, position(step))
        }
        "remove_inherit" => {
            let path = loaded.store.path(step["path"].as_str().unwrap());
            txn.remove_inherit(at, path)
        }
        "set_inherits" if value.is_null() => txn.clear_inherits(at),
        "set_inherits" => {
            let list = paths(loaded, value);
            txn.set_inherits(at, list)
        }
        "add_specialize" => {
            let path = loaded.store.path(step["path"].as_str().unwrap());
            txn.add_specialize(at, path, position(step))
        }
        "remove_specialize" => {
            let path = loaded.store.path(step["path"].as_str().unwrap());
            txn.remove_specialize(at, path)
        }
        "set_specializes" if value.is_null() => txn.clear_specializes(at),
        "set_specializes" => {
            let list = paths(loaded, value);
            txn.set_specializes(at, list)
        }
        "set_active" if value.is_null() => txn.clear_active(at),
        "set_active" => txn.set_active(at, value.as_bool().unwrap()),
        "set_instanceable" if value.is_null() => txn.clear_instanceable(at),
        "set_instanceable" => txn.set_instanceable(at, value.as_bool().unwrap()),
        "set_type_name" if value.is_null() => txn.clear_type_name(at),
        "set_type_name" => {
            let name = loaded.store.tokens.intern(value.as_str().unwrap());
            txn.set_type_name(at, name)
        }
        "set_specifier" => txn.set_specifier(
            at,
            match value.as_str().unwrap() {
                "def" => Specifier::Def,
                "over" => Specifier::Over,
                "class" => Specifier::Class,
                other => panic!("no specifier {other}"),
            },
        ),
        "set_prim_order" if value.is_null() => txn.clear_prim_order(at),
        "set_prim_order" => {
            let order = tokens(loaded, value);
            txn.set_prim_order(at, order)
        }
        "set_property_order" if value.is_null() => txn.clear_property_order(at),
        "set_property_order" => {
            let order = tokens(loaded, value);
            txn.set_property_order(at, order)
        }
        "add_applied_schema" => {
            let name = loaded.store.tokens.intern(step["name"].as_str().unwrap());
            txn.add_applied_schema(at, name)
        }
        "remove_applied_schema" => {
            let name = loaded.store.tokens.intern(step["name"].as_str().unwrap());
            txn.remove_applied_schema(at, name)
        }
        other => panic!("no call {other}"),
    };
    txn
}

fn list_op<T>(op: &ListOp<T>, item: impl Fn(&T) -> Json) -> Json {
    let items = |list: &[T]| list.iter().map(&item).collect::<Vec<_>>();
    json!({
        "explicit": op.explicit.as_deref().map(items),
        "prepend": items(&op.prepend),
        "append": items(&op.append),
        "delete": items(&op.delete),
    })
}

/// The fields the oracle records, as it records them.
fn fields(loaded: &LoadedStage, spec: Spec<'_>) -> Json {
    let store = &loaded.store;
    let path = |p: &PathId| json!(store.paths.display(*p, &store.tokens));
    let reference = |r: &Reference| {
        json!({
            "asset": r.asset_path().unwrap_or_default(),
            "path": match r.target {
                ReferenceTarget::Prim(p) => store.paths.display(p, &store.tokens),
                _ => String::new(),
            },
            "offset": r.layer_offset.offset,
            "scale": r.layer_offset.scale,
        })
    };
    let names = |names: &[layerstack::TokenId]| {
        json!(
            names
                .iter()
                .map(|n| store.tokens.resolve(*n))
                .collect::<Vec<_>>()
        )
    };
    let mut out = serde_json::Map::new();
    let (field, references, payloads, inherits, specializes, property_order) = match spec {
        Spec::Prim(spec) => {
            let specifier = match spec.specifier {
                Some(Specifier::Def) => "def",
                Some(Specifier::Class) => "class",
                _ => "over",
            };
            out.insert("specifier".into(), json!(specifier));
            if let Some(name) = spec.type_name {
                out.insert("typeName".into(), json!(store.tokens.resolve(name)));
            }
            if let Some(active) = spec.active {
                out.insert("active".into(), json!(active));
            }
            if let Some(instanceable) = spec.instanceable {
                out.insert("instanceable".into(), json!(instanceable));
            }
            if let Some(order) = &spec.prim_order {
                out.insert("primOrder".into(), names(order));
            }
            (
                &spec.fields,
                &spec.references,
                &spec.payloads,
                &spec.inherits,
                &spec.specializes,
                &spec.property_order,
            )
        }
        Spec::Variant(spec) => {
            // OpenUSD's variant prim specs are `over`s.
            out.insert("specifier".into(), json!("over"));
            (
                &spec.fields,
                &spec.references,
                &spec.payloads,
                &spec.inherits,
                &spec.specializes,
                &spec.property_order,
            )
        }
    };
    for entry in field {
        let name = store.tokens.resolve(entry.name);
        match (name, &entry.value) {
            (
                "active" | "instanceable",
                layerstack::FieldValue::Value(layerstack::Value::Bool(b)),
            ) => {
                out.insert(name.into(), json!(b));
            }
            ("apiSchemas", layerstack::FieldValue::TokenListOp(list)) => {
                out.insert(
                    name.into(),
                    list_op(list, |t| json!(store.tokens.resolve(*t))),
                );
            }
            _ => {}
        }
    }
    if let Some(order) = property_order {
        out.insert("propertyOrder".into(), names(order));
    }
    for (key, list) in [("references", references), ("payload", payloads)] {
        if *list != ListOp::default() {
            out.insert(key.into(), list_op(list, reference));
        }
    }
    for (key, list) in [("inheritPaths", inherits), ("specializes", specializes)] {
        if *list != ListOp::default() {
            out.insert(key.into(), list_op(list, path));
        }
    }
    Json::Object(out)
}

#[derive(Clone, Copy)]
enum Spec<'a> {
    Prim(&'a PrimSpec),
    Variant(&'a VariantSpec),
}

fn spec_at<'a>(loaded: &'a LoadedStage, layer: LayerId, path: &SpecPath) -> Option<Spec<'a>> {
    let layer = &loaded.store.layers[&layer];
    if let Some(variant) = layer.variant_spec_at(path, &loaded.store.paths) {
        return Some(Spec::Variant(variant));
    }
    let sites: Vec<_> = path.variant_selection_sites().collect();
    layer.prim_spec_in(path.prim_path(), &sites).map(Spec::Prim)
}

/// Every composed prim (inactive ones too), with its prim stack, whether
/// it is active and whether it is an instance.
fn composed(stage: &Stage, store: &InMemoryStore) -> Vec<(String, Vec<String>, bool, bool)> {
    let root = store.paths.lookup(&Path::root()).unwrap();
    stage
        .traverse_all(root)
        .filter_map(|prim| stage.prim(prim, store))
        .map(|prim| {
            let stack = prim.prim_stack();
            let stack = stack
                .iter()
                .map(|(layer, path)| {
                    format!("{}:{}", layer.0, path.display(&store.paths, &store.tokens))
                })
                .collect();
            (
                prim.to_string(),
                stack,
                prim.is_active(),
                prim.is_instance(),
            )
        })
        .collect()
}

/// The live stage composes what a fresh composition of its layers does.
fn assert_fresh(live: &LiveStage, store: &mut InMemoryStore, root: LayerId, context: &str) {
    let fresh = Stage::compose(store, root, StageOptions::default());
    assert_eq!(
        composed(live.stage(), store),
        composed(&fresh, store),
        "{context}"
    );
}

#[test]
fn prim_field_edits_agree_with_openusd_call_by_call() {
    let oracle: Json = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load();
        let original = loaded.store.layers.clone();
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let target = edit_target(&mut loaded, &live, &case["target"]);
        let prim = loaded.store.path(case["prim"].as_str().unwrap());
        let spec_path = target.map_to_spec_path(prim, &mut loaded.store.paths);
        let authored = |loaded: &LoadedStage| {
            let path = spec_path.as_ref()?;
            spec_at(loaded, target.layer(), path).map(|spec| fields(loaded, spec))
        };
        let before = authored(&loaded);
        let mut accepted = Vec::new();
        let mut inverses = Vec::new();
        for step in case["steps"].as_array().unwrap() {
            let txn = transaction(&mut loaded, &target, prim, step);
            match live.apply(&mut loaded.store, &txn) {
                Ok(applied) => {
                    accepted.push(true);
                    inverses.push(applied.inverse);
                }
                Err(_) => accepted.push(false),
            }
            let context = format!("{name}: step {} recomposes", accepted.len());
            assert_fresh(&live, &mut loaded.store, loaded.root_layer, &context);
        }
        let deliberate = DELIBERATE.iter().find(|(n, _)| *n == name);
        let expected: Vec<bool> = match deliberate {
            Some((_, accepted)) => accepted.to_vec(),
            None => serde_json::from_value(case["accepted"].clone()).unwrap(),
        };
        assert_eq!(accepted, expected, "{name}: accepted calls");

        let got = authored(&loaded);
        let want = match deliberate {
            Some(_) => before,
            None => Some(case["spec"].clone()).filter(|spec| !spec.is_null()),
        };
        // The pseudo-root holds none of the recorded fields.
        let got = got.filter(|_| name != "pseudo_root");
        let want = want.filter(|spec| spec.as_object().is_some_and(|o| !o.is_empty()));
        assert_eq!(got, want, "{name}: authored spec");

        for inverse in inverses.iter().rev() {
            live.apply(&mut loaded.store, inverse)
                .unwrap_or_else(|e| panic!("{name}: undo: {e}"));
        }
        assert_fresh(
            &live,
            &mut loaded.store,
            loaded.root_layer,
            &format!("{name}: undo recomposes"),
        );
        for (id, layer) in &original {
            let now = &loaded.store.layers[id];
            assert_eq!(
                now.prims(),
                layer.prims(),
                "{name}: undo restores layer {id:?}"
            );
            assert_eq!(
                now.variant_prims(),
                layer.variant_prims(),
                "{name}: undo restores layer {id:?}"
            );
        }
    }
}
