// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Value explanations: `Stage::explain_*` against hand-checked expectations.

#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::{
    ArcKind, ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, Contribution, DictionaryMerge,
    ExplainedOpinion, IgnoreCause, InMemoryStore, InterpolationType, Layer, LayerId, LayerOffset,
    OpinionRole, PrimSpec, PropertySpec, PropertyType, Reference, ResolvedValue, SampleUse,
    SchemaDefinition, SchemaRegistry, Stage, StageOptions, SublayerEntry, TokenId, Value,
    ValueSource,
};

const VALUE: OpinionRole = OpinionRole::Contributed(Contribution::Value);
const EDIT: OpinionRole = OpinionRole::Contributed(Contribution::ArrayEdit);
const SHADOWED: OpinionRole = OpinionRole::Ignored(IgnoreCause::Shadowed);
const CUT_OFF: OpinionRole = OpinionRole::Ignored(IgnoreCause::CutOffByBlock);

/// `(layer, role)` of each explained opinion, strongest first.
fn roles(opinions: &[ExplainedOpinion<'_>]) -> Vec<(LayerId, OpinionRole)> {
    opinions
        .iter()
        .map(|opinion| (opinion.layer(), opinion.role.clone()))
        .collect()
}

fn attribute(type_name: &str, is_array: bool, scalar: Value, value: Value) -> PropertySpec {
    PropertySpec::attribute()
        .with_type(PropertyType::new(type_name, is_array, scalar))
        .with_default(value)
}

fn double(value: f64) -> PropertySpec {
    attribute("double", false, Value::Double(0.0), value.into())
}

fn ints(values: &[i32]) -> Value {
    Value::Array(values.iter().copied().map(Value::Int).collect())
}

fn int_array(value: Value) -> PropertySpec {
    attribute("int", true, Value::Int(0), value)
}

fn write(value: i32, index: i64) -> Value {
    Value::ArrayEdit(ArrayEdit {
        ops: vec![ArrayEditOp::Write {
            src: ArrayEditOperand::Literal(Value::Int(value)),
            index: ArrayIndex::Position(index),
        }],
    })
}

/// A root layer (1) over sublayers 2, 3, …, each authoring `/Tree` with the
/// spec `specs` gives it, strongest first.
fn layered(store: &mut InMemoryStore, specs: Vec<PrimSpec>) -> Stage {
    let tree = store.path("/Tree");
    let mut root = Layer::new(LayerId(1));
    root.sublayers = (0..specs.len())
        .map(|i| SublayerEntry::new(LayerId(i as u64 + 2)))
        .collect();
    root.insert_prim(tree, PrimSpec::def());
    store.insert_layer(root);
    for (i, spec) in specs.into_iter().enumerate() {
        let mut layer = Layer::new(LayerId(i as u64 + 2));
        layer.insert_prim(tree, spec);
        store.insert_layer(layer);
    }
    Stage::compose(store, LayerId(1), StageOptions::default())
}

#[test]
fn stronger_dense_value_shadows_weaker_ones() {
    let mut store = InMemoryStore::default();
    let height = store.tokens.intern("height");
    let stage = layered(
        &mut store,
        vec![
            PrimSpec::over().with_property(height, double(4.0)),
            PrimSpec::over().with_property(height, double(2.0)),
        ],
    );
    let path = store.property_path("/Tree.height");

    let explained = stage.explain_property_value(path).expect("authored");
    assert_eq!(
        explained.value,
        Some(ResolvedValue::Scalar(Value::Double(4.0)))
    );
    assert_eq!(explained.source, ValueSource::Default);
    assert!(!explained.seeded_by_fallback);
    assert_eq!(
        roles(&explained.opinions),
        vec![(LayerId(2), VALUE), (LayerId(3), SHADOWED)]
    );
    assert_eq!(explained.contributors().count(), 1);
    assert_eq!(explained.opinions[0].arc_kind(), Some(ArcKind::Local));
    assert_eq!(
        explained.opinions[0].spec_path().display(&store.tokens),
        "/Tree.height"
    );
}

#[test]
fn block_cuts_off_weaker_opinions() {
    let mut store = InMemoryStore::default();
    let height = store.tokens.intern("height");
    let points = store.tokens.intern("points");
    let stage = layered(
        &mut store,
        vec![
            PrimSpec::over()
                .with_property(height, double(4.0))
                .with_property(points, int_array(write(9, 0))),
            PrimSpec::over()
                .with_property(
                    height,
                    attribute("double", false, Value::Double(0.0), Value::Blocked),
                )
                .with_property(points, int_array(Value::Blocked)),
            PrimSpec::over()
                .with_property(height, double(2.0))
                .with_property(points, int_array(ints(&[1, 2, 3]))),
        ],
    );

    // A scalar: the strongest value wins; the block only cuts off what is
    // weaker than it.
    let height = stage
        .explain_property_value(store.property_path("/Tree.height"))
        .expect("authored");
    assert_eq!(
        height.value,
        Some(ResolvedValue::Scalar(Value::Double(4.0)))
    );
    assert_eq!(
        roles(&height.opinions),
        vec![
            (LayerId(2), VALUE),
            (LayerId(3), SHADOWED),
            (LayerId(4), SHADOWED)
        ]
    );

    // A sparse array: the stronger edit composes over the empty array the
    // block leaves, and the weaker dense array is cut off.
    let points = stage
        .explain_property_value(store.property_path("/Tree.points"))
        .expect("authored");
    assert_eq!(points.value, Some(ResolvedValue::Scalar(ints(&[]))));
    assert_eq!(points.source, ValueSource::Default);
    assert_eq!(
        roles(&points.opinions),
        vec![
            (LayerId(2), EDIT),
            (LayerId(3), OpinionRole::Block),
            (LayerId(4), CUT_OFF)
        ]
    );
}

#[test]
fn strongest_block_resolves_no_value() {
    let mut store = InMemoryStore::default();
    let height = store.tokens.intern("height");
    let stage = layered(
        &mut store,
        vec![
            PrimSpec::over().with_property(
                height,
                attribute("double", false, Value::Double(0.0), Value::Blocked),
            ),
            PrimSpec::over().with_property(height, double(2.0)),
        ],
    );
    let path = store.property_path("/Tree.height");
    let explained = stage.explain_property_value(path).expect("authored");
    assert_eq!(explained.value, None);
    assert_eq!(stage.resolve_property_path(path), None);
    assert_eq!(explained.source, ValueSource::None);
    assert_eq!(
        roles(&explained.opinions),
        vec![(LayerId(2), OpinionRole::Block), (LayerId(3), CUT_OFF)]
    );
}

#[test]
fn sparse_edits_compose_over_a_dense_base_across_layers() {
    let mut store = InMemoryStore::default();
    let points = store.tokens.intern("points");
    let color = store.tokens.intern("color");
    let stage = layered(
        &mut store,
        vec![
            PrimSpec::over().with_property(points, int_array(write(9, 0))),
            PrimSpec::over()
                .with_property(points, int_array(write(8, -1)))
                // Authors no value: not consulted.
                .with_property(color, double(1.0)),
            PrimSpec::def().with_property(points, int_array(ints(&[1, 2, 3]))),
            PrimSpec::over().with_property(points, int_array(ints(&[7]))),
        ],
    );
    let path = store.property_path("/Tree.points");
    let explained = stage.explain_property_value(path).expect("authored");
    assert_eq!(
        explained.value,
        Some(ResolvedValue::Scalar(ints(&[9, 2, 8])))
    );
    assert_eq!(explained.source, ValueSource::Default);
    assert_eq!(
        roles(&explained.opinions),
        vec![
            (LayerId(2), EDIT),
            (LayerId(3), EDIT),
            (LayerId(4), VALUE),
            (LayerId(5), SHADOWED)
        ]
    );
    assert_eq!(
        explained.value,
        stage
            .resolve_property_path(path)
            .map(|resolved| resolved.value)
    );
}

fn key_path(keys: &[&str]) -> Vec<Arc<str>> {
    keys.iter().map(|key| Arc::from(*key)).collect()
}

fn dict(entries: Vec<(&str, Value)>) -> Value {
    Value::Dictionary(
        entries
            .into_iter()
            .map(|(key, value)| (Arc::from(key), value))
            .collect(),
    )
}

#[test]
fn dictionary_opinions_report_what_each_supplied() {
    let mut store = InMemoryStore::default();
    let custom_data = store.tokens.intern("customData");
    let stage = layered(
        &mut store,
        vec![
            PrimSpec::over().with_field(
                custom_data,
                dict(vec![
                    ("species", Value::string("oak")),
                    ("growth", dict(vec![("rate", Value::Double(0.5))])),
                ]),
            ),
            // Not a dictionary: skipped by the combination.
            PrimSpec::over().with_field(custom_data, Value::Int(3)),
            PrimSpec::def().with_field(
                custom_data,
                dict(vec![
                    ("species", Value::string("elm")),
                    (
                        "growth",
                        dict(vec![
                            ("rate", Value::Double(0.1)),
                            ("max", Value::Double(9.0)),
                        ]),
                    ),
                    ("planted", Value::Int(1999)),
                ]),
            ),
        ],
    );
    let tree = store.path("/Tree");
    let explained = stage.explain_value(tree, custom_data).expect("authored");
    assert_eq!(
        explained.value,
        stage
            .resolve_value(tree, custom_data)
            .map(|resolved| resolved.value)
    );
    assert_eq!(explained.source, ValueSource::Default);
    assert_eq!(
        roles(&explained.opinions),
        vec![
            (
                LayerId(2),
                OpinionRole::Contributed(Contribution::Dictionary(DictionaryMerge {
                    supplied: vec![key_path(&["species"]), key_path(&["growth"])],
                    merged: vec![],
                    overridden: vec![],
                }))
            ),
            (LayerId(3), OpinionRole::Ignored(IgnoreCause::Incompatible)),
            (
                LayerId(4),
                OpinionRole::Contributed(Contribution::Dictionary(DictionaryMerge {
                    supplied: vec![key_path(&["growth", "max"]), key_path(&["planted"])],
                    merged: vec![key_path(&["growth"])],
                    overridden: vec![key_path(&["species"]), key_path(&["growth", "rate"])],
                }))
            ),
        ]
    );
}

#[test]
fn time_samples_are_explained_in_stage_time_through_a_layer_offset() {
    let mut store = InMemoryStore::default();
    let height = store.tokens.intern("height");
    let tree = store.path("/Tree");

    // The weaker sublayer is offset by 10: its samples at layer times 0 and
    // 10 sit at stage times 10 and 20. The stronger sublayer authors only a
    // default, which a time query reads after every weaker sample.
    let mut root = Layer::new(LayerId(1));
    root.sublayers = vec![
        SublayerEntry::with_offset(
            LayerId(2),
            LayerOffset {
                offset: 10.0,
                scale: 1.0,
            },
        ),
        SublayerEntry::new(LayerId(3)),
    ];
    root.insert_prim(tree, PrimSpec::def());
    store.insert_layer(root);
    let mut sampled = Layer::new(LayerId(2));
    sampled.insert_prim(
        tree,
        PrimSpec::over().with_property(
            height,
            PropertySpec::attribute()
                .with_time_samples(vec![(0.0, Value::Double(1.0)), (10.0, Value::Double(2.0))]),
        ),
    );
    store.insert_layer(sampled);
    let mut weak = Layer::new(LayerId(3));
    weak.insert_prim(tree, PrimSpec::def().with_property(height, double(5.0)));
    store.insert_layer(weak);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let path = store.property_path("/Tree.height");

    let explained = stage
        .explain_property_value_at_time(path, 15.0, InterpolationType::Linear)
        .expect("authored");
    assert_eq!(explained.value, Some(Value::Double(1.5)));
    assert_eq!(
        explained.source,
        ValueSource::TimeSamples {
            lower: 10.0,
            upper: 20.0,
            interpolation: InterpolationType::Linear,
            lower_seeded_by_fallback: false,
            upper_seeded_by_fallback: false,
        }
    );
    assert_eq!(
        roles(&explained.opinions),
        vec![(LayerId(2), VALUE), (LayerId(3), SHADOWED)]
    );
    let offset = explained.opinions[0].layer_offset();
    assert_eq!((offset.offset, offset.scale), (10.0, 1.0));
    assert_eq!(
        explained.opinions[0].samples,
        vec![
            SampleUse {
                time: 10.0,
                role: VALUE
            },
            SampleUse {
                time: 20.0,
                role: VALUE
            },
        ]
    );

    // Held interpolation reads the lower sample only.
    let held = stage
        .explain_property_value_at_time(path, 15.0, InterpolationType::Held)
        .expect("authored");
    assert_eq!(held.value, Some(Value::Double(1.0)));
    assert_eq!(
        held.source,
        ValueSource::TimeSamples {
            lower: 10.0,
            upper: 10.0,
            interpolation: InterpolationType::Held,
            lower_seeded_by_fallback: false,
            upper_seeded_by_fallback: false,
        }
    );
}

#[test]
fn offset_time_samples_report_stage_times() {
    let mut store = InMemoryStore::default();
    let points = store.tokens.intern("points");
    let tree = store.path("/Tree");

    // The weaker layer is offset by 10: its samples at layer times 0 and 10
    // sit at stage times 10 and 20. The stronger layer edits every sample.
    let mut root = Layer::new(LayerId(1));
    root.sublayers = vec![
        SublayerEntry::new(LayerId(2)),
        SublayerEntry::with_offset(
            LayerId(3),
            LayerOffset {
                offset: 10.0,
                scale: 1.0,
            },
        ),
    ];
    root.insert_prim(tree, PrimSpec::def());
    store.insert_layer(root);
    let mut strong = Layer::new(LayerId(2));
    let edit = Value::ArrayEdit(ArrayEdit {
        ops: vec![ArrayEditOp::Write {
            src: ArrayEditOperand::Literal(Value::Float(9.0)),
            index: ArrayIndex::Position(0),
        }],
    });
    strong.insert_prim(
        tree,
        PrimSpec::over().with_property(points, attribute("float", true, Value::Float(0.0), edit)),
    );
    store.insert_layer(strong);
    let mut weak = Layer::new(LayerId(3));
    weak.insert_prim(
        tree,
        PrimSpec::def().with_property(
            points,
            PropertySpec::attribute()
                .with_type(PropertyType::new("float", true, Value::Float(0.0)))
                .with_time_samples(vec![
                    (
                        0.0,
                        Value::Array(vec![Value::Float(0.0), Value::Float(0.0)]),
                    ),
                    (
                        10.0,
                        Value::Array(vec![Value::Float(2.0), Value::Float(4.0)]),
                    ),
                ]),
        ),
    );
    store.insert_layer(weak);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let path = store.property_path("/Tree.points");

    let explained = stage
        .explain_property_value_at_time(path, 15.0, InterpolationType::Linear)
        .expect("authored");
    assert_eq!(
        explained.value,
        Some(Value::Array(vec![Value::Float(9.0), Value::Float(2.0)]))
    );
    assert_eq!(
        explained.value,
        stage
            .resolve_property_path_at_time(path, 15.0, InterpolationType::Linear)
            .map(|resolved| resolved.value)
    );
    assert_eq!(
        explained.source,
        ValueSource::TimeSamples {
            lower: 10.0,
            upper: 20.0,
            interpolation: InterpolationType::Linear,
            lower_seeded_by_fallback: false,
            upper_seeded_by_fallback: false,
        }
    );
    assert_eq!(
        roles(&explained.opinions),
        vec![(LayerId(2), EDIT), (LayerId(3), VALUE)]
    );
    let offset = explained.opinions[1].layer_offset();
    assert_eq!((offset.offset, offset.scale), (10.0, 1.0));
    assert_eq!(
        explained.opinions[1].samples,
        vec![
            SampleUse {
                time: 10.0,
                role: VALUE
            },
            SampleUse {
                time: 20.0,
                role: VALUE
            },
        ]
    );
    assert_eq!(
        explained.opinions[0].samples,
        vec![
            SampleUse {
                time: f64::NEG_INFINITY,
                role: EDIT
            },
            SampleUse {
                time: f64::NEG_INFINITY,
                role: EDIT
            },
        ]
    );
}

/// A grove references a tree asset, which inherits a class.
fn grove(store: &mut InMemoryStore) -> (Stage, TokenId, TokenId) {
    let height = store.tokens.intern("height");
    let bark = store.tokens.intern("bark");
    let grove = store.path("/Grove");
    let tree = store.path("/Tree");
    let class = store.path("/_class_Tree");

    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        grove,
        PrimSpec::def()
            .with_reference(Reference::new(LayerId(2), tree))
            .with_property(height, double(6.0)),
    );
    store.insert_layer(root);
    let mut asset = Layer::new(LayerId(2));
    asset.insert_prim(
        tree,
        PrimSpec::def()
            .with_inherit(class)
            .with_property(height, double(3.0)),
    );
    asset.insert_prim(
        class,
        PrimSpec::class().with_property(
            bark,
            attribute("string", false, Value::string(""), Value::string("rough")),
        ),
    );
    store.insert_layer(asset);
    (
        Stage::compose(store, LayerId(1), StageOptions::default()),
        height,
        bark,
    )
}

#[test]
fn opinions_name_the_arc_that_reaches_them() {
    let mut store = InMemoryStore::default();
    let (stage, _, _) = grove(&mut store);

    let height = stage
        .explain_property_value(store.property_path("/Grove.height"))
        .expect("authored");
    assert_eq!(
        height.value,
        Some(ResolvedValue::Scalar(Value::Double(6.0)))
    );
    let arcs: Vec<_> = height
        .opinions
        .iter()
        .map(|opinion| (opinion.arc_kind(), opinion.role.clone()))
        .collect();
    assert_eq!(
        arcs,
        vec![
            (Some(ArcKind::Local), VALUE),
            (Some(ArcKind::References), SHADOWED)
        ]
    );
    let node = height.opinions[1].node.expect("graph node");
    assert_eq!(node.site().display(&store.tokens), "/Tree");
    assert_eq!(node.layer_stack(), LayerId(2));

    let bark = stage
        .explain_property_value(store.property_path("/Grove.bark"))
        .expect("authored");
    assert_eq!(
        bark.value,
        Some(ResolvedValue::Scalar(Value::string("rough")))
    );
    assert_eq!(bark.opinions.len(), 1);
    let node = bark.opinions[0].node.expect("graph node");
    assert_eq!(node.arc_kind(), ArcKind::Inherits);
    assert_eq!(node.site().display(&store.tokens), "/_class_Tree");
    assert_eq!(bark.opinions[0].layer(), LayerId(2));
}

#[test]
fn schema_fallback_is_explained() {
    let mut store = InMemoryStore::default();
    let height = store.tokens.intern("height");
    let points = store.tokens.intern("points");
    let tree_type = store.tokens.intern("Tree");
    let mut registry = SchemaRegistry::new();
    registry.register(
        SchemaDefinition::typed(tree_type)
            .with_property(height, Value::Double(1.0))
            .with_property(points, ints(&[1, 2, 3])),
    );
    let stage = layered(
        &mut store,
        vec![
            PrimSpec::def()
                .with_type_name(tree_type)
                .with_property(points, int_array(write(9, 0))),
        ],
    );
    let tree = store.path("/Tree");

    // Nothing authored: the fallback alone.
    let explained = stage
        .explain_value_with_schema(tree, height, &store, &registry, None)
        .expect("fallback");
    assert_eq!(
        explained.value,
        Some(ResolvedValue::Scalar(Value::Double(1.0)))
    );
    assert_eq!(explained.source, ValueSource::Fallback);
    assert!(explained.opinions.is_empty());

    // An authored edit composes over the fallback seed.
    let explained = stage
        .explain_value_with_schema(tree, points, &store, &registry, None)
        .expect("authored");
    assert_eq!(
        explained.value,
        Some(ResolvedValue::Scalar(ints(&[9, 2, 3])))
    );
    assert_eq!(explained.source, ValueSource::Default);
    assert!(explained.seeded_by_fallback);
    assert_eq!(roles(&explained.opinions), vec![(LayerId(2), EDIT)]);

    let at_time = stage
        .explain_value_at_time_with_schema(
            tree,
            height,
            3.0,
            InterpolationType::Linear,
            &store,
            &registry,
            None,
        )
        .expect("fallback");
    assert_eq!(at_time.value, Some(Value::Double(1.0)));
    assert_eq!(at_time.source, ValueSource::Fallback);
}

#[test]
fn a_block_resolves_the_schema_fallback() {
    let mut store = InMemoryStore::default();
    let height = store.tokens.intern("height");
    let tree_type = store.tokens.intern("Tree");
    let mut registry = SchemaRegistry::new();
    registry.register(SchemaDefinition::typed(tree_type).with_property(height, Value::Double(1.0)));
    let stage = layered(
        &mut store,
        vec![
            PrimSpec::def().with_type_name(tree_type).with_property(
                height,
                attribute("double", false, Value::Double(0.0), Value::Blocked),
            ),
            PrimSpec::over().with_property(height, double(2.0)),
        ],
    );
    let tree = store.path("/Tree");
    let explained = stage
        .explain_value_with_schema(tree, height, &store, &registry, None)
        .expect("authored");
    assert_eq!(
        explained.value,
        Some(ResolvedValue::Scalar(Value::Double(1.0)))
    );
    assert_eq!(explained.source, ValueSource::Fallback);
    assert_eq!(
        roles(&explained.opinions),
        vec![(LayerId(2), OpinionRole::Block), (LayerId(3), CUT_OFF)]
    );
}

#[test]
fn list_ops_report_each_edit() {
    use layerstack::{FieldValue, ListOp};

    let mut store = InMemoryStore::default();
    let api_schemas = store.tokens.intern("apiSchemas");
    let a = store.tokens.intern("ShadeAPI");
    let b = store.tokens.intern("WindAPI");
    let stage = layered(
        &mut store,
        vec![
            PrimSpec::over().with_field(
                api_schemas,
                FieldValue::TokenListOp(ListOp::prepended(vec![a])),
            ),
            PrimSpec::def().with_field(
                api_schemas,
                FieldValue::TokenListOp(ListOp::appended(vec![b])),
            ),
        ],
    );
    let tree = store.path("/Tree");
    let explained = stage.explain_value(tree, api_schemas).expect("authored");
    assert_eq!(explained.value, Some(ResolvedValue::TokenList(vec![a, b])));
    let edit = OpinionRole::Contributed(Contribution::ListEdit);
    assert_eq!(
        roles(&explained.opinions),
        vec![(LayerId(2), edit.clone()), (LayerId(3), edit)]
    );
}

/// A typed `/Tree` whose schema gives `points` the fallback `[10, 20]` and
/// `height` none, with the given authored properties.
fn typed_tree(
    store: &mut InMemoryStore,
    properties: Vec<(&str, PropertySpec)>,
) -> (Stage, SchemaRegistry) {
    let points = store.tokens.intern("points");
    let tree_type = store.tokens.intern("Tree");
    let mut registry = SchemaRegistry::new();
    registry.register(SchemaDefinition::typed(tree_type).with_property(
        points,
        Value::Array(vec![Value::Float(10.0), Value::Float(20.0)]),
    ));
    let tree = store.path("/Tree");
    let mut spec = PrimSpec::def().with_type_name(tree_type);
    for (name, property) in properties {
        spec = spec.with_property(store.tokens.intern(name), property);
    }
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(tree, spec);
    store.insert_layer(root);
    let stage = Stage::compose(store, LayerId(1), StageOptions::default());
    (stage, registry)
}

fn floats(values: &[f32]) -> Value {
    Value::Array(values.iter().copied().map(Value::Float).collect())
}

fn write_float(value: f32, index: i64) -> Value {
    Value::ArrayEdit(ArrayEdit {
        ops: vec![ArrayEditOp::Write {
            src: ArrayEditOperand::Literal(Value::Float(value)),
            index: ArrayIndex::Position(index),
        }],
    })
}

#[test]
fn blocks_without_a_fallback_are_still_explained() {
    let mut store = InMemoryStore::default();
    let (stage, registry) = typed_tree(
        &mut store,
        vec![
            (
                "height",
                PropertySpec::attribute().with_default(Value::Blocked),
            ),
            (
                "width",
                PropertySpec::attribute()
                    .with_time_samples(vec![(0.0, Value::Blocked), (10.0, Value::Double(1.0))]),
            ),
        ],
    );
    let tree = store.path("/Tree");
    let height = store.tokens.intern("height");
    let width = store.tokens.intern("width");
    let depth = store.tokens.intern("depth");
    let linear = InterpolationType::Linear;

    // An authored default block, at a numeric time and at the default time.
    let at_time = stage
        .explain_value_at_time_with_schema(tree, height, 5.0, linear, &store, &registry, None)
        .expect("the authored block is explained");
    assert_eq!(at_time.value, None);
    assert_eq!(at_time.source, ValueSource::None);
    assert_eq!(
        roles(&at_time.opinions),
        vec![(LayerId(1), OpinionRole::Block)]
    );
    let default = stage
        .explain_value_with_schema(tree, height, &store, &registry, None)
        .expect("the authored block is explained");
    assert_eq!(default.value, None);
    assert_eq!(
        roles(&default.opinions),
        vec![(LayerId(1), OpinionRole::Block)]
    );
    let plain = stage
        .explain_property_value_at_time(store.property_path("/Tree.height"), 5.0, linear)
        .expect("authored");
    assert_eq!(plain.value, None);
    assert_eq!(
        roles(&plain.opinions),
        vec![(LayerId(1), OpinionRole::Block)]
    );

    // A blocked sample held at the query time.
    let sampled = stage
        .explain_value_at_time_with_schema(tree, width, 5.0, linear, &store, &registry, None)
        .expect("the blocked sample is explained");
    assert_eq!(sampled.value, None);
    assert_eq!(sampled.source, ValueSource::None);
    assert_eq!(
        roles(&sampled.opinions),
        vec![(LayerId(1), OpinionRole::Block)]
    );

    // Nothing authored and no fallback: nothing to explain.
    assert!(
        stage
            .explain_value_at_time_with_schema(tree, depth, 5.0, linear, &store, &registry, None)
            .is_none()
    );
    assert!(
        stage
            .explain_value_with_schema(tree, depth, &store, &registry, None)
            .is_none()
    );
}

/// Linear interpolation between a dense sample and a sparse sample seeded by
/// the fallback `[10, 20]`: the explanation names the seeded sample.
#[expect(
    clippy::missing_assert_message,
    reason = "a test helper; the assertions name what they check"
)]
fn check_mixed_samples(samples: Vec<(f64, Value)>, lower_seeded: bool) {
    let mut store = InMemoryStore::default();
    let (stage, registry) = typed_tree(
        &mut store,
        vec![(
            "points",
            PropertySpec::attribute()
                .with_type(PropertyType::new("float", true, Value::Float(0.0)))
                .with_time_samples(samples),
        )],
    );
    let tree = store.path("/Tree");
    let points = store.tokens.intern("points");
    let linear = InterpolationType::Linear;

    let explained = stage
        .explain_value_at_time_with_schema(tree, points, 5.0, linear, &store, &registry, None)
        .expect("authored");
    // `[0, 0]` and `[30, 20]`, the fallback supplying the `20`.
    assert_eq!(explained.value, Some(floats(&[15.0, 10.0])));
    assert_eq!(
        explained.value,
        stage
            .resolve_value_at_time_with_schema(tree, points, 5.0, linear, &store, &registry, None)
            .map(|resolved| resolved.value)
    );
    assert!(explained.seeded_by_fallback);
    assert_eq!(
        explained.source,
        ValueSource::TimeSamples {
            lower: 0.0,
            upper: 10.0,
            interpolation: linear,
            lower_seeded_by_fallback: lower_seeded,
            upper_seeded_by_fallback: !lower_seeded,
        }
    );
    let (lower_role, upper_role) = if lower_seeded {
        (EDIT, VALUE)
    } else {
        (VALUE, EDIT)
    };
    assert_eq!(
        explained.opinions[0].samples,
        vec![
            SampleUse {
                time: 0.0,
                role: lower_role
            },
            SampleUse {
                time: 10.0,
                role: upper_role
            },
        ]
    );
}

#[test]
fn fallback_use_is_reported_per_composed_sample() {
    check_mixed_samples(
        vec![(0.0, floats(&[0.0, 0.0])), (10.0, write_float(30.0, 0))],
        false,
    );
    check_mixed_samples(
        vec![(0.0, write_float(30.0, 0)), (10.0, floats(&[0.0, 0.0]))],
        true,
    );
}
