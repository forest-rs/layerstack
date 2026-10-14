// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Incremental composition at asset, relocation and variant boundaries.
//!
//! These fixtures compare the published live scene with fresh composition,
//! including its namespace, complete prim stacks, values and errors.

#![allow(missing_docs, reason = "integration tests")]

use layerstack::{
    CompositionError, InMemoryStore, Layer, LayerId, LayerOffset, LiveStage, PrimSpec,
    PropertyPath, PropertySpec, Reference, ReferenceTarget, Relocate, Stage, StageOptions,
    SublayerEntry, Value, VariantSetSpec, VariantSpec,
};

fn options() -> StageOptions {
    StageOptions {
        with_provenance: true,
        ..StageOptions::default()
    }
}

fn assert_matches_fresh(store: &mut InMemoryStore, live: &LiveStage) {
    let fresh = Stage::compose(store, LayerId(1), options());
    let root = store.path("/");
    let mut paths: Vec<_> = fresh
        .traverse(root)
        .chain(live.stage().traverse(root))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let x = store.tokens.intern("x");
    for path in paths {
        let name = store.paths.display(path, &store.tokens);
        assert_eq!(live.stage().has_prim(path), fresh.has_prim(path), "{name}");
        assert_eq!(
            live.stage().children_of(path),
            fresh.children_of(path),
            "{name}"
        );
        assert_eq!(
            live.stage().explain_prim(path),
            fresh.explain_prim(path),
            "{name}"
        );
        assert_eq!(
            live.stage()
                .resolve_field_path(PropertyPath::new(path, x))
                .map(|value| value.value),
            fresh
                .resolve_field_path(PropertyPath::new(path, x))
                .map(|value| value.value),
            "{name}.x"
        );
    }
    let errors = |stage: &Stage| {
        let mut errors: Vec<_> = stage
            .composition_errors()
            .iter()
            .map(|error| format!("{error:?}"))
            .collect();
        errors.sort();
        errors
    };
    assert_eq!(
        errors(live.stage()),
        errors(&fresh),
        "composition diagnostics"
    );
}

/// Resolving an expression asset is a host operation. Replacing its mapping
/// must update both the value source and children on the next synchronization.
/// AOUSD Core §9.4 (layer-relative assets), §10.3.2.1 (references).
#[test]
fn replacing_an_anchored_asset_mapping_replaces_its_scene_and_provenance() {
    let mut store = InMemoryStore::default();
    let destination = store.path("/Use");
    let source = store.path("/Asset");
    let old_child = store.path("/Asset/Old");
    let new_child = store.path("/Asset/New");
    let old_destination = store.path("/Use/Old");
    let new_destination = store.path("/Use/New");
    let x = store.tokens.intern("x");
    let mut root = Layer::new(LayerId(1));
    root.insert_prim(
        destination,
        PrimSpec::def().with_reference(Reference::expression(
            LayerId(1),
            "`\"./asset.usda\"`",
            ReferenceTarget::Prim(source),
            LayerOffset::IDENTITY,
        )),
    );
    store.insert_layer(root);
    for (id, child, value) in [(LayerId(2), old_child, 1), (LayerId(3), new_child, 2)] {
        let mut layer = Layer::new(id);
        layer.insert_prim(
            source,
            PrimSpec::def().with_property(x, PropertySpec::attribute().with_default(value)),
        );
        layer.insert_prim(child, PrimSpec::def());
        store.insert_layer(layer);
    }
    store.insert_asset_layer(LayerId(1), "./asset.usda", LayerId(2));
    let mut live = LiveStage::compose(&mut store, LayerId(1), options());
    assert!(live.stage().has_prim(old_destination));
    assert!(!live.stage().has_prim(new_destination));
    assert_matches_fresh(&mut store, &live);

    for (id, expected, present, absent) in [
        (LayerId(3), 2, new_destination, old_destination),
        (LayerId(2), 1, old_destination, new_destination),
    ] {
        store.insert_asset_layer(LayerId(1), "./asset.usda", id);
        live.synchronize(&mut store);
        assert!(live.stage().has_prim(present));
        assert!(!live.stage().has_prim(absent));
        assert_eq!(
            live.stage()
                .resolve_field_path(PropertyPath::new(destination, x))
                .unwrap()
                .value,
            Value::Int(expected)
        );
        assert_eq!(
            live.stage()
                .explain_prim(destination)
                .unwrap()
                .last()
                .unwrap()
                .layer_id,
            id
        );
        assert_matches_fresh(&mut store, &live);
    }
}

/// Relocates are validated per stack, rather than only when the layer
/// authoring them arrives. Two individually valid stacks may conflict when
/// a newly loaded layer combines them without authoring any relocates itself.
/// Native OpenUSD 26.8 reports `ErrorInvalidSameTargetRelocations` only while
/// the join includes both existing stacks.
/// AOUSD Core §10.3.2.6 (relocates).
#[test]
fn a_new_sublayer_join_revalidates_existing_relocates() {
    let mut store = InMemoryStore::default();
    let model = store.path("/Model");
    let source_a = store.path("/Model/A");
    let source_b = store.path("/Model/B");
    let target = store.path("/Model/Target");
    let joined_a = store.path("/Joined/A");
    let joined_b = store.path("/Joined/B");
    let joined_target = store.path("/Joined/Target");
    let x = store.tokens.intern("x");
    let mut base = Layer::new(LayerId(5));
    base.insert_prim(model, PrimSpec::def());
    for (source, value) in [(source_a, 1), (source_b, 2)] {
        base.insert_prim(
            source,
            PrimSpec::def().with_property(x, PropertySpec::attribute().with_default(value)),
        );
    }
    store.insert_layer(base);
    let mut root = Layer::new(LayerId(1));
    for (name, id, asset) in [
        ("/Left", LayerId(2), "left.usda"),
        ("/Right", LayerId(3), "right.usda"),
    ] {
        let path = store.path(name);
        root.insert_prim(
            path,
            PrimSpec::def().with_reference(Reference::with_asset(id, model, asset)),
        );
    }
    let joined = store.path("/Joined");
    root.insert_prim(
        joined,
        PrimSpec::def().with_reference(Reference::expression(
            LayerId(1),
            "`\"./join.usda\"`",
            ReferenceTarget::Prim(model),
            LayerOffset::IDENTITY,
        )),
    );
    store.insert_layer(root);
    for (id, source) in [(LayerId(2), source_a), (LayerId(3), source_b)] {
        let mut layer = Layer::new(id);
        layer.insert_prim(
            model,
            PrimSpec::def().with_reference(Reference::with_asset(LayerId(5), model, "base.usda")),
        );
        layer.relocates.push(Relocate {
            source,
            target: Some(target),
        });
        store.insert_layer(layer);
    }
    let mut live = LiveStage::compose(&mut store, LayerId(1), options());
    let conflicts = |stage: &Stage| {
        stage
            .composition_errors()
            .iter()
            .filter(|error| matches!(error, CompositionError::InvalidSameTargetRelocations(_)))
            .count()
    };
    assert_eq!(conflicts(live.stage()), 0);
    assert_matches_fresh(&mut store, &live);

    let mut join = Layer::new(LayerId(4));
    join.sublayers = vec![
        SublayerEntry::new(LayerId(2)),
        SublayerEntry::new(LayerId(3)),
    ];
    store.insert_layer(join);
    store.insert_asset_layer(LayerId(1), "./join.usda", LayerId(4));
    live.synchronize(&mut store);
    assert_eq!(conflicts(live.stage()), 1);
    assert!(live.stage().has_prim(joined_a));
    assert!(live.stage().has_prim(joined_b));
    assert!(!live.stage().has_prim(joined_target));
    assert_matches_fresh(&mut store, &live);

    let mut join = Layer::new(LayerId(4));
    join.sublayers = vec![SublayerEntry::new(LayerId(2))];
    store.insert_layer(join);
    live.synchronize(&mut store);
    assert_matches_fresh(&mut store, &live);
    assert_eq!(conflicts(live.stage()), 0);
    assert!(!live.stage().has_prim(joined_a));
    assert!(live.stage().has_prim(joined_b));
    assert!(live.stage().has_prim(joined_target));
}

/// Native OpenUSD 26.8 was checked for all three states: a stronger literal
/// selection masks the missing-variable error; clearing it reveals exactly
/// one error and no variant value; restoring it removes the error and restores
/// the value. AOUSD Core §10.3.2.5.1 (strongest variant selection).
#[test]
fn a_stronger_selection_masks_a_weaker_expression_error_after_edits() {
    let mut store = InMemoryStore::default();
    let prim = store.path("/P");
    let v = store.tokens.intern("v");
    let a = store.tokens.intern("a");
    let missing = store.tokens.intern("`${MISSING}`");
    let x = store.tokens.intern("x");
    let mut weak = Layer::new(LayerId(2));
    let mut spec = PrimSpec::def();
    spec.variant_set_order.push(v);
    spec.variant_selections.insert(v, missing);
    let branch = VariantSpec {
        properties: PrimSpec::def()
            .with_property(x, PropertySpec::attribute().with_default(7))
            .properties,
        ..VariantSpec::default()
    };
    let mut set = VariantSetSpec::default();
    set.variants.insert(a, branch);
    spec.variant_sets.insert(v, set);
    weak.insert_prim(prim, spec);
    store.insert_layer(weak);
    let root = |selected: bool| {
        let mut root = Layer::new(LayerId(1));
        root.sublayers.push(SublayerEntry::new(LayerId(2)));
        let mut spec = PrimSpec::over();
        if selected {
            spec.variant_selections.insert(v, a);
        }
        root.insert_prim(prim, spec);
        root
    };
    store.insert_layer(root(true));
    let mut live = LiveStage::compose(&mut store, LayerId(1), options());
    for selected in [true, false, true] {
        store.insert_layer(root(selected));
        live.synchronize(&mut store);
        let errors: Vec<_> = live
            .stage()
            .composition_errors()
            .iter()
            .filter(|error| matches!(error, CompositionError::VariableExpressionError(_)))
            .collect();
        assert_eq!(errors.len(), usize::from(!selected));
        assert_eq!(
            live.stage()
                .resolve_field_path(PropertyPath::new(prim, x))
                .map(|value| value.value),
            selected.then_some(Value::Int(7))
        );
        assert_matches_fresh(&mut store, &live);
    }
}
