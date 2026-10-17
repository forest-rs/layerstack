// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim and property deletes agree with OpenUSD 26.08's
//! `UsdNamespaceEditor`.
//!
//! `scripts/namespace_deletes_oracle.py` opens
//! `fixtures/namespace_deletes/scene.usda` once per case and deletes one
//! object, recording whether the editor applies the delete, the specs each
//! root-stack layer then holds, the root layer's relocates and
//! `defaultPrim`, the paths `/Watch` authors, and every composed prim in
//! order. Here `NamespaceEdit::prepare_delete` must accept or reject the
//! same deletes and, applied through a `LiveStage`, leave the same result;
//! the live stage must then match a fresh composition, and the inverse
//! must restore every layer.
//!
//! OpenUSD 26.08 fails a verification in `Pcp_ForEachDependentNode` when
//! deleting `/A` here (`/Watch` references `/A/Child` while `/A` has a
//! variant set). Its expectation is what OpenUSD does with the same delete
//! in a scene without the variant set: every spec of `/A` goes,
//! `defaultPrim` is cleared, and the paths into `/A` are dropped
//! (`DELIBERATE`).
//!
//! Spec: AOUSD Core §8 (namespace), §10.3.2.6 (relocates), §12.4.

#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    EditTarget, Layer, LayerId, ListOp, LiveStage, NamespaceEdit, Path, PathId, PrimSpec,
    PropertyPath, Reference, ReferenceTarget, Stage, StageOptions, TargetPath, VariantSetSpec,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use serde_json::{Value as Json, json};

const ORACLE: &str = include_str!("../fixtures/namespace_deletes/oracle.json");

fn load() -> LoadedStage {
    let path =
        workspace_root().join("layerstack_conformance/fixtures/namespace_deletes/scene.usda");
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

/// The spec paths a layer holds, as `SdfLayer::Traverse` lists prim,
/// variant set and variant paths.
fn layer_specs(loaded: &LoadedStage, layer: &Layer) -> Vec<String> {
    let store = &loaded.store;
    let mut out = Vec::new();
    let mut add = |path: PathId, spec: &PrimSpec| {
        let names: Vec<String> = store
            .paths
            .resolve(path)
            .segments()
            .iter()
            .map(|t| store.tokens.resolve(*t).to_string())
            .collect();
        // The branch selections enclosing this spec, after the name of the
        // prim hosting each.
        let mut text = String::new();
        for (depth, name) in names.iter().enumerate() {
            if depth > 0 && !text.ends_with('}') || depth == 0 {
                text.push('/');
            }
            text.push_str(name);
            let host = Path::root().join(&store.paths.resolve(path).segments()[..=depth]);
            for site in &spec.outer_variant_sites {
                if store.paths.resolve(site.host_path) == &host {
                    text.push_str(&format!(
                        "{{{}={}}}",
                        store.tokens.resolve(site.set),
                        store.tokens.resolve(site.variant)
                    ));
                }
            }
        }
        if !names.is_empty() {
            out.push(text.clone());
        }
        fn variant_paths(
            store: &layerstack::InMemoryStore,
            prefix: &str,
            sets: &layerstack::HashMap<layerstack::TokenId, VariantSetSpec>,
            out: &mut Vec<String>,
        ) {
            for (set, spec) in sets {
                let set = store.tokens.resolve(*set);
                out.push(format!("{prefix}{{{set}=}}"));
                for (variant, branch) in &spec.variants {
                    let path = format!("{prefix}{{{set}={}}}", store.tokens.resolve(*variant));
                    out.push(path.clone());
                    variant_paths(store, &path, &branch.variant_sets, out);
                }
            }
        }
        variant_paths(store, &text, &spec.variant_sets, &mut out);
    };
    for (path, spec) in layer.prims() {
        add(*path, spec);
    }
    for (path, branches) in layer.variant_prims() {
        for spec in branches {
            add(*path, spec);
        }
    }
    out.sort();
    out
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

/// Drops list ops that author nothing: OpenUSD leaves an emptied list op
/// authored, which the layer model does not distinguish from none.
fn normalized(mut watch: Json) -> Json {
    if let Json::Object(fields) = &mut watch {
        fields.retain(|_, value| {
            value.get("explicit").is_none_or(|explicit| {
                !explicit.is_null()
                    || ["prepend", "append", "delete"]
                        .iter()
                        .any(|k| value[*k].as_array().is_some_and(|a| !a.is_empty()))
            })
        });
    }
    watch
}

fn snapshot(loaded: &LoadedStage, stage: &Stage) -> Json {
    let store = &loaded.store;
    let path = |p: PathId| store.paths.display(p, &store.tokens);
    let target = |t: &TargetPath| match t {
        TargetPath::Prim(p) => path(*p),
        TargetPath::Property(p) => p.display(&store.paths, &store.tokens),
    };
    let root_layer = &store.layers[&loaded.root_layer];
    let sub = &store.layers[&layer_named(loaded, "sub.usda")];
    let watch_path = store
        .paths
        .lookup(&Path::parse_absolute("/Watch", &store.tokens).unwrap())
        .unwrap();
    let spec = &root_layer.prims()[&watch_path];
    let property = |name: &str| spec.property(store.tokens.lookup(name).unwrap()).unwrap();
    let arc = |r: &Reference| {
        json!({
            "asset": r.asset.clone().unwrap_or_default(),
            "path": match r.target { ReferenceTarget::Prim(p) => path(p), _ => String::new() },
        })
    };
    let mut watch = json!({
        "inheritPaths": list_op(&spec.inherits, |p| json!(path(*p))),
        "specializes": list_op(&spec.specializes, |p| json!(path(*p))),
        "references": list_op(&spec.references, arc),
        "look": list_op(property("look").targets.as_ref().unwrap(), |t| json!(target(t))),
        "connections": property("z").targets.as_ref().map(|t| list_op(t, |t| json!(target(t)))),
        "expr": match property("expr").default.as_ref() {
            Some(layerstack::Value::PathExpression(text)) => json!(text.to_string()),
            other => panic!("expr is {other:?}"),
        },
    });
    if watch["connections"].is_null() {
        watch
            .as_object_mut()
            .unwrap()
            .insert("connections".into(), Json::Null);
    }
    let root = store.paths.lookup(&Path::root()).unwrap();
    json!({
        "layers": {
            "scene.usda": layer_specs(loaded, root_layer),
            "sub.usda": layer_specs(loaded, sub),
        },
        "relocates": root_layer.relocates().iter().map(|r| json!([path(r.source), r.target.map(path).unwrap_or_default()])).collect::<Vec<_>>(),
        "defaultPrim": root_layer.default_prim().map(|t| store.tokens.resolve(t).to_string()).unwrap_or_default(),
        "watch": normalized(watch),
        "prims": stage.traverse_all(root).filter(|p| *p != root).map(path).collect::<Vec<_>>(),
    })
}

fn expected(case: &Json) -> Json {
    json!({
        "layers": case["layers"],
        "relocates": case["relocates"],
        "defaultPrim": case["defaultPrim"],
        "watch": normalized(case["watch"].clone()),
        "prims": case["prims"],
    })
}

/// What OpenUSD does when it deletes `/A` without failing (see the module
/// docs).
fn root_prim_expectation() -> Json {
    json!({
        "layers": {
            "scene.usda": ["/Cls", "/R", "/R/Local", "/Watch"],
            "sub.usda": [],
        },
        "relocates": [],
        "defaultPrim": "",
        "watch": {
            "connections": {"append": [], "delete": [], "explicit": [], "prepend": []},
            "expr": "/R/Local",
            "inheritPaths": {"append": [], "delete": [], "explicit": null, "prepend": ["/Cls"]},
            "look": {"append": [], "delete": [], "explicit": ["/R/FromAsset"], "prepend": []},
        },
        "prims": ["/Cls", "/R", "/R/FromAsset", "/R/Local", "/Watch"],
    })
}

#[test]
fn deletes_agree_with_openusd_namespace_editor() {
    let oracle: Json = serde_json::from_str(ORACLE).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut loaded = load();
        let original = loaded.store.layers.clone();
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let text = case["path"].as_str().unwrap();
        let object = match text.split_once('.') {
            Some((prim, property)) => {
                let prim = loaded.store.path(prim);
                let property = loaded.store.tokens.intern(property);
                TargetPath::Property(PropertyPath::new(prim, property))
            }
            None => TargetPath::Prim(loaded.store.path(text)),
        };
        let before = snapshot(&loaded, live.stage());
        let target = EditTarget::for_layer(loaded.root_layer);
        let prepared =
            NamespaceEdit::prepare_delete(live.stage(), &mut loaded.store, &target, object);
        let (applied, want) = match case["applied"].as_str() {
            Some("failed") => {
                assert_eq!(name, "root_prim", "an unexpected OpenUSD failure");
                (true, root_prim_expectation())
            }
            _ => {
                let applied = case["applied"].as_bool().unwrap();
                (
                    applied,
                    if applied {
                        expected(case)
                    } else {
                        before.clone()
                    },
                )
            }
        };
        assert_eq!(prepared.is_ok(), applied, "{name}: applied ({prepared:?})");
        let inverse = match prepared {
            Ok(edit) => {
                let result = live.apply(&mut loaded.store, edit.transaction()).unwrap();
                Some(result.inverse)
            }
            Err(_) => None,
        };
        assert_eq!(snapshot(&loaded, live.stage()), want, "{name}: result");
        let fresh = Stage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        assert_eq!(
            snapshot(&loaded, live.stage()),
            snapshot(&loaded, &fresh),
            "{name}: the live stage recomposes"
        );
        if let Some(inverse) = inverse {
            live.apply(&mut loaded.store, &inverse).unwrap();
            for (id, layer) in &original {
                let now = &loaded.store.layers[id];
                assert_eq!(now.prims(), layer.prims(), "{name}: undo restores {id:?}");
                assert_eq!(
                    now.relocates(),
                    layer.relocates(),
                    "{name}: undo restores {id:?}"
                );
                assert_eq!(
                    now.default_prim(),
                    layer.default_prim(),
                    "{name}: undo restores {id:?}"
                );
            }
            assert_eq!(snapshot(&loaded, live.stage()), before, "{name}: undo");
        }
    }
}
