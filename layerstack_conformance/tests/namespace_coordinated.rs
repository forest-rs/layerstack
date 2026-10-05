// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Root-stack splits, reference/payload relocates, dependent overrides and
//! direct-reference repairs match OpenUSD 26.08 `UsdNamespaceEditor`.
//! AOUSD Core §8 and §10.3.2.6. Evidence is generated independently by
//! `scripts/namespace_coordinated_oracle.py`; all inputs precede the native edit.
#![allow(missing_docs, reason = "integration test")]

use layerstack::{
    EditTarget, InMemoryStore, LayerId, LiveStage, NamespaceEdit, PropertyKind, PropertyPath,
    Stage, StageOptions, TargetPath, Value,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

fn stage_snapshot(stage: &Stage, store: &InMemoryStore) -> Json {
    let mut prims = Vec::new();
    let mut values = BTreeMap::<String, Json>::new();
    let mut expressions = BTreeMap::new();
    let mut targets = BTreeMap::new();
    let mut connections = BTreeMap::new();
    for prim in stage.traverse(store.paths.lookup(&layerstack::Path::root()).unwrap()) {
        let path = store.paths.display(prim, &store.tokens);
        if path == "/" {
            continue;
        }
        prims.push(path);
        for name in stage.property_names(prim, store) {
            let property = PropertyPath::new(prim, name);
            let path = property.display(&store.paths, &store.tokens);
            if let Some(list) = stage.resolve_target_list_path(property) {
                let list: Vec<_> = list
                    .value
                    .into_iter()
                    .map(|p| p.display(&store.paths, &store.tokens))
                    .collect();
                if stage.property_kind(property).unwrap() == PropertyKind::Relationship {
                    targets.insert(path.clone(), list);
                } else if !list.is_empty() {
                    connections.insert(path.clone(), list);
                }
            }
            if let Some(value) = stage.resolve_field_path(property) {
                match value.value {
                    Value::Double(n) => {
                        values.insert(path, json!(n));
                    }
                    Value::Float(n) => {
                        values.insert(path, json!(n));
                    }
                    Value::PathExpression(text) => {
                        expressions.insert(path, text.to_string());
                    }
                    _ => {}
                }
            }
        }
    }
    prims.sort();
    json!({"prims":prims,"values":values,"targets":targets,"connections":connections,"expressions":expressions})
}
fn layer_snapshot(store: &InMemoryStore, layer: LayerId) -> Json {
    let layer = &store.layers[&layer];
    let mut specs: Vec<_> = layer
        .prims
        .keys()
        .map(|p| store.paths.display(*p, &store.tokens))
        .collect();
    specs.push("/".into());
    specs.sort();
    specs.dedup();
    let relocates: Vec<_> = layer
        .relocates
        .iter()
        .map(|r| {
            vec![
                store.paths.display(r.source, &store.tokens),
                r.target
                    .map_or(String::new(), |p| store.paths.display(p, &store.tokens)),
            ]
        })
        .collect();
    json!({"specs":specs,"relocates":relocates})
}

#[test]
fn coordinated_namespace_matches_native_usd_and_restores_every_layer() {
    let directory = workspace_root().join("layerstack_conformance/fixtures/namespace_coordinated");
    let oracle: Json = serde_json::from_str(include_str!(
        "../fixtures/namespace_coordinated/oracle.json"
    ))
    .unwrap();
    for (case, evidence) in oracle.as_object().unwrap() {
        let mut loaded = load_entry_usda(&directory.join(case).join("inventory.usda"));
        assert!(loaded.invalid.is_empty());
        let named: BTreeMap<_, _> = loaded
            .layer_names
            .iter()
            .map(|(id, name)| (name.clone(), *id))
            .collect();
        let root = named[evidence["root"].as_str().unwrap()];
        let before: Vec<_> = loaded
            .store
            .layers
            .iter()
            .map(|(id, l)| (*id, l.clone()))
            .collect();
        let mut primary = LiveStage::compose(&mut loaded.store, root, StageOptions::default());
        let mut dependents: Vec<_> = evidence["dependents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| {
                let id = named[name.as_str().unwrap()];
                LiveStage::compose(&mut loaded.store, id, StageOptions::default())
            })
            .collect();
        let from = TargetPath::parse(
            evidence["source"].as_str().unwrap(),
            &mut loaded.store.tokens,
            &mut loaded.store.paths,
        )
        .unwrap();
        let to = TargetPath::parse(
            evidence["destination"].as_str().unwrap(),
            &mut loaded.store.tokens,
            &mut loaded.store.paths,
        )
        .unwrap();
        let dependent_refs: Vec<_> = dependents.iter().map(LiveStage::stage).collect();
        let edit = NamespaceEdit::prepare_with_dependents(
            primary.stage(),
            &dependent_refs,
            &mut loaded.store,
            &EditTarget::for_layer(root),
            from,
            to,
        )
        .unwrap();
        for (id, layer) in &before {
            assert_eq!(&loaded.store.layers[id], layer, "{case} preview");
        }
        let inverse = primary
            .apply(&mut loaded.store, edit.transaction())
            .unwrap()
            .inverse;
        for dependent in &mut dependents {
            dependent.synchronize(&mut loaded.store);
        }
        for stage in core::iter::once(&primary).chain(&dependents) {
            let id = stage.stage().root_layer().unwrap();
            let name = &loaded.layer_names[&id];
            assert_eq!(
                stage_snapshot(stage.stage(), &loaded.store),
                evidence["stages"][name],
                "{case} stage {name}"
            );
        }
        for (name, expected) in evidence["layers"].as_object().unwrap() {
            assert_eq!(
                layer_snapshot(&loaded.store, named[name]),
                *expected,
                "{case} layer {name}"
            );
        }
        primary.apply(&mut loaded.store, &inverse).unwrap();
        for (id, layer) in before {
            assert_eq!(loaded.store.layers[&id], layer, "{case} undo layer {id:?}");
        }
    }
}

#[test]
fn relocation_expression_map_domains_match_native_usd() {
    let directory = workspace_root().join("layerstack_conformance/fixtures/namespace_coordinated");
    let mut loaded = load_entry_usda(&directory.join("expression_maps/root.usda"));
    let before: Vec<_> = loaded
        .store
        .layers
        .iter()
        .map(|(id, l)| (*id, l.clone()))
        .collect();
    let stage = Stage::compose(
        &mut loaded.store,
        loaded.root_layer,
        StageOptions::default(),
    );
    assert!(
        stage.composition_errors().is_empty(),
        "valid native expression-map fixture"
    );
    let oracle: Json = serde_json::from_str(include_str!(
        "../fixtures/namespace_coordinated/expression_maps.json"
    ))
    .unwrap();
    for (property, expected) in oracle.as_object().unwrap() {
        let path = loaded.store.property_path(property);
        let resolved = stage.resolve_field_path(path).expect(property);
        assert_eq!(
            resolved.value,
            Value::PathExpression(expected.as_str().unwrap().into()),
            "{property}"
        );
    }
    for (id, layer) in before {
        assert_eq!(
            loaded.store.layers[&id], layer,
            "expression mapping must preserve authored sources"
        );
    }
}
