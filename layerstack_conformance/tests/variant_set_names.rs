// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `variantSetNames` agrees with OpenUSD 26.08: as it composes, flattens,
//! reads and writes the list op, and as its authoring calls edit it, call
//! by call.
//!
//! `scripts/variant_set_names_oracle.py` writes the layers of
//! `fixtures/variant_set_names` and records what OpenUSD does with them
//! (see the script for every case):
//!
//! - composition: a variant set composes only where a site lists it, the
//!   list ops of a site's layers applied weakest first; a set spec nothing
//!   lists contributes nothing, a listed set without a spec contributes
//!   no opinions, each node composes its own site (a stronger node's
//!   `delete` leaves a referenced node's listing alone), and a variant
//!   node composes its branch's list ops for the sets nested in it;
//! - layer stack flattening combines the list ops as
//!   `SdfListOp::ApplyOperations` does;
//! - every USDA list op form reads as OpenUSD reads it, from USDA and
//!   from OpenUSD's USDC, and saves in both formats as layerstack reads
//!   it back; with a Python whose `pxr` is OpenUSD (`LAYERSTACK_USD_PYTHON`,
//!   or `python3`), OpenUSD must read the saved files the same way too;
//! - `Transaction::add_variant_set`, `set_variant_set_names`,
//!   `clear_variant_set_names`, `remove_variant_set` and `create_prim` of a
//!   variant path accept or reject each call as
//!   `UsdVariantSets::AddVariantSet`, `UsdPrim::SetMetadata` /
//!   `ClearMetadata`, `SdfPrimSpec::RemoveVariantSet` and
//!   `SdfCreatePrimInLayer` do, and leave the same list op and variant set
//!   specs after each call. Each call is one transaction applied through a
//!   `LiveStage`, which must compose as a fresh stage does; undoing every
//!   call restores the layers.
//!
//! Spec: AOUSD Core §7.3.6 (variant set specs), §7.6.2.3.5
//! (`variantSetNames`), §10.5 (variant selection), §12.4 (list ops).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};
use std::process::Command;

use layerstack::{
    Address, ArcKind, EditTarget, InMemoryStore, Layer, LayerId, ListOp, ListPosition, LiveStage,
    Path, PrimSpec, SpecPath, Specifier, Stage, StageOptions, TokenId, TokenInterner, Transaction,
    Value, VariantSetSpec, VariantSpec, flatten_layer_stack,
};
use layerstack_conformance::{
    save_corpus::Imported,
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../fixtures/variant_set_names/oracle.json");

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn fixture(name: &str) -> PathBuf {
    workspace_root()
        .join("layerstack_conformance/fixtures/variant_set_names")
        .join(name)
}

fn load(name: &str) -> LoadedStage {
    let loaded = load_entry_usda(&fixture(name));
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

/// A list op as the oracle records it.
fn list_op(op: &ListOp<TokenId>, tokens: &TokenInterner) -> Json {
    let names = |list: &[TokenId]| {
        list.iter()
            .map(|name| tokens.resolve(*name).to_string())
            .collect::<Vec<_>>()
    };
    json!({
        "explicit": op.explicit.as_deref().map(names),
        "prepend": names(&op.prepend),
        "append": names(&op.append),
        "delete": names(&op.delete),
        "add": names(&op.add),
        "reorder": names(&op.reorder),
    })
}

/// A prim or variant spec's `variantSetNames` (`null` when it authors
/// none) and variant sets, as the oracle records them.
fn spec_record(
    names: &ListOp<TokenId>,
    sets: &layerstack::HashMap<TokenId, VariantSetSpec>,
    tokens: &TokenInterner,
) -> Json {
    let sets: BTreeMap<String, Vec<String>> = sets
        .iter()
        .map(|(name, set)| {
            let mut variants: Vec<String> = set
                .variants
                .keys()
                .map(|v| tokens.resolve(*v).to_string())
                .collect();
            variants.sort();
            (tokens.resolve(*name).to_string(), variants)
        })
        .collect();
    json!({
        "names": (!names.is_empty()).then(|| list_op(names, tokens)),
        "sets": sets,
    })
}

#[derive(Clone, Copy)]
enum Spec<'a> {
    Prim(&'a PrimSpec),
    Variant(&'a VariantSpec),
}

impl Spec<'_> {
    fn record(self, tokens: &TokenInterner) -> Json {
        match self {
            Self::Prim(spec) => spec_record(&spec.variant_set_names, &spec.variant_sets, tokens),
            Self::Variant(spec) => spec_record(&spec.variant_set_names, &spec.variant_sets, tokens),
        }
    }
}

fn spec_at<'a>(
    layer: &'a Layer,
    path: &SpecPath,
    paths: &layerstack::PathInterner,
) -> Option<Spec<'a>> {
    if let Some(variant) = layer.variant_spec_at(path, paths) {
        return Some(Spec::Variant(variant));
    }
    if !path.variant_chain().is_empty() {
        return None;
    }
    let sites: Vec<_> = path.variant_selection_sites().collect();
    layer.prim_spec_in(path.prim_path(), &sites).map(Spec::Prim)
}

/// Whether `layer` holds a spec at `path`: a prim, variant or property
/// spec (`/P{v=x}C.attr`).
fn exists(loaded: &LoadedStage, layer: LayerId, path: &str) -> bool {
    let store = &loaded.store;
    let layer = &store.layers[&layer];
    let (owner, property) = match path.rsplit_once('.') {
        Some((owner, property)) => (owner, Some(property)),
        None => (path, None),
    };
    let owner = SpecPath::parse(owner, &store.tokens, &store.paths).expect("probe path");
    let Some(spec) = spec_at(layer, &owner, &store.paths) else {
        return false;
    };
    let Some(property) = property else {
        return true;
    };
    let properties = match spec {
        Spec::Prim(spec) => &spec.properties,
        Spec::Variant(spec) => &spec.properties,
    };
    properties
        .iter()
        .any(|entry| store.tokens.resolve(entry.name) == property)
}

fn value(value: &Value) -> Option<Json> {
    match value {
        Value::Bool(b) => Some(json!(b)),
        Value::Double(d) => Some(json!(d)),
        Value::Float(f) => Some(json!(f64::from(*f))),
        Value::Int(i) => Some(json!(f64::from(*i))),
        _ => None,
    }
}

/// Each prim composes the variant sets, selections and values OpenUSD
/// composes.
#[test]
fn variant_sets_compose_as_openusd_composes_them() {
    let oracle = oracle();
    let mut loaded = load("root.usda");
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    let records = oracle["compose"].as_object().unwrap();
    assert!(records.len() >= 12);
    let mut mismatches = Vec::new();
    for (path, want) in records {
        let prim = stage.prim(loaded.store.path(path), &loaded.store);
        let sets = prim.map(|prim| prim.variant_sets()).unwrap_or_default();
        let names: Vec<&str> = sets.iter().map(|(set, _)| *set).collect();
        let selections: BTreeMap<&str, &str> = sets
            .iter()
            .map(|(set, variant)| (*set, variant.unwrap_or("")))
            .collect();
        let mut values = BTreeMap::new();
        for property in prim.map(|prim| prim.properties()).unwrap_or_default() {
            if let Some(resolved) = property
                .as_attribute()
                .and_then(|attribute| attribute.authored_value(layerstack::Time::Default))
                && let Some(value) = value(&resolved.value)
            {
                values.insert(property.name().to_string(), value);
            }
        }
        let got = json!({"names": names, "selections": selections, "values": values});
        if got != *want {
            mismatches.push(format!("{path}\n    OpenUSD {want}\n    got     {got}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Flattening the layer stack combines the list ops as OpenUSD does.
#[test]
fn flattening_combines_the_list_ops() {
    let oracle = oracle();
    let loaded = load("flatten.usda");
    let flat = flatten_layer_stack(
        &loaded.store,
        loaded.root_layer.into(),
        LayerId(1000),
        &mut |_, asset| Some(asset.to_string()),
    )
    .expect("flattens");
    let store = loaded.store;
    for (path, want) in oracle["flatten"].as_object().unwrap() {
        let prim = store.path(path);
        let spec = flat.layer.prim(prim).expect("flattened prim spec");
        let got = Spec::Prim(spec).record(&store.tokens);
        assert_eq!(got, *want, "{path}");
    }
}

/// The `forms` records of `layer`.
fn forms(oracle: &Json, imported: &mut Imported) -> Json {
    let mut out = serde_json::Map::new();
    for path in oracle["forms"].as_object().unwrap().keys() {
        let spec_path = SpecPath::parse(path, &imported.tokens, &imported.paths).expect("path");
        let record = spec_at(&imported.layer, &spec_path, &imported.paths)
            .map(|spec| spec.record(&imported.tokens));
        out.insert(path.clone(), record.unwrap_or(Json::Null));
    }
    Json::Object(out)
}

/// The records with the variant sets that hold no variants left out:
/// the writers write none (USDA has no spelling for one, and OpenUSD's
/// writer leaves them out too), so a saved layer holds them only where its
/// text form recreates them.
fn without_empty_sets(records: &Json) -> Json {
    let mut records = records.clone();
    for record in records.as_object_mut().unwrap().values_mut() {
        if let Some(sets) = record.get_mut("sets").and_then(Json::as_object_mut) {
            sets.retain(|_, variants| variants.as_array().is_some_and(|v| !v.is_empty()));
        }
    }
    records
}

/// The Python with OpenUSD's `pxr`, if any.
fn usd_python() -> Option<String> {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    Command::new(&python)
        .args(["-c", "import pxr.Usd"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|_| python)
}

/// The `forms` records of the file at `path` as OpenUSD reads it.
fn openusd_forms(python: &str, path: &FsPath) -> Json {
    let script =
        workspace_root().join("layerstack_conformance/scripts/variant_set_names_oracle.py");
    let out = Command::new(python)
        .arg(script)
        .arg("--read")
        .arg(path)
        .output()
        .expect("runs the oracle");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON")
}

/// Every list op form reads as OpenUSD reads it, from USDA and from
/// OpenUSD's USDC, and saves in both formats as it was read.
#[test]
fn list_op_forms_read_and_save_as_authored() {
    let oracle = oracle();
    let text = std::fs::read_to_string(fixture("forms.usda")).unwrap();
    let mut from_usda = Imported::usda(&text);
    assert_eq!(
        forms(&oracle, &mut from_usda),
        oracle["forms"],
        "forms.usda"
    );
    let bytes = std::fs::read(fixture("forms.usdc")).unwrap();
    let mut from_usdc = Imported::usdc(&bytes);
    assert_eq!(
        forms(&oracle, &mut from_usdc),
        oracle["forms_usdc"],
        "OpenUSD's forms.usdc"
    );

    let saved_usda = from_usda.save_usda().expect("saves USDA");
    let saved_usdc = from_usda.save_usdc().expect("saves USDC");
    let mut again = Imported::usda(&saved_usda);
    assert_eq!(
        forms(&oracle, &mut again),
        oracle["forms"],
        "saved USDA, read back\n{saved_usda}"
    );
    let mut again = Imported::usdc(&saved_usdc);
    assert_eq!(
        without_empty_sets(&forms(&oracle, &mut again)),
        without_empty_sets(&oracle["forms"]),
        "saved USDC, read back"
    );

    let Some(python) = usd_python() else {
        eprintln!("skipped OpenUSD reading the saved files: no Python with pxr");
        return;
    };
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("variant-set-names");
    std::fs::create_dir_all(&dir).unwrap();
    let (usda, usdc) = (dir.join("saved.usda"), dir.join("saved.usdc"));
    std::fs::write(&usda, &saved_usda).unwrap();
    std::fs::write(&usdc, &saved_usdc).unwrap();
    assert_eq!(
        openusd_forms(&python, &usda),
        oracle["forms"],
        "OpenUSD reads the saved USDA\n{saved_usda}"
    );
    assert_eq!(
        without_empty_sets(&openusd_forms(&python, &usdc)),
        without_empty_sets(&oracle["forms"]),
        "OpenUSD reads the saved USDC"
    );
}

/// USDA has no spelling for an empty explicit `variantSets`, so saving one
/// as USDA is refused; USDC holds it.
#[test]
fn an_empty_explicit_list_saves_only_as_usdc() {
    let mut imported = Imported::usda("#usda 1.0\ndef \"Rock\"\n{\n}\n");
    imported.prim("/Rock").variant_set_names = ListOp::explicit(Vec::new());
    let error = imported.save_usda().expect_err("USDA refuses it");
    assert!(
        error.to_string().contains("empty explicit variantSets"),
        "{error}"
    );
    let bytes = imported.save_usdc().expect("USDC holds it");
    let mut again = Imported::usdc(&bytes);
    assert_eq!(
        again.prim("/Rock").variant_set_names,
        ListOp::explicit(Vec::new())
    );
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
    match step["position"].as_str().unwrap_or("back_of_prepend") {
        "front_of_prepend" => ListPosition::FrontOfPrepend,
        "back_of_prepend" => ListPosition::BackOfPrepend,
        "front_of_append" => ListPosition::FrontOfAppend,
        "back_of_append" => ListPosition::BackOfAppend,
        other => panic!("no position {other}"),
    }
}

/// The list op a `set_variant_set_names` step authors.
fn names_op(store: &InMemoryStore, value: &Json) -> ListOp<TokenId> {
    let names = |key: &str| -> Option<Vec<TokenId>> {
        value.get(key).map(|items| {
            items
                .as_array()
                .unwrap()
                .iter()
                .map(|name| store.tokens.intern(name.as_str().unwrap()))
                .collect()
        })
    };
    ListOp {
        explicit: names("explicit"),
        prepend: names("prepend").unwrap_or_default(),
        append: names("append").unwrap_or_default(),
        delete: names("delete").unwrap_or_default(),
        add: names("add").unwrap_or_default(),
        reorder: names("reorder").unwrap_or_default(),
    }
}

/// The transaction making the step's call at `prim` through `target`.
fn transaction(
    loaded: &mut LoadedStage,
    target: &EditTarget,
    prim: layerstack::PathId,
    step: &Json,
) -> Transaction {
    let at = target.prim(prim);
    let mut txn = Transaction::new();
    match step["call"].as_str().unwrap() {
        "add_variant_set" => {
            let name = loaded.store.tokens.intern(step["name"].as_str().unwrap());
            txn.add_variant_set(at, name, position(step))
        }
        "remove_variant_set" => {
            let name = loaded.store.tokens.intern(step["name"].as_str().unwrap());
            txn.remove_variant_set(at, name)
        }
        "set_variant_set_names" if step["value"].is_null() => txn.clear_variant_set_names(at),
        "set_variant_set_names" => {
            let names = names_op(&loaded.store, &step["value"]);
            txn.set_variant_set_names(at, names)
        }
        "create_prim_in_layer" => {
            let path = SpecPath::parse(
                step["path"].as_str().unwrap(),
                &loaded.store.tokens,
                &loaded.store.paths,
            )
            .expect("spec path");
            txn.create_prim(Address::spec(target.layer(), path), Specifier::Over, None)
        }
        other => panic!("no call {other}"),
    };
    txn
}

/// Every composed prim with its prim stack, to compare a live stage with a
/// fresh composition.
fn composed(stage: &Stage, store: &InMemoryStore) -> Vec<(String, Vec<String>)> {
    let root = store.paths.lookup(&Path::root()).unwrap();
    stage
        .traverse_all(root)
        .map(|prim| {
            let stack = stage
                .prim(prim, store)
                .map(|prim| prim.prim_stack())
                .unwrap_or_default();
            let stack = stack
                .iter()
                .map(|(layer, path)| {
                    format!("{}:{}", layer.0, path.display(&store.paths, &store.tokens))
                })
                .collect();
            (store.paths.display(prim, &store.tokens), stack)
        })
        .collect()
}

#[test]
fn variant_set_edits_agree_with_openusd_call_by_call() {
    let oracle = oracle();
    let cases = oracle["edits"].as_array().unwrap();
    assert!(cases.len() >= 18);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load("edits.usda");
        let original = loaded.store.layers.clone();
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let target = edit_target(&mut loaded, &live, &case["target"]);
        let prim = loaded.store.path(case["prim"].as_str().unwrap());
        let spec_path = target.map_to_spec_path(prim, &mut loaded.store.paths);
        let pseudo_root = case["prim"] == "/";
        let addressed = |loaded: &LoadedStage| {
            let path = spec_path.as_ref().filter(|_| !pseudo_root)?;
            let layer = &loaded.store.layers[&target.layer()];
            spec_at(layer, path, &loaded.store.paths).map(|spec| spec.record(&loaded.store.tokens))
        };
        let steps = case["steps"].as_array().unwrap();
        let mut accepted = Vec::new();
        let mut inverses = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            let txn = transaction(&mut loaded, &target, prim, step);
            match live.apply(&mut loaded.store, &txn) {
                Ok(applied) => {
                    accepted.push(true);
                    inverses.push(applied.inverse);
                }
                Err(_) => accepted.push(false),
            }
            let fresh = Stage::compose(
                &mut loaded.store,
                loaded.root_layer,
                StageOptions::default(),
            );
            assert_eq!(
                composed(live.stage(), &loaded.store),
                composed(&fresh, &loaded.store),
                "{name}: step {index} recomposes"
            );
            let got = addressed(&loaded).unwrap_or(Json::Null);
            assert_eq!(got, case["after"][index], "{name}: spec after step {index}");
        }
        let want: Vec<bool> = serde_json::from_value(case["accepted"].clone()).unwrap();
        assert_eq!(accepted, want, "{name}: accepted calls");
        for (path, want) in case["probes"].as_object().unwrap() {
            assert_eq!(
                exists(&loaded, target.layer(), path),
                want.as_bool().unwrap(),
                "{name}: {path}"
            );
        }

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
