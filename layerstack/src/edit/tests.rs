// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{vec, vec::Vec};

use super::*;
use crate::{
    ArcKind, FieldValue, HashMap, InMemoryStore, Layer, LayerId, LayerOffset, LayerStore, NodeId,
    PathId, PrimSpec, PropertySpec, PropertyType, Reference, SpecPath, Specifier, Stage,
    StageOptions, SublayerEntry, TokenId, Value, VariantSetSpec, VariantSpec,
};

const SCENE: LayerId = LayerId(1);
const ROCK: LayerId = LayerId(2);
const SCENE_SUB: LayerId = LayerId(3);

fn double() -> PropertyType {
    PropertyType::new("double", false, Value::Double(0.0))
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
    let mut variants = HashMap::new();
    for (name, value) in [(round, 0.1), (jagged, 0.9)] {
        variants.insert(
            name,
            VariantSpec {
                properties: vec![crate::PropertyEntry {
                    name: roughness,
                    spec: attr(value),
                }],
                ..VariantSpec::default()
            },
        );
    }
    rock_spec
        .variant_sets
        .insert(shape, VariantSetSpec { variants });
    rock_spec.variant_set_order.push(shape);
    rock_spec.variant_selections.insert(shape, round);
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
    SpecPath::parse(text, &mut store.tokens, &mut store.paths).expect("spec path")
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
    store.layers[&layer]
        .property_at(&path, &store.paths)?
        .default
        .clone()
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
            .map_to_spec_path(path, &mut store.paths)
            .map(|spec| spec.display(&store.tokens))
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
            .map_property_to_spec_path(size, &mut store.paths)
            .map(|spec| spec.display(&store.tokens))
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
            .property_at(&spin, &store.paths)
            .and_then(|p| p.time_samples.clone())
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
        store.layers[&SCENE]
            .property_at(&size_a_spec, &store.paths)
            .is_none(),
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
            .property_at(&path, &store.paths)
            .and_then(|p| p.time_samples.clone()),
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
            .property_at(&path, &store.paths)
            .map(|p| p.time_samples.clone()),
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
            reason: Rejection::TypeMismatch { .. }
        }
    ));
    assert_eq!(layers(&store), original, "the first edit was rolled back");
    assert_eq!(
        store.layers[&ROCK].generation(),
        original[1].generation(),
        "a failed transaction leaves the generation"
    );

    let mut block = Transaction::new();
    block.set_default(roughness, Value::Blocked);
    block.apply(&mut store).expect("a block fits any type");

    let color3f = PropertyType::new("color3f", true, Value::Vec3f([0.0; 3]));
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
    let mut tuple = Transaction::new();
    tuple.set_default(
        color.clone(),
        Value::Array(vec![Value::Array(vec![Value::Float(0.1); 3])]),
    );
    tuple
        .apply(&mut store)
        .expect("tuples may be spelled as arrays");
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
    assert_eq!(
        edit.apply(&mut store),
        Err(EditError::Rejected {
            op: 1,
            reason: Rejection::Unmappable
        })
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
        store.layers[&SCENE].variant_selection_at(&rock_a, shape, &store.paths),
        Some(jagged)
    );
    let rock = store.path("/Rock");
    let set = &store.layers[&ROCK].prims[&rock].variant_sets[&shape];
    assert!(
        set.variants[&smooth]
            .authored_children
            .contains(&store.tokens.intern("Moss"))
    );
    let (moss, smooth_branch) = (
        spec(&mut store, "/Rock{shape=smooth}Moss"),
        spec(&mut store, "/Rock{shape=smooth}"),
    );
    assert!(store.layers[&ROCK].has_spec_at(&moss, &store.paths));
    assert_eq!(
        store.layers[&ROCK].metadata_at(&smooth_branch, kind, &store.paths),
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
    assert!(!store.layers[&ROCK].has_spec_at(&moss, &store.paths));
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
        store.layers[&ROCK].metadata_at(&size_path, interpolation, &store.paths),
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
            "{context}: selections"
        );
        for &name in &names {
            assert_eq!(
                stage.resolve_value(prim, name),
                fresh.resolve_value(prim, name),
                "{context}: default of {}",
                store.tokens.resolve(name)
            );
            for time in [0.0, 2.0, 5.0, 14.0] {
                let linear = crate::InterpolationType::Linear;
                assert_eq!(
                    stage.resolve_value_at_time(prim, name, time, linear),
                    fresh.resolve_value_at_time(prim, name, time, linear),
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
        live.stage().variant_selections(rock_a, &store).get(&shape),
        Some(&jagged)
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
            .property_at(&spin_path, &store.paths)
            .and_then(|p| p.time_samples.clone()),
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
    let color3f = PropertyType::new("color3f", true, Value::Vec3f([0.0; 3]));
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
        .relocates
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
            .map_property_to_spec_path(size, &mut store.paths)
            .map(|p| p.display(&store.tokens))
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
        .variant_selections
        .clear();
    let options = StageOptions {
        variant_fallbacks: [(shape, vec![jagged])].into_iter().collect(),
        ..StageOptions::default()
    };
    let mut live = crate::LiveStage::compose(&mut store, SCENE, options.clone());
    assert_eq!(
        live.stage().variant_selections(rock_a, &store).get(&shape),
        Some(&jagged),
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
        !rock_spec.variant_sets.contains_key(&size),
        "the set is nested, not on the prim spec"
    );
    let outer = rock_spec
        .variant_spec(&[(shape, round)])
        .expect("outer branch");
    assert_eq!(outer.variant_set_order, [size]);
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
