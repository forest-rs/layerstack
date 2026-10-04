// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Namespace moves agree with OpenUSD 26.08's `UsdNamespaceEditor`, including
//! relationships, shader connections, propertyOrder, path expressions and
//! defaultPrim. Expectations come from `scripts/namespace_edits_oracle.py`.
//! AOUSD Core §8, §10, §12.4.
#![allow(missing_docs, reason = "integration test")]

use layerstack::{
    EditTarget, LiveStage, NamespaceEdit, Path, PropertyPath, StageOptions, TargetPath, Value,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

fn snapshot(
    live: &LiveStage,
    store: &layerstack::InMemoryStore,
    root_layer: layerstack::LayerId,
) -> Json {
    let stage = live.stage();
    let root = store.paths.lookup(&Path::root()).unwrap();
    let mut prims = Vec::new();
    let mut properties = BTreeMap::new();
    let mut relative = BTreeMap::new();
    for prim in stage.traverse(root) {
        let path = store.paths.display(prim, &store.tokens);
        if path == "/" {
            continue;
        }
        prims.push(path.clone());
        let mut names: Vec<_> = stage
            .property_names(prim, store)
            .into_iter()
            .map(|n| store.tokens.resolve(n))
            .collect();
        names.sort_unstable();
        let order: Vec<_> = store.layers[&root_layer].prims[&prim]
            .property_order
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|n| store.tokens.resolve(*n))
            .collect();
        properties.insert(path, json!({"names":names,"order":order}));
        if let Some(name) = store.tokens.lookup("relative") {
            let property = PropertyPath::new(prim, name);
            if let Some(value) = stage.resolve_field_path(property)
                && let Value::PathExpression(text) = value.value
            {
                relative.insert(
                    property.display(&store.paths, &store.tokens),
                    text.to_string(),
                );
            }
        }
    }
    prims.sort();
    let watch = store
        .paths
        .lookup(&Path::root().join(&[store.tokens.lookup("Watch").unwrap()]))
        .unwrap();
    let target_list = |name| {
        stage
            .resolve_target_list_path(PropertyPath::new(watch, store.tokens.lookup(name).unwrap()))
            .unwrap()
            .value
            .into_iter()
            .map(|p| p.display(&store.paths, &store.tokens))
            .collect::<Vec<_>>()
    };
    let expression = stage
        .resolve_field_path(PropertyPath::new(
            watch,
            store.tokens.lookup("expression").unwrap(),
        ))
        .unwrap()
        .value;
    let Value::PathExpression(expression) = expression else {
        panic!("expression");
    };
    json!({"prims":prims,"defaultPrim":store.tokens.resolve(store.layers[&root_layer].default_prim.unwrap()),"watchTargets":target_list("look"),"connections":target_list("inputs:value"),"expression":expression.as_ref(),"properties":properties,"relativeExpressions":relative})
}

#[test]
fn namespace_moves_match_native_usd() {
    let fixtures = workspace_root().join("layerstack_conformance/fixtures/namespace_edits");
    let oracle: Json =
        serde_json::from_str(include_str!("../fixtures/namespace_edits/oracle.json")).unwrap();
    for (name, case) in oracle.as_object().unwrap() {
        let mut loaded = load_entry_usda(&fixtures.join(format!("{name}.usda")));
        let before = loaded.store.layers[&loaded.root_layer].clone();
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let source = TargetPath::parse(
            case["source"].as_str().unwrap(),
            &mut loaded.store.tokens,
            &mut loaded.store.paths,
        )
        .unwrap();
        let destination = TargetPath::parse(
            case["destination"].as_str().unwrap(),
            &mut loaded.store.tokens,
            &mut loaded.store.paths,
        )
        .unwrap();
        let edit = NamespaceEdit::prepare(
            live.stage(),
            &mut loaded.store,
            &EditTarget::for_layer(loaded.root_layer),
            source,
            destination,
        )
        .unwrap();
        assert_eq!(
            loaded.store.layers[&loaded.root_layer], before,
            "{name} preview"
        );
        let applied = live.apply(&mut loaded.store, edit.transaction()).unwrap();
        assert_eq!(
            snapshot(&live, &loaded.store, loaded.root_layer),
            case["snapshot"],
            "{name}"
        );
        live.apply(&mut loaded.store, &applied.inverse).unwrap();
        assert_eq!(
            loaded.store.layers[&loaded.root_layer], before,
            "{name} undo"
        );
    }
}
