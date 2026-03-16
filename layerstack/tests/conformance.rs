#![allow(missing_docs, reason = "integration tests")]

use layerstack::HashMap;

use layerstack::{
    ArcKind, FieldValue, Layer, LayerId, ListOp, PrimSpec, Reference, ResolvedValue, Stage,
    StageOptions, Value, VariantSetSpec, VariantSpec, doc::InMemoryStore,
};

#[test]
fn sublayer_strength_local_beats_sublayer() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let p = store.path("/P");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![LayerId(2)];
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
fn variants_selection_is_strength_ordered() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let prim = store.path("/P");

    let set_v = store.tokens.intern("v");
    let variant_a = store.tokens.intern("A");
    let variant_b = store.tokens.intern("B");

    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![LayerId(2)];
    let mut root_spec = PrimSpec::default();
    root_spec.variant_selections.insert(set_v, variant_a);
    root_layer.insert_prim(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer::new(LayerId(2));
    let mut sub_spec = PrimSpec::default();
    sub_spec.variant_selections.insert(set_v, variant_b);

    let mut set_spec = VariantSetSpec::default();
    let mut fields_a = HashMap::new();
    fields_a.insert(field_x, Value::Int64(1).into());
    set_spec.variants.insert(
        variant_a,
        VariantSpec {
            fields: fields_a,
            ..Default::default()
        },
    );

    let mut fields_b = HashMap::new();
    fields_b.insert(field_x, Value::Int64(2).into());
    set_spec.variants.insert(
        variant_b,
        VariantSpec {
            fields: fields_b,
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
    root_layer.sublayers = vec![LayerId(2)];
    let mut root_spec = PrimSpec::default();
    root_spec.fields.insert(
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
    sub_spec.fields.insert(
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
    spec.fields.insert(
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
    root_layer.sublayers = vec![LayerId(2), LayerId(3)];
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
    root_layer.sublayers = vec![LayerId(2)];
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
    root_layer.sublayers = vec![LayerId(2)];
    let mut root_spec = PrimSpec::default();
    root_spec.fields.insert(
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
    sub_spec.fields.insert(
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
    root_layer.sublayers = vec![LayerId(2), LayerId(3)];
    let mut root_spec = PrimSpec::default();
    root_spec.fields.insert(
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
    sub1_spec.fields.insert(
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
    sub2_spec.fields.insert(
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
fn specifier_resolution_follows_strongest_defining() {
    use layerstack::Specifier;

    let mut store = InMemoryStore::default();
    let prim = store.path("/P");

    // Layer 1 (strongest): over — non-defining
    let mut root_layer = Layer::new(LayerId(1));
    root_layer.sublayers = vec![LayerId(2), LayerId(3)];
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
    root_layer.sublayers = vec![LayerId(2)];
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
    root_layer.sublayers = vec![LayerId(2)];
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
    spec.fields.insert(
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
    spec.fields.insert(
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
    spec.fields.insert(
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
    spec.fields.insert(
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
    root.sublayers = vec![LayerId(2)];
    let mut root_spec = PrimSpec::default();
    root_spec.fields.insert(
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
    root.sublayers = vec![LayerId(2)];
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
    let inh_arc = arcs
        .iter()
        .find(|a| a.arc_kind == ArcKind::Inherits)
        .expect("inherit arc exists");
    assert_eq!(inh_arc.source, base);
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
