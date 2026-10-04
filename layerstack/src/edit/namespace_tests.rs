// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{
    ArcKind, InMemoryStore, Layer, LayerId, LayerStore, ListOp, LiveStage, PrimSpec, PropertyPath,
    PropertySpec, PropertyType, Reference, ReferenceTarget, Stage, StageOptions, SublayerEntry,
    TargetPath, Value,
};
use alloc::{sync::Arc, vec, vec::Vec};

const ROOT: LayerId = LayerId(1);
const ASSET: LayerId = LayerId(2);
fn attribute(value: f64) -> PropertySpec {
    PropertySpec::typed_attribute(PropertyType::new("double", false, Value::Double(0.0)))
        .with_default(Value::Double(value))
}
fn base() -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let a = store.path("/A");
    let child = store.path("/A/Child");
    let b = store.path("/B");
    let watch = store.path("/Watch");
    let name = store.tokens.intern("Child");
    let out = store.tokens.intern("outputs:value");
    let mut layer = Layer::new(ROOT);
    layer.default_prim = Some(store.tokens.intern("A"));
    layer.insert_prim(
        a,
        PrimSpec::def()
            .with_children(vec![name])
            .with_property(out, attribute(4.0)),
    );
    layer.insert_prim(child, PrimSpec::def());
    layer.insert_prim(b, PrimSpec::def());
    layer.insert_prim(watch, PrimSpec::def());
    store.insert_layer(layer);
    store
}
fn prim_move(
    stage: &Stage,
    store: &mut InMemoryStore,
    from: &str,
    to: &str,
) -> Result<NamespaceEdit, NamespaceError> {
    let from = TargetPath::Prim(store.path(from));
    let to = TargetPath::Prim(store.path(to));
    NamespaceEdit::prepare(stage, store, &EditTarget::for_layer(ROOT), from, to)
}
fn property_move(
    stage: &Stage,
    store: &mut InMemoryStore,
    from: &str,
    to: &str,
) -> Result<NamespaceEdit, NamespaceError> {
    let from = TargetPath::Property(store.property_path(from));
    let to = TargetPath::Property(store.property_path(to));
    NamespaceEdit::prepare(stage, store, &EditTarget::for_layer(ROOT), from, to)
}
fn reads(store: &InMemoryStore, layer: LayerId, property: PropertyPath) -> Vec<TargetPath> {
    store.layers[&layer]
        .property(property)
        .unwrap()
        .targets
        .as_ref()
        .unwrap()
        .apply_to(&[])
}

#[test]
fn move_subtree_repairs_dependent_targets_expressions_and_arcs_then_undoes() {
    let mut store = base();
    let a = store.path("/A");
    let child = store.path("/A/Child");
    let a_output = store.property_path("/A.outputs:value");
    let watch = store.path("/Watch");
    let rel = store.tokens.intern("look");
    let conn = store.tokens.intern("inputs:value");
    let expr = store.tokens.intern("expression");
    let relative = store.tokens.intern("relative");
    let expression_type = PropertyType::new(
        "pathExpression",
        false,
        Value::PathExpression(Arc::from("")),
    );
    let layer = store.layers.get_mut(&ROOT).unwrap();
    let watcher = layer.prims.get_mut(&watch).unwrap();
    watcher.properties.push(crate::PropertyEntry {
        name: rel,
        spec: PropertySpec::relationship()
            .with_targets(ListOp::explicit(vec![
                TargetPath::Prim(a),
                TargetPath::Prim(child),
            ]))
            .into(),
    });
    watcher.properties.push(crate::PropertyEntry {
        name: conn,
        spec: attribute(0.0)
            .with_targets(ListOp::explicit(vec![TargetPath::Property(a_output)]))
            .into(),
    });
    watcher.properties.push(crate::PropertyEntry {
        name: expr,
        spec: PropertySpec::typed_attribute(expression_type.clone())
            .with_default(Value::PathExpression(Arc::from("/A// /AX//")))
            .into(),
    });
    layer
        .prims
        .get_mut(&a)
        .unwrap()
        .properties
        .push(crate::PropertyEntry {
            name: relative,
            spec: PropertySpec::typed_attribute(expression_type)
                .with_default(Value::PathExpression(Arc::from("Child//")))
                .into(),
        });
    let reference = store.path("/Reference");
    let inherit = store.path("/Inherit");
    let specialize = store.path("/Specialize");
    let payload = store.path("/Payload");
    let layer = store.layers.get_mut(&ROOT).unwrap();
    layer.insert_prim(
        reference,
        PrimSpec::def().with_reference(Reference::new(ROOT, a)),
    );
    layer.insert_prim(inherit, PrimSpec::def().with_inherit(a));
    layer.insert_prim(specialize, PrimSpec::def().with_specialize(a));
    let mut payload_spec = PrimSpec::def();
    payload_spec.payloads = ListOp::explicit(vec![Reference::new(ROOT, a)]);
    layer.insert_prim(payload, payload_spec);
    let before = layer.clone();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let edit = prim_move(live.stage(), &mut store, "/A", "/B/New").unwrap();
    assert_eq!(
        store.layers[&ROOT], before,
        "preview cannot mutate authored content"
    );
    assert_eq!(edit.layers_to_edit(), [ROOT]);
    let applied = live.apply(&mut store, edit.transaction()).unwrap();
    let new = store.path("/B/New");
    let new_child = store.path("/B/New/Child");
    assert!(live.stage().has_prim(new_child));
    assert!(!live.stage().has_prim(a));
    assert!(applied.changes.created.contains(&new));
    assert!(applied.changes.removed.contains(&child));
    assert_eq!(
        reads(&store, ROOT, PropertyPath::new(watch, rel)),
        vec![TargetPath::Prim(new), TargetPath::Prim(new_child)]
    );
    let newout = store.property_path("/B/New.outputs:value");
    assert_eq!(
        reads(&store, ROOT, PropertyPath::new(watch, conn)),
        vec![TargetPath::Property(newout)]
    );
    assert_eq!(
        store
            .tokens
            .resolve(store.layers[&ROOT].default_prim.unwrap()),
        "/B/New"
    );
    assert_eq!(
        live.stage()
            .resolve_field_path(PropertyPath::new(watch, expr))
            .unwrap()
            .value,
        Value::PathExpression(Arc::from("/B/New// /AX//"))
    );
    assert_eq!(
        store.layers[&ROOT]
            .property(PropertyPath::new(new, relative))
            .unwrap()
            .default,
        Some(Value::PathExpression(Arc::from("/B/New/Child//")))
    );
    let layer = &store.layers[&ROOT];
    assert_eq!(
        layer.prims[&reference].references.apply_to(&[])[0].target,
        ReferenceTarget::Prim(new)
    );
    assert_eq!(
        layer.prims[&payload].payloads.explicit.as_ref().unwrap()[0].target,
        ReferenceTarget::Prim(new)
    );
    assert_eq!(layer.prims[&inherit].inherits.apply_to(&[]), &[new]);
    assert_eq!(layer.prims[&specialize].specializes.apply_to(&[]), &[new]);
    assert!(live.stage().has_prim(store.path("/Reference/Child")));
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(store.layers[&ROOT], before);
}

#[test]
fn property_move_repairs_every_list_bucket_and_preserves_authored_property_order() {
    let mut store = base();
    let source = store.property_path("/A.outputs:value");
    let dest = store.property_path("/B.inputs:renamed");
    let watch = store.path("/Watch");
    let rel = store.tokens.intern("look");
    let op = ListOp {
        explicit: None,
        prepend: vec![TargetPath::Property(source)],
        append: vec![TargetPath::Property(source)],
        delete: vec![TargetPath::Property(source)],
        add: vec![TargetPath::Property(source)],
        reorder: vec![TargetPath::Property(source)],
    };
    store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .prims
        .get_mut(&watch)
        .unwrap()
        .properties
        .push(crate::PropertyEntry {
            name: rel,
            spec: PropertySpec::relationship().with_targets(op).into(),
        });
    let a = source.prim_path();
    store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .prims
        .get_mut(&a)
        .unwrap()
        .property_order = Some(vec![source.property()]);
    let before = store.layers[&ROOT].clone();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let edit = property_move(
        live.stage(),
        &mut store,
        "/A.outputs:value",
        "/B.inputs:renamed",
    )
    .unwrap();
    let applied = live.apply(&mut store, edit.transaction()).unwrap();
    assert!(!live.stage().has_property_path(source));
    assert!(live.stage().has_property_path(dest));
    assert_eq!(
        store.layers[&ROOT].prims[&a].property_order,
        Some(vec![source.property()])
    );
    let op = store.layers[&ROOT]
        .property(PropertyPath::new(watch, rel))
        .unwrap()
        .targets
        .as_ref()
        .unwrap();
    for bucket in [&op.prepend, &op.append, &op.delete, &op.add, &op.reorder] {
        assert_eq!(bucket, &[TargetPath::Property(dest)]);
    }
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(store.layers[&ROOT], before);
}

#[test]
fn property_reparent_anchors_relative_expression_at_its_previous_owner() {
    let mut store = base();
    let source = store.property_path("/A.expression");
    let property = PropertySpec::typed_attribute(PropertyType::new(
        "pathExpression",
        false,
        Value::PathExpression(Arc::from("")),
    ))
    .with_default(Value::PathExpression(Arc::from("Child//")));
    store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .prims
        .get_mut(&source.prim_path())
        .unwrap()
        .properties
        .push(crate::PropertyEntry {
            name: source.property(),
            spec: property.into(),
        });
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let edit = property_move(live.stage(), &mut store, "/A.expression", "/B.expression").unwrap();
    live.apply(&mut store, edit.transaction()).unwrap();
    let moved = store.property_path("/B.expression");
    assert_eq!(
        store.layers[&ROOT].property(moved).unwrap().default,
        Some(Value::PathExpression(Arc::from("/A/Child//")))
    );
}

#[test]
fn mapped_source_layer_move_repairs_primary_stage_dependents() {
    let mut store = InMemoryStore::default();
    let asset = store.path("/Asset");
    let child = store.path("/Asset/Child");
    let mount = store.path("/Mount");
    let watch = store.path("/Watch");
    let child_name = store.tokens.intern("Child");
    let rel = store.tokens.intern("look");
    let source = store.path("/Mount/Child");
    let dest = store.path("/Mount/Renamed");
    let mut layer = Layer::new(ASSET);
    layer.insert_prim(asset, PrimSpec::def().with_children(vec![child_name]));
    layer.insert_prim(child, PrimSpec::def());
    store.insert_layer(layer);
    let mut layer = Layer::new(ROOT);
    layer.insert_prim(
        mount,
        PrimSpec::def().with_reference(Reference::with_asset(ASSET, asset, "asset.usda")),
    );
    layer.insert_prim(
        watch,
        PrimSpec::def().with_property(
            rel,
            PropertySpec::relationship()
                .with_targets(ListOp::explicit(vec![TargetPath::Prim(source)])),
        ),
    );
    store.insert_layer(layer);
    let before_root = store.layers[&ROOT].clone();
    let before_asset = store.layers[&ASSET].clone();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let graph = live.stage().explain_prim_graph(mount).unwrap();
    let node = graph
        .nodes()
        .find(|(_, n)| n.arc_kind() == ArcKind::References)
        .unwrap()
        .0;
    let target = EditTarget::for_node(live.stage(), mount, node).unwrap();
    let edit = NamespaceEdit::prepare(
        live.stage(),
        &mut store,
        &target,
        TargetPath::Prim(source),
        TargetPath::Prim(dest),
    )
    .unwrap();
    assert_eq!(edit.layers_to_edit(), [ROOT, ASSET]);
    let applied = live.apply(&mut store, edit.transaction()).unwrap();
    let spec_dest = store.path("/Asset/Renamed");
    assert!(store.layers[&ASSET].prims.contains_key(&spec_dest));
    assert!(live.stage().has_prim(dest));
    assert_eq!(
        reads(&store, ROOT, PropertyPath::new(watch, rel)),
        vec![TargetPath::Prim(dest)]
    );
    live.apply(&mut store, &applied.inverse).unwrap();
    assert_eq!(store.layers[&ROOT], before_root);
    assert_eq!(store.layers[&ASSET], before_asset);
    let outside = store.path("/Outside");
    assert_eq!(
        NamespaceEdit::prepare(
            live.stage(),
            &mut store,
            &target,
            TargetPath::Prim(source),
            TargetPath::Prim(outside)
        ),
        Err(NamespaceError::Unmappable)
    );
}

#[test]
fn collisions_cycles_missing_parents_and_split_opinions_change_nothing() {
    let mut store = base();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let before = store.layers[&ROOT].clone();
    assert_eq!(
        prim_move(live.stage(), &mut store, "/A", "/B"),
        Err(NamespaceError::Collision)
    );
    assert_eq!(
        prim_move(live.stage(), &mut store, "/A", "/A/Child/New"),
        Err(NamespaceError::Cycle)
    );
    assert_eq!(
        prim_move(live.stage(), &mut store, "/A", "/Missing/New"),
        Err(NamespaceError::MissingParent)
    );
    assert_eq!(store.layers[&ROOT], before);
    let a = store.path("/A");
    let mut weak = Layer::new(ASSET);
    weak.insert_prim(a, PrimSpec::over());
    store.insert_layer(weak);
    store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .sublayers
        .push(SublayerEntry::new(ASSET));
    live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let error = prim_move(live.stage(), &mut store, "/A", "/Renamed").unwrap_err();
    assert!(matches!(
        error,
        NamespaceError::SplitOpinions { layer: ASSET, .. }
    ));
}

#[test]
fn stale_preview_and_conflicting_undo_are_atomic() {
    let mut store = base();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let edit = prim_move(live.stage(), &mut store, "/A", "/Renamed").unwrap();
    store.layer_mut(ROOT).unwrap().touch();
    let before = store.layers[&ROOT].clone();
    assert!(matches!(
        live.apply(&mut store, edit.transaction()),
        Err(EditError::StaleGeneration { .. })
    ));
    assert_eq!(store.layers[&ROOT], before);
    let edit = prim_move(live.stage(), &mut store, "/A", "/Renamed").unwrap();
    let applied = live.apply(&mut store, edit.transaction()).unwrap();
    let property = store.property_path("/Renamed.outputs:value");
    let mut other = Transaction::new();
    other.set_default(
        EditTarget::for_layer(ROOT).property(property),
        Value::Double(99.0),
    );
    live.apply(&mut store, &other).unwrap();
    let before = store.layers[&ROOT].clone();
    let generation = store.layers[&ROOT].generation();
    assert!(matches!(
        live.apply(&mut store, &applied.inverse),
        Err(EditError::StaleValue { .. })
    ));
    assert_eq!(store.layers[&ROOT], before);
    assert_eq!(store.layers[&ROOT].generation(), generation);
}

#[test]
fn namespace_steps_roll_back_with_later_failed_transaction_operations() {
    let mut store = base();
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let before = store.layers[&ROOT].clone();
    let generation = before.generation();
    let mut transaction = prim_move(live.stage(), &mut store, "/A", "/Renamed")
        .unwrap()
        .into_transaction();
    let nonexistent = store.property_path("/No.attribute");
    transaction.set_default(
        EditTarget::for_layer(ROOT).property(nonexistent),
        Value::Double(1.0),
    );
    assert!(live.apply(&mut store, &transaction).is_err());
    assert_eq!(store.layers[&ROOT], before);
    assert_eq!(store.layers[&ROOT].generation(), generation);
}

#[test]
fn edits_beneath_instances_are_rejected() {
    let mut store = base();
    let a = store.path("/A");
    let instance = store.path("/Instance");
    store.layers.get_mut(&ROOT).unwrap().insert_prim(
        instance,
        PrimSpec::def()
            .with_reference(Reference::new(ROOT, a))
            .with_instanceable(true),
    );
    let live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    assert!(live.stage().is_instance(instance));
    assert_eq!(
        prim_move(live.stage(), &mut store, "/Instance/Child", "/Instance/New"),
        Err(NamespaceError::InstanceProxy)
    );
    assert_eq!(
        property_move(
            live.stage(),
            &mut store,
            "/Instance.outputs:value",
            "/Instance.outputs:new"
        ),
        Err(NamespaceError::NoSuchObject)
    );
    let local = store.tokens.intern("local");
    store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .prims
        .get_mut(&instance)
        .unwrap()
        .properties
        .push(crate::PropertyEntry {
            name: local,
            spec: attribute(1.0).into(),
        });
    let mut live = LiveStage::compose(&mut store, ROOT, StageOptions::default());
    let edit = property_move(
        live.stage(),
        &mut store,
        "/Instance.local",
        "/Instance.renamed",
    )
    .unwrap();
    live.apply(&mut store, edit.transaction()).unwrap();
    assert!(
        live.stage()
            .has_property_path(store.property_path("/Instance.renamed")),
        "instance roots permit local property edits"
    );
}

#[test]
fn dependent_descendant_namespace_requires_explicit_coordination() {
    let mut store = base();
    let a = store.path("/A");
    let alias = store.path("/Alias");
    store.layers.get_mut(&ROOT).unwrap().insert_prim(
        alias,
        PrimSpec::def().with_reference(Reference::new(ROOT, a)),
    );
    let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
    assert!(matches!(
        prim_move(&stage, &mut store, "/A/Child", "/A/New"),
        Err(NamespaceError::UnsupportedComposition(_))
    ));
}

#[test]
fn inactive_roots_and_their_dormant_namespace_are_rejected() {
    let mut store = base();
    let source = store.path("/A");
    store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .prims
        .get_mut(&source)
        .unwrap()
        .active = Some(false);
    let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
    assert!(stage.has_prim(source), "inactive roots remain inspectable");
    assert!(!stage.is_active(source), "the source remains inactive");
    let error = prim_move(&stage, &mut store, "/A", "/Renamed").unwrap_err();
    assert!(
        matches!(error, NamespaceError::UnsupportedComposition(_)),
        "a retained inactive root requires dormant namespace coordination"
    );
}

#[test]
fn active_parent_cannot_move_uncoordinated_inactive_children() {
    let mut store = base();
    let child = store.path("/A/Child");
    store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .prims
        .get_mut(&child)
        .unwrap()
        .active = Some(false);
    let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
    assert!(matches!(
        prim_move(&stage, &mut store, "/A", "/Renamed"),
        Err(NamespaceError::UnsupportedComposition(_))
    ));
}

#[test]
fn dormant_weak_specs_are_rejected_even_when_they_are_not_populated() {
    let mut store = base();
    let child = store.path("/A/Child");
    let host = store.path("/A");
    let mut weak = Layer::new(ASSET);
    let mut dormant = PrimSpec::over();
    dormant
        .outer_variant_sites
        .push(crate::spec_path::VariantSelectionSite {
            host_path: host,
            set: store.tokens.intern("unselected"),
            variant: store.tokens.intern("dormant"),
        });
    weak.insert_prim(child, dormant);
    store.insert_layer(weak);
    store
        .layers
        .get_mut(&ROOT)
        .unwrap()
        .sublayers
        .push(SublayerEntry::new(ASSET));
    let stage = Stage::compose(&mut store, ROOT, StageOptions::default());
    assert!(matches!(
        prim_move(&stage, &mut store, "/A", "/Renamed"),
        Err(NamespaceError::SplitOpinions { layer: ASSET, .. })
    ));
}
