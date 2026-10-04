// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{
    EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec, Transaction,
    Value,
};

#[test]
fn forwarded_queries_retain_absence_and_refresh_when_a_terminal_becomes_a_relationship() {
    let mut store = InMemoryStore::default();
    let root = LayerId(1);
    let recipe = store.path("/Recipe");
    let other = store.path("/Other");
    let outside = store.path("/Outside");
    let source = store.property_path("/Recipe.source");
    let link = store.property_path("/Other.link");
    let missing = store.property_path("/Missing.future");
    let unrelated = store.property_path("/Outside.value");
    let mut layer = Layer::new(root);
    for path in [recipe, other, outside] {
        layer.insert_prim(path, PrimSpec::def());
    }
    layer.set_property(
        source,
        PropertySpec::relationship()
            .with_targets(ListOp::explicit(vec![TargetPath::Property(link)])),
    );
    layer.set_property(
        link,
        PropertySpec::relationship()
            .with_targets(ListOp::explicit(vec![TargetPath::Property(missing)])),
    );
    layer.set_property(
        unrelated,
        PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
            .with_default(Value::Int(1)),
    );
    store.insert_layer(layer);
    let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
    let mut query = RelationshipQuery::new(source);
    assert_eq!(
        query.get(live.stage()),
        Some(vec![TargetPath::Property(missing)])
    );
    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(root).property(unrelated),
        Value::Int(2),
    );
    live.apply(&mut store, &edit).unwrap();
    assert!(query.is_current(live.stage()));
    assert_eq!(
        query.get(live.stage()),
        Some(vec![TargetPath::Property(missing)])
    );
    assert_eq!(
        query.work(),
        RelationshipQueryWork {
            evaluations: 1,
            cache_hits: 1
        }
    );
    let final_target = store.path("/Final");
    let mut creation = Transaction::new();
    creation.create_prim(
        EditTarget::for_layer(root).prim(missing.prim_path()),
        Specifier::Def,
        None,
    );
    creation.create_property(
        EditTarget::for_layer(root).property(missing),
        PropertySpec::relationship()
            .with_targets(ListOp::explicit(vec![TargetPath::Prim(final_target)])),
    );
    live.apply(&mut store, &creation).unwrap();
    assert!(!query.is_current(live.stage()));
    assert_eq!(
        query.get(live.stage()),
        Some(vec![TargetPath::Prim(final_target)])
    );
    let mut cycle = Transaction::new();
    cycle.set_targets(
        EditTarget::for_layer(root).property(missing),
        ListOp::explicit(vec![TargetPath::Property(source)]),
    );
    live.apply(&mut store, &cycle).unwrap();
    assert_eq!(query.get(live.stage()), Some(Vec::new()));
    assert_eq!(query.work().evaluations, 3);
}
