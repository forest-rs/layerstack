// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{boxed::Box, string::ToString, vec, vec::Vec};

use super::*;
use crate::{
    ArcKind, FieldValue, InMemoryStore, Layer, LayerId, LayerOffset, NodeId, PathId, PrimSpec,
    PropertySpec, PropertyType, Reference, SpecPath, Specifier, Stage, StageOptions, SublayerEntry,
    TargetPath, Time, TokenId, Value, VariantSetSpec, VariantSpec,
};

const SCENE: LayerId = LayerId(1);
const ROCK: LayerId = LayerId(2);
const SCENE_SUB: LayerId = LayerId(3);

fn double() -> PropertyType {
    PropertyType::parse("double").unwrap()
}

fn attr(value: f64) -> PropertySpec {
    PropertySpec::typed_attribute(double()).with_default(Value::Double(value))
}

/// Two rocks referencing one asset: `/World/RockA` plainly, `/World/RockB`
/// with `(offset = 10; scale = 2)`. The asset's `/Rock` has `size = 1`,
/// `spin` samples `0: 0, 10: 100`, and a `shape` variant set selecting
/// `round` (`roughness = 0.1`) over `jagged` (`roughness = 0.9`). The scene
/// has a sublayer offset by 5 frames.
fn rocks() -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let t = |store: &mut InMemoryStore, s: &str| store.tokens.intern(s);
    let (size, spin, roughness, shape, round, jagged) = (
        t(&mut store, "size"),
        t(&mut store, "spin"),
        t(&mut store, "roughness"),
        t(&mut store, "shape"),
        t(&mut store, "round"),
        t(&mut store, "jagged"),
    );
    let (rock_name, world_name, a_name, b_name) = (
        t(&mut store, "Rock"),
        t(&mut store, "World"),
        t(&mut store, "RockA"),
        t(&mut store, "RockB"),
    );
    let (root, rock, world, rock_a, rock_b) = (
        store.path("/"),
        store.path("/Rock"),
        store.path("/World"),
        store.path("/World/RockA"),
        store.path("/World/RockB"),
    );

    let mut asset = Layer::new(ROCK);
    asset.insert_prim(root, PrimSpec::default().with_children(vec![rock_name]));
    let mut rock_spec = PrimSpec::def()
        .with_property(size, attr(1.0))
        .with_property(
            spin,
            PropertySpec::typed_attribute(double()).with_time_samples(vec![
                (0.0, Value::Double(0.0)),
                (10.0, Value::Double(100.0)),
            ]),
        );
    let mut variants = crate::Variants::new();
    for (name, value) in [(round, 0.1), (jagged, 0.9)] {
        variants.insert(
            name,
            VariantSpec {
                properties: vec![crate::PropertyEntry {
                    name: roughness,
                    spec: attr(value).into(),
                }],
                ..VariantSpec::default()
            },
        );
    }
    rock_spec
        .rare_mut()
        .variant_sets
        .insert(shape, VariantSetSpec { variants });
    rock_spec
        .rare_mut()
        .variant_set_names
        .make_mut()
        .prepend
        .push(shape);
    rock_spec.rare_mut().variant_selections.insert(shape, round);
    asset.insert_prim(rock, rock_spec);
    store.insert_layer(asset);

    let mut scene = Layer::new(SCENE);
    scene.sublayers.push(SublayerEntry::with_offset(
        SCENE_SUB,
        LayerOffset {
            offset: 5.0,
            scale: 1.0,
        },
    ));
    scene.insert_prim(root, PrimSpec::default().with_children(vec![world_name]));
    scene.insert_prim(world, PrimSpec::def().with_children(vec![a_name, b_name]));
    scene.insert_prim(
        rock_a,
        PrimSpec::def().with_reference(Reference::new(ROCK, rock)),
    );
    let shifted = Reference {
        layer_offset: LayerOffset {
            offset: 10.0,
            scale: 2.0,
        },
        ..Reference::new(ROCK, rock)
    };
    scene.insert_prim(rock_b, PrimSpec::def().with_reference(shifted));
    store.insert_layer(scene);
    store.insert_layer(Layer::new(SCENE_SUB));
    store
}

fn compose(store: &mut InMemoryStore) -> Stage {
    Stage::compose(store, SCENE, StageOptions::default())
}

fn spec(store: &mut InMemoryStore, text: &str) -> SpecPath {
    SpecPath::parse(text, &store.tokens, &store.paths).expect("spec path")
}

fn node(stage: &Stage, prim: PathId, kind: ArcKind) -> NodeId {
    let graph = stage.explain_prim_graph(prim).expect("graph");
    graph
        .depth_first()
        .into_iter()
        .find(|id| graph.node(*id).is_some_and(|n| n.arc_kind() == kind))
        .expect("node of that kind")
}

fn layers(store: &InMemoryStore) -> Vec<Layer> {
    let mut out: Vec<Layer> = store.layers.values().cloned().collect();
    out.sort_by_key(|layer| layer.id);
    out
}

fn authored_default(store: &mut InMemoryStore, layer: LayerId, path: &str) -> Option<Value> {
    let path = spec(store, path);
    store.layers[&layer].property_at(&path)?.default.clone()
}

#[test]
fn targets_map_through_reference_and_variant_nodes() {
    let mut store = rocks();
    let stage = compose(&mut store);
    let (rock_a, rock_b) = (store.path("/World/RockA"), store.path("/World/RockB"));
    let child = store.path("/World/RockB/Pebble");
    let world = store.path("/World");

    let local = EditTarget::for_node(&stage, rock_b, NodeId::ROOT).expect("root");
    assert_eq!(
        local,
        EditTarget::for_layer(SCENE),
        "the root node maps identically"
    );

    let reference = EditTarget::for_node(&stage, rock_b, node(&stage, rock_b, ArcKind::References))
        .expect("reference node");
    assert_eq!(reference.layer(), ROCK);
    let mapped = |target: &EditTarget, store: &mut InMemoryStore, path| {
        target
            .map_to_spec_path(path, &store.paths)
            .map(|spec| spec.display(&store.paths, &store.tokens))
    };
    assert_eq!(
        mapped(&reference, &mut store, rock_b).as_deref(),
        Some("/Rock")
    );
    assert_eq!(
        mapped(&reference, &mut store, child).as_deref(),
        Some("/Rock/Pebble")
    );
    assert_eq!(
        mapped(&reference, &mut store, world),
        None,
        "outside the arc"
    );
    assert_eq!(mapped(&reference, &mut store, rock_a), None);
    assert_eq!(reference.map_to_spec_time(14.0), 2.0, "(14 - 10) / 2");

    let variant = EditTarget::for_node(&stage, rock_a, node(&stage, rock_a, ArcKind::Variants))
        .expect("variant node");
    let size = store.property_path("/World/RockA.roughness");
    assert_eq!(
        variant
            .map_property_to_spec_path(size, &store.paths)
            .map(|spec| spec.display(&store.paths, &store.tokens))
            .as_deref(),
        Some("/Rock{shape=round}.roughness")
    );

    let sub = EditTarget::for_node_layer(&stage, &store, rock_b, NodeId::ROOT, SCENE_SUB)
        .expect("sublayer of the root stack");
    assert_eq!(sub.layer(), SCENE_SUB);
    assert_eq!(sub.map_to_spec_time(7.0), 2.0, "the sublayer's own offset");
    assert!(EditTarget::for_node_layer(&stage, &store, rock_b, NodeId::ROOT, ROCK).is_none());

    let jagged = spec(&mut store, "/Rock{shape=jagged}");
    let local_variant = EditTarget::for_local_variant(ROCK, &jagged);
    let rock = store.path("/Rock");
    assert_eq!(
        mapped(&local_variant, &mut store, rock).as_deref(),
        Some("/Rock{shape=jagged}")
    );
    // Outside the branch, a local variant target maps every path to itself
    // (OpenUSD: `ForLocalDirectVariant`'s root identity); a reference's
    // does not (checked above).
    assert_eq!(
        mapped(&local_variant, &mut store, world).as_deref(),
        Some("/World")
    );
}

#[test]
fn edits_write_through_targets_and_undo_exactly() {
    let mut store = rocks();
    let original = layers(&store);
    let stage = compose(&mut store);
    let rock_b = store.path("/World/RockB");
    let through_b = EditTarget::for_node(&stage, rock_b, node(&stage, rock_b, ArcKind::References))
        .expect("reference node");
    let size_b = store.property_path("/World/RockB.size");
    let spin_b = store.property_path("/World/RockB.spin");
    let size_a = store.property_path("/World/RockA.size");

    let mut edit = Transaction::new();
    edit.set_default(
        EditTarget::for_layer(SCENE).property(size_a),
        Value::Double(2.0),
    )
    .set_default(through_b.property(size_b), Value::Double(3.0))
    .set_time_sample(through_b.property(spin_b), 14.0, Value::Double(70.0));
    let inverse = edit
        .apply(&mut store)
        .expect_err("the scene has no declaration of size to create the spec with");
    assert!(matches!(
        inverse,
        EditError::Rejected {
            op: 0,
            reason: Rejection::UndeclaredType(_)
        }
    ));
    assert_eq!(layers(&store), original, "nothing applied");

    let mut edit = Transaction::new();
    edit.create_property(EditTarget::for_layer(SCENE).property(size_a), attr(2.0))
        .set_default(through_b.property(size_b), Value::Double(3.0))
        .set_time_sample(through_b.property(spin_b), 14.0, Value::Double(70.0));
    let inverse = edit.apply(&mut store).expect("applies");
    assert_eq!(
        authored_default(&mut store, SCENE, "/World/RockA.size"),
        Some(Value::Double(2.0))
    );
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock.size"),
        Some(Value::Double(3.0))
    );
    let spin = spec(&mut store, "/Rock.spin");
    let samples = |store: &InMemoryStore| {
        store.layers[&ROCK]
            .property_at(&spin)
            .and_then(|p| p.time_samples.get().map(<[_]>::to_vec))
    };
    assert_eq!(
        samples(&store),
        Some(vec![
            (0.0, Value::Double(0.0)),
            (2.0, Value::Double(70.0)),
            (10.0, Value::Double(100.0)),
        ]),
        "stage time 14 is layer time 2"
    );

    let redo = inverse.apply(&mut store).expect("undo applies");
    assert_eq!(
        layers(&store),
        original,
        "undo restores both layers exactly"
    );
    let size_a_spec = spec(&mut store, "/World/RockA.size");
    assert!(
        store.layers[&SCENE].property_at(&size_a_spec).is_none(),
        "undo removes the created spec"
    );
    redo.apply(&mut store).expect("redo applies");
    assert_eq!(
        authored_default(&mut store, SCENE, "/World/RockA.size"),
        Some(Value::Double(2.0))
    );
}

#[test]
fn a_sample_inverse_touches_only_its_key() {
    let mut store = rocks();
    let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
    let mut first = Transaction::new();
    first.set_time_sample(spin.clone(), 2.0, Value::Double(70.0));
    let undo_first = first.apply(&mut store).expect("applies");
    let mut second = Transaction::new();
    second
        .set_time_sample(spin.clone(), 5.0, Value::Double(50.0))
        .set_time_sample(spin.clone(), 10.0, Value::Double(90.0));
    let _undo_second = second.apply(&mut store).expect("applies");

    // Undoing the first edit out of order keeps the second's samples.
    undo_first.apply(&mut store).expect("applies");
    let path = spec(&mut store, "/Rock.spin");
    assert_eq!(
        store.layers[&ROCK]
            .property_at(&path)
            .and_then(|p| p.time_samples.get().map(<[_]>::to_vec)),
        Some(vec![
            (0.0, Value::Double(0.0)),
            (5.0, Value::Double(50.0)),
            (10.0, Value::Double(90.0)),
        ])
    );

    let mut remove = Transaction::new();
    remove
        .remove_time_sample(spin.clone(), 0.0)
        .remove_time_sample(spin.clone(), 5.0)
        .remove_time_sample(spin.clone(), 10.0)
        .remove_time_sample(spin, 3.0);
    let before = layers(&store);
    let undo = remove.apply(&mut store).expect("applies");
    assert_eq!(
        store.layers[&ROCK]
            .property_at(&path)
            .map(|p| p.time_samples.get().map(<[_]>::to_vec)),
        Some(None),
        "removing the last sample removes the field"
    );
    undo.apply(&mut store).expect("applies");
    assert_eq!(layers(&store), before);
}

#[test]
fn values_must_conform_to_the_declared_type() {
    let mut store = rocks();
    let original = layers(&store);
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let roughness = Address::spec(ROCK, spec(&mut store, "/Rock{shape=jagged}.roughness"));
    let mut edit = Transaction::new();
    edit.set_default(size.clone(), Value::Double(11.0))
        .set_default(roughness.clone(), Value::string("rough"));
    let error = edit
        .apply(&mut store)
        .expect_err("a string is not a double");
    assert!(matches!(
        error,
        EditError::Rejected {
            op: 1,
            reason: Rejection::TypeMismatch {
                offered: "string",
                ..
            }
        }
    ));
    assert_eq!(
        error.display(&store).to_string(),
        "edit 1 rejected: the value is a `string`, but /Rock{shape=jagged}.roughness is a `double`"
    );
    assert_eq!(layers(&store), original, "the first edit was rolled back");
    assert_eq!(
        store.layers[&ROCK].generation(),
        original[1].generation(),
        "a failed transaction leaves the generation"
    );

    let mut block = Transaction::new();
    block.set_default(roughness, Value::Blocked);
    block.apply(&mut store).expect("a block fits any type");

    let color3f = PropertyType::parse("color3f[]").unwrap();
    let color = Address::spec(ROCK, spec(&mut store, "/Rock.primvars:color"));
    let mut create = Transaction::new();
    create.create_property(
        color.clone(),
        PropertySpec::typed_attribute(color3f)
            .with_default(Value::Array(vec![Value::Vec3f([0.5; 3])])),
    );
    create
        .apply(&mut store)
        .expect("an array of the element type");
    for components in [
        Value::Array(vec![Value::Float(0.1); 3]),
        Value::array(vec![Value::Float(0.1); 3]),
    ] {
        let mut tuple = Transaction::new();
        tuple.set_default(color.clone(), Value::Array(vec![components]));
        tuple
            .apply(&mut store)
            .expect("tuples may be spelled as arrays");
    }

    let tint = Address::spec(ROCK, spec(&mut store, "/Rock.tint"));
    let mut tuple = Transaction::new();
    tuple.create_property(
        tint.clone(),
        PropertySpec::typed_attribute(PropertyType::parse("color3f").unwrap())
            .with_default(Value::array(vec![Value::Float(0.1); 3])),
    );
    tuple
        .apply(&mut store)
        .expect("native scalar tuple components");
    for wrong in [
        Value::array(vec![Value::Float(0.1); 2]),
        Value::array(vec![Value::Double(0.1); 3]),
    ] {
        let mut tuple = Transaction::new();
        tuple.set_default(tint.clone(), wrong);
        assert!(matches!(
            tuple.apply(&mut store),
            Err(EditError::Rejected {
                reason: Rejection::TypeMismatch { .. },
                ..
            })
        ));
    }
    let mut wrong_edit = Transaction::new();
    wrong_edit.set_default(
        color.clone(),
        Value::typed_array_edit(
            crate::ArrayEdit::default(),
            PropertyType::parse("float[]").unwrap(),
        ),
    );
    assert!(matches!(
        wrong_edit.apply(&mut store),
        Err(EditError::Rejected {
            reason: Rejection::TypeMismatch { .. },
            ..
        })
    ));
    let mut wrong_literal = Transaction::new();
    wrong_literal.set_default(
        color.clone(),
        Value::typed_array_edit(
            crate::ArrayEdit {
                ops: vec![crate::ArrayEditOp::ResizeFill {
                    len: 1,
                    fill: Value::Int(7),
                }],
            },
            PropertyType::parse("point3f[]").unwrap(),
        ),
    );
    assert!(matches!(
        wrong_literal.apply(&mut store),
        Err(EditError::Rejected {
            reason: Rejection::TypeMismatch { .. },
            ..
        })
    ));
    let mut alias_edit = Transaction::new();
    alias_edit.set_default(
        color.clone(),
        Value::typed_array_edit(
            crate::ArrayEdit::default(),
            PropertyType::parse("point3f[]").unwrap(),
        ),
    );
    alias_edit
        .apply(&mut store)
        .expect("role aliases share the actual storage kind");
    let mut scalar = Transaction::new();
    scalar.set_default(color, Value::Vec3f([0.5; 3]));
    assert!(
        scalar.apply(&mut store).is_err(),
        "a scalar is not an array"
    );

    let relationship = Address::spec(ROCK, spec(&mut store, "/Rock.friend"));
    let mut rel = Transaction::new();
    rel.create_property(relationship.clone(), PropertySpec::relationship());
    rel.apply(&mut store).expect("creates a relationship");
    let mut not_attr = Transaction::new();
    not_attr.set_default(relationship.clone(), Value::Double(1.0));
    assert!(matches!(
        not_attr.apply(&mut store),
        Err(EditError::Rejected {
            reason: Rejection::NotAnAttribute(_),
            ..
        })
    ));
}

#[test]
fn unmappable_paths_are_rejected_when_applied() {
    let mut store = rocks();
    let original = layers(&store);
    let stage = compose(&mut store);
    let rock_b = store.path("/World/RockB");
    let through_b = EditTarget::for_node(&stage, rock_b, node(&stage, rock_b, ArcKind::References))
        .expect("reference node");
    let size_a = store.property_path("/World/RockA.size");
    let size_b = store.property_path("/World/RockB.size");
    let mut edit = Transaction::new();
    edit.set_default(through_b.property(size_b), Value::Double(10.0))
        .set_default(through_b.property(size_a), Value::Double(11.0));
    let error = edit.apply(&mut store).unwrap_err();
    assert_eq!(
        error,
        EditError::Rejected {
            op: 1,
            reason: Rejection::Unmappable(TargetPath::Property(size_a))
        }
    );
    assert_eq!(
        error.display(&store).to_string(),
        "edit 1 rejected: the edit target does not map /World/RockA.size"
    );
    assert_eq!(layers(&store), original);
}

#[test]
fn prims_variants_and_selections_are_created_and_removed() {
    let mut store = rocks();
    let original = layers(&store);
    let (shape, jagged, smooth) = (
        store.tokens.intern("shape"),
        store.tokens.intern("jagged"),
        store.tokens.intern("smooth"),
    );
    let kind = store.tokens.intern("kind");
    let mut edit = Transaction::new();
    edit.create_prim(
        Address::spec(SCENE, spec(&mut store, "/World/Grove/Tree")),
        Specifier::Def,
        None,
    )
    .set_variant_selection(
        Address::spec(SCENE, spec(&mut store, "/World/RockA")),
        shape,
        Some(jagged),
    )
    .create_property(
        Address::spec(ROCK, spec(&mut store, "/Rock{shape=smooth}Moss.height")),
        attr(0.2),
    )
    .set_metadata(
        Address::spec(ROCK, spec(&mut store, "/Rock{shape=smooth}")),
        kind,
        FieldValue::Value(Value::string("component")),
    );
    let undo = edit.apply(&mut store).expect("applies");

    let grove = store.path("/World/Grove");
    assert_eq!(
        store.layers[&SCENE].prims[&grove].specifier,
        Some(Specifier::Over),
        "a missing ancestor is created as an over"
    );
    let world = store.path("/World");
    let grove_name = store.tokens.intern("Grove");
    assert!(
        store.layers[&SCENE].prims[&world]
            .authored_children
            .contains(&grove_name)
    );
    let tree = store.path("/World/Grove/Tree");
    assert_eq!(
        store.layers[&SCENE].prims[&tree].specifier,
        Some(Specifier::Def)
    );
    let rock_a = spec(&mut store, "/World/RockA");
    assert_eq!(
        store.layers[&SCENE].variant_selection_at(&rock_a, shape),
        Some(jagged)
    );
    let rock = store.path("/Rock");
    let set = &store.layers[&ROCK].prims[&rock].variant_sets()[&shape];
    assert!(
        set.variants[&smooth]
            .authored_children
            .contains(&store.tokens.intern("Moss"))
    );
    let (moss, smooth_branch) = (
        spec(&mut store, "/Rock{shape=smooth}Moss"),
        spec(&mut store, "/Rock{shape=smooth}"),
    );
    assert!(store.layers[&ROCK].has_spec_at(&moss));
    assert_eq!(
        store.layers[&ROCK].metadata_at(&smooth_branch, kind),
        Some(&FieldValue::Value(Value::string("component")))
    );

    // The new branch composes once selected.
    let stage = compose(&mut store);
    assert!(stage.has_prim(store.path("/World/RockA")));

    let mut exists = Transaction::new();
    exists.create_prim(
        Address::spec(SCENE, spec(&mut store, "/World/Grove/Tree")),
        Specifier::Def,
        None,
    );
    assert!(matches!(
        exists.apply(&mut store),
        Err(EditError::Rejected {
            reason: Rejection::SpecExists(_),
            ..
        })
    ));

    let mut remove = Transaction::new();
    remove
        .remove_spec(Address::spec(SCENE, spec(&mut store, "/World/Grove")))
        .remove_spec(Address::spec(ROCK, spec(&mut store, "/Rock{shape=smooth}")));
    let before_remove = layers(&store);
    let undo_remove = remove.apply(&mut store).expect("applies");
    assert!(
        !store.layers[&SCENE].prims.contains_key(&tree),
        "the subtree goes"
    );
    assert!(!store.layers[&ROCK].has_spec_at(&moss));
    undo_remove.apply(&mut store).expect("applies");
    assert_eq!(layers(&store), before_remove);

    undo.apply(&mut store).expect("applies");
    assert_eq!(
        layers(&store),
        original,
        "undo removes what the edit created"
    );
}

#[test]
fn metadata_is_set_blocked_and_cleared() {
    let mut store = rocks();
    let original = layers(&store);
    let (doc, interpolation, active) = (
        store.tokens.intern("documentation"),
        store.tokens.intern("interpolation"),
        store.tokens.intern("active"),
    );
    let rock = Address::spec(ROCK, spec(&mut store, "/Rock"));
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let missing = Address::spec(ROCK, spec(&mut store, "/Rock.missing"));
    let mut edit = Transaction::new();
    edit.set_metadata(
        rock.clone(),
        doc,
        FieldValue::Value(Value::string("a rock")),
    )
    .block_metadata(size.clone(), interpolation);
    let undo = edit.apply(&mut store).expect("applies");
    let size_path = spec(&mut store, "/Rock.size");
    assert_eq!(
        store.layers[&ROCK].metadata_at(&size_path, interpolation),
        Some(&FieldValue::Value(Value::Blocked))
    );
    let mut clear = Transaction::new();
    clear
        .clear_metadata(rock.clone(), doc)
        .clear_metadata(size, interpolation);
    clear.apply(&mut store).expect("applies");
    assert_eq!(layers(&store), original);
    // The first edit's inverse finds its fields gone: it refuses to guess.
    assert!(matches!(
        undo.apply(&mut store),
        Err(EditError::Rejected {
            reason: Rejection::Diverged(_),
            ..
        })
    ));
    assert_eq!(layers(&store), original);

    let mut reserved = Transaction::new();
    reserved.set_metadata(rock, active, FieldValue::Value(Value::Bool(false)));
    assert!(matches!(
        reserved.apply(&mut store),
        Err(EditError::Rejected {
            reason: Rejection::ReservedField(_),
            ..
        })
    ));
    let mut no_property = Transaction::new();
    no_property.set_metadata(missing, doc, FieldValue::Value(Value::string("x")));
    assert!(matches!(
        no_property.apply(&mut store),
        Err(EditError::Rejected {
            reason: Rejection::NoSuchSpec(_),
            ..
        })
    ));
}

#[test]
fn stale_preparations_fail_even_when_the_value_is_back() {
    let mut store = rocks();
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let mut prepared = Transaction::new();
    prepared.set_default(size.clone(), Value::Double(5.0));
    prepared.expect_unchanged(&mut store);

    // Something else writes the spec and puts the value back.
    let path = store.property_path("/Rock.size");
    let layer = store.layers.get_mut(&ROCK).expect("rock");
    layer.property_mut(path).expect("size").default = Some(Value::Double(7.0));
    layer.property_mut(path).expect("size").default = Some(Value::Double(1.0));
    let before = layers(&store);
    assert!(matches!(
        prepared.apply(&mut store),
        Err(EditError::StaleGeneration { layer: ROCK, .. })
    ));
    assert_eq!(layers(&store), before);

    // A value expectation fails on its own when the value differs.
    let mut guarded = Transaction::new();
    guarded
        .expect_default(size.clone(), Some(Value::Double(4.0)))
        .set_default(size.clone(), Value::Double(5.0));
    assert!(matches!(
        guarded.apply(&mut store),
        Err(EditError::StaleValue { layer: ROCK, .. })
    ));
    let mut fresh = Transaction::new();
    fresh.set_default(size, Value::Double(5.0));
    fresh.expect_unchanged(&mut store);
    let generation = store.layers[&ROCK].generation();
    fresh.apply(&mut store).expect("nothing changed since");
    assert_eq!(
        store.layers[&ROCK].generation(),
        generation + 1,
        "one transaction moves the generation once"
    );
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).expect("small")
    }
}

/// A random edit over a small vocabulary of rock specs; many are invalid
/// or no-ops on purpose.
fn random_edit(rng: &mut Rng, store: &mut InMemoryStore, txn: &mut Transaction) {
    const PRIMS: &[&str] = &[
        "/Rock",
        "/Rock{shape=jagged}",
        "/Rock{shape=round}",
        "/Rock{shape=smooth}",
        "/Rock/Pebble",
        "/Rock{shape=jagged}Pebble",
        "/World/RockA",
        "/World/Grove",
    ];
    const NAMES: &[&str] = &["size", "roughness", "spin", "height"];
    let layer = [SCENE, ROCK, SCENE_SUB][rng.below(3)];
    let prim = PRIMS[rng.below(PRIMS.len())];
    let name = NAMES[rng.below(NAMES.len())];
    let prim_at = Address::spec(layer, spec(store, prim));
    let prop_at = Address::spec(layer, spec(store, &alloc::format!("{prim}.{name}")));
    let time = [0.0, 2.0, 5.0, 10.0][rng.below(4)];
    let number = Value::Double(f64::from(u32::try_from(rng.below(100)).expect("small")));
    let shape = store.tokens.intern("shape");
    let kind = store.tokens.intern("kind");
    let variant = [
        store.tokens.intern("jagged"),
        store.tokens.intern("round"),
        store.tokens.intern("smooth"),
    ][rng.below(3)];
    match rng.below(12) {
        0 => txn.create_prim(prim_at, Specifier::Def, None),
        1 => txn.create_property(prop_at, attr(1.0)),
        2 => txn.remove_spec(prim_at),
        3 => txn.remove_spec(prop_at),
        4 => txn.set_default(prop_at, number),
        5 => txn.clear_default(prop_at),
        6 => txn.set_time_sample(prop_at, time, number),
        7 => txn.remove_time_sample(prop_at, time),
        8 => txn.set_metadata(prim_at, kind, FieldValue::Value(Value::string("group"))),
        9 => txn.clear_metadata(prim_at, kind),
        10 => txn.set_variant_selection(prim_at, shape, Some(variant)),
        _ => txn.set_variant_selection(prim_at, shape, None),
    };
}

/// For any sequence of transactions, applying the inverses of those that
/// applied, newest first, restores every layer exactly; a transaction that
/// fails changes nothing.
#[test]
fn inverses_in_reverse_order_restore_every_layer() {
    let (mut applied, mut rejected, mut steps) = (0, 0, 0);
    for seed in 1..=64_u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut store = rocks();
        let mut history = vec![layers(&store)];
        let mut inverses = Vec::new();
        for _ in 0..24 {
            let mut txn = Transaction::new();
            for _ in 0..=rng.below(4) {
                random_edit(&mut rng, &mut store, &mut txn);
            }
            let before = layers(&store);
            match txn.apply(&mut store) {
                Ok(inverse) => {
                    applied += 1;
                    steps += inverse.len();
                    inverses.push(inverse);
                    history.push(layers(&store));
                }
                Err(error) => {
                    rejected += 1;
                    assert_eq!(layers(&store), before, "seed {seed}: {error}");
                }
            }
        }
        while let Some(inverse) = inverses.pop() {
            history.pop();
            inverse
                .apply(&mut store)
                .unwrap_or_else(|error| panic!("seed {seed}: {error}"));
            assert_eq!(
                layers(&store),
                *history.last().expect("state"),
                "seed {seed}: undo restores the state before the transaction"
            );
        }
    }
    // The vocabulary exercises both outcomes, with real edits.
    assert!(
        applied > 500 && rejected > 100,
        "{applied} applied, {rejected} rejected"
    );
    assert!(steps > 2 * applied, "{steps} inverse steps");
}

#[test]
fn generations_move_once_per_edited_layer() {
    let mut store = rocks();
    let before: Vec<u64> = [SCENE, ROCK]
        .map(|id| store.layers[&id].generation())
        .to_vec();
    let kind: TokenId = store.tokens.intern("kind");
    let mut noop = Transaction::new();
    noop.clear_metadata(Address::spec(ROCK, spec(&mut store, "/Rock")), kind);
    noop.apply(&mut store).expect("nothing to clear");
    let after: Vec<u64> = [SCENE, ROCK]
        .map(|id| store.layers[&id].generation())
        .to_vec();
    assert_eq!(before, after, "an edit that changes nothing moves nothing");
    assert!(store.layer_mut(ROCK).is_some());
}

/// Asserts that `live` serves what a fresh composition of `store` does.
fn assert_live_matches_fresh(live: &crate::LiveStage, store: &mut InMemoryStore, context: &str) {
    let fresh = compose(store);
    let stage = live.stage();
    let root = store.path("/");
    let order: Vec<PathId> = stage.traverse(root).collect();
    assert_eq!(
        order,
        fresh.traverse(root).collect::<Vec<_>>(),
        "{context}: prims"
    );
    let names: Vec<TokenId> = ["size", "roughness", "spin", "height", "kind"]
        .map(|name| store.tokens.intern(name))
        .to_vec();
    for prim in order {
        assert_eq!(
            stage.children_of(prim),
            fresh.children_of(prim),
            "{context}"
        );
        assert_eq!(
            stage.variant_selections(prim, store),
            fresh.variant_selections(prim, store),
            "{context}: selections at {}",
            store.paths.display(prim, &store.tokens)
        );
        for &name in &names {
            assert_eq!(
                stage.resolve_value(prim, name),
                fresh.resolve_value(prim, name),
                "{context}: default of {}",
                store.tokens.resolve(name)
            );
            for time in [0.0, 2.0, 5.0, 14.0] {
                assert_eq!(
                    stage.resolve_value_at_time(prim, name, Time::at(time)),
                    fresh.resolve_value_at_time(prim, name, Time::at(time)),
                    "{context}: {} at {time}",
                    store.tokens.resolve(name)
                );
            }
        }
    }
}

/// Every edit through `LiveStage::apply`, and every undo, leaves the live
/// stage equal to a fresh composition; value edits recompose only the
/// prims that draw on the edited spec.
#[test]
fn live_stage_apply_recomposes_what_the_edits_affect() {
    let mut store = rocks();
    let mut live = crate::LiveStage::compose(&mut store, SCENE, StageOptions::default());
    let (rock_a, rock_b) = (store.path("/World/RockA"), store.path("/World/RockB"));
    let (shape, jagged) = (store.tokens.intern("shape"), store.tokens.intern("jagged"));
    let size_a = store.property_path("/World/RockA.size");
    let size_b = store.property_path("/World/RockB.size");
    let roughness_a = store.property_path("/World/RockA.roughness");
    let spin_b = store.property_path("/World/RockB.spin");
    let scene = EditTarget::for_layer(SCENE);
    let mut undo = Vec::new();

    // a: a local override, typed from the composed declaration.
    let mut edit = Transaction::new();
    edit.set_default(scene.property(size_a), Value::Double(2.0));
    let applied = live.apply(&mut store, &edit).expect("a");
    assert_eq!(applied.recomposed, [rock_a], "only RockA draws on its spec");
    undo.push(applied.inverse);
    assert_live_matches_fresh(&live, &mut store, "a");

    // b: the shared definition through RockB's reference.
    let through_b = EditTarget::for_node(
        live.stage(),
        rock_b,
        node(live.stage(), rock_b, ArcKind::References),
    )
    .expect("reference node");
    let mut edit = Transaction::new();
    edit.set_default(through_b.property(size_b), Value::Double(3.0));
    let applied = live.apply(&mut store, &edit).expect("b");
    let mut recomposed = applied.recomposed.clone();
    recomposed.sort_unstable();
    let mut both = vec![rock_a, rock_b];
    both.sort_unstable();
    assert_eq!(recomposed, both, "both rocks draw on /Rock");
    undo.push(applied.inverse);
    assert_live_matches_fresh(&live, &mut store, "b");

    // c: select a variant, then edit inside it through the variant node.
    let mut edit = Transaction::new();
    edit.set_variant_selection(scene.prim(rock_a), shape, Some(jagged));
    undo.push(live.apply(&mut store, &edit).expect("c select").inverse);
    assert_eq!(
        live.stage().variant_selections(rock_a, &store).get(shape),
        Some(jagged)
    );
    let in_jagged = EditTarget::for_node(
        live.stage(),
        rock_a,
        node(live.stage(), rock_a, ArcKind::Variants),
    )
    .expect("variant node");
    let mut edit = Transaction::new();
    edit.set_default(in_jagged.property(roughness_a), Value::Double(0.95));
    undo.push(live.apply(&mut store, &edit).expect("c edit").inverse);
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock{shape=jagged}.roughness"),
        Some(Value::Double(0.95))
    );
    assert_live_matches_fresh(&live, &mut store, "c");

    // d: a time sample through the offset reference.
    let mut edit = Transaction::new();
    edit.set_time_sample(through_b.property(spin_b), 14.0, Value::Double(70.0));
    undo.push(live.apply(&mut store, &edit).expect("d").inverse);
    assert_live_matches_fresh(&live, &mut store, "d");

    // e: undo everything, newest first.
    while let Some(inverse) = undo.pop() {
        live.apply(&mut store, &inverse).expect("undo");
        assert_live_matches_fresh(&live, &mut store, "undo");
    }
    assert_eq!(layers(&store), layers(&rocks()), "undo restores the layers");

    // f: an edit behind the stage's back is found by its generation.
    let rock_size = store.property_path("/Rock.size");
    store
        .layers
        .get_mut(&ROCK)
        .and_then(|layer| layer.property_mut(rock_size))
        .expect("size")
        .default = Some(Value::Double(7.0));
    assert_eq!(live.notify_changed_layers(&store), [ROCK]);
    live.recompose(&mut store);
    assert_live_matches_fresh(&live, &mut store, "f");
    assert_eq!(live.notify_changed_layers(&store), [], "seen now");
}

/// Random transactions through `LiveStage::apply`, then their inverses:
/// the live stage matches a fresh composition after every one.
#[test]
fn live_stage_stays_fresh_under_random_transactions() {
    for seed in 1..=12_u64 {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
        let mut store = rocks();
        let mut live = crate::LiveStage::compose(&mut store, SCENE, StageOptions::default());
        let mut inverses = Vec::new();
        for step in 0..10 {
            let mut txn = Transaction::new();
            for _ in 0..=rng.below(3) {
                random_edit(&mut rng, &mut store, &mut txn);
            }
            if let Ok(applied) = live.apply(&mut store, &txn) {
                inverses.push(applied.inverse);
            }
            let context = alloc::format!("seed {seed} step {step}");
            assert_live_matches_fresh(&live, &mut store, &context);
        }
        while let Some(inverse) = inverses.pop() {
            live.apply(&mut store, &inverse).expect("undo applies");
            let context = alloc::format!("seed {seed} undo");
            assert_live_matches_fresh(&live, &mut store, &context);
        }
        assert_eq!(layers(&store), layers(&rocks()), "seed {seed}");
    }
}

/// An inverse restores a slot only while it holds what the undone edit
/// wrote: undoing after another edit of the same slot fails and changes
/// nothing, so the later edit is not overwritten.
#[test]
fn an_inverse_does_not_overwrite_a_later_edit_of_its_slot() {
    let mut store = rocks();
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
    let mut edit = Transaction::new();
    edit.set_default(size.clone(), Value::Double(2.0))
        .set_time_sample(spin.clone(), 2.0, Value::Double(70.0));
    let undo = edit.apply(&mut store).expect("applies");

    let mut later = Transaction::new();
    later.set_default(size.clone(), Value::Double(3.0));
    later.apply(&mut store).expect("applies");
    let before = layers(&store);
    let stale = undo.apply(&mut store);
    assert!(
        matches!(
            stale,
            Err(EditError::StaleValue {
                layer: ROCK,
                slot: Slot::Default,
                ..
            })
        ),
        "{stale:?}"
    );
    assert_eq!(
        stale.unwrap_err().display(&store).to_string(),
        alloc::format!(
            "the default of /Rock.size in layer {} changed since the edit was prepared",
            ROCK.0
        )
    );
    assert_eq!(layers(&store), before, "the later edit stays");
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock.size"),
        Some(Value::Double(3.0))
    );

    // A later sample at the same time is guarded the same way; a created
    // spec is not removed once someone else wrote into it.
    let mut edit = Transaction::new();
    edit.create_property(
        Address::spec(ROCK, spec(&mut store, "/Rock.height")),
        attr(1.0),
    );
    let undo_create = edit.apply(&mut store).expect("applies");
    let mut later = Transaction::new();
    later.set_default(
        Address::spec(ROCK, spec(&mut store, "/Rock.height")),
        Value::Double(4.0),
    );
    later.apply(&mut store).expect("applies");
    assert!(matches!(
        undo_create.apply(&mut store),
        Err(EditError::StaleValue {
            slot: Slot::Spec,
            ..
        })
    ));
}

/// Undoing after edits of other slots, of the same spec, the same layer
/// or another layer, works and keeps those edits.
#[test]
fn an_inverse_applies_after_unrelated_edits() {
    let mut store = rocks();
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
    let jagged = Address::spec(ROCK, spec(&mut store, "/Rock{shape=jagged}.roughness"));
    let mut edit = Transaction::new();
    edit.set_default(size.clone(), Value::Double(2.0))
        .set_time_sample(spin.clone(), 2.0, Value::Double(70.0));
    let undo = edit.apply(&mut store).expect("applies");

    let mut unrelated = Transaction::new();
    unrelated
        .set_time_sample(spin.clone(), 5.0, Value::Double(50.0))
        .set_default(jagged, Value::Double(0.5))
        .create_property(
            Address::spec(SCENE, spec(&mut store, "/World/RockA.size")),
            attr(9.0),
        );
    unrelated.apply(&mut store).expect("applies");
    undo.apply(&mut store)
        .expect("other slots do not stop the undo");
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock.size"),
        Some(Value::Double(1.0))
    );
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock{shape=jagged}.roughness"),
        Some(Value::Double(0.5))
    );
    let spin_path = spec(&mut store, "/Rock.spin");
    assert_eq!(
        store.layers[&ROCK]
            .property_at(&spin_path)
            .and_then(|p| p.time_samples.get().map(<[_]>::to_vec)),
        Some(vec![
            (0.0, Value::Double(0.0)),
            (5.0, Value::Double(50.0)),
            (10.0, Value::Double(100.0)),
        ]),
        "only the undone sample goes"
    );
}

/// `expect_unchanged` prepares an inverse, or the redo an inverse returns,
/// against its layers' generations: any later edit of them, even of
/// another slot, makes it stale.
#[test]
fn prepared_inverses_and_redos_are_guarded_by_generation() {
    let mut store = rocks();
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let kind = store.tokens.intern("kind");
    let rock = Address::spec(ROCK, spec(&mut store, "/Rock"));
    let mut edit = Transaction::new();
    edit.set_default(size.clone(), Value::Double(2.0));
    let mut undo = edit.apply(&mut store).expect("applies");
    undo.expect_unchanged(&mut store);
    let mut unrelated = Transaction::new();
    unrelated.set_metadata(
        rock.clone(),
        kind,
        FieldValue::Value(Value::string("group")),
    );
    unrelated.apply(&mut store).expect("applies");
    let before = layers(&store);
    assert!(matches!(
        undo.apply(&mut store),
        Err(EditError::StaleGeneration { layer: ROCK, .. })
    ));
    assert_eq!(layers(&store), before);

    let mut undo = edit.apply(&mut store).expect("applies");
    undo.expect_unchanged(&mut store);
    let mut redo = undo.apply(&mut store).expect("nothing changed since");
    redo.expect_unchanged(&mut store);
    let mut later = Transaction::new();
    later.set_default(size.clone(), Value::Double(3.0));
    later.apply(&mut store).expect("applies");
    assert!(matches!(
        redo.apply(&mut store),
        Err(EditError::StaleGeneration { layer: ROCK, .. })
    ));
    redo.preconditions.clear();
    assert!(
        matches!(redo.apply(&mut store), Err(EditError::StaleValue { .. })),
        "unprepared, the redo is still guarded by the value it restores"
    );
}

/// A structural edit found by polling rebuilds the namespace; an
/// initially empty sublayer is polled too.
#[test]
fn live_stage_polls_structural_edits_and_empty_sublayers() {
    let mut store = rocks();
    let mut live = crate::LiveStage::compose(&mut store, SCENE, StageOptions::default());
    let new = store.path("/Rock/New");
    store
        .layers
        .get_mut(&ROCK)
        .expect("rock")
        .insert_prim(new, PrimSpec::def());
    assert_eq!(live.notify_changed_layers(&store), [ROCK]);
    live.recompose(&mut store);
    assert_live_matches_fresh(&live, &mut store, "a new prim spec");

    let added = store.path("/Added");
    store
        .layers
        .get_mut(&SCENE_SUB)
        .expect("sublayer")
        .insert_prim(added, PrimSpec::def());
    assert_eq!(live.notify_changed_layers(&store), [SCENE_SUB]);
    live.recompose(&mut store);
    assert_live_matches_fresh(&live, &mut store, "a prim in an empty sublayer");
}

/// Guards compare authored floats by bits: an unchanged NaN still matches
/// what the edit wrote, so the undo applies; a later edit from `+0.0` to
/// `-0.0`, or to another NaN payload, is a change, so the undo fails
/// with `StaleValue` and leaves it.
#[test]
fn guards_compare_floats_by_bits() {
    let nan = f64::from_bits(0x7ff8_0000_0000_0123);
    let other_nan = f64::from_bits(0x7ff8_0000_0000_0456);
    let mut store = rocks();
    let original = layers(&store);
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
    let size_path = store.property_path("/Rock.size");

    // Undo right after writing NaN, as a default and as a sample.
    let mut edit = Transaction::new();
    edit.set_default(size.clone(), Value::Double(nan))
        .set_time_sample(spin.clone(), 2.0, Value::Double(nan));
    let undo = edit.apply(&mut store).expect("applies");
    let redo = undo
        .apply(&mut store)
        .expect("an unchanged NaN is unchanged");
    assert_eq!(layers(&store), original);
    let undo = redo.apply(&mut store).expect("redo restores the NaN");
    let bits = store.layers[&ROCK]
        .property(size_path)
        .and_then(|p| p.default.clone());
    assert!(matches!(bits, Some(Value::Double(x)) if x.to_bits() == nan.to_bits()));
    undo.apply(&mut store).expect("undo again");

    // A later +0.0 → -0.0 edit is not overwritten.
    let mut edit = Transaction::new();
    edit.set_default(size.clone(), Value::Double(0.0));
    let undo = edit.apply(&mut store).expect("applies");
    let mut later = Transaction::new();
    later.set_default(size.clone(), Value::Double(-0.0));
    later.apply(&mut store).expect("applies");
    let before = layers(&store);
    assert!(matches!(
        undo.apply(&mut store),
        Err(EditError::StaleValue {
            slot: Slot::Default,
            ..
        })
    ));
    assert_eq!(layers(&store), before);
    let stored = store.layers[&ROCK]
        .property(size_path)
        .and_then(|p| p.default.clone());
    assert!(matches!(stored, Some(Value::Double(x)) if x.to_bits() == (-0.0_f64).to_bits()));

    // Nor is a later change of NaN payload in a sample.
    let mut edit = Transaction::new();
    edit.set_time_sample(spin.clone(), 3.0, Value::Double(nan));
    let undo = edit.apply(&mut store).expect("applies");
    let mut later = Transaction::new();
    later.set_time_sample(spin, 3.0, Value::Double(other_nan));
    later.apply(&mut store).expect("applies");
    assert!(matches!(
        undo.apply(&mut store),
        Err(EditError::StaleValue {
            slot: Slot::TimeSample(_),
            ..
        })
    ));
}

/// NaNs nested in an array, in metadata and in a created spec are guarded
/// by bits too, and a failed guarded transaction stays atomic.
#[test]
fn nested_nans_are_guarded_by_bits() {
    let mut store = rocks();
    let color = Address::spec(ROCK, spec(&mut store, "/Rock.primvars:color"));
    let color3f = PropertyType::parse("color3f[]").unwrap();
    let colors = |x: f32| Value::Array(vec![Value::Vec3f([0.5, x, 0.5])]);
    let limits = store.tokens.intern("limits");
    let rock = Address::spec(ROCK, spec(&mut store, "/Rock"));
    let dict =
        |x: f64| FieldValue::Value(Value::Dictionary(vec![("max".into(), Value::Double(x))]));
    let mut edit = Transaction::new();
    edit.create_property(
        color.clone(),
        PropertySpec::typed_attribute(color3f).with_default(colors(f32::NAN)),
    )
    .set_metadata(rock.clone(), limits, dict(f64::NAN));
    let undo = edit.apply(&mut store).expect("applies");
    let redo = undo
        .apply(&mut store)
        .expect("unchanged NaNs in arrays and dictionaries");
    let undo = redo.apply(&mut store).expect("redo");

    let mut later = Transaction::new();
    later.set_default(color.clone(), colors(-0.0));
    later.apply(&mut store).expect("applies");
    let mut first = Transaction::new();
    first.set_default(color, colors(0.0));
    let undo_first = first.apply(&mut store).expect("applies");
    let mut flip = Transaction::new();
    flip.set_default(
        Address::spec(ROCK, spec(&mut store, "/Rock.primvars:color")),
        colors(-0.0),
    );
    flip.apply(&mut store).expect("applies");
    // Layers holding NaN never compare equal; their debug text, counters
    // included, shows whether anything changed.
    let before = alloc::format!("{:?}", layers(&store));
    assert!(matches!(
        undo_first.apply(&mut store),
        Err(EditError::StaleValue { .. })
    ));
    assert_eq!(alloc::format!("{:?}", layers(&store)), before, "atomic");
    assert!(
        matches!(undo.apply(&mut store), Err(EditError::StaleValue { .. })),
        "the created spec was changed since"
    );
    assert_eq!(alloc::format!("{:?}", layers(&store)), before, "atomic");
}

/// `expect_default` compares by bits: a NaN expectation holds against the
/// same NaN, and a `+0.0` expectation fails against `-0.0`.
#[test]
fn value_preconditions_compare_floats_by_bits() {
    let nan = f64::from_bits(0x7ff8_0000_0000_0123);
    let mut store = rocks();
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let mut edit = Transaction::new();
    edit.set_default(size.clone(), Value::Double(nan));
    edit.apply(&mut store).expect("applies");
    let mut guarded = Transaction::new();
    guarded
        .expect_default(size.clone(), Some(Value::Double(nan)))
        .set_default(size.clone(), Value::Double(-0.0));
    guarded.apply(&mut store).expect("the same NaN is expected");
    let mut zero = Transaction::new();
    zero.expect_default(size.clone(), Some(Value::Double(0.0)))
        .set_default(size, Value::Double(1.0));
    assert!(matches!(
        zero.apply(&mut store),
        Err(EditError::StaleValue {
            slot: Slot::Default,
            ..
        })
    ));
}

/// A relocated prim is edited through the nodes it reads: the reference
/// node beneath the relocation maps it to the referenced child it moved.
/// Its relocate node, whose source specs never contribute, is no target.
#[test]
fn relocated_prims_edit_through_the_nodes_they_read() {
    let mut store = rocks();
    let (chip, chip_name) = (store.path("/Rock/Chip"), store.tokens.intern("Chip"));
    let (source, moved) = (
        store.path("/World/RockA/Chip"),
        store.path("/World/RockA/Moved"),
    );
    let rock = store.path("/Rock");
    let asset = store.layers.get_mut(&ROCK).expect("rock");
    asset.insert_prim(
        chip,
        PrimSpec::def().with_property(store.tokens.intern("size"), attr(1.0)),
    );
    asset
        .prims
        .get_mut(&rock)
        .expect("rock spec")
        .authored_children
        .push(chip_name);
    store
        .layers
        .get_mut(&SCENE)
        .expect("scene")
        .relocates_mut()
        .push(crate::doc::Relocate {
            source,
            target: Some(moved),
        });
    let mut live = crate::LiveStage::compose(&mut store, SCENE, StageOptions::default());
    assert!(live.stage().has_prim(moved) && !live.stage().has_prim(source));

    let graph = live.stage().explain_prim_graph(moved).expect("graph");
    let relocate = node(live.stage(), moved, ArcKind::Relocates);
    assert!(EditTarget::for_node(live.stage(), moved, relocate).is_none());
    let reference = graph
        .depth_first()
        .into_iter()
        .find(|id| {
            graph
                .node(*id)
                .is_some_and(|n| n.arc_kind() == ArcKind::References)
        })
        .expect("reference node");
    let target = EditTarget::for_node(live.stage(), moved, reference).expect("target");
    let size = store.property_path("/World/RockA/Moved.size");
    assert_eq!(
        target
            .map_property_to_spec_path(size, &store.paths)
            .map(|p| p.display(&store.paths, &store.tokens))
            .as_deref(),
        Some("/Rock/Chip.size")
    );
    let mut edit = Transaction::new();
    edit.set_default(target.property(size), Value::Double(4.0));
    live.apply(&mut store, &edit).expect("applies");
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock/Chip.size"),
        Some(Value::Double(4.0))
    );
    assert_live_matches_fresh(&live, &mut store, "relocated edit");
}

/// A branch the stage's variant fallbacks select is edited through its
/// variant node like any other: `/Rock` authors no `shape` selection, the
/// fallback `shape=[jagged]` selects `jagged`, and an edit through
/// `/World/RockA`'s variant node lands in `/Rock{shape=jagged}`. The live
/// stage then equals a fresh composition with the same fallbacks.
///
/// Spec: AOUSD Core §10.3.2.5; OpenUSD `PcpCache::SetVariantFallbacks`.
#[test]
fn live_stage_edits_a_branch_selected_by_a_fallback() {
    let mut store = rocks();
    let (rock, rock_a) = (store.path("/Rock"), store.path("/World/RockA"));
    let (shape, jagged, roughness) = (
        store.tokens.intern("shape"),
        store.tokens.intern("jagged"),
        store.tokens.intern("roughness"),
    );
    store
        .layers
        .get_mut(&ROCK)
        .and_then(|layer| layer.prims.get_mut(&rock))
        .expect("/Rock")
        .rare_mut()
        .variant_selections
        .clear();
    let options = StageOptions {
        variant_fallbacks: [(shape, vec![jagged])].into_iter().collect(),
        ..StageOptions::default()
    };
    let mut live = crate::LiveStage::compose(&mut store, SCENE, options.clone());
    assert_eq!(
        live.stage().variant_selections(rock_a, &store).get(shape),
        Some(jagged),
        "the fallback is the selection"
    );

    let in_jagged = EditTarget::for_node(
        live.stage(),
        rock_a,
        node(live.stage(), rock_a, ArcKind::Variants),
    )
    .expect("variant node");
    let roughness_a = store.property_path("/World/RockA.roughness");
    let mut edit = Transaction::new();
    edit.set_default(in_jagged.property(roughness_a), Value::Double(0.95));
    live.apply(&mut store, &edit).expect("edit");
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock{shape=jagged}.roughness"),
        Some(Value::Double(0.95))
    );

    let fresh = Stage::compose(&mut store, SCENE, options);
    let stage = live.stage();
    let root = store.path("/");
    let order: Vec<PathId> = stage.traverse(root).collect();
    assert_eq!(order, fresh.traverse(root).collect::<Vec<_>>());
    for prim in order {
        assert_eq!(
            stage.variant_selections(prim, &store),
            fresh.variant_selections(prim, &store)
        );
        let property = crate::PropertyPath::new(prim, roughness);
        assert_eq!(
            stage.resolve_field_path(property),
            fresh.resolve_field_path(property)
        );
    }
    assert_eq!(
        stage
            .resolve_field_path(roughness_a)
            .map(|resolved| resolved.value),
        Some(Value::Double(0.95))
    );
}

/// A variant spec nested in another branch of the same prim
/// (`/Rock{shape=round}{size=big}`) is created, with the set holding it,
/// inside the outer branch's variant spec. An opinion authored there
/// through `LiveStage::apply` composes once both branches are selected, the
/// live stage equals a fresh composition, and the inverse restores every
/// layer.
///
/// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set specs),
/// §10.3.2.5 (only the selected variant contributes).
#[test]
fn live_stage_edits_a_variant_spec_nested_in_a_branch() {
    let mut store = rocks();
    let original = layers(&store);
    let mut live = crate::LiveStage::compose(&mut store, SCENE, StageOptions::default());
    let (shape, round, size, big, height) = (
        store.tokens.intern("shape"),
        store.tokens.intern("round"),
        store.tokens.intern("size"),
        store.tokens.intern("big"),
        store.tokens.intern("height"),
    );
    let (rock, rock_a) = (store.path("/Rock"), store.path("/World/RockA"));

    let mut edit = Transaction::new();
    edit.create_property(
        Address::spec(
            ROCK,
            spec(&mut store, "/Rock{shape=round}{size=big}.height"),
        ),
        attr(3.0),
    )
    .set_variant_selection(
        Address::spec(ROCK, spec(&mut store, "/Rock{shape=round}")),
        size,
        Some(big),
    );
    let applied = live.apply(&mut store, &edit).expect("applies");

    let rock_spec = &store.layers[&ROCK].prims[&rock];
    assert!(
        !rock_spec.variant_sets().contains_key(&size),
        "the set is nested, not on the prim spec"
    );
    let outer = rock_spec
        .variant_spec(&[(shape, round)])
        .expect("outer branch");
    assert_eq!(
        outer.variant_set_names,
        crate::listop::ListOp::prepended(vec![size]),
        "creating the nested set lists it, as `SdfCreatePrimInLayer` does"
    );
    assert!(
        rock_spec
            .variant_spec(&[(shape, round), (size, big)])
            .is_some()
    );
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock{shape=round}{size=big}.height"),
        Some(Value::Double(3.0))
    );
    assert_eq!(
        live.stage()
            .resolve_field_path(crate::PropertyPath::new(rock_a, height))
            .map(|resolved| resolved.value),
        Some(Value::Double(3.0)),
        "the nested branch composes"
    );
    assert_live_matches_fresh(&live, &mut store, "nested branch");

    live.apply(&mut store, &applied.inverse).expect("undo");
    assert_eq!(layers(&store), original, "the inverse removes the set");
    assert_live_matches_fresh(&live, &mut store, "undone");
}

/// Variant sets compose only where `variantSetNames` lists them: adding a
/// set creates its spec and lists it, clearing the names leaves the spec
/// unlisted, and removing the set removes its spec and the branch's
/// opinions but not its listing. Every edit's inverse restores the layers.
///
/// Spec: AOUSD Core §7.3.6 (variant set specs), §7.6.2.3.5
/// (`variantSetNames`), §10.5 (variant selection). OpenUSD:
/// `UsdVariantSets::AddVariantSet`, `SdfPrimSpec::RemoveVariantSet`.
#[test]
fn variant_sets_compose_only_where_listed() {
    let mut store = rocks();
    let original = layers(&store);
    let mut live = crate::LiveStage::compose(&mut store, SCENE, StageOptions::default());
    let (shape, round, look, wet, shine) = (
        store.tokens.intern("shape"),
        store.tokens.intern("round"),
        store.tokens.intern("look"),
        store.tokens.intern("wet"),
        store.tokens.intern("shine"),
    );
    let (rock, rock_a) = (store.path("/Rock"), store.path("/World/RockA"));
    let at = Address::spec(ROCK, SpecPath::from_prim_path(rock));
    let shine_at = |live: &crate::LiveStage| {
        live.stage()
            .resolve_field_path(crate::PropertyPath::new(rock_a, shine))
            .map(|resolved| resolved.value)
    };
    let names_at =
        |store: &InMemoryStore| store.layers[&ROCK].prims[&rock].variant_set_names().clone();

    // Clearing the names leaves `shape` authored but unlisted.
    let mut clear = Transaction::new();
    clear.clear_variant_set_names(at.clone());
    let cleared = live.apply(&mut store, &clear).expect("clears");
    assert!(names_at(&store).is_empty());
    assert_eq!(live.stage().variant_sets(rock_a, &store), []);
    assert_live_matches_fresh(&live, &mut store, "unlisted");
    live.apply(&mut store, &cleared.inverse)
        .expect("undo clear");
    assert_eq!(layers(&store), original);

    // Adding `look` with a branch and a selection: it composes after
    // `shape`.
    let mut add = Transaction::new();
    add.add_variant_set(at.clone(), look, ListPosition::BackOfPrepend)
        .create_property(
            Address::spec(ROCK, spec(&mut store, "/Rock{look=wet}.shine")),
            attr(0.5),
        )
        .set_variant_selection(at.clone(), look, Some(wet));
    let added = live.apply(&mut store, &add).expect("adds");
    assert_eq!(
        names_at(&store),
        crate::ListOp::prepended(vec![shape, look])
    );
    assert_eq!(
        live.stage().variant_sets(rock_a, &store),
        [(shape, Some(round)), (look, Some(wet))]
    );
    assert_eq!(shine_at(&live), Some(Value::Double(0.5)));
    assert_live_matches_fresh(&live, &mut store, "added");

    // Removing the set removes its branch, and leaves it listed.
    let mut remove = Transaction::new();
    remove.remove_variant_set(at.clone(), look);
    let removed = live.apply(&mut store, &remove).expect("removes");
    assert!(
        !store.layers[&ROCK].prims[&rock]
            .variant_sets()
            .contains_key(&look)
    );
    assert_eq!(
        names_at(&store),
        crate::ListOp::prepended(vec![shape, look])
    );
    assert_eq!(shine_at(&live), None);
    assert_live_matches_fresh(&live, &mut store, "removed");

    live.apply(&mut store, &removed.inverse)
        .expect("undo remove");
    assert_eq!(shine_at(&live), Some(Value::Double(0.5)));
    live.apply(&mut store, &added.inverse).expect("undo add");
    assert_eq!(layers(&store), original, "the inverses restore the layers");
    assert_live_matches_fresh(&live, &mut store, "undone");

    // A name that is not an identifier is rejected.
    let bad = store.tokens.intern("1bad");
    let mut invalid = Transaction::new();
    invalid.add_variant_set(at, bad, ListPosition::BackOfPrepend);
    assert!(matches!(
        live.apply(&mut store, &invalid),
        Err(EditError::Rejected {
            reason: Rejection::InvalidVariantSetName(_),
            ..
        })
    ));
}

/// Variant set specs keep the order they were created in, whatever order
/// `variantSetNames` lists them in, and undoing a removal puts a set back
/// in its place.
///
/// OpenUSD: `SdfVariantSetSpec::New` appends to `variantSetChildren`
/// (`Sdf_ChildrenUtils::CreateSpec`). Spec: AOUSD Core §7.3.6.
#[test]
fn variant_sets_keep_creation_order_through_undo() {
    let mut store = rocks();
    let rock = store.path("/Rock");
    let at = Address::spec(ROCK, SpecPath::from_prim_path(rock));
    let [shape, look, age] = ["shape", "look", "age"].map(|name| store.tokens.intern(name));
    let sets = |store: &InMemoryStore| -> Vec<TokenId> {
        let prim = &store.layers[&ROCK].prims[&rock];
        prim.variant_sets().keys().copied().collect()
    };
    let mut add = Transaction::new();
    add.add_variant_set(at.clone(), look, ListPosition::FrontOfPrepend)
        .add_variant_set(at.clone(), age, ListPosition::FrontOfPrepend);
    add.apply(&mut store).expect("adds");
    assert_eq!(sets(&store), [shape, look, age], "in creation order");
    let original = layers(&store);

    let mut remove = Transaction::new();
    remove.remove_variant_set(at, shape);
    let inverse = remove.apply(&mut store).expect("removes");
    assert_eq!(sets(&store), [look, age]);
    inverse.apply(&mut store).expect("undo applies");
    assert_eq!(sets(&store), [shape, look, age], "back in its place");
    assert_eq!(layers(&store), original);
}

/// Relationship targets and attribute connections are set, replaced and
/// cleared on their spec, a missing relationship spec is created `custom`
/// (nothing declares it), and each edit's inverse restores the layer
/// exactly, with redo reapplying it.
///
/// Spec: AOUSD Core §7.6.4.2.3 (`connectionPaths`), §7.6.5.1.1
/// (`targetPaths`), §12.4.
#[test]
fn targets_and_connections_undo_and_redo_exactly() {
    use crate::{ListOp, PropertyKind, PropertyPath, TargetPath};

    let mut store = rocks();
    let rock = store.path("/Rock");
    let world = store.path("/World");
    let look = store.tokens.intern("look");
    let size = store.tokens.intern("size");
    let target = |path| TargetPath::Prim(path);
    let at = |name| EditTarget::for_layer(ROCK).property(PropertyPath::new(rock, name));
    let property = |store: &InMemoryStore, name| {
        store.layers[&ROCK].prims[&rock]
            .properties
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.spec.clone())
    };
    let original = layers(&store);

    // A missing relationship is created with its targets.
    let mut create = Transaction::new();
    create.set_targets(at(look), ListOp::explicit(vec![target(world)]));
    let undo_create = create.apply(&mut store).expect("creates");
    let created = property(&store, look).expect("relationship");
    assert_eq!(created.kind, PropertyKind::Relationship);
    assert!(created.custom, "nothing declares the relationship");
    assert_eq!(
        created.targets,
        Some(Box::new(ListOp::explicit(vec![target(world)])))
    );
    let after_create = layers(&store);

    // An attribute gains connections; the relationship's targets change.
    let mut edit = Transaction::new();
    edit.set_targets(at(size), ListOp::prepended(vec![target(world)]))
        .set_targets(at(look), ListOp::appended(vec![target(rock)]));
    let undo_edit = edit.apply(&mut store).expect("sets");
    assert_eq!(
        property(&store, size).and_then(|p| p.targets.clone()),
        Some(Box::new(ListOp::prepended(vec![target(world)])))
    );
    assert_eq!(
        property(&store, size).and_then(|p| p.default.clone()),
        Some(Value::Double(1.0))
    );
    let after_edit = layers(&store);

    // Clearing keeps the spec; clearing nothing does nothing.
    let mut clear = Transaction::new();
    clear.clear_targets(at(look)).clear_targets(at(look));
    let undo_clear = clear.apply(&mut store).expect("clears");
    assert_eq!(
        property(&store, look).map(|p| p.targets.clone()),
        Some(None)
    );

    let redo_clear = undo_clear.apply(&mut store).expect("undo clear");
    assert_eq!(layers(&store), after_edit);
    let redo_edit = undo_edit.apply(&mut store).expect("undo edit");
    assert_eq!(layers(&store), after_create);
    let redo_create = undo_create.apply(&mut store).expect("undo create");
    assert_eq!(layers(&store), original, "no relationship is left behind");

    redo_create.apply(&mut store).expect("redo create");
    redo_edit.apply(&mut store).expect("redo edit");
    assert_eq!(layers(&store), after_edit);
    redo_clear.apply(&mut store).expect("redo clear");
    assert_eq!(
        property(&store, look).map(|p| p.targets.clone()),
        Some(None)
    );
}

/// Applying an API schema adds it to the end of the spec's `apiSchemas`
/// prepends (or its explicit list), once, creating the prim spec as an
/// `over` when it is missing; the inverse restores the layer exactly.
///
/// OpenUSD: `UsdPrim::AddAppliedSchema`.
#[test]
fn applied_schemas_are_prepended_once_and_undo_exactly() {
    use crate::ListOp;

    let mut store = rocks();
    let rock = store.path("/Rock");
    let fresh = store.path("/Fresh");
    let api_schemas = store.tokens.intern("apiSchemas");
    let [label, slot, other] =
        ["LabelAPI", "SlotAPI:main", "OtherAPI"].map(|n| store.tokens.intern(n));
    let at = |path| EditTarget::for_layer(ROCK).prim(path);
    let list = |store: &InMemoryStore, path| {
        store.layers[&ROCK].prims.get(&path).and_then(|spec| {
            match crate::doc::get_field(&spec.fields, &api_schemas) {
                Some(FieldValue::TokenListOp(list)) => Some((**list).clone()),
                _ => None,
            }
        })
    };
    let original = layers(&store);

    let mut apply = Transaction::new();
    apply
        .add_applied_schema(at(rock), label)
        .add_applied_schema(at(rock), slot)
        .add_applied_schema(at(rock), label)
        .add_applied_schema(at(fresh), label);
    let undo = apply.apply(&mut store).expect("applies");
    assert_eq!(
        list(&store, rock),
        Some(ListOp::prepended(vec![label, slot]))
    );
    assert_eq!(
        store.layers[&ROCK].prims[&fresh].specifier,
        Some(Specifier::Over)
    );
    assert_eq!(list(&store, fresh), Some(ListOp::prepended(vec![label])));
    let applied = layers(&store);

    let redo = undo.apply(&mut store).expect("undo");
    assert_eq!(layers(&store), original);
    redo.apply(&mut store).expect("redo");
    assert_eq!(layers(&store), applied);

    // An explicit list gains the name at its end; appended names count.
    let mut explicit = Transaction::new();
    explicit.set_metadata(
        at(rock),
        api_schemas,
        FieldValue::TokenListOp(Box::new(ListOp::explicit(vec![other]))),
    );
    explicit.add_applied_schema(at(rock), label);
    explicit.apply(&mut store).expect("explicit");
    assert_eq!(
        list(&store, rock),
        Some(ListOp::explicit(vec![other, label]))
    );
    let mut appended = Transaction::new();
    appended.set_metadata(
        at(rock),
        api_schemas,
        FieldValue::TokenListOp(Box::new(ListOp::appended(vec![label]))),
    );
    appended.add_applied_schema(at(rock), label);
    appended.apply(&mut store).expect("appended");
    assert_eq!(list(&store, rock), Some(ListOp::appended(vec![label])));
}

/// Targets are added at each list position and removed one at a time:
/// an addition moves a target already in the chosen list, leaves it in
/// the other list and in the deleted targets, and goes to an explicit
/// list whatever the position; a removal deletes the target, once, or
/// takes it out of an explicit list. Removing from a property the layer
/// does not author creates the spec, holding the deletion, and a target
/// through a reference node maps to the asset's namespace. Undo is exact.
///
/// OpenUSD: `UsdRelationship::AddTarget` (`Usd_InsertListItem`) and
/// `RemoveTarget` (`SdfListEditorProxy::Remove`).
///
/// Spec: AOUSD Core §7.6.5.1.1 (`targetPaths`), §12.4 (list ops).
#[test]
fn targets_are_added_and_removed_one_at_a_time() {
    use crate::{ListOp, PropertyKind, PropertyPath, TargetPath};

    let mut store = rocks();
    let stage = compose(&mut store);
    let (rock, world) = (store.path("/Rock"), store.path("/World"));
    let (rock_b, pebble_b) = (
        store.path("/World/RockB"),
        store.path("/World/RockB/Pebble"),
    );
    let (a, b, c) = (
        TargetPath::Prim(store.path("/A")),
        TargetPath::Prim(store.path("/B")),
        TargetPath::Prim(store.path("/C")),
    );
    let (look, size) = (store.tokens.intern("look"), store.tokens.intern("size"));
    let at = |name| EditTarget::for_layer(ROCK).property(PropertyPath::new(rock, name));
    let targets = |store: &InMemoryStore, path, name| {
        store.layers[&ROCK].prims[&path]
            .properties
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| {
                (
                    entry.spec.kind,
                    entry.spec.custom,
                    entry.spec.targets.as_deref().cloned(),
                )
            })
    };
    let original = layers(&store);

    let mut add = Transaction::new();
    add.add_target(at(look), a, ListPosition::BackOfPrepend)
        .add_target(at(look), b, ListPosition::FrontOfPrepend)
        .add_target(at(look), c, ListPosition::BackOfAppend)
        .add_target(at(look), a, ListPosition::BackOfAppend)
        .add_target(at(look), b, ListPosition::BackOfPrepend)
        .remove_target(at(look), c)
        .remove_target(at(look), c);
    let undo_add = add.apply(&mut store).expect("adds and removes");
    assert_eq!(
        targets(&store, rock, look),
        Some((
            PropertyKind::Relationship,
            true,
            Some(ListOp {
                prepend: vec![a, b],
                append: vec![a],
                delete: vec![c],
                ..ListOp::default()
            })
        )),
        "a relationship nothing declares is created custom"
    );

    // An attribute's connections are edited as targets are, and an
    // explicit list takes every addition.
    let mut explicit = Transaction::new();
    explicit
        .set_targets(at(size), ListOp::explicit(vec![a, b]))
        .add_target(at(size), c, ListPosition::FrontOfAppend)
        .add_target(at(size), a, ListPosition::BackOfPrepend)
        .remove_target(at(size), b);
    let undo_explicit = explicit.apply(&mut store).expect("edits the explicit list");
    let (kind, _, connections) = targets(&store, rock, size).expect("the attribute");
    assert_eq!(kind, PropertyKind::Attribute);
    assert_eq!(connections, Some(ListOp::explicit(vec![c, a])));

    // Through the reference node of `/World/RockB`, the asset's `/Rock`
    // gains the deletion of the mapped target.
    let through_b = EditTarget::for_node(&stage, rock_b, node(&stage, rock_b, ArcKind::References))
        .expect("reference node");
    let shine = store.tokens.intern("shine");
    let mut remove = Transaction::new();
    remove.remove_target(
        through_b.property(PropertyPath::new(rock_b, shine)),
        TargetPath::Prim(pebble_b),
    );
    let undo_remove = remove.apply(&mut store).expect("removes");
    let pebble = TargetPath::Prim(store.path("/Rock/Pebble"));
    assert_eq!(
        targets(&store, rock, shine).and_then(|(_, _, targets)| targets),
        Some(ListOp {
            delete: vec![pebble],
            ..ListOp::default()
        })
    );
    let mut outside = Transaction::new();
    outside.add_target(
        through_b.property(PropertyPath::new(rock_b, shine)),
        TargetPath::Prim(world),
        ListPosition::BackOfAppend,
    );
    assert_eq!(
        outside.apply(&mut store),
        Err(EditError::Rejected {
            op: 0,
            reason: Rejection::UnmappableTarget(TargetPath::Prim(world)),
        })
    );

    for undo in [undo_remove, undo_explicit, undo_add] {
        undo.apply(&mut store).expect("undo");
    }
    assert_eq!(layers(&store), original);
}

/// Targets given through an edit target are stage paths, mapped to the
/// target layer's namespace as the property's own path is: through a
/// reference `/World/RockB/Pebble` is authored as `/Rock/Pebble`, and
/// through a variant the authored target names no variant selection. A
/// target the edit target does not map rejects the edit; the inverse
/// restores the layer exactly.
///
/// OpenUSD: `UsdRelationship::_GetTargetForAuthoring`
/// (`UsdEditTarget::MapToSpecPath`, variant selections stripped).
#[test]
fn targets_map_through_the_edit_target() {
    use crate::{ListOp, PropertyPath, TargetPath};

    let mut store = rocks();
    let stage = compose(&mut store);
    let (rock_a, rock_b) = (store.path("/World/RockA"), store.path("/World/RockB"));
    let (pebble_b, pebble_a) = (
        store.path("/World/RockB/Pebble"),
        store.path("/World/RockA/Pebble"),
    );
    let (rock, pebble) = (store.path("/Rock"), store.path("/Rock/Pebble"));
    let look = store.tokens.intern("look");
    let size = store.tokens.intern("size");
    let through_b = EditTarget::for_node(&stage, rock_b, node(&stage, rock_b, ArcKind::References))
        .expect("reference node");
    let in_round = EditTarget::for_node(&stage, rock_a, node(&stage, rock_a, ArcKind::Variants))
        .expect("variant node");
    let original = layers(&store);

    let mut edit = Transaction::new();
    edit.set_targets(
        through_b.property(PropertyPath::new(rock_b, look)),
        ListOp {
            prepend: vec![
                TargetPath::Prim(pebble_b),
                TargetPath::Property(PropertyPath::new(rock_b, size)),
            ],
            delete: vec![TargetPath::Prim(rock_b)],
            ..ListOp::default()
        },
    )
    .set_targets(
        in_round.property(PropertyPath::new(rock_a, look)),
        ListOp::explicit(vec![TargetPath::Prim(pebble_a)]),
    );
    let undo = edit.apply(&mut store).expect("maps every target");
    let rock_spec = &store.layers[&ROCK].prims[&rock];
    let authored = rock_spec
        .properties
        .iter()
        .find(|entry| entry.name == look)
        .and_then(|entry| entry.spec.targets.clone());
    assert_eq!(
        authored,
        Some(Box::new(ListOp {
            prepend: vec![
                TargetPath::Prim(pebble),
                TargetPath::Property(PropertyPath::new(rock, size)),
            ],
            delete: vec![TargetPath::Prim(rock)],
            ..ListOp::default()
        }))
    );
    let round = store.tokens.intern("round");
    let shape = store.tokens.intern("shape");
    let in_variant = rock_spec.variant_sets()[&shape].variants[&round]
        .properties
        .iter()
        .find(|entry| entry.name == look)
        .and_then(|entry| entry.spec.targets.clone());
    assert_eq!(
        in_variant,
        Some(Box::new(ListOp::explicit(vec![TargetPath::Prim(pebble)]))),
        "a target names no variant selection"
    );
    let after = layers(&store);

    // The composed targets are the stage paths given.
    let recomposed = compose(&mut store);
    assert_eq!(
        recomposed
            .resolve_target_list_path(PropertyPath::new(rock_b, look))
            .map(|resolved| resolved.value),
        Some(vec![
            TargetPath::Prim(pebble_b),
            TargetPath::Property(PropertyPath::new(rock_b, size)),
        ])
    );

    let redo = undo.apply(&mut store).expect("undo");
    assert_eq!(layers(&store), original);
    redo.apply(&mut store).expect("redo");
    assert_eq!(layers(&store), after);

    // A target outside the reference's namespace does not map.
    let before = layers(&store);
    let mut outside = Transaction::new();
    outside.set_targets(
        through_b.property(PropertyPath::new(rock_b, look)),
        ListOp::explicit(vec![TargetPath::Prim(pebble_b), TargetPath::Prim(rock_a)]),
    );
    assert_eq!(
        outside.apply(&mut store),
        Err(EditError::Rejected {
            op: 0,
            reason: Rejection::UnmappableTarget(TargetPath::Prim(rock_a)),
        })
    );
    assert_eq!(layers(&store), before);
}

/// A local variant target keeps targets outside its branch: `/Light` and
/// `/Light.intensity` are authored as themselves, and `/Rock/Pebble`
/// inside the branch names no variant selection. Undo and redo are exact.
///
/// OpenUSD: `UsdEditTarget::ForLocalDirectVariant`, whose map function
/// has the root identity.
#[test]
fn local_variant_targets_keep_paths_outside_the_branch() {
    use crate::{ListOp, PropertyPath, TargetPath};

    let mut store = rocks();
    let rock = store.path("/Rock");
    let (light, pebble) = (store.path("/Light"), store.path("/Rock/Pebble"));
    let look = store.tokens.intern("look");
    let intensity = store.tokens.intern("intensity");
    let jagged = spec(&mut store, "/Rock{shape=jagged}");
    let target = EditTarget::for_local_variant(ROCK, &jagged);
    let targets = vec![
        TargetPath::Prim(light),
        TargetPath::Property(PropertyPath::new(light, intensity)),
        TargetPath::Prim(pebble),
    ];
    let original = layers(&store);

    let mut edit = Transaction::new();
    edit.set_targets(
        target.property(PropertyPath::new(rock, look)),
        ListOp::explicit(targets.clone()),
    );
    let undo = edit.apply(&mut store).expect("every target maps");
    let shape = store.tokens.intern("shape");
    let jagged_name = store.tokens.intern("jagged");
    let authored = store.layers[&ROCK].prims[&rock].variant_sets()[&shape].variants[&jagged_name]
        .properties
        .iter()
        .find(|entry| entry.name == look)
        .and_then(|entry| entry.spec.targets.clone());
    assert_eq!(authored, Some(Box::new(ListOp::explicit(targets))));
    let after = layers(&store);

    let redo = undo.apply(&mut store).expect("undo");
    assert_eq!(layers(&store), original);
    redo.apply(&mut store).expect("redo");
    assert_eq!(layers(&store), after);
}

/// Inherit and specialize node targets keep the root identity: a path
/// outside the arc's namespace maps to itself, while one in the namespace
/// the arc already maps its prim to (`/Class/X`) does not map, as a
/// `PcpMapFunction` maps each path by its most specific pair. Targets
/// follow the same map; undo and redo are exact.
///
/// OpenUSD: `UsdEditTarget(layer, node)`, whose map is the node's map to
/// the root, with `PcpMapExpression::AddRootIdentity` for class arcs.
#[test]
fn class_arc_targets_keep_the_root_identity() {
    use crate::{ListOp, PropertyPath, TargetPath};

    let mut store = InMemoryStore::default();
    let root = store.path("/");
    let names = ["Class", "Base", "Inst", "Spec", "Light"];
    let children = names.map(|n| store.tokens.intern(n)).to_vec();
    let [class, base, inst, spec_prim, light] = names.map(|n| store.path(&alloc::format!("/{n}")));
    let mut layer = Layer::new(SCENE);
    layer.insert_prim(root, PrimSpec::default().with_children(children));
    layer.insert_prim(class, PrimSpec::class());
    layer.insert_prim(base, PrimSpec::def());
    layer.insert_prim(inst, PrimSpec::def().with_inherit(class));
    layer.insert_prim(spec_prim, PrimSpec::def().with_specialize(base));
    layer.insert_prim(light, PrimSpec::def());
    store.insert_layer(layer);
    let stage = compose(&mut store);
    let look = store.tokens.intern("look");

    for (prim, source, kind) in [
        (inst, class, ArcKind::Inherits),
        (spec_prim, base, ArcKind::Specializes),
    ] {
        let target = EditTarget::for_node(&stage, prim, node(&stage, prim, kind)).expect("node");
        let child = store.paths.intern(store.paths.resolve(prim).join(&[look]));
        let source_child = store
            .paths
            .intern(store.paths.resolve(source).join(&[look]));
        let mapped = |store: &mut InMemoryStore, path| {
            target
                .map_to_spec_path(path, &store.paths)
                .map(|spec| spec.prim_path())
        };
        assert_eq!(mapped(&mut store, prim), Some(source));
        assert_eq!(mapped(&mut store, child), Some(source_child));
        assert_eq!(mapped(&mut store, light), Some(light), "the root identity");
        assert_eq!(
            mapped(&mut store, source),
            None,
            "the arc maps the prim there"
        );
        assert_eq!(mapped(&mut store, source_child), None);

        let at = target.property(PropertyPath::new(prim, look));
        let before = layers(&store);
        let mut edit = Transaction::new();
        edit.set_targets(
            at.clone(),
            ListOp::explicit(vec![TargetPath::Prim(light), TargetPath::Prim(child)]),
        );
        let undo = edit.apply(&mut store).expect("maps");
        let authored = store.layers[&SCENE].prims[&source]
            .properties
            .iter()
            .find(|entry| entry.name == look)
            .and_then(|entry| entry.spec.targets.clone());
        assert_eq!(
            authored,
            Some(Box::new(ListOp::explicit(vec![
                TargetPath::Prim(light),
                TargetPath::Prim(source_child)
            ])))
        );
        let after = layers(&store);
        let redo = undo.apply(&mut store).expect("undo");
        assert_eq!(layers(&store), before);
        let undo = redo.apply(&mut store).expect("redo");
        assert_eq!(layers(&store), after);
        undo.apply(&mut store).expect("undo again");

        let mut shadowed = Transaction::new();
        shadowed.set_targets(at, ListOp::explicit(vec![TargetPath::Prim(source_child)]));
        assert_eq!(
            shadowed.apply(&mut store),
            Err(EditError::Rejected {
                op: 0,
                reason: Rejection::UnmappableTarget(TargetPath::Prim(source_child)),
            })
        );
        assert_eq!(layers(&store), before);
    }
}

/// Refresh only new value slots: a referenced expression is mapped once,
/// and editing a sample does not remap the already composed default again.
#[test]
fn live_value_slots_preserve_mapping_and_undo() {
    let mut store = rocks();
    let path = spec(&mut store, "/Rock.selection");
    let at = Address::spec(ROCK, path);
    let mut setup = Transaction::new();
    setup.create_property(
        at.clone(),
        PropertySpec::typed_attribute(PropertyType::parse("pathExpression").unwrap())
            .with_default(Value::PathExpression("./Old".into())),
    );
    setup.apply(&mut store).unwrap();
    let mut live = crate::LiveStage::compose(&mut store, SCENE, StageOptions::default());
    let a = store.property_path("/World/RockA.selection");
    let b = store.property_path("/World/RockB.selection");
    let check = |live: &crate::LiveStage, store: &mut InMemoryStore| {
        let fresh = compose(store);
        for path in [a, b] {
            assert_eq!(
                live.stage().resolve_property_path(path),
                fresh.resolve_property_path(path)
            );
            for time in [0.0, 5.0, 14.0] {
                assert_eq!(
                    live.stage()
                        .resolve_property_path_at_time(path, Time::at(time)),
                    fresh.resolve_property_path_at_time(path, Time::at(time))
                );
            }
        }
    };
    let mut inverses = Vec::new();
    for (sample, value) in [(false, "./New"), (true, "./Sample")] {
        let mut txn = Transaction::new();
        if sample {
            txn.set_time_sample(at.clone(), 2.0, Value::PathExpression(value.into()));
        } else {
            txn.set_default(at.clone(), Value::PathExpression(value.into()));
        }
        inverses.push(live.apply(&mut store, &txn).unwrap().inverse);
        check(&live, &mut store);
    }
    for undo in inverses.into_iter().rev() {
        let redo = live.apply(&mut store, &undo).unwrap().inverse;
        check(&live, &mut store);
        let undo = live.apply(&mut store, &redo).unwrap().inverse;
        check(&live, &mut store);
        live.apply(&mut store, &undo).unwrap();
        check(&live, &mut store);
    }
}

/// A precise value transaction must not consume or conceal older pending
/// notifications, nor hide another authored slot changed without notification.
#[test]
fn live_value_refresh_respects_pending_and_unnotified_edits() {
    for notify in [false, true] {
        let mut store = rocks();
        let mut live = crate::LiveStage::compose(&mut store, SCENE, StageOptions::default());
        let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
        let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
        let rock = store.path("/Rock");
        let mut outside = Transaction::new();
        outside.set_time_sample(spin, 5.0, Value::Double(75.0));
        outside.apply(&mut store).unwrap();
        if notify {
            live.notify_layer_prim_edits(ROCK, &[rock]);
        }
        let mut txn = Transaction::new();
        txn.set_default(size, Value::Double(4.0));
        live.apply(&mut store, &txn).unwrap();
        assert_live_matches_fresh(&live, &mut store, "pending/unnotified slot");
    }
}

#[test]
fn appended_transactions_keep_order_and_one_atomic_inverse() {
    let mut store = rocks();
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
    let original = layers(&store);
    let generation = store.layers[&ROCK].generation();
    let mut batch = Transaction::new();
    batch.set_default(size.clone(), Value::Double(2.));
    batch.expect_unchanged(&mut store);
    let mut other = Transaction::new();
    other.set_default(size, Value::Double(3.));
    other.set_time_sample(spin, 5., Value::Double(50.));
    other.expect_unchanged(&mut store);
    batch.append(other).append(Transaction::new());
    assert_eq!(batch.len(), 3);
    let inverse = batch.apply(&mut store).unwrap();
    assert_eq!(
        authored_default(&mut store, ROCK, "/Rock.size"),
        Some(Value::Double(3.))
    );
    assert_eq!(store.layers[&ROCK].generation(), generation + 1);
    inverse.apply(&mut store).unwrap();
    // Undo advances generations, but restores authored content.
    for layer in original {
        let actual = &store.layers[&layer.id];
        assert_eq!(actual.prims, layer.prims);
    }
}

#[test]
fn appended_transactions_roll_back_edits_across_layers() {
    let mut store = rocks();
    let original = layers(&store);
    let mut batch = Transaction::new();
    batch.set_default(
        Address::spec(ROCK, spec(&mut store, "/Rock.size")),
        Value::Double(2.),
    );
    let mut other = Transaction::new();
    other.create_prim(
        Address::spec(SCENE_SUB, spec(&mut store, "/New")),
        Specifier::Def,
        None,
    );
    other.set_default(
        Address::spec(ROCK, spec(&mut store, "/Rock.spin")),
        Value::string("wrong type"),
    );
    batch.append(other);
    assert!(batch.apply(&mut store).is_err());
    assert_eq!(layers(&store), original);
}

#[test]
fn appended_transactions_preserve_generation_and_value_guards() {
    let mut store = rocks();
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
    let mut batch = Transaction::new();
    batch.set_default(spin.clone(), Value::Double(10.));
    let mut guarded = Transaction::new();
    guarded
        .set_default(size.clone(), Value::Double(2.))
        .expect_unchanged(&mut store);
    batch.append(guarded);
    let mut later = Transaction::new();
    later.set_time_sample(spin.clone(), 6., Value::Double(60.));
    later.apply(&mut store).unwrap();
    let original = layers(&store);
    assert!(matches!(
        batch.apply(&mut store),
        Err(EditError::StaleGeneration { .. })
    ));
    assert_eq!(layers(&store), original);

    let mut batch = Transaction::new();
    batch.set_default(size.clone(), Value::Double(2.));
    // A guard-only transaction must survive append, and sees initial state.
    let mut guarded = Transaction::new();
    guarded.expect_default(size, Some(Value::Double(2.)));
    batch.append(guarded);
    assert!(matches!(
        batch.apply(&mut store),
        Err(EditError::StaleValue { .. })
    ));
    assert_eq!(layers(&store), original);

    // Guards already in the receiving transaction also survive.
    let mut batch = Transaction::new();
    batch.expect_generation(ROCK, store.layers[&ROCK].generation() + 1);
    let mut other = Transaction::new();
    other.set_default(spin, Value::Double(20.));
    batch.append(other);
    assert!(matches!(
        batch.apply(&mut store),
        Err(EditError::StaleGeneration { .. })
    ));
    assert_eq!(layers(&store), original);
}

#[test]
fn appended_inverses_keep_their_written_value_guards() {
    let mut store = rocks();
    let size = Address::spec(ROCK, spec(&mut store, "/Rock.size"));
    let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
    let mut first = Transaction::new();
    first.set_default(size.clone(), Value::Double(2.));
    let undo_first = first.apply(&mut store).unwrap();
    let mut second = Transaction::new();
    second.set_default(spin, Value::Double(5.));
    let mut undo = second.apply(&mut store).unwrap();
    undo.append(undo_first);
    let mut later = Transaction::new();
    later.set_default(size, Value::Double(3.));
    later.apply(&mut store).unwrap();
    let original = layers(&store);
    assert!(matches!(
        undo.apply(&mut store),
        Err(EditError::StaleValue { .. })
    ));
    assert_eq!(
        layers(&store),
        original,
        "a failed later inverse rolls back earlier inverses"
    );
}

#[test]
fn attribute_block_clears_animation_and_preserves_unrelated_edits_on_undo() {
    let mut store = rocks();
    let spin = Address::spec(ROCK, spec(&mut store, "/Rock.spin"));
    let path = spec(&mut store, "/Rock.spin");
    let original = store.layers[&ROCK].property_at(&path).unwrap().clone();
    let mut block = Transaction::new();
    block.block_attribute(spin.clone());
    let inverse = block.apply(&mut store).unwrap();
    let attr = store.layers[&ROCK].property_at(&path).unwrap();
    assert_eq!(attr.default, Some(Value::Blocked));
    assert!(!attr.time_samples.is_authored() && attr.spline.is_none());
    let note = store.tokens.intern("annotation");
    let mut later = Transaction::new();
    later.set_metadata(spin.clone(), note, FieldValue::Value(Value::string("keep")));
    later.apply(&mut store).unwrap();
    inverse.apply(&mut store).unwrap();
    let attr = store.layers[&ROCK].property_at(&path).unwrap();
    assert_eq!(attr.time_samples, original.time_samples);
    assert_eq!(attr.default, original.default);
    assert_eq!(
        attr.metadata(note),
        Some(&FieldValue::Value(Value::string("keep")))
    );

    let inverse = block.apply(&mut store).unwrap();
    let mut later = Transaction::new();
    later.set_time_sample(spin, 9., Value::Double(90.));
    later.apply(&mut store).unwrap();
    let before = layers(&store);
    assert!(matches!(
        inverse.apply(&mut store),
        Err(EditError::StaleValue {
            slot: Slot::Animation,
            ..
        })
    ));
    assert_eq!(layers(&store), before);
}

#[test]
fn attribute_block_preserves_empty_sample_fields_and_splines_on_undo() {
    use crate::spline::{CurveType, Extrapolation, SplineData, SplineDataType};
    let mut store = rocks();
    let path = spec(&mut store, "/Rock.animated");
    let at = Address::spec(ROCK, path.clone());
    let mut attr = attr(1.);
    attr.time_samples = vec![].into();
    attr.spline = Some(Box::new(SplineData {
        pre_loop_boundary: None,
        post_loop_boundary: None,
        data_type: SplineDataType::Double,
        default_curve_type: CurveType::Bezier,
        pre_extrapolation: Extrapolation::Sloped(-0.0),
        post_extrapolation: Extrapolation::Held,
        loop_params: None,
        knots: vec![],
    }));
    let mut create = Transaction::new();
    create.create_property(at.clone(), attr.clone());
    create.apply(&mut store).unwrap();
    let mut block = Transaction::new();
    block.block_attribute(at);
    let inverse = block.apply(&mut store).unwrap();
    let blocked = store.layers[&ROCK].property_at(&path).unwrap();
    assert!(!blocked.time_samples.is_authored() && blocked.spline.is_none());
    inverse.apply(&mut store).unwrap();
    let restored = store.layers[&ROCK].property_at(&path).unwrap();
    assert!(
        same::Same::same(restored, &attr),
        "all authored bits survive block/undo"
    );
}

#[test]
fn prim_field_inverses_guard_their_field_alone() {
    let mut store = rocks();
    let (rock_a, rock) = (store.path("/World/RockA"), store.path("/Rock"));
    let at = EditTarget::for_layer(SCENE).prim(rock_a);
    let mut add = Transaction::new();
    add.add_inherit(at.clone(), rock, ListPosition::BackOfPrepend);
    let undo = add.apply(&mut store).unwrap();

    // A later edit of another field of the spec does not stop the undo.
    let mut other = Transaction::new();
    other.set_active(at.clone(), false);
    other.apply(&mut store).unwrap();
    undo.apply(&mut store).unwrap();
    let spec = &store.layers[&SCENE].prims[&rock_a];
    assert_eq!(*spec.inherits(), crate::ListOp::default());
    assert_eq!(spec.active, Some(false));

    // A later edit of the same field does.
    let undo = add.apply(&mut store).unwrap();
    let mut again = Transaction::new();
    again.add_inherit(at, rock_a, ListPosition::BackOfAppend);
    again.apply(&mut store).unwrap();
    let before = layers(&store);
    assert!(matches!(
        undo.apply(&mut store),
        Err(EditError::StaleValue {
            slot: Slot::Prim(PrimField::Inherits),
            ..
        })
    ));
    assert_eq!(layers(&store), before);
}

#[test]
fn prepared_prim_field_edits_expect_their_authored_values() {
    let mut store = rocks();
    let rock_a = store.path("/World/RockA");
    let at = EditTarget::for_layer(SCENE).prim(rock_a);
    let mut edit = Transaction::new();
    edit.set_instanceable(at, true).expect_unchanged(&mut store);
    // A write that bypasses transactions leaves the generation alone, but
    // not the value the prepared edit expects.
    let layer = store.layers.get_mut(&SCENE).unwrap();
    layer.prims.get_mut(&rock_a).unwrap().instanceable = Some(false);
    assert!(matches!(
        edit.apply(&mut store),
        Err(EditError::StaleValue {
            slot: Slot::Prim(PrimField::Instanceable),
            ..
        })
    ));
}

#[test]
fn attributes_need_a_registered_value_type() {
    let mut store = rocks();
    let layer = EditTarget::for_layer(SCENE);
    let reason = |store: &mut InMemoryStore, txn: &Transaction| match txn.apply(store) {
        Err(EditError::Rejected { reason, .. }) => reason,
        other => panic!("expected a rejection, got {other:?}"),
    };
    let moss = store.property_path("/World/RockA.moss");
    let float5 = PropertyType::unregistered(PropertyType::parse("float5").unwrap_err());
    let unknown = Rejection::UnknownValueType {
        path: SpecPath::from_property_path(moss),
        type_name: "float5".into(),
    };
    let mut txn = Transaction::new();
    txn.create_property(
        layer.property(moss),
        PropertySpec::typed_attribute(float5.clone()),
    );
    assert_eq!(reason(&mut store, &txn), unknown);

    // An attribute a layer authors with the unknown type takes no value.
    let spec = PropertySpec::typed_attribute(float5);
    let rock_a = store.path("/World/RockA");
    let name = store.tokens.intern("moss");
    store
        .layers
        .get_mut(&SCENE)
        .unwrap()
        .prims
        .get_mut(&rock_a)
        .unwrap()
        .set_property(name, spec);
    let mut txn = Transaction::new();
    txn.set_default(layer.property(moss), Value::Float(1.0));
    assert_eq!(reason(&mut store, &txn), unknown);
    assert_eq!(
        unknown.display(&store).to_string(),
        "`float5`, the type of /World/RockA.moss, is not a registered value type"
    );
}

#[test]
fn prim_field_edits_reject_what_they_cannot_author() {
    let mut store = rocks();
    let stage = compose(&mut store);
    let (root, rock_a, rock) = (
        store.path("/"),
        store.path("/World/RockA"),
        store.path("/Rock"),
    );
    let layer = EditTarget::for_layer(SCENE);
    let reason = |store: &mut InMemoryStore, txn: &Transaction| match txn.apply(store) {
        Err(EditError::Rejected { reason, .. }) => reason,
        other => panic!("expected a rejection, got {other:?}"),
    };

    let mut txn = Transaction::new();
    txn.set_active(layer.prim(root), false);
    assert_eq!(reason(&mut store, &txn), Rejection::PseudoRoot);

    // Every edit of the pseudo-root but its root prim order is rejected,
    // properties on it included; layer metadata has its own edits.
    let (doc, size, world) = (
        store.tokens.intern("documentation"),
        store.tokens.intern("size"),
        store.tokens.intern("World"),
    );
    let on_root = layer.property(crate::PropertyPath::new(root, size));
    let mut txn = Transaction::new();
    txn.set_metadata(
        layer.prim(root),
        doc,
        FieldValue::Value(Value::String("moss".into())),
    );
    assert_eq!(reason(&mut store, &txn), Rejection::PseudoRoot);
    let mut txn = Transaction::new();
    txn.create_property(on_root, attr(1.0));
    assert_eq!(reason(&mut store, &txn), Rejection::PseudoRoot);
    let mut txn = Transaction::new();
    txn.remove_spec(layer.prim(root));
    assert_eq!(reason(&mut store, &txn), Rejection::PseudoRoot);
    let mut txn = Transaction::new();
    txn.set_prim_order(layer.prim(root), vec![world]);
    let undo = txn.apply(&mut store).expect("the root prim order");
    assert_eq!(
        store.layers[&SCENE].prim(root).unwrap().prim_order(),
        Some(&[world][..])
    );
    undo.apply(&mut store).unwrap();

    let mut txn = Transaction::new();
    txn.add_inherit(layer.prim(rock_a), root, ListPosition::BackOfPrepend);
    assert_eq!(reason(&mut store, &txn), Rejection::PseudoRootArc);

    let mut txn = Transaction::new();
    let size = store.property_path("/World/RockA.size");
    txn.set_active(layer.property(size), false);
    assert!(matches!(reason(&mut store, &txn), Rejection::NotAPrim(_)));

    // A variant spec has no specifier.
    let round = spec(&mut store, "/Rock{shape=round}");
    let mut txn = Transaction::new();
    txn.set_specifier(Address::spec(ROCK, round), Specifier::Class);
    assert_eq!(
        reason(&mut store, &txn),
        Rejection::NotOnVariant(PrimField::Specifier)
    );

    // Through `/World/RockA`'s reference, `/Rock` (a path outside the
    // reference's namespace) does not map.
    let node = node(&stage, rock_a, ArcKind::References);
    let target = EditTarget::for_node(&stage, rock_a, node).unwrap();
    let mut txn = Transaction::new();
    txn.add_inherit(target.prim(rock_a), rock, ListPosition::BackOfPrepend);
    assert_eq!(
        reason(&mut store, &txn),
        Rejection::UnmappableTarget(TargetPath::Prim(rock))
    );
    let mut txn = Transaction::new();
    txn.add_reference(
        target.prim(rock_a),
        Reference::new(ROCK, rock),
        ListPosition::BackOfPrepend,
    );
    assert_eq!(
        reason(&mut store, &txn),
        Rejection::UnmappableTarget(TargetPath::Prim(rock))
    );
}

#[test]
fn writing_what_a_prim_field_holds_changes_nothing() {
    let mut store = rocks();
    let rock_a = store.path("/World/RockA");
    let at = EditTarget::for_layer(SCENE).prim(rock_a);
    let generation = store.layers[&SCENE].generation();
    let mut txn = Transaction::new();
    txn.set_specifier(at.clone(), Specifier::Def)
        .clear_active(at.clone())
        .remove_applied_schema(at.clone(), store.tokens.intern("NoneAPI"))
        .clear_inherits(at);
    // Removing an applied schema authors its deletion, so only that edit
    // changes the layer.
    let undo = txn.apply(&mut store).unwrap();
    assert_eq!(undo.len(), 1);
    assert_eq!(store.layers[&SCENE].generation(), generation + 1);
}

#[test]
fn sublayer_edits_reject_lists_composition_could_not_use() {
    let mut store = rocks();
    let reason = |store: &mut InMemoryStore, txn: &Transaction| match txn.apply(store) {
        Err(EditError::Rejected { reason, .. }) => reason,
        other => panic!("expected a rejection, got {other:?}"),
    };
    let mut txn = Transaction::new();
    txn.insert_sublayer(SCENE, 1, SublayerEntry::new(LayerId(99)));
    assert_eq!(
        reason(&mut store, &txn),
        Rejection::NoSuchLayer(LayerId(99))
    );

    let mut txn = Transaction::new();
    txn.insert_sublayer(SCENE, 0, SublayerEntry::new(SCENE_SUB));
    assert_eq!(reason(&mut store, &txn), Rejection::DuplicateSublayer(1));

    let mut txn = Transaction::new();
    txn.insert_sublayer(SCENE, 3, SublayerEntry::new(ROCK));
    assert_eq!(reason(&mut store, &txn), Rejection::NoSuchSublayer(3));

    // The sublayer reaches the edited layer through its own sublayers.
    let mut txn = Transaction::new();
    txn.insert_sublayer(ROCK, 0, SublayerEntry::new(SCENE));
    txn.apply(&mut store).unwrap();
    let mut txn = Transaction::new();
    txn.insert_sublayer(SCENE, 1, SublayerEntry::new(ROCK));
    assert_eq!(reason(&mut store, &txn), Rejection::SublayerCycle(ROCK));
}

#[test]
fn relocate_edits_reject_entries_composition_would_ignore() {
    use crate::{InvalidRelocationReason as R, Relocate};
    let mut store = rocks();
    let (rock, world, rock_a, rock_b) = (
        store.path("/Rock"),
        store.path("/World"),
        store.path("/World/RockA"),
        store.path("/World/RockB"),
    );
    let deep = store.path("/World/RockA/Deep");
    let reason = |store: &mut InMemoryStore, relocates: Vec<Relocate>| {
        let mut txn = Transaction::new();
        txn.set_layer_relocates(SCENE, relocates);
        match txn.apply(store) {
            Err(EditError::Rejected { reason, .. }) => reason,
            other => panic!("expected a rejection, got {other:?}"),
        }
    };
    let relocate = |source, target| Relocate {
        source,
        target: Some(target),
    };
    for (entry, why) in [
        (relocate(rock, rock_a), R::RootPrimSource),
        (relocate(rock_a, rock_a), R::TargetIsSource),
        (relocate(rock_a, world), R::TargetIsAncestor),
        (relocate(rock_a, deep), R::TargetIsDescendant),
    ] {
        assert_eq!(
            reason(&mut store, vec![entry]),
            Rejection::InvalidRelocate {
                source: entry.source,
                reason: why,
            }
        );
    }
    let moved = store.path("/World/Moved");
    assert_eq!(
        reason(
            &mut store,
            vec![relocate(rock_a, moved), relocate(rock_a, rock_b)]
        ),
        Rejection::DuplicateRelocateSource(rock_a)
    );
}

#[test]
fn copies_reject_what_they_cannot_copy() {
    let mut store = rocks();
    let reason = |store: &mut InMemoryStore, from: &str, to: &str| {
        let (from, to) = (spec(store, from), spec(store, to));
        let mut txn = Transaction::new();
        txn.copy_spec(Address::spec(SCENE, from), Address::spec(SCENE, to));
        match txn.apply(store) {
            Err(EditError::Rejected { reason, .. }) => reason,
            other => panic!("expected a rejection, got {other:?}"),
        }
    };
    assert_eq!(
        reason(&mut store, "/World/RockA", "/World/RockA.size"),
        Rejection::CopyBetweenKinds
    );
    assert_eq!(
        reason(&mut store, "/World/RockA", "/World/RockA/Inside"),
        Rejection::CopyIntoSource
    );
    assert_eq!(
        reason(&mut store, "/World/RockA", "/World"),
        Rejection::CopyIntoSource
    );
    let mut txn = Transaction::new();
    let (round, copy) = (
        spec(&mut store, "/Rock{shape=round}"),
        spec(&mut store, "/Copy"),
    );
    txn.copy_spec(Address::spec(ROCK, round), Address::spec(ROCK, copy));
    assert!(matches!(
        txn.apply(&mut store),
        Err(EditError::Rejected {
            reason: Rejection::VariantCopy,
            ..
        })
    ));
}

#[test]
fn a_prim_copied_to_another_layer_anchors_its_internal_arcs_there() {
    let mut store = rocks();
    // `/World/Proxy` references `/World/RockA` internally.
    let (proxy, rock_a) = (store.path("/World/Proxy"), store.path("/World/RockA"));
    let proxy_name = store.tokens.intern("Proxy");
    let mut txn = Transaction::new();
    txn.create_prim(
        EditTarget::for_layer(SCENE).prim(proxy),
        Specifier::Def,
        None,
    );
    txn.add_reference(
        EditTarget::for_layer(SCENE).prim(proxy),
        Reference::new(SCENE, rock_a),
        ListPosition::BackOfPrepend,
    );
    txn.apply(&mut store).unwrap();
    // Copy all of `/World` into the asset layer.
    let (world, copy) = (spec(&mut store, "/World"), spec(&mut store, "/Copy"));
    let mut txn = Transaction::new();
    txn.copy_spec(Address::spec(SCENE, world), Address::spec(ROCK, copy));
    txn.apply(&mut store).unwrap();
    let copied_proxy = store.path("/Copy/Proxy");
    let copied_rock_a = store.path("/Copy/RockA");
    let spec = &store.layers[&ROCK].prims[&copied_proxy];
    assert_eq!(
        spec.references.prepend,
        vec![Reference::new(ROCK, copied_rock_a)]
    );
    let copy_root = store.path("/Copy");
    assert!(
        store.layers[&ROCK].prims[&copy_root]
            .authored_children
            .contains(&proxy_name)
    );
    // Composed from the asset, the copy's internal arc reaches the copied
    // rock, which references `/Rock` of the same layer.
    let stage = Stage::compose(&mut store, ROCK, StageOptions::default());
    let size = store.property_path("/Copy/Proxy.size");
    assert_eq!(
        stage.resolve_property_path(size).map(|r| r.value),
        Some(crate::ResolvedValue::Scalar(Value::Double(1.0)))
    );
}

/// Creating children of a prim lists each once, after the names the prim
/// already lists, in the order a transaction creates them, whether it
/// creates one child of the prim or several; a name the prim already
/// lists, though no spec backs it, is not listed again. The inverse
/// restores the list.
///
/// OpenUSD: `SdfCreatePrimInLayer` (a created prim spec is added to its
/// parent's `primChildren`).
#[test]
fn created_children_are_listed_once_in_creation_order() {
    let mut store = InMemoryStore::default();
    let world = store.path("/World");
    let [a, b, c, d, e, f] = ["A", "B", "C", "D", "E", "F"].map(|name| store.tokens.intern(name));
    let mut layer = Layer::new(SCENE);
    let mut spec = PrimSpec::def();
    // `D` and `E` are listed without specs.
    spec.authored_children = vec![a, d, e];
    layer.insert_prim(world, spec);
    layer.insert_prim(store.path("/World/A"), PrimSpec::def());
    store.insert_layer(layer);
    let listed =
        |store: &InMemoryStore| store.layers[&SCENE].prims[&world].authored_children.clone();
    let target = EditTarget::for_layer(SCENE);
    let create = |store: &mut InMemoryStore, names: &[&str]| {
        let mut txn = Transaction::new();
        for name in names {
            txn.create_prim(target.prim(store.path(name)), Specifier::Def, None);
        }
        txn.apply(store).expect("applies")
    };

    let undo_one = create(&mut store, &["/World/D"]);
    assert_eq!(listed(&store), [a, d, e]);
    let undo_two = create(&mut store, &["/World/B"]);
    assert_eq!(listed(&store), [a, d, e, b]);
    let undo_many = create(
        &mut store,
        &["/World/C", "/World/E", "/World/B/Leaf", "/World/F"],
    );
    assert_eq!(listed(&store), [a, d, e, b, c, f]);

    for undo in [undo_many, undo_two, undo_one] {
        undo.apply(&mut store).expect("undoes");
    }
    assert_eq!(listed(&store), [a, d, e]);
}

#[test]
fn slots_name_their_fields_in_words() {
    let store = InMemoryStore::default();
    let (kind, season) = (store.tokens.intern("kind"), store.tokens.intern("season"));
    for (slot, text) in [
        (Slot::Default, "the default"),
        (Slot::TimeSample(5.0), "the time sample at 5"),
        (Slot::Metadata(kind), "the `kind` metadata"),
        (
            Slot::VariantSelection(season),
            "the selection of variant set `season`",
        ),
        (Slot::Prim(PrimField::TypeName), "the `typeName`"),
    ] {
        assert_eq!(slot.display(&store).to_string(), text);
    }
}

#[test]
fn edit_errors_have_stable_codes() {
    let mut store = rocks();
    let missing = spec(&mut store, "/Rock.missing");
    let mut txn = Transaction::new();
    txn.set_metadata(
        Address::spec(ROCK, missing),
        store.tokens.intern("doc"),
        FieldValue::Value(Value::string("x")),
    );
    let error = txn.apply(&mut store).unwrap_err();
    assert_eq!(error.code(), "edit.rejected.no_such_spec");
    let stale = EditError::StaleGeneration {
        layer: ROCK,
        expected: 1,
        found: 2,
    };
    assert_eq!(stale.code(), "edit.stale");
    for (reason, code) in [
        (Rejection::PseudoRoot, "edit.rejected.pseudo_root"),
        (Rejection::NoSuchLayer(ROCK), "edit.rejected.no_such_layer"),
        (
            Rejection::NoSuchSublayer(0),
            "edit.rejected.no_such_sublayer",
        ),
        (Rejection::CopyIntoSource, "edit.rejected.copy_into_source"),
    ] {
        assert_eq!(reason.code(), code);
        let rejected = EditError::Rejected { op: 0, reason };
        assert_eq!(rejected.code(), code);
    }
}
