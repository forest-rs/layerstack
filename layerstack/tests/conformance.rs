// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

#![allow(missing_docs, reason = "integration tests")]

use alloc::sync::Arc;

extern crate alloc;

use layerstack::{
    ArcKind, ArrayEdit, ArrayEditOp, FieldEntry, FieldValue, InterpolationType, Layer, LayerId,
    ListOp, PrimSpec, PropertyType, Reference, ResolvedValue, SchemaDefinition, SchemaRegistry,
    Stage, StageOptions, SublayerEntry, TargetPath, Value, VariantSetSpec, VariantSpec,
    doc::InMemoryStore,
};

#[test]
fn sublayer_strength_local_beats_sublayer() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let p = store.path("/P");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    root_layer.insert_prim(p, PrimSpec::default().with_field(field_x, 1_i64));
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(p, PrimSpec::default().with_field(field_x, 2_i64));
    store.insert_layer(sub_layer);

    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );

    let resolved = stage.resolve_field(p, field_x).expect("field exists");
    assert_eq!(resolved.value, Value::Int64(1));
    let prov = resolved.provenance.expect("provenance enabled");
    assert_eq!(prov.layer, LayerId(1));
}

#[test]
fn property_path_queries_match_prim_plus_field_queries() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let prim = store.path("/P");
    let property_path = store.property_path("/P.x");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(prim, PrimSpec::default().with_field(field_x, 7_i64));
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(stage.has_property_path(property_path));
    assert_eq!(
        stage
            .resolve_property_path(property_path)
            .expect("property exists")
            .value,
        ResolvedValue::Scalar(Value::Int64(7))
    );
    assert_eq!(
        stage
            .explain_property_path(property_path)
            .expect("opinion stack exists")
            .len(),
        1
    );
}

#[test]
fn property_path_time_queries_match_prim_plus_field_queries() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let prim = store.path("/P");
    let property_path = store.property_path("/P.x");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        prim,
        PrimSpec::default().with_field(
            field_x,
            FieldValue::TimeSamples(vec![(1.0, Value::Double(10.0)), (3.0, Value::Double(30.0))]),
        ),
    );
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let by_field = stage
        .resolve_value_at_time(prim, field_x, 2.0, InterpolationType::Held)
        .expect("sampled property exists");
    let by_property_path = stage
        .resolve_property_path_at_time(property_path, 2.0, InterpolationType::Held)
        .expect("sampled property exists");

    assert_eq!(by_property_path.value, by_field.value);
}

#[test]
fn target_list_preserves_property_target_identity() {
    let mut store = InMemoryStore::default();

    let input = store.tokens.intern("inputs:surface");
    let material = store.path("/Looks/Material");
    let shader = store.path("/Looks/Shader");
    let shader_output = store.property_path("/Looks/Shader.outputs:surface");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        material,
        PrimSpec::default().with_field(
            input,
            FieldValue::PathListOp(ListOp {
                explicit: Some(vec![store.target_path("/Looks/Shader.outputs:surface")]),
                ..ListOp::default()
            }),
        ),
    );
    layer.insert_prim(shader, PrimSpec::default());
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_path_list(material, input)
        .expect("target list exists");

    assert_eq!(resolved.value, vec![TargetPath::property(shader_output)]);
}

#[test]
fn referenced_property_targets_are_remapped_structurally() {
    let mut store = InMemoryStore::default();

    let input = store.tokens.intern("inputs:surface");
    let model = store.path("/Model");
    let asset = store.path("/Asset");
    let asset_shader = store.path("/Asset/Shader");
    let model_shader_output = store.property_path("/Model/Shader.outputs:surface");

    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        model,
        PrimSpec::default().with_reference(Reference::new(LayerId(2), asset)),
    );
    store.insert_layer(root);

    let mut asset_layer = Layer::new(LayerId(2));
    asset_layer.insert_prim(
        asset,
        PrimSpec::default().with_field(
            input,
            FieldValue::PathListOp(ListOp {
                explicit: Some(vec![store.target_path("/Asset/Shader.outputs:surface")]),
                ..ListOp::default()
            }),
        ),
    );
    asset_layer.insert_prim(asset_shader, PrimSpec::default());
    store.insert_layer(asset_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_path_list(model, input)
        .expect("remapped target list exists");

    assert_eq!(
        resolved.value,
        vec![TargetPath::property(model_shader_output)]
    );
}

#[test]
fn property_path_query_helpers_cover_field_and_target_lists() {
    let mut store = InMemoryStore::default();

    let labels = store.tokens.intern("labels");
    let input = store.tokens.intern("inputs:surface");
    let token_a = store.tokens.intern("A");
    let token_b = store.tokens.intern("B");
    let prim = store.path("/P");
    let labels_path = store.property_path("/P.labels");
    let input_path = store.property_path("/P.inputs:surface");
    let shader_output = store.property_path("/Q.outputs:surface");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        prim,
        PrimSpec::default()
            .with_field(
                labels,
                FieldValue::TokenListOp(ListOp {
                    explicit: Some(vec![token_a, token_b]),
                    ..ListOp::default()
                }),
            )
            .with_field(
                input,
                FieldValue::PathListOp(ListOp {
                    explicit: Some(vec![TargetPath::property(shader_output)]),
                    ..ListOp::default()
                }),
            ),
    );
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert_eq!(
        stage
            .resolve_token_list_path(labels_path)
            .expect("labels")
            .value,
        vec![token_a, token_b]
    );
    assert_eq!(
        stage
            .resolve_target_list_path(input_path)
            .expect("target list")
            .value,
        vec![TargetPath::property(shader_output)]
    );
}

#[test]
fn layer_property_helpers_author_without_manual_prim_lookup() {
    let mut store = InMemoryStore::default();

    let property = store.property_path("/P.answer");
    let mut layer = Layer::new(LayerId(1));
    layer.set_property(property, 42_i64);
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        stage
            .resolve_field_path(property)
            .expect("property exists")
            .value,
        Value::Int64(42)
    );
}

#[test]
fn layer_typed_property_helper_preserves_sparse_array_defaults() {
    let mut store = InMemoryStore::default();

    let property = store.property_path("/P.indices");
    let mut layer = Layer::new(LayerId(1));
    layer.set_typed_property(
        property,
        Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Resize { len: 3 }],
        }),
        PropertyType::new("int[]", true, Value::Int(0)),
    );
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        stage
            .resolve_property_path(property)
            .expect("property exists")
            .value,
        ResolvedValue::Scalar(Value::Array(vec![
            Value::Int(0),
            Value::Int(0),
            Value::Int(0),
        ]))
    );
}

#[test]
fn reference_opinions_are_weaker_than_local() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let p = store.path("/P");
    let q = store.path("/Q");
    let q_child = store.path("/Q/Child");
    let p_child = store.path("/P/Child");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.insert_prim(
        p,
        PrimSpec::default()
            .with_reference(Reference::new(LayerId(2), q))
            .with_field(field_x, 2_i64),
    );
    store.insert_layer(root_layer);

    let mut ref_layer = Layer::new(LayerId(2));
    ref_layer.insert_prim(q, PrimSpec::default().with_field(field_x, 1_i64));
    ref_layer.insert_prim(q_child, PrimSpec::default());
    store.insert_layer(ref_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(
        stage.has_prim(p_child),
        "reference populates descendant prims"
    );
    let resolved = stage.resolve_field(p, field_x).expect("field exists");
    assert_eq!(resolved.value, Value::Int64(2));
}

#[test]
fn omitted_reference_target_resolves_through_default_prim() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let model = store.tokens.intern("Model");
    let p = store.path("/P");
    let model_path = store.path("/Model");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.insert_prim(
        p,
        PrimSpec::default().with_reference(Reference::to_default_prim(LayerId(2))),
    );
    store.insert_layer(root_layer);

    let mut ref_layer = Layer::new(LayerId(2));
    ref_layer.default_prim = Some(model);
    ref_layer.insert_prim(model_path, PrimSpec::default().with_field(field_x, 7_i64));
    store.insert_layer(ref_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage.resolve_field(p, field_x).expect("field exists");
    assert_eq!(resolved.value, Value::Int64(7));
}

#[test]
fn omitted_payload_target_resolves_through_default_prim() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let model = store.tokens.intern("PayloadRoot");
    let p = store.path("/P");
    let model_path = store.path("/PayloadRoot");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.insert_prim(
        p,
        PrimSpec::default().with_payload(Reference::to_default_prim(LayerId(2))),
    );
    store.insert_layer(root_layer);

    let mut payload_layer = Layer::new(LayerId(2));
    payload_layer.default_prim = Some(model);
    payload_layer.insert_prim(model_path, PrimSpec::default().with_field(field_x, 11_i64));
    store.insert_layer(payload_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage.resolve_field(p, field_x).expect("field exists");
    assert_eq!(resolved.value, Value::Int64(11));
}

#[test]
fn variants_selection_is_strength_ordered() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let prim = store.path("/P");

    let set_v = store.tokens.intern("v");
    let variant_a = store.tokens.intern("A");
    let variant_b = store.tokens.intern("B");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    let mut root_spec = PrimSpec::default();
    root_spec.variant_selections.insert(set_v, variant_a);
    root_layer.insert_prim(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    let mut sub_spec = PrimSpec::default();
    sub_spec.variant_selections.insert(set_v, variant_b);

    let mut set_spec = VariantSetSpec::default();
    set_spec.variants.insert(
        variant_a,
        VariantSpec {
            fields: vec![FieldEntry {
                name: field_x,
                value: Value::Int64(1).into(),
                property_type: None,
            }],
            ..Default::default()
        },
    );

    set_spec.variants.insert(
        variant_b,
        VariantSpec {
            fields: vec![FieldEntry {
                name: field_x,
                value: Value::Int64(2).into(),
                property_type: None,
            }],
            ..Default::default()
        },
    );

    sub_spec.variant_sets.insert(set_v, set_spec);
    sub_layer.insert_prim(prim, sub_spec);
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage.resolve_field(prim, field_x).expect("field exists");
    assert_eq!(resolved.value, Value::Int64(1));
}

#[test]
fn listop_chain_is_applied_strong_to_weak() {
    let mut store = InMemoryStore::default();

    let field_classes = store.tokens.intern("classes");
    let class_a = store.tokens.intern("a");
    let class_b = store.tokens.intern("b");
    let prim = store.path("/P");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    let mut root_spec = PrimSpec::default();
    root_spec.set_field(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            append: vec![class_a],
            ..ListOp::default()
        }),
    );
    root_layer.insert_prim(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    let mut sub_spec = PrimSpec::default();
    sub_spec.set_field(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            append: vec![class_b],
            ..ListOp::default()
        }),
    );
    sub_layer.insert_prim(prim, sub_spec);
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_token_list(prim, field_classes)
        .expect("field exists");
    assert_eq!(resolved.value, vec![class_b, class_a]);
}

#[test]
fn resolve_value_distinguishes_scalar_and_list() {
    let mut store = InMemoryStore::default();

    let prim = store.path("/P");
    let field_x = store.tokens.intern("x");
    let field_classes = store.tokens.intern("classes");
    let class_a = store.tokens.intern("a");

    let mut layer = Layer::new(LayerId(1));
    let mut spec = PrimSpec::default().with_field(field_x, 123_i64);
    spec.set_field(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            append: vec![class_a],
            ..ListOp::default()
        }),
    );
    layer.insert_prim(prim, spec);
    store.insert_layer(layer);

    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_provenance: true,
            ..StageOptions::default()
        },
    );

    let resolved_x = stage.resolve_value(prim, field_x).expect("field exists");
    assert_eq!(resolved_x.value, ResolvedValue::Scalar(Value::Int64(123)));
    assert_eq!(resolved_x.provenance.unwrap().layer, LayerId(1));

    let resolved_classes = stage
        .resolve_value(prim, field_classes)
        .expect("field exists");
    assert_eq!(
        resolved_classes.value,
        ResolvedValue::TokenList(vec![class_a])
    );
    assert_eq!(resolved_classes.provenance.unwrap().layer, LayerId(1));

    assert_eq!(
        stage.resolve_field(prim, field_x).expect("scalar").value,
        Value::Int64(123)
    );
    assert!(stage.resolve_field(prim, field_classes).is_none());
}

#[test]
fn child_order_reorder_name_children_matches_supplemental_fixture() {
    // Matches the supplemental composition fixture `BasicListEditing_root`.
    //
    // This exercises composed child ordering driven by:
    // - authored child insertion order per layer, and
    // - chained `reorder nameChildren = [...]` opinions across the prim stack.
    //
    // Spec: AOUSD Core §11 (stage population) and supplemental suite
    // `primOrder` (`reorder nameChildren`) semantics.
    let mut store = InMemoryStore::default();

    let a = store.tokens.intern("a");
    let b = store.tokens.intern("b");
    let c = store.tokens.intern("c");
    let f = store.tokens.intern("f");
    let x = store.tokens.intern("x");
    let y = store.tokens.intern("y");
    let z = store.tokens.intern("z");

    let prim_a = store.path("/A");
    let a_a = store.path("/A/a");
    let a_b = store.path("/A/b");
    let a_c = store.path("/A/c");
    let a_f = store.path("/A/f");
    let a_x = store.path("/A/x");
    let a_y = store.path("/A/y");
    let a_z = store.path("/A/z");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![
        SublayerEntry::new(LayerId(2)),
        SublayerEntry::new(LayerId(3)),
    ];
    let root_spec = PrimSpec {
        authored_children: vec![f],
        prim_order: Some(vec![z, f, y]),
        ..PrimSpec::default()
    };
    root_layer.insert_prim(prim_a, root_spec);
    root_layer.insert_prim(a_f, PrimSpec::default());
    store.insert_layer(root_layer);

    let mut sub1_layer = Layer::new(LayerId(2));
    let sub1_spec = PrimSpec {
        authored_children: vec![a, b, c],
        prim_order: Some(vec![z, x, b]),
        ..PrimSpec::default()
    };
    sub1_layer.insert_prim(prim_a, sub1_spec);
    sub1_layer.insert_prim(a_a, PrimSpec::default());
    sub1_layer.insert_prim(a_b, PrimSpec::default());
    sub1_layer.insert_prim(a_c, PrimSpec::default());
    store.insert_layer(sub1_layer);

    let mut sub2_layer = Layer::new(LayerId(3));
    let sub2_spec = PrimSpec {
        authored_children: vec![x, y, z],
        ..PrimSpec::default()
    };
    sub2_layer.insert_prim(prim_a, sub2_spec);
    sub2_layer.insert_prim(a_x, PrimSpec::default());
    sub2_layer.insert_prim(a_y, PrimSpec::default());
    sub2_layer.insert_prim(a_z, PrimSpec::default());
    store.insert_layer(sub2_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let actual = stage.children_of(prim_a).expect("children list");
    assert_eq!(actual, &[a_z, a_a, a_x, a_f, a_y, a_b, a_c]);
}

#[test]
fn explain_field_returns_sorted_opinion_stack() {
    let mut store = InMemoryStore::default();

    let prim = store.path("/P");
    let field_x = store.tokens.intern("x");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    root_layer.insert_prim(prim, PrimSpec::default().with_field(field_x, 1_i64));
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(prim, PrimSpec::default().with_field(field_x, 2_i64));
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let stack = stage.explain_field(prim, field_x).expect("opinions exist");
    assert_eq!(stack.len(), 2);

    // Strongest-first means the root layer's opinion comes first.
    assert_eq!(stack[0].key.layer_id, LayerId(1));
    assert_eq!(stack[1].key.layer_id, LayerId(2));

    assert_eq!(
        stage.resolve_field(prim, field_x).expect("scalar").value,
        Value::Int64(1)
    );
}

#[test]
fn token_listop_append_reorders_duplicates() {
    let mut store = InMemoryStore::default();

    let prim = store.path("/P");
    let field_classes = store.tokens.intern("classes");
    let class_a = store.tokens.intern("a");
    let class_b = store.tokens.intern("b");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    let mut root_spec = PrimSpec::default();
    root_spec.set_field(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            append: vec![class_a],
            ..ListOp::default()
        }),
    );
    root_layer.insert_prim(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    let mut sub_spec = PrimSpec::default();
    sub_spec.set_field(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            explicit: Some(vec![class_a, class_b]),
            ..ListOp::default()
        }),
    );
    sub_layer.insert_prim(prim, sub_spec);
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_token_list(prim, field_classes)
        .expect("field exists");
    assert_eq!(resolved.value, vec![class_b, class_a]);
}

#[test]
fn token_listop_prepend_and_append_match_supplemental_list_editing_order() {
    // Inspired by the supplemental composition test `BasicListEditing_root`, but
    // expressed using our current token-list `ListOp` support.
    let mut store = InMemoryStore::default();

    let prim = store.path("/A");
    let field_targets = store.tokens.intern("targets");

    let root_prepend = store.tokens.intern("root_prepend");
    let sub1_prepend = store.tokens.intern("sub1_prepend");
    let sub2_prepend = store.tokens.intern("sub2_prepend");
    let sub2_append = store.tokens.intern("sub2_append");
    let sub1_append = store.tokens.intern("sub1_append");
    let root_append = store.tokens.intern("root_append");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![
        SublayerEntry::new(LayerId(2)),
        SublayerEntry::new(LayerId(3)),
    ];
    let mut root_spec = PrimSpec::default();
    root_spec.set_field(
        field_targets,
        FieldValue::TokenListOp(ListOp {
            prepend: vec![root_prepend],
            append: vec![root_append],
            ..ListOp::default()
        }),
    );
    root_layer.insert_prim(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub1_layer = Layer::new(LayerId(2));
    let mut sub1_spec = PrimSpec::default();
    sub1_spec.set_field(
        field_targets,
        FieldValue::TokenListOp(ListOp {
            prepend: vec![sub1_prepend],
            append: vec![sub1_append],
            ..ListOp::default()
        }),
    );
    sub1_layer.insert_prim(prim, sub1_spec);
    store.insert_layer(sub1_layer);

    let mut sub2_layer = Layer::new(LayerId(3));
    let mut sub2_spec = PrimSpec::default();
    sub2_spec.set_field(
        field_targets,
        FieldValue::TokenListOp(ListOp {
            prepend: vec![sub2_prepend],
            append: vec![sub2_append],
            ..ListOp::default()
        }),
    );
    sub2_layer.insert_prim(prim, sub2_spec);
    store.insert_layer(sub2_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_token_list(prim, field_targets)
        .expect("field exists");
    assert_eq!(
        resolved.value,
        vec![
            root_prepend,
            sub1_prepend,
            sub2_prepend,
            sub2_append,
            sub1_append,
            root_append
        ]
    );
}

#[test]
fn token_listop_prepend_composes_before_explicit() {
    // Simulates: sublayer has `apiSchemas = ["Original"]` (explicit),
    // root has `prepend apiSchemas = ["Prepended"]`.
    // Composed result: ["Prepended", "Original"].
    let mut store = InMemoryStore::default();

    let prim = store.path("/Card");
    let api_schemas = store.tokens.intern("apiSchemas");
    let original = store.tokens.intern("OriginalAPI");
    let prepended = store.tokens.intern("PrependedAPI");

    // Sublayer (weaker): explicit apiSchemas = ["OriginalAPI"].
    let mut sub_layer = Layer::new(LayerId(2));
    let mut sub_spec = PrimSpec::default();
    sub_spec.set_field(
        api_schemas,
        FieldValue::TokenListOp(ListOp {
            explicit: Some(vec![original]),
            ..ListOp::default()
        }),
    );
    sub_layer.insert_prim(prim, sub_spec);
    store.insert_layer(sub_layer);

    // Root layer (stronger): prepend apiSchemas = ["PrependedAPI"].
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    let mut root_spec = PrimSpec::default();
    root_spec.set_field(
        api_schemas,
        FieldValue::TokenListOp(ListOp {
            prepend: vec![prepended],
            ..ListOp::default()
        }),
    );
    root_layer.insert_prim(prim, root_spec);
    store.insert_layer(root_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_token_list(prim, api_schemas)
        .expect("apiSchemas field");
    assert_eq!(
        resolved.value,
        vec![prepended, original],
        "prepend should appear before the explicit list"
    );
}

#[test]
fn specifier_resolution_follows_strongest_defining() {
    use layerstack::Specifier;

    let mut store = InMemoryStore::default();
    let prim = store.path("/P");

    // Layer 1 (strongest): over — non-defining
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![
        SublayerEntry::new(LayerId(2)),
        SublayerEntry::new(LayerId(3)),
    ];
    root_layer.insert_prim(prim, PrimSpec::over());
    store.insert_layer(root_layer);

    // Layer 2: def — strongest defining opinion
    let mut sub1 = Layer::new(LayerId(2));
    sub1.insert_prim(prim, PrimSpec::def());
    store.insert_layer(sub1);

    // Layer 3: class — weaker defining opinion
    let mut sub2 = Layer::new(LayerId(3));
    sub2.insert_prim(prim, PrimSpec::class());
    store.insert_layer(sub2);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // Strongest defining opinion is def from layer 2.
    assert_eq!(stage.resolve_specifier(prim, &store), Some(Specifier::Def));
    assert!(stage.is_defined(prim, &store));
    assert!(!stage.is_abstract(prim, &store));
}

#[test]
fn specifier_all_over_is_undefining() {
    use layerstack::Specifier;

    let mut store = InMemoryStore::default();
    let prim = store.path("/P");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    root_layer.insert_prim(prim, PrimSpec::over());
    store.insert_layer(root_layer);

    let mut sub = Layer::new(LayerId(2));
    sub.insert_prim(prim, PrimSpec::over());
    store.insert_layer(sub);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert_eq!(stage.resolve_specifier(prim, &store), Some(Specifier::Over));
    assert!(!stage.is_defined(prim, &store));
}

#[test]
fn specifier_class_is_abstract() {
    use layerstack::Specifier;

    let mut store = InMemoryStore::default();
    let prim = store.path("/P");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.insert_prim(prim, PrimSpec::class());
    store.insert_layer(root_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert_eq!(
        stage.resolve_specifier(prim, &store),
        Some(Specifier::Class)
    );
    assert!(stage.is_defined(prim, &store));
    assert!(stage.is_abstract(prim, &store));
}

#[test]
fn value_blocked_suppresses_weaker_opinions() {
    // Spec: AOUSD Core §12.3 — a `Blocked` value suppresses all weaker opinions.
    let mut store = InMemoryStore::default();

    let prim = store.path("/P");
    let field_x = store.tokens.intern("x");

    // Strongest layer blocks the field.
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    root_layer.insert_prim(
        prim,
        PrimSpec::default().with_field(field_x, Value::Blocked),
    );
    store.insert_layer(root_layer);

    // Weaker layer provides a real value.
    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(prim, PrimSpec::default().with_field(field_x, 42_i64));
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // Blocked suppresses weaker opinions — resolve returns None.
    assert!(stage.resolve_field(prim, field_x).is_none());
    assert!(stage.resolve_value(prim, field_x).is_none());
}

#[test]
fn value_blocked_only_affects_blocked_field() {
    // Blocking one field should not affect other fields on the same prim.
    let mut store = InMemoryStore::default();

    let prim = store.path("/P");
    let field_x = store.tokens.intern("x");
    let field_y = store.tokens.intern("y");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        prim,
        PrimSpec::default()
            .with_field(field_x, Value::Blocked)
            .with_field(field_y, 99_i64),
    );
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(stage.resolve_field(prim, field_x).is_none());
    assert_eq!(
        stage.resolve_field(prim, field_y).expect("y exists").value,
        Value::Int64(99)
    );
}

#[test]
fn time_samples_held_interpolation() {
    use layerstack::InterpolationType;

    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("x");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    let mut spec = PrimSpec::default();
    spec.set_field(
        field,
        FieldValue::TimeSamples(vec![
            (1.0, Value::Double(10.0)),
            (3.0, Value::Double(30.0)),
            (5.0, Value::Double(50.0)),
        ]),
    );
    layer.insert_prim(p, spec);
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // Exact samples.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 1.0, InterpolationType::Held)
            .unwrap()
            .value,
        Value::Double(10.0)
    );
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 3.0, InterpolationType::Held)
            .unwrap()
            .value,
        Value::Double(30.0)
    );

    // Between samples: held returns earlier value.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 2.0, InterpolationType::Held)
            .unwrap()
            .value,
        Value::Double(10.0)
    );
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 4.0, InterpolationType::Held)
            .unwrap()
            .value,
        Value::Double(30.0)
    );

    // Before first sample: return first value.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 0.0, InterpolationType::Held)
            .unwrap()
            .value,
        Value::Double(10.0)
    );

    // After last sample: return last value.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 100.0, InterpolationType::Held)
            .unwrap()
            .value,
        Value::Double(50.0)
    );
}

#[test]
fn time_samples_linear_interpolation() {
    use layerstack::InterpolationType;

    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("x");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    let mut spec = PrimSpec::default();
    spec.set_field(
        field,
        FieldValue::TimeSamples(vec![
            (0.0, Value::Double(0.0)),
            (10.0, Value::Double(100.0)),
        ]),
    );
    layer.insert_prim(p, spec);
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // Midpoint: linear interpolation.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 5.0, InterpolationType::Linear)
            .unwrap()
            .value,
        Value::Double(50.0)
    );

    // Quarter point.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 2.5, InterpolationType::Linear)
            .unwrap()
            .value,
        Value::Double(25.0)
    );

    // Exact sample.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 0.0, InterpolationType::Linear)
            .unwrap()
            .value,
        Value::Double(0.0)
    );

    // Beyond range: clamp.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, -1.0, InterpolationType::Linear)
            .unwrap()
            .value,
        Value::Double(0.0)
    );
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 20.0, InterpolationType::Linear)
            .unwrap()
            .value,
        Value::Double(100.0)
    );
}

#[test]
fn time_samples_linear_int_interpolation() {
    use layerstack::InterpolationType;

    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("x");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    let mut spec = PrimSpec::default();
    spec.set_field(
        field,
        FieldValue::TimeSamples(vec![(0.0, Value::Int64(0)), (10.0, Value::Int64(100))]),
    );
    layer.insert_prim(p, spec);
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // Midpoint: linear interpolation, rounded to nearest int.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 5.0, InterpolationType::Linear)
            .unwrap()
            .value,
        Value::Int64(50)
    );
}

#[test]
fn time_samples_non_numeric_falls_back_to_held() {
    use layerstack::InterpolationType;

    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("name");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    let mut spec = PrimSpec::default();
    spec.set_field(
        field,
        FieldValue::TimeSamples(vec![
            (1.0, Value::string("hello")),
            (5.0, Value::string("world")),
        ]),
    );
    layer.insert_prim(p, spec);
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // Linear on non-numeric falls back to held (earlier value).
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 3.0, InterpolationType::Linear)
            .unwrap()
            .value,
        Value::string("hello")
    );
}

#[test]
fn time_samples_override_default_value() {
    use layerstack::InterpolationType;

    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("x");
    let p = store.path("/P");

    // Root layer: timeSamples.
    let mut root = Layer::new(LayerId(1));
    root.sublayers = vec![SublayerEntry::new(LayerId(2))];
    let mut root_spec = PrimSpec::default();
    root_spec.set_field(
        field,
        FieldValue::TimeSamples(vec![(1.0, Value::Double(10.0))]),
    );
    root.insert_prim(p, root_spec);
    store.insert_layer(root);

    // Sublayer: default value.
    let mut sub = Layer::new(LayerId(2));
    sub.insert_prim(p, PrimSpec::default().with_field(field, 999.0));
    store.insert_layer(sub);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // TimeSamples from stronger layer takes priority.
    assert_eq!(
        stage
            .resolve_value_at_time(p, field, 1.0, InterpolationType::Held)
            .unwrap()
            .value,
        Value::Double(10.0)
    );

    // Default resolve (no time) returns the stronger timeSamples, which yields None
    // since we don't have a time context. But resolve_value checks the *first* opinion
    // which is TimeSamples, so it returns None. The weaker default is not reached.
    // For the default value, the user should use resolve_value_at_time.
    assert!(stage.resolve_value(p, field).is_none());
}

// ── Dependency map integration tests ───────────────────────────────────────

#[test]
fn dependency_map_none_by_default() {
    let mut store = InMemoryStore::default();
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(p, PrimSpec::default());
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert!(
        !stage.has_dependencies(),
        "dependencies should be disabled by default"
    );
}

#[test]
fn dependency_map_records_local_layer_opinions() {
    let mut store = InMemoryStore::default();
    let field_x = store.tokens.intern("x");
    let p = store.path("/P");

    let mut root = Layer::new(LayerId(1));
    root.sublayers = vec![SublayerEntry::new(LayerId(2))];
    root.insert_prim(p, PrimSpec::default().with_field(field_x, 1_i64));
    store.insert_layer(root);

    let mut sub = Layer::new(LayerId(2));
    sub.insert_prim(p, PrimSpec::default().with_field(field_x, 2_i64));
    store.insert_layer(sub);

    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_dependencies: true,
            ..StageOptions::default()
        },
    );

    assert!(stage.has_dependencies());

    // Both layers should be recorded as affecting prim P.
    let layers = stage.layers_affecting_prim(p);
    assert!(layers.contains(&LayerId(1)));
    assert!(layers.contains(&LayerId(2)));

    // P should appear in prims affected by both layers.
    assert!(stage.prims_affected_by_layer(LayerId(1)).contains(&p));
    assert!(stage.prims_affected_by_layer(LayerId(2)).contains(&p));

    // No arc dependencies for local-only composition.
    assert!(stage.arc_dependencies().is_empty());
}

#[test]
fn dependency_map_records_reference_arc_and_layer() {
    let mut store = InMemoryStore::default();
    let field_x = store.tokens.intern("x");
    let p = store.path("/P");
    let q = store.path("/Q");

    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        p,
        PrimSpec::default().with_reference(Reference::new(LayerId(2), q)),
    );
    store.insert_layer(root);

    let mut ref_layer = Layer::new(LayerId(2));
    ref_layer.insert_prim(q, PrimSpec::default().with_field(field_x, 42_i64));
    store.insert_layer(ref_layer);

    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_dependencies: true,
            ..StageOptions::default()
        },
    );

    assert!(stage.has_dependencies());

    // Should have a reference arc from Q → P.
    let arcs = stage.arcs_targeting(p);
    assert!(!arcs.is_empty(), "reference arc should target P");
    let ref_arc = arcs
        .iter()
        .find(|a| a.arc_kind == ArcKind::References)
        .expect("reference arc exists");
    assert_eq!(ref_arc.source, q);
    assert_eq!(ref_arc.layer, LayerId(2));

    // LayerId(2) should affect P through the reference.
    assert!(stage.layers_affecting_prim(p).contains(&LayerId(2)));
}

#[test]
fn dependency_map_records_inherit_arc() {
    let mut store = InMemoryStore::default();
    let field_x = store.tokens.intern("x");
    let p = store.path("/P");
    let base = store.path("/Base");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(p, PrimSpec::default().with_inherit(base));
    layer.insert_prim(base, PrimSpec::default().with_field(field_x, 10_i64));
    store.insert_layer(layer);

    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_dependencies: true,
            ..StageOptions::default()
        },
    );

    assert!(stage.has_dependencies());

    let arcs = stage.arcs_targeting(p);
    let inherits_arc = arcs
        .iter()
        .find(|a| a.arc_kind == ArcKind::Inherits)
        .expect("inherit arc exists");
    assert_eq!(inherits_arc.source, base);
}

#[test]
fn dependency_map_records_specializes_arc() {
    let mut store = InMemoryStore::default();
    let field_x = store.tokens.intern("x");
    let p = store.path("/P");
    let base = store.path("/Base");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(p, PrimSpec::default().with_specialize(base));
    layer.insert_prim(base, PrimSpec::default().with_field(field_x, 10_i64));
    store.insert_layer(layer);

    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            with_dependencies: true,
            ..StageOptions::default()
        },
    );

    assert!(stage.has_dependencies());

    let arcs = stage.arcs_targeting(p);
    let spec_arc = arcs
        .iter()
        .find(|a| a.arc_kind == ArcKind::Specializes)
        .expect("specializes arc exists");
    assert_eq!(spec_arc.source, base);
}

/// Deactivated prims (`active = false`) are excluded from the stage.
///
/// Spec: AOUSD Core §7.6 (active metadata), §11 (stage population).
#[test]
fn deactivated_prim_excluded_from_stage() {
    let mut store = InMemoryStore::default();

    let root = store.path("/");
    let p = store.path("/Active");
    let q = store.path("/Inactive");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        root,
        PrimSpec::default().with_children(vec![
            store.tokens.intern("Active"),
            store.tokens.intern("Inactive"),
        ]),
    );
    layer.insert_prim(p, PrimSpec::def());
    layer.insert_prim(q, PrimSpec::def().with_active(false));
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(stage.has_prim(p), "active prim should be present");
    assert!(!stage.has_prim(q), "deactivated prim should be excluded");
}

/// Descendants of a deactivated prim are also excluded.
///
/// Spec: AOUSD Core §7.6 (active metadata), §11 (stage population).
#[test]
fn deactivated_prim_descendants_excluded() {
    let mut store = InMemoryStore::default();

    let root = store.path("/");
    let parent = store.path("/Parent");
    let child = store.path("/Parent/Child");
    let grandchild = store.path("/Parent/Child/Grandchild");

    let parent_tok = store.tokens.intern("Parent");
    let child_tok = store.tokens.intern("Child");
    let gc_tok = store.tokens.intern("Grandchild");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(root, PrimSpec::default().with_children(vec![parent_tok]));
    layer.insert_prim(
        parent,
        PrimSpec::def()
            .with_active(false)
            .with_children(vec![child_tok]),
    );
    layer.insert_prim(child, PrimSpec::def().with_children(vec![gc_tok]));
    layer.insert_prim(grandchild, PrimSpec::def());
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(!stage.has_prim(parent), "deactivated parent excluded");
    assert!(
        !stage.has_prim(child),
        "child of deactivated parent excluded"
    );
    assert!(
        !stage.has_prim(grandchild),
        "grandchild of deactivated parent excluded"
    );
}

/// Explicitly active prims (`active = true`) remain in the stage.
///
/// Spec: AOUSD Core §7.6 (active metadata).
#[test]
fn explicitly_active_prim_remains() {
    let mut store = InMemoryStore::default();

    let root = store.path("/");
    let p = store.path("/P");
    let p_tok = store.tokens.intern("P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(root, PrimSpec::default().with_children(vec![p_tok]));
    layer.insert_prim(p, PrimSpec::def().with_active(true));
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(stage.has_prim(p), "explicitly active prim should remain");
}

/// Strongest `active` opinion wins across layers.
///
/// Spec: AOUSD Core §7.6 (active metadata), §9 (LIVERPS strength ordering).
#[test]
fn active_strongest_opinion_wins() {
    let mut store = InMemoryStore::default();

    let root = store.path("/");
    let p = store.path("/P");
    let p_tok = store.tokens.intern("P");
    let field_x = store.tokens.intern("x");

    // Root layer: active = true (stronger).
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    root_layer.insert_prim(root, PrimSpec::default().with_children(vec![p_tok]));
    root_layer.insert_prim(
        p,
        PrimSpec::def().with_active(true).with_field(field_x, 1_i64),
    );
    store.insert_layer(root_layer);

    // Sublayer: active = false (weaker).
    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(p, PrimSpec::def().with_active(false));
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(
        stage.has_prim(p),
        "stronger active=true should override weaker active=false"
    );
    let resolved = stage.resolve_field(p, field_x).expect("field exists");
    assert_eq!(resolved.value, Value::Int64(1));
}

/// When a weaker layer says `active = false` with no stronger override, the
/// prim is deactivated.
///
/// Spec: AOUSD Core §7.6 (active metadata), §9 (LIVERPS strength ordering).
#[test]
fn weaker_active_false_deactivates() {
    let mut store = InMemoryStore::default();

    let root = store.path("/");
    let p = store.path("/P");
    let p_tok = store.tokens.intern("P");

    // Root layer: no active opinion.
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    root_layer.insert_prim(root, PrimSpec::default().with_children(vec![p_tok]));
    root_layer.insert_prim(p, PrimSpec::def());
    store.insert_layer(root_layer);

    // Sublayer: active = false.
    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(p, PrimSpec::def().with_active(false));
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(
        !stage.has_prim(p),
        "only opinion is active=false; prim should be excluded"
    );
}

/// Type name is resolved from the strongest opinion.
///
/// Spec: AOUSD Core §7.6 (typeName field), §12.2.3 (type name resolution).
#[test]
fn type_name_resolved_from_strongest() {
    let mut store = InMemoryStore::default();

    let root = store.path("/");
    let p = store.path("/P");
    let p_tok = store.tokens.intern("P");
    let xform_tok = store.tokens.intern("Xform");
    let scope_tok = store.tokens.intern("Scope");

    // Root layer: type = Xform (stronger).
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![SublayerEntry::new(LayerId(2))];
    root_layer.insert_prim(root, PrimSpec::default().with_children(vec![p_tok]));
    root_layer.insert_prim(p, PrimSpec::def().with_type_name(xform_tok));
    store.insert_layer(root_layer);

    // Sublayer: type = Scope (weaker).
    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(p, PrimSpec::def().with_type_name(scope_tok));
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let resolved = stage.resolve_type_name(p, &store);
    assert_eq!(resolved, Some(xform_tok));
}

/// An `over` without a type name does not contribute; the reference target's
/// type name is used instead.
///
/// Spec: AOUSD Core §7.6 (typeName field), §12.2.3 (type name resolution).
#[test]
fn type_name_from_reference_when_local_untyped() {
    let mut store = InMemoryStore::default();

    let root = store.path("/");
    let p = store.path("/P");
    let q = store.path("/Q");
    let p_tok = store.tokens.intern("P");
    let mesh_tok = store.tokens.intern("Mesh");

    // Root layer: /P is an untyped over with a reference to /Q in layer 2.
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.insert_prim(root, PrimSpec::default().with_children(vec![p_tok]));
    root_layer.insert_prim(
        p,
        PrimSpec::over().with_reference(Reference::new(LayerId(2), q)),
    );
    store.insert_layer(root_layer);

    // Layer 2: /Q is a typed Mesh.
    let mut ref_layer = Layer::new(LayerId(2));
    ref_layer.insert_prim(q, PrimSpec::def().with_type_name(mesh_tok));
    store.insert_layer(ref_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let resolved = stage.resolve_type_name(p, &store);
    assert_eq!(resolved, Some(mesh_tok));
}

/// A prim with no type name anywhere in its opinion stack returns `None`.
///
/// Spec: AOUSD Core §7.6 (typeName field).
#[test]
fn type_name_none_when_untyped() {
    let mut store = InMemoryStore::default();

    let root = store.path("/");
    let p = store.path("/P");
    let p_tok = store.tokens.intern("P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(root, PrimSpec::default().with_children(vec![p_tok]));
    layer.insert_prim(p, PrimSpec::def());
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert_eq!(stage.resolve_type_name(p, &store), None);
}

// --- Schema fallback integration tests ---

#[test]
fn schema_fallback_provides_value_when_no_opinion() {
    // When no authored opinion exists for a field that has a schema fallback,
    // resolve_field_with_schema returns the fallback.
    // Spec: AOUSD Core §13.3.2.4 (fallback value resolution).
    let mut store = InMemoryStore::default();

    let mesh_tok = store.tokens.intern("Mesh");
    let extent_tok = store.tokens.intern("extent");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(p, PrimSpec::def().with_type_name(mesh_tok));
    store.insert_layer(layer);

    let mut registry = SchemaRegistry::new();
    registry
        .register(SchemaDefinition::typed(mesh_tok).with_property(extent_tok, Value::Double(0.0)));

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // No authored opinion on "extent" → fallback from schema.
    assert_eq!(stage.resolve_field(p, extent_tok), None);
    let resolved = stage
        .resolve_field_with_schema(p, extent_tok, &store, &registry, None)
        .expect("schema fallback");
    assert_eq!(resolved.value, Value::Double(0.0));
}

#[test]
fn authored_opinion_beats_schema_fallback() {
    // Authored opinions are always stronger than schema fallback.
    // Spec: AOUSD Core §13.3.2.4.
    let mut store = InMemoryStore::default();

    let mesh_tok = store.tokens.intern("Mesh");
    let double_sided_tok = store.tokens.intern("doubleSided");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        p,
        PrimSpec::def()
            .with_type_name(mesh_tok)
            .with_field(double_sided_tok, Value::Bool(true)),
    );
    store.insert_layer(layer);

    let mut registry = SchemaRegistry::new();
    registry.register(
        SchemaDefinition::typed(mesh_tok).with_property(double_sided_tok, Value::Bool(false)),
    );

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // Authored value wins over schema fallback.
    let resolved = stage
        .resolve_field_with_schema(p, double_sided_tok, &store, &registry, None)
        .expect("resolved");
    assert_eq!(resolved.value, Value::Bool(true));
}

#[test]
fn schema_isa_inheritance_fallback() {
    // IsA inheritance: Mesh inherits from Gprim. A field defined on Gprim
    // but not on Mesh should still be available as a fallback.
    // Spec: AOUSD Core §13.3.1 (typed schema inheritance).
    let mut store = InMemoryStore::default();

    let mesh_tok = store.tokens.intern("Mesh");
    let gprim_tok = store.tokens.intern("Gprim");
    let visibility_tok = store.tokens.intern("visibility");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(p, PrimSpec::def().with_type_name(mesh_tok));
    store.insert_layer(layer);

    let mut registry = SchemaRegistry::new();
    registry.register(
        SchemaDefinition::typed(gprim_tok).with_property(visibility_tok, Value::from("inherited")),
    );
    registry.register(SchemaDefinition::typed(mesh_tok).with_parent(gprim_tok));

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let resolved = stage
        .resolve_field_with_schema(p, visibility_tok, &store, &registry, None)
        .expect("inherited fallback");
    assert_eq!(resolved.value, Value::from("inherited"));
}

#[test]
fn schema_applied_api_provides_fallback() {
    // Applied API schemas provide fallback values for their properties.
    // Spec: AOUSD Core §13.3.2 (applied schemas), §13.2.1 (apiSchemas field).
    let mut store = InMemoryStore::default();

    let mesh_tok = store.tokens.intern("Mesh");
    let collection_api_tok = store.tokens.intern("CollectionAPI");
    let includes_tok = store.tokens.intern("includes");
    let api_schemas_tok = store.tokens.intern("apiSchemas");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    // Prim has typeName=Mesh and apiSchemas=[CollectionAPI].
    layer.insert_prim(
        p,
        PrimSpec::def().with_type_name(mesh_tok).with_field(
            api_schemas_tok,
            FieldValue::TokenListOp(ListOp {
                append: vec![collection_api_tok],
                ..ListOp::default()
            }),
        ),
    );
    store.insert_layer(layer);

    let mut registry = SchemaRegistry::new();
    registry.register(SchemaDefinition::typed(mesh_tok));
    registry.register(
        SchemaDefinition::api(collection_api_tok).with_property(includes_tok, Value::Null),
    );

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    // Without apiSchemas token → only typed schema is consulted, no fallback.
    assert!(
        stage
            .resolve_field_with_schema(p, includes_tok, &store, &registry, None)
            .is_none()
    );

    // With apiSchemas token → applied API schema fallback is found.
    let resolved = stage
        .resolve_field_with_schema(p, includes_tok, &store, &registry, Some(api_schemas_tok))
        .expect("api schema fallback");
    assert_eq!(resolved.value, Value::Null);
}

#[test]
fn schema_no_type_no_fallback() {
    // A prim with no typeName gets no schema fallback.
    let mut store = InMemoryStore::default();

    let extent_tok = store.tokens.intern("extent");
    let mesh_tok = store.tokens.intern("Mesh");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(p, PrimSpec::def());
    store.insert_layer(layer);

    let mut registry = SchemaRegistry::new();
    registry
        .register(SchemaDefinition::typed(mesh_tok).with_property(extent_tok, Value::Double(0.0)));

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(
        stage
            .resolve_field_with_schema(p, extent_tok, &store, &registry, None)
            .is_none()
    );
}

#[test]
fn schema_builtin_api_fallback() {
    // Built-in API schemas on a typed schema provide fallback values
    // even without explicit apiSchemas authoring.
    // Spec: AOUSD Core §13.3.2.1 (schema inclusions — built-ins).
    let mut store = InMemoryStore::default();

    let mesh_tok = store.tokens.intern("Mesh");
    let some_api_tok = store.tokens.intern("SomeAPI");
    let api_field_tok = store.tokens.intern("apiField");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(p, PrimSpec::def().with_type_name(mesh_tok));
    store.insert_layer(layer);

    let mut registry = SchemaRegistry::new();
    registry
        .register(SchemaDefinition::api(some_api_tok).with_property(api_field_tok, Value::Int(42)));
    registry.register(SchemaDefinition::typed(mesh_tok).with_built_in_api(some_api_tok));

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let resolved = stage
        .resolve_field_with_schema(p, api_field_tok, &store, &registry, None)
        .expect("built-in api fallback");
    assert_eq!(resolved.value, Value::Int(42));
}

// ── Dictionary combining ──────────────────────────────────────────────

fn dict_entry(key: &str, val: Value) -> (Arc<str>, Value) {
    (Arc::from(key), val)
}

#[test]
fn dictionary_combining_across_sublayers() {
    // Spec: AOUSD Core §6.6.2.1, §12.2.5 — dictionaries combine across opinions.
    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("customData");
    let p = store.path("/P");

    // Root (stronger): {a: 1}
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers.push(SublayerEntry::new(LayerId(2)));
    root_layer.insert_prim(
        p,
        PrimSpec::def().with_field(
            field,
            Value::Dictionary(vec![dict_entry("a", Value::Int(1))]),
        ),
    );
    store.insert_layer(root_layer);

    // Sublayer (weaker): {a: 99, b: 2}
    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(
        p,
        PrimSpec::default().with_field(
            field,
            Value::Dictionary(vec![
                dict_entry("a", Value::Int(99)),
                dict_entry("b", Value::Int(2)),
            ]),
        ),
    );
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_dictionary(p, field)
        .expect("dictionary resolved");
    // a=1 (stronger wins), b=2 (weaker-only preserved).
    assert_eq!(
        resolved.value,
        vec![
            dict_entry("a", Value::Int(1)),
            dict_entry("b", Value::Int(2))
        ]
    );
}

#[test]
fn dictionary_nested_combining() {
    // Spec: §6.6.2.1 — nested dictionaries combine recursively.
    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("customData");
    let p = store.path("/P");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers.push(SublayerEntry::new(LayerId(2)));
    root_layer.insert_prim(
        p,
        PrimSpec::def().with_field(
            field,
            Value::Dictionary(vec![dict_entry(
                "sub",
                Value::Dictionary(vec![dict_entry("x", Value::Int(10))]),
            )]),
        ),
    );
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(
        p,
        PrimSpec::default().with_field(
            field,
            Value::Dictionary(vec![dict_entry(
                "sub",
                Value::Dictionary(vec![
                    dict_entry("x", Value::Int(99)),
                    dict_entry("y", Value::Int(20)),
                ]),
            )]),
        ),
    );
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_dictionary(p, field)
        .expect("nested dict resolved");
    // sub.x=10 (stronger), sub.y=20 (weaker-only).
    assert_eq!(
        resolved.value,
        vec![dict_entry(
            "sub",
            Value::Dictionary(vec![
                dict_entry("x", Value::Int(10)),
                dict_entry("y", Value::Int(20)),
            ])
        )]
    );
}

#[test]
fn dictionary_combining_across_reference_arc() {
    // Spec: §6.6.2.1, §10 — dictionaries combine across reference arcs.
    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("customData");
    let p = store.path("/P");
    let q = store.path("/Q");

    // Root: /P references /Q in layer 2, with local dictionary {a: 1}.
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.insert_prim(
        p,
        PrimSpec::def()
            .with_field(
                field,
                Value::Dictionary(vec![dict_entry("a", Value::Int(1))]),
            )
            .with_reference(Reference::new(LayerId(2), q)),
    );
    store.insert_layer(root_layer);

    // Reference target layer: /Q with dictionary {a: 99, b: 2}.
    let mut ref_layer = Layer::new(LayerId(2));
    ref_layer.insert_prim(
        q,
        PrimSpec::def().with_field(
            field,
            Value::Dictionary(vec![
                dict_entry("a", Value::Int(99)),
                dict_entry("b", Value::Int(2)),
            ]),
        ),
    );
    store.insert_layer(ref_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage
        .resolve_dictionary(p, field)
        .expect("dict across reference");
    assert_eq!(
        resolved.value,
        vec![
            dict_entry("a", Value::Int(1)),
            dict_entry("b", Value::Int(2))
        ]
    );
}

#[test]
fn dictionary_blocked_value_suppresses() {
    // Spec: §12.3 — a blocked value suppresses weaker dictionary opinions.
    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("customData");
    let p = store.path("/P");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers.push(SublayerEntry::new(LayerId(2)));
    root_layer.insert_prim(p, PrimSpec::def().with_field(field, Value::Blocked));
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(
        p,
        PrimSpec::default().with_field(
            field,
            Value::Dictionary(vec![dict_entry("a", Value::Int(1))]),
        ),
    );
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    // Blocked suppresses — resolve_value should return None, resolve_dictionary too.
    let resolved = stage.resolve_dictionary(p, field);
    assert!(resolved.is_none(), "blocked should suppress dictionary");
}

#[test]
fn resolve_value_returns_dictionary_variant() {
    // Verify that resolve_value returns ResolvedValue::Dictionary for dict fields.
    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("customData");
    let p = store.path("/P");

    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        p,
        PrimSpec::def().with_field(
            field,
            Value::Dictionary(vec![dict_entry("x", Value::Double(1.5))]),
        ),
    );
    store.insert_layer(layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage.resolve_value(p, field).expect("dictionary resolves");
    match resolved.value {
        ResolvedValue::Dictionary(d) => {
            assert_eq!(d, vec![dict_entry("x", Value::Double(1.5))]);
        }
        other => panic!("expected ResolvedValue::Dictionary, got {:?}", other),
    }
}

// ── Array value resolution ────────────────────────────────────────────

#[test]
fn array_value_strongest_wins() {
    // Array values resolve with strongest-wins like other scalars.
    let mut store = InMemoryStore::default();
    let field = store.tokens.intern("points");
    let p = store.path("/P");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers.push(SublayerEntry::new(LayerId(2)));
    root_layer.insert_prim(
        p,
        PrimSpec::def().with_field(
            field,
            Value::Array(vec![Value::Float(1.0), Value::Float(2.0)]),
        ),
    );
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    sub_layer.insert_prim(
        p,
        PrimSpec::default().with_field(
            field,
            Value::Array(vec![
                Value::Float(9.0),
                Value::Float(8.0),
                Value::Float(7.0),
            ]),
        ),
    );
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage.resolve_field(p, field).expect("array resolves");
    // Strongest wins — root's 2-element array, not sublayer's 3-element array.
    assert_eq!(
        resolved.value,
        Value::Array(vec![Value::Float(1.0), Value::Float(2.0)])
    );
}
