// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Namespace edits leave the child and property orders a prim spec authors
//! as OpenUSD 26.11 leaves them.
//!
//! `scripts/namespace_orders_oracle.py` opens
//! `fixtures/namespace_orders/scene.usda` once per case and makes one
//! call: a `UsdNamespaceEditor` rename, reparent or delete, an
//! `SdfLayer::Apply` on the referenced `asset.usda`, a spec removal or an
//! `SdfCopySpec`. It records whether the call applies, every prim and
//! variant prim spec of each layer with its children, properties,
//! `primOrder` and `propertyOrder`, and every composed prim's children and
//! property names. `variant.usda` holds the edits through a variant, which
//! OpenUSD and layerstack refuse. Here each case makes the layerstack call
//! that documents itself as that one, applied through a `LiveStage`:
//!
//! - `editor`: `NamespaceEdit::prepare` (a move) or `prepare_delete`
//!   (`UsdNamespaceEditor`), through the root layer or `/V`'s variant;
//! - `layer_apply`: `NamespaceEdit::prepare` through `/R`'s reference node,
//!   which edits `asset.usda` as the one `SdfBatchNamespaceEdit` the
//!   oracle applies there;
//! - `remove_spec`: `Transaction::remove_spec`
//!   (`SdfPrimSpec::RemoveNameChild`, `RemoveProperty`);
//! - `copy_spec`: `Transaction::copy_spec` (`SdfCopySpec`).
//!
//! The layers and composition must match the oracle's, the live stage
//! must match a fresh composition, and the inverse must restore every
//! layer. The oracle's `set_name` records (`SdfPrimSpec::SetName`, which
//! alone rewrites its parent's `primOrder`) have no layerstack counterpart
//! and are not replayed.
//!
//! Spec: AOUSD Core §7.6.2 (`primOrder`, `propertyOrder`), §8 (namespace).

use layerstack::{
    Address, ArcKind, EditTarget, InMemoryStore, Layer, LayerId, LiveStage, NamespaceEdit, Path,
    PathId, PrimSpec, PropertyEntry, PropertyPath, SpecPath, Stage, StageOptions, TargetPath,
    TokenId, Transaction, VariantSets, spec_path::VariantSelectionSite,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Map, Value as Json, json};

const ORACLE: &str = include_str!("../../fixtures/namespace_orders/oracle.json");

/// The stage paths of the move the oracle's `layer_apply` case makes in
/// `asset.usda`: `/R` references `/Asset`.
const THROUGH_REFERENCE: (&str, &str) = ("/R/J", "/R/J2");

fn load(scene: &str) -> LoadedStage {
    let path = workspace_root()
        .join("layerstack_conformance/fixtures/namespace_orders")
        .join(scene);
    let loaded = load_entry_usda(&path);
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

fn names(store: &InMemoryStore, names: &[TokenId]) -> Json {
    json!(
        names
            .iter()
            .map(|n| store.tokens().resolve(*n))
            .collect::<Vec<_>>()
    )
}

fn order(store: &InMemoryStore, order: Option<&[TokenId]>) -> Json {
    order.map_or(Json::Null, |order| names(store, order))
}

fn spec_record(
    store: &InMemoryStore,
    children: &[TokenId],
    prim_order: Option<&[TokenId]>,
    properties: &[PropertyEntry],
    property_order: Option<&[TokenId]>,
) -> Json {
    let properties: Vec<_> = properties.iter().map(|p| p.name).collect();
    json!({
        "children": names(store, children),
        "primOrder": order(store, prim_order),
        "properties": names(store, &properties),
        "propertyOrder": order(store, property_order),
    })
}

/// Every prim spec and variant prim spec of `layer`, the pseudo-root's
/// included, as the oracle's `layer_record` lists them.
fn layer_record(store: &InMemoryStore, layer: &Layer) -> Json {
    let mut out = Map::new();
    let root = store.paths().lookup(&Path::root()).unwrap();
    out.insert(
        "/".into(),
        json!({"children": [], "primOrder": null, "properties": [], "propertyOrder": null}),
    );
    let mut add = |path: PathId, spec: &PrimSpec| {
        let sites = spec.outer_variant_sites();
        let at = SpecPath::from_variant_selection_sites(path, sites, store.paths());
        let text = if path == root {
            "/".to_string()
        } else {
            at.display(store.paths(), store.tokens())
        };
        out.insert(
            text,
            spec_record(
                store,
                &spec.authored_children,
                spec.prim_order(),
                &spec.properties,
                spec.property_order(),
            ),
        );
        fn variants(
            store: &InMemoryStore,
            path: PathId,
            outer: &[VariantSelectionSite],
            sets: &VariantSets,
            out: &mut Map<String, Json>,
        ) {
            for (set, spec) in sets {
                for (variant, branch) in &spec.variants {
                    let mut sites = outer.to_vec();
                    sites.push(VariantSelectionSite {
                        host_path: path,
                        set: *set,
                        variant: *variant,
                    });
                    let at = SpecPath::from_variant_selection_sites(path, &sites, store.paths());
                    out.insert(
                        at.display(store.paths(), store.tokens()),
                        spec_record(
                            store,
                            &branch.authored_children,
                            branch.prim_order.as_deref(),
                            &branch.properties,
                            branch.property_order.as_deref(),
                        ),
                    );
                    variants(store, path, &sites, &branch.variant_sets, out);
                }
            }
        }
        variants(store, path, sites, spec.variant_sets(), &mut out);
    };
    for (path, spec) in layer.prims() {
        add(*path, spec);
    }
    for (path, branches) in layer.variant_prims().iter() {
        for spec in branches {
            add(*path, spec);
        }
    }
    Json::Object(out)
}

fn composed_record(store: &InMemoryStore, stage: &Stage) -> Json {
    let root = store.paths().lookup(&Path::root()).unwrap();
    let mut out = Map::new();
    for path in core::iter::once(root).chain(stage.traverse_all(root).filter(|p| *p != root)) {
        let prim = stage.prim(path, store).unwrap();
        out.insert(
            store.paths().display(path, store.tokens()),
            json!({
                "children": prim.all_children().map(|c| c.name().to_string()).collect::<Vec<_>>(),
                "properties": prim.property_names(),
            }),
        );
    }
    Json::Object(out)
}

fn record(loaded: &LoadedStage, stage: &Stage) -> Json {
    let mut layers = Map::new();
    for name in loaded.layer_names.values() {
        let name = name.rsplit('/').next().unwrap();
        let layer = loaded.store.layer(layer_named(loaded, name)).unwrap();
        layers.insert(name.into(), layer_record(&loaded.store, layer));
    }
    json!({"layers": layers, "composed": composed_record(&loaded.store, stage)})
}

fn expected(case: &Json) -> Json {
    json!({"layers": case["layers"], "composed": case["composed"]})
}

/// The places where `got` and `want` differ, as `/`-joined keys.
fn differences(got: &Json, want: &Json, at: &str, out: &mut Vec<String>) {
    match (got, want) {
        (Json::Object(g), Json::Object(w)) => {
            for key in g.keys().chain(w.keys().filter(|k| !g.contains_key(*k))) {
                let (a, b) = (&g.get(key), &w.get(key));
                match (a, b) {
                    (Some(a), Some(b)) => differences(a, b, &format!("{at} {key}"), out),
                    _ => out.push(format!("{at} {key}: {a:?} != {b:?}")),
                }
            }
        }
        _ if got != want => out.push(format!("{at}: {got} != {want}")),
        _ => {}
    }
}

fn assert_same(got: &Json, want: &Json, what: &str) {
    let mut out = Vec::new();
    differences(got, want, "", &mut out);
    assert!(out.is_empty(), "{what} (got != want):\n{}", out.join("\n"));
}

fn target_path(store: &mut InMemoryStore, text: &str) -> TargetPath {
    match text.split_once('.') {
        Some((prim, property)) => {
            let prim = store.path(prim);
            let property = store.tokens().intern(property);
            TargetPath::Property(PropertyPath::new(prim, property))
        }
        None => TargetPath::Prim(store.path(text)),
    }
}

fn spec_address(store: &mut InMemoryStore, layer: LayerId, text: &str) -> Address {
    let path = SpecPath::parse(text, store.tokens(), store.paths()).unwrap();
    Address::spec(layer, path)
}

/// `/R`'s reference node, as the oracle's edit through it would target.
fn reference_target(store: &mut InMemoryStore, stage: &Stage) -> EditTarget {
    let prim = store.path("/R");
    let graph = stage
        .prim(prim, &*store)
        .map(|p| p.composition_graph())
        .unwrap();
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| {
            graph
                .node(*id)
                .is_some_and(|n| n.arc_kind() == ArcKind::References)
        })
        .unwrap();
    EditTarget::for_node(stage, prim, node).unwrap()
}

/// The layerstack call a case makes, as a transaction to apply; `None`
/// when it refuses the edit.
fn prepare(loaded: &mut LoadedStage, live: &LiveStage, case: &Json) -> Option<Transaction> {
    let call = case["call"].as_str().unwrap();
    let target = case["target"].as_str().unwrap();
    let path = case["path"].as_str().unwrap();
    let destination = case["destination"].as_str();
    let store = &mut loaded.store;
    let namespace = |store: &mut InMemoryStore, target: &EditTarget, from: &str, to| {
        let source = target_path(store, from);
        let edit = match to {
            Some(to) => {
                let destination = target_path(store, to);
                NamespaceEdit::prepare(live.stage(), store, target, source, destination)
            }
            None => NamespaceEdit::prepare_delete(live.stage(), store, target, source),
        };
        edit.ok().map(NamespaceEdit::into_transaction)
    };
    match call {
        "editor" => {
            let target = match target {
                "root" => EditTarget::for_layer(loaded.root_layer),
                "variant" => {
                    let variant = SpecPath::parse("/V{v=x}", store.tokens(), store.paths());
                    EditTarget::for_local_variant(loaded.root_layer, &variant.unwrap())
                }
                other => panic!("no target {other}"),
            };
            namespace(store, &target, path, destination)
        }
        "layer_apply" => {
            assert_eq!(target, "asset.usda", "a layer apply edits the asset");
            let target = reference_target(store, live.stage());
            let (from, to) = THROUGH_REFERENCE;
            namespace(store, &target, from, Some(to))
        }
        "remove_spec" | "copy_spec" => {
            let layer = layer_named(loaded, target);
            let store = &mut loaded.store;
            let mut transaction = Transaction::new();
            let from = spec_address(store, layer, path);
            match destination {
                Some(to) => {
                    let to = spec_address(store, layer, to);
                    transaction.copy_spec(from, to)
                }
                None => transaction.remove_spec(from),
            };
            Some(transaction)
        }
        other => panic!("no call {other}"),
    }
}

#[test]
fn namespace_edits_keep_orders_as_openusd_does() {
    let oracle: Json = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 15);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let scene = case["scene"].as_str().unwrap();
        let mut loaded = load(scene);
        let original: Vec<_> = loaded.store.layers().cloned().collect();
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let before = record(&loaded, live.stage());
        assert_eq!(before, expected(&oracle["before"][scene]), "{name}: before");
        let prepared = prepare(&mut loaded, &live, case);
        let applied = case["applied"].as_bool().unwrap();
        assert_eq!(prepared.is_some(), applied, "{name}: applied");
        let inverse = prepared.map(|transaction| {
            live.apply(&mut loaded.store, &transaction)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"))
                .inverse
        });
        let want = if applied {
            expected(case)
        } else {
            before.clone()
        };
        assert_same(
            &record(&loaded, live.stage()),
            &want,
            &format!("{name}: result"),
        );
        let fresh = Stage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        assert_eq!(
            record(&loaded, live.stage()),
            record(&loaded, &fresh),
            "{name}: the live stage recomposes"
        );
        if let Some(inverse) = inverse {
            live.apply(&mut loaded.store, &inverse).unwrap();
            for layer in &original {
                let now = loaded.store.layer(layer.id()).unwrap();
                assert_eq!(now.prims(), layer.prims(), "{name}: undo restores");
            }
            assert_eq!(record(&loaded, live.stage()), before, "{name}: undo");
        }
    }
}
