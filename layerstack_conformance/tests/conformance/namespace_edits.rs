// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Namespace moves agree with OpenUSD 26.08's `UsdNamespaceEditor`, including
//! relationships, shader connections, propertyOrder, path expressions and
//! defaultPrim. Expectations come from `scripts/namespace_edits_oracle.py`.
//! AOUSD Core §8, §10, §12.4.

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
        let mut names = stage
            .prim(prim, store)
            .map(|p| p.property_names())
            .unwrap_or_default();
        names.sort_unstable();
        let order: Vec<_> = store.layer(root_layer).unwrap().prims()[&prim]
            .property_order
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|n| store.tokens.resolve(*n))
            .collect();
        properties.insert(path, json!({"names":names,"order":order}));
        if let Some(name) = store.tokens.lookup("relative") {
            let property = PropertyPath::new(prim, name);
            if let Some(value) = stage
                .view(store)
                .attribute(property)
                .and_then(|a| a.authored_value(layerstack::Time::Default))
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
            .view(store)
            .property(PropertyPath::new(watch, store.tokens.lookup(name).unwrap()))
            .map(|p| match p.as_attribute() {
                Some(a) => a.connections(),
                None => p.as_relationship().map(|r| r.targets()).unwrap_or_default(),
            })
            .unwrap()
            .into_iter()
            .map(|p| p.display(&store.paths, &store.tokens))
            .collect::<Vec<_>>()
    };
    let expression = stage
        .view(store)
        .attribute(PropertyPath::new(
            watch,
            store.tokens.lookup("expression").unwrap(),
        ))
        .and_then(|a| a.authored_value(layerstack::Time::Default))
        .unwrap()
        .value;
    let Value::PathExpression(expression) = expression else {
        panic!("expression");
    };
    json!({"prims":prims,"defaultPrim":store.tokens.resolve(store.layer(root_layer).unwrap().default_prim().unwrap()),"watchTargets":target_list("look"),"connections":target_list("inputs:value"),"expression":expression.as_ref(),"properties":properties,"relativeExpressions":relative})
}

#[test]
fn namespace_moves_match_native_usd() {
    let fixtures = workspace_root().join("layerstack_conformance/fixtures/namespace_edits");
    let oracle: Json =
        serde_json::from_str(include_str!("../../fixtures/namespace_edits/oracle.json")).unwrap();
    for (name, case) in oracle.as_object().unwrap() {
        let mut loaded = load_entry_usda(&fixtures.join(format!("{name}.usda")));
        let before = loaded.store.layer(loaded.root_layer).unwrap().clone();
        let mut live = LiveStage::compose(
            &mut loaded.store,
            loaded.root_layer,
            StageOptions::default(),
        );
        let source = TargetPath::parse(
            case["source"].as_str().unwrap(),
            &loaded.store.tokens,
            &loaded.store.paths,
        )
        .unwrap();
        let destination = TargetPath::parse(
            case["destination"].as_str().unwrap(),
            &loaded.store.tokens,
            &loaded.store.paths,
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
            *loaded.store.layer(loaded.root_layer).unwrap(),
            before,
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
            *loaded.store.layer(loaded.root_layer).unwrap(),
            before,
            "{name} undo"
        );
    }
}
