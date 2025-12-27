#![allow(missing_docs, reason = "integration tests")]

use layerstack::HashMap;

use layerstack::{
    FieldValue, Layer, LayerId, ListOp, Path, PrimSpec, Reference, ResolvedValue, Stage,
    StageOptions, Value, VariantSetSpec, VariantSpec, doc::InMemoryStore,
};

fn path(store: &mut InMemoryStore, s: &str) -> layerstack::PathId {
    let p = Path::parse_absolute(s, &mut store.tokens).expect("valid path");
    store.paths.intern(p)
}

#[test]
fn sublayer_strength_local_beats_sublayer() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let p = path(&mut store, "/P");

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![LayerId(2)],
        prims: HashMap::new(),
    };
    let mut root_spec = PrimSpec::default();
    root_spec
        .fields
        .insert(field_x, FieldValue::Value(Value::Int(1)));
    root_layer.prims.insert(p, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut sub_spec = PrimSpec::default();
    sub_spec
        .fields
        .insert(field_x, FieldValue::Value(Value::Int(2)));
    sub_layer.prims.insert(p, sub_spec);
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
    assert_eq!(resolved.value, Value::Int(1));
    let prov = resolved.provenance.expect("provenance enabled");
    assert_eq!(prov.layer, LayerId(1));
}

#[test]
fn reference_opinions_are_weaker_than_local() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let p = path(&mut store, "/P");
    let q = path(&mut store, "/Q");
    let q_child = path(&mut store, "/Q/Child");
    let p_child = path(&mut store, "/P/Child");

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut root_spec = PrimSpec::default();
    root_spec.references.append.push(Reference {
        layer: LayerId(2),
        prim_path: q,
        asset: None,
    });
    root_spec
        .fields
        .insert(field_x, FieldValue::Value(Value::Int(2)));
    root_layer.prims.insert(p, root_spec);
    store.insert_layer(root_layer);

    let mut ref_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut ref_spec = PrimSpec::default();
    ref_spec
        .fields
        .insert(field_x, FieldValue::Value(Value::Int(1)));
    ref_layer.prims.insert(q, ref_spec);
    ref_layer.prims.insert(q_child, PrimSpec::default());
    store.insert_layer(ref_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    assert!(
        stage.has_prim(p_child),
        "reference populates descendant prims"
    );
    let resolved = stage.resolve_field(p, field_x).expect("field exists");
    assert_eq!(resolved.value, Value::Int(2));
}

#[test]
fn variants_selection_is_strength_ordered() {
    let mut store = InMemoryStore::default();

    let field_x = store.tokens.intern("x");
    let prim = path(&mut store, "/P");

    let set_v = store.tokens.intern("v");
    let variant_a = store.tokens.intern("A");
    let variant_b = store.tokens.intern("B");

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![LayerId(2)],
        prims: HashMap::new(),
    };
    let mut root_spec = PrimSpec::default();
    root_spec.variant_selections.insert(set_v, variant_a);
    root_layer.prims.insert(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut sub_spec = PrimSpec::default();
    sub_spec.variant_selections.insert(set_v, variant_b);

    let mut set_spec = VariantSetSpec::default();
    let mut fields_a = HashMap::new();
    fields_a.insert(field_x, FieldValue::Value(Value::Int(1)));
    set_spec
        .variants
        .insert(variant_a, VariantSpec { fields: fields_a });

    let mut fields_b = HashMap::new();
    fields_b.insert(field_x, FieldValue::Value(Value::Int(2)));
    set_spec
        .variants
        .insert(variant_b, VariantSpec { fields: fields_b });

    sub_spec.variant_sets.insert(set_v, set_spec);
    sub_layer.prims.insert(prim, sub_spec);
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let resolved = stage.resolve_field(prim, field_x).expect("field exists");
    assert_eq!(resolved.value, Value::Int(1));
}

#[test]
fn listop_chain_is_applied_strong_to_weak() {
    let mut store = InMemoryStore::default();

    let field_classes = store.tokens.intern("classes");
    let class_a = store.tokens.intern("a");
    let class_b = store.tokens.intern("b");
    let prim = path(&mut store, "/P");

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![LayerId(2)],
        prims: HashMap::new(),
    };
    let mut root_spec = PrimSpec::default();
    root_spec.fields.insert(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            append: vec![class_a],
            ..ListOp::default()
        }),
    );
    root_layer.prims.insert(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut sub_spec = PrimSpec::default();
    sub_spec.fields.insert(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            append: vec![class_b],
            ..ListOp::default()
        }),
    );
    sub_layer.prims.insert(prim, sub_spec);
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

    let prim = path(&mut store, "/P");
    let field_x = store.tokens.intern("x");
    let field_classes = store.tokens.intern("classes");
    let class_a = store.tokens.intern("a");

    let mut layer = Layer {
        id: LayerId(1),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut spec = PrimSpec::default();
    spec.fields
        .insert(field_x, FieldValue::Value(Value::Int(123)));
    spec.fields.insert(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            append: vec![class_a],
            ..ListOp::default()
        }),
    );
    layer.prims.insert(prim, spec);
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
    assert_eq!(resolved_x.value, ResolvedValue::Scalar(Value::Int(123)));
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
        Value::Int(123)
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

    let prim_a = path(&mut store, "/A");
    let a_a = path(&mut store, "/A/a");
    let a_b = path(&mut store, "/A/b");
    let a_c = path(&mut store, "/A/c");
    let a_f = path(&mut store, "/A/f");
    let a_x = path(&mut store, "/A/x");
    let a_y = path(&mut store, "/A/y");
    let a_z = path(&mut store, "/A/z");

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![LayerId(2), LayerId(3)],
        prims: HashMap::new(),
    };
    let root_spec = PrimSpec {
        authored_children: vec![f],
        prim_order: Some(vec![z, f, y]),
        ..PrimSpec::default()
    };
    root_layer.prims.insert(prim_a, root_spec);
    root_layer.prims.insert(a_f, PrimSpec::default());
    store.insert_layer(root_layer);

    let mut sub1_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let sub1_spec = PrimSpec {
        authored_children: vec![a, b, c],
        prim_order: Some(vec![z, x, b]),
        ..PrimSpec::default()
    };
    sub1_layer.prims.insert(prim_a, sub1_spec);
    sub1_layer.prims.insert(a_a, PrimSpec::default());
    sub1_layer.prims.insert(a_b, PrimSpec::default());
    sub1_layer.prims.insert(a_c, PrimSpec::default());
    store.insert_layer(sub1_layer);

    let mut sub2_layer = Layer {
        id: LayerId(3),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let sub2_spec = PrimSpec {
        authored_children: vec![x, y, z],
        ..PrimSpec::default()
    };
    sub2_layer.prims.insert(prim_a, sub2_spec);
    sub2_layer.prims.insert(a_x, PrimSpec::default());
    sub2_layer.prims.insert(a_y, PrimSpec::default());
    sub2_layer.prims.insert(a_z, PrimSpec::default());
    store.insert_layer(sub2_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let actual = stage.children_of(prim_a).expect("children list");
    assert_eq!(actual, &[a_z, a_a, a_x, a_f, a_y, a_b, a_c]);
}

#[test]
fn explain_field_returns_sorted_opinion_stack() {
    let mut store = InMemoryStore::default();

    let prim = path(&mut store, "/P");
    let field_x = store.tokens.intern("x");

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![LayerId(2)],
        prims: HashMap::new(),
    };
    let mut root_spec = PrimSpec::default();
    root_spec
        .fields
        .insert(field_x, FieldValue::Value(Value::Int(1)));
    root_layer.prims.insert(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut sub_spec = PrimSpec::default();
    sub_spec
        .fields
        .insert(field_x, FieldValue::Value(Value::Int(2)));
    sub_layer.prims.insert(prim, sub_spec);
    store.insert_layer(sub_layer);

    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let stack = stage.explain_field(prim, field_x).expect("opinions exist");
    assert_eq!(stack.len(), 2);

    // Strongest-first means the root layer's opinion comes first.
    assert_eq!(stack[0].key.layer_id, LayerId(1));
    assert_eq!(stack[1].key.layer_id, LayerId(2));

    assert_eq!(
        stage.resolve_field(prim, field_x).expect("scalar").value,
        Value::Int(1)
    );
}

#[test]
fn token_listop_append_reorders_duplicates() {
    let mut store = InMemoryStore::default();

    let prim = path(&mut store, "/P");
    let field_classes = store.tokens.intern("classes");
    let class_a = store.tokens.intern("a");
    let class_b = store.tokens.intern("b");

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![LayerId(2)],
        prims: HashMap::new(),
    };
    let mut root_spec = PrimSpec::default();
    root_spec.fields.insert(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            append: vec![class_a],
            ..ListOp::default()
        }),
    );
    root_layer.prims.insert(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut sub_spec = PrimSpec::default();
    sub_spec.fields.insert(
        field_classes,
        FieldValue::TokenListOp(ListOp {
            explicit: Some(vec![class_a, class_b]),
            ..ListOp::default()
        }),
    );
    sub_layer.prims.insert(prim, sub_spec);
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

    let prim = path(&mut store, "/A");
    let field_targets = store.tokens.intern("targets");

    let root_prepend = store.tokens.intern("root_prepend");
    let sub1_prepend = store.tokens.intern("sub1_prepend");
    let sub2_prepend = store.tokens.intern("sub2_prepend");
    let sub2_append = store.tokens.intern("sub2_append");
    let sub1_append = store.tokens.intern("sub1_append");
    let root_append = store.tokens.intern("root_append");

    let mut root_layer = Layer {
        id: LayerId(1),
        sublayers: vec![LayerId(2), LayerId(3)],
        prims: HashMap::new(),
    };
    let mut root_spec = PrimSpec::default();
    root_spec.fields.insert(
        field_targets,
        FieldValue::TokenListOp(ListOp {
            prepend: vec![root_prepend],
            append: vec![root_append],
            ..ListOp::default()
        }),
    );
    root_layer.prims.insert(prim, root_spec);
    store.insert_layer(root_layer);

    let mut sub1_layer = Layer {
        id: LayerId(2),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut sub1_spec = PrimSpec::default();
    sub1_spec.fields.insert(
        field_targets,
        FieldValue::TokenListOp(ListOp {
            prepend: vec![sub1_prepend],
            append: vec![sub1_append],
            ..ListOp::default()
        }),
    );
    sub1_layer.prims.insert(prim, sub1_spec);
    store.insert_layer(sub1_layer);

    let mut sub2_layer = Layer {
        id: LayerId(3),
        sublayers: vec![],
        prims: HashMap::new(),
    };
    let mut sub2_spec = PrimSpec::default();
    sub2_spec.fields.insert(
        field_targets,
        FieldValue::TokenListOp(ListOp {
            prepend: vec![sub2_prepend],
            append: vec![sub2_append],
            ..ListOp::default()
        }),
    );
    sub2_layer.prims.insert(prim, sub2_spec);
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
