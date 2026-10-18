// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::doc::{LayerOffset, Reference};
use crate::{
    InMemoryStore, InterpolationType, Layer, PrimSpec, PropertyPath, PropertySpec, PropertyType,
    SublayerEntry, Value,
};
use alloc::vec;

fn compare(
    store: &mut InMemoryStore,
    options: StageOptions,
    paths: &[PathId],
    property: PropertyPath,
) {
    let fast = Stage::compose(store, LayerId(1), options.clone());
    let root = store.path("/");
    let general = Stage::compose(
        store,
        LayerId(1),
        StageOptions {
            mask: Some(crate::PopulationMask {
                include: vec![root],
            }),
            ..options
        },
    );
    assert!(fast.composition_errors().is_empty());
    assert!(general.composition_errors().is_empty());
    for &path in paths {
        assert_eq!(fast.has_prim(path), general.has_prim(path));
        assert_eq!(fast.children_of(path), general.children_of(path));
        assert_eq!(
            fast.layers_affecting_prim(path),
            general.layers_affecting_prim(path)
        );
        assert_eq!(
            alloc::format!("{:?}", fast.explain_prim_graph(path)),
            alloc::format!("{:?}", general.explain_prim_graph(path))
        );
    }
    assert_eq!(
        fast.explain_property_path(property),
        general.explain_property_path(property)
    );
    for time in [-10.0, 0.0, 3.5, 15.0, 30.0] {
        assert_eq!(
            fast.resolve_property_path_at_time(property, time, InterpolationType::Linear)
                .map(|r| r.value),
            general
                .resolve_property_path_at_time(property, time, InterpolationType::Linear)
                .map(|r| r.value)
        );
    }
    for id in [LayerId(1), LayerId(2), LayerId(3)] {
        assert_eq!(
            fast.prims_affected_by_layer(id, store),
            general.prims_affected_by_layer(id, store)
        );
    }
}

#[test]
fn local_stack_matches_general_population_with_sessions_offsets_ordering_and_activation() {
    let mut store = InMemoryStore::default();
    let names = ["/", "/World", "/World/A", "/World/B", "/Source"];
    let paths: Vec<_> = names.iter().map(|s| store.path(s)).collect();
    let field = store.tokens.intern("amount");
    let source_field = store.tokens.intern("fromSource");
    let a = store.tokens.intern("A");
    let b = store.tokens.intern("B");
    let property = PropertyPath::new(paths[1], field);
    let mut weak = Layer::new(LayerId(2));
    weak.insert_prim(
        paths[1],
        PrimSpec::def().with_children(vec![b, a]).with_property(
            field,
            PropertySpec::typed_attribute(PropertyType::parse("double").unwrap())
                .with_time_samples(vec![(0.0, Value::Double(0.0)), (10.0, Value::Double(10.0))]),
        ),
    );
    weak.insert_prim(paths[2], PrimSpec::def());
    weak.insert_prim(paths[3], PrimSpec::def().with_active(false));
    let mut root = Layer::new(LayerId(1));
    root.sublayers.push(SublayerEntry {
        layer: LayerId(2),
        offset: LayerOffset {
            offset: 5.0,
            scale: 2.0,
        },
        ..SublayerEntry::new(LayerId(2))
    });
    let mut world = PrimSpec::over();
    world.prim_order = Some(vec![a, b]);
    root.insert_prim(paths[1], world);
    root.insert_prim(
        paths[4],
        PrimSpec::def().with_property(
            source_field,
            PropertySpec::attribute().with_default(Value::Double(7.0)),
        ),
    );
    let mut session = Layer::new(LayerId(3));
    session.insert_prim(
        paths[2],
        PrimSpec::over().with_field(store.tokens.intern("comment"), Value::string("session")),
    );
    store.insert_layer(root);
    store.insert_layer(weak);
    store.insert_layer(session);
    let options = StageOptions {
        session_layer: Some(LayerId(3)),
        with_dependencies: true,
        with_provenance: true,
        ..Default::default()
    };
    compare(&mut store, options.clone(), &paths, property);
    let mut muted = options.clone();
    muted.muted_layers.insert(LayerId(2));
    compare(&mut store, muted, &paths, property);
    // A new arc invalidates the absence proof on the next full composition.
    let layer = store.layers.get_mut(&LayerId(1)).unwrap();
    layer
        .prims
        .get_mut(&paths[1])
        .unwrap()
        .add_reference(Reference::new(LayerId(1), paths[4]));
    layer.touch();
    compare(
        &mut store,
        options.clone(),
        &paths,
        PropertyPath::new(paths[1], source_field),
    );
    let stage = Stage::compose(&mut store, LayerId(1), options);
    assert_eq!(
        stage
            .resolve_field_path(PropertyPath::new(paths[1], source_field))
            .unwrap()
            .value,
        Value::Double(7.0)
    );
}

#[test]
fn one_shot_namespace_matches_retained_indexes_before_and_after_structural_edits() {
    let mut store = InMemoryStore::default();
    let world = store.path("/World");
    let a = store.path("/World/A");
    let b = store.path("/World/B");
    let options = StageOptions {
        with_dependencies: true,
        ..Default::default()
    };
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(world, PrimSpec::def());
    layer.insert_prim(a, PrimSpec::def());
    store.insert_layer(layer);
    let mut live = crate::LiveStage::compose(&mut store, LayerId(1), options.clone());
    let stage = Stage::compose(&mut store, LayerId(1), options.clone());
    assert_eq!(stage.children_of(world), live.stage().children_of(world));
    assert_eq!(
        alloc::format!("{:?}", stage.explain_prim_graph(a)),
        alloc::format!("{:?}", live.stage().explain_prim_graph(a))
    );

    store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .insert_prim(b, PrimSpec::def());
    live.notify_structural_change();
    live.recompose(&mut store);
    let stage = Stage::compose(&mut store, LayerId(1), options);
    assert_eq!(stage.children_of(world), Some(&[a, b][..]));
    assert_eq!(stage.children_of(world), live.stage().children_of(world));
    assert_eq!(
        alloc::format!("{:?}", stage.explain_prim_graph(b)),
        alloc::format!("{:?}", live.stage().explain_prim_graph(b))
    );
}
