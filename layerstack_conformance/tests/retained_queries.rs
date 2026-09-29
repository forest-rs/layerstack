// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained queries remain equivalent to fresh domain computations across edits.
#![allow(missing_docs, reason = "integration tests")]
use layerstack::{
    EditTarget, InMemoryStore, LayerId, PathId, PropertyPath, Stage, StageOptions, Transaction,
    Value,
};
use layerstack_conformance::{usda_real::load_entry_usda, workspace_root};
use layerstack_schemas::{
    Scene, Time, XformCache,
    bounds::{BoundsCache, BoundsOptions},
    retained::{Query, QueryAnswer, QuerySession},
};
use std::sync::Arc;

struct Fixture {
    session: QuerySession<InMemoryStore>,
    root: PathId,
    child: PathId,
    reset_child: PathId,
    translate: PropertyPath,
    unrelated: PropertyPath,
    size: PropertyPath,
    input: PropertyPath,
    a: PropertyPath,
    b: PropertyPath,
    layer: LayerId,
}
fn fixture() -> Fixture {
    let mut loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/retained_queries.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    let s = &mut loaded.store;
    let root = s.path("/World");
    let child = s.path("/World/Shape");
    let reset_child = s.path("/World/Reset/Shape");
    let translate = s.property_path("/World.xformOp:translate");
    let unrelated = s.property_path("/World.inputs:unrelated");
    let size = s.property_path("/World/Shape.size");
    let input = s.property_path("/Material/Surface.inputs:roughness");
    let a = s.property_path("/Material.inputs:a");
    let b = s.property_path("/Material.inputs:b");
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut s.tokens))),
        ..StageOptions::default()
    };
    Fixture {
        session: QuerySession::new(
            loaded.store,
            loaded.root_layer,
            options,
            Time::Default,
            BoundsOptions::default(),
        ),
        root,
        child,
        reset_child,
        translate,
        unrelated,
        size,
        input,
        a,
        b,
        layer: loaded.root_layer,
    }
}
fn set(layer: LayerId, path: PropertyPath, value: Value) -> Transaction {
    let mut txn = Transaction::new();
    txn.set_default(EditTarget::for_layer(layer).property(path), value);
    txn
}
fn assert_fresh(
    f: &mut Fixture,
    x: layerstack_schemas::retained::QueryId,
    b: layerstack_schemas::retained::QueryId,
    time: Time,
) {
    // Rebuild an independently seeded store with identical interned identifiers,
    // then replace its authored layers with the current edited source state.
    let mut store = fixture().session.into_store();
    store.layers = f.session.store().layers.clone();
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..StageOptions::default()
    };
    let stage = Stage::compose(&mut store, f.layer, options);
    let scene = Scene::new(&stage, &store);
    assert_eq!(
        f.session.poll(x).unwrap().answer,
        &QueryAnswer::WorldTransform(XformCache::new(time).local_to_world(&scene, f.child)),
        "retained transform matches clean composition"
    );
    assert_eq!(
        f.session.poll(b).unwrap().answer,
        &QueryAnswer::WorldBound(
            BoundsCache::new(time, BoundsOptions::default()).world_bound(&scene, f.root)
        ),
        "retained bounds match clean composition"
    );
}
#[test]
fn edits_time_undo_and_reset_dependencies() {
    let mut f = fixture();
    let x = f.session.observe(Query::WorldTransform(f.child));
    let b = f.session.observe(Query::WorldBound(f.root));
    let r = f.session.observe(Query::WorldTransform(f.reset_child));
    assert!(
        f.session.poll(x).unwrap().answer_changed,
        "initial result is delivered"
    );
    f.session.poll(b).unwrap();
    f.session.poll(r).unwrap();
    f.session
        .apply(&set(f.layer, f.unrelated, Value::Double(2.0)))
        .unwrap();
    assert!(
        !f.session.poll(x).unwrap().evaluated,
        "unrelated property leaves transform clean"
    );
    assert!(
        !f.session.poll(b).unwrap().evaluated,
        "unrelated property leaves bound clean"
    );
    let edit = set(f.layer, f.translate, Value::Vec3d([4.0, 0.0, 0.0]));
    let applied = f.session.apply(&edit).unwrap();
    assert!(
        f.session.poll(x).unwrap().answer_changed,
        "ancestor transform changes child world"
    );
    assert!(
        !f.session.poll(r).unwrap().evaluated,
        "reset stops ancestor dependencies"
    );
    assert_fresh(&mut f, x, b, Time::Default);
    f.session.apply(&applied.inverse).unwrap();
    assert_fresh(&mut f, x, b, Time::Default);
    f.session.set_time(Time::at(1.0));
    assert_fresh(&mut f, x, b, Time::at(1.0));
    let applied = f
        .session
        .apply(&set(f.layer, f.size, Value::Double(4.0)))
        .unwrap();
    assert!(
        !f.session.poll(x).unwrap().evaluated,
        "extent edit does not touch transform"
    );
    assert_fresh(&mut f, x, b, Time::at(1.0));
    f.session.apply(&applied.inverse).unwrap();
    assert_fresh(&mut f, x, b, Time::at(1.0));
    assert!(f.session.remove(x), "registered handle retires");
    assert!(
        f.session.poll(x).is_none(),
        "retired handle cannot alias a new query"
    );
}
#[test]
fn equal_values_distinguish_provider_changes_and_time_keeps_topology() {
    use layerstack::{ListOp, TargetPath};
    let mut f = fixture();
    let q = f.session.observe(Query::ShadingValue(f.input));
    f.session.poll(q).unwrap();
    let mut edit = Transaction::new();
    edit.set_targets(
        EditTarget::for_layer(f.layer).property(f.input),
        ListOp::explicit(vec![TargetPath::Property(f.b)]),
    );
    let applied = f.session.apply(&edit).unwrap();
    let update = f.session.poll(q).unwrap();
    assert!(
        !update.answer_changed,
        "equal constants preserve evaluated answer"
    );
    assert!(update.provenance_changed, "provider identity changed");
    assert!(update.dependencies_changed, "provider dependency changed");
    f.session.apply(&applied.inverse).unwrap();
    f.session.poll(q).unwrap();
    let walks = f.session.stats().shading_walks;
    f.session.set_time(Time::at(1.0));
    let update = f.session.poll(q).unwrap();
    let QueryAnswer::ShadingValue(value) = update.answer else {
        panic!("shading query");
    };
    assert_eq!(value.values, vec![Some(Value::Float(0.5))]);
    assert!(update.answer_changed, "sampled value changes at new time");
    assert!(
        !update.dependencies_changed,
        "time preserves provider dependencies"
    );
    assert_eq!(
        f.session.stats().shading_walks,
        walks,
        "time does not traverse connections"
    );
    f.session
        .apply(&set(f.layer, f.a, Value::Float(0.5)))
        .unwrap();
    f.session.poll(q).unwrap();
    assert!(
        !f.session.poll(q).unwrap().evaluated,
        "clean polls retain the answer"
    );
}

#[test]
fn deletion_recreation_missing_providers_and_external_edits() {
    let mut f = fixture();
    let x = f.session.observe(Query::WorldTransform(f.child));
    let b = f.session.observe(Query::WorldBound(f.root));
    let q = f.session.observe(Query::ShadingValue(f.input));
    f.session.poll(x).unwrap();
    f.session.poll(b).unwrap();
    f.session.poll(q).unwrap();
    let mut remove = Transaction::new();
    remove.remove_spec(EditTarget::for_layer(f.layer).prim(f.child));
    let removed = f.session.apply(&remove).unwrap();
    assert_eq!(
        f.session.poll(x).unwrap().answer,
        &QueryAnswer::WorldTransform(None),
        "deleted prim becomes absent"
    );
    assert_fresh(&mut f, x, b, Time::Default);
    f.session.apply(&removed.inverse).unwrap();
    assert_fresh(&mut f, x, b, Time::Default);
    let mut remove = Transaction::new();
    remove.remove_spec(EditTarget::for_layer(f.layer).property(f.a));
    let removed = f.session.apply(&remove).unwrap();
    let update = f.session.poll(q).unwrap();
    let QueryAnswer::ShadingValue(value) = update.answer else {
        panic!("shading result");
    };
    assert!(
        value.providers.sources.is_empty(),
        "deleted provider cannot produce a value"
    );
    assert!(
        !value.providers.issues.is_empty(),
        "missing target remains diagnosable"
    );
    f.session.apply(&removed.inverse).unwrap();
    assert!(
        f.session.poll(q).unwrap().answer_changed,
        "recreated provider is observed"
    );
    let layer = f.layer;
    let size = f.size;
    f.session.edit_sources(|store| {
        let spec = store
            .layers
            .get_mut(&layer)
            .unwrap()
            .property_mut(size)
            .unwrap();
        spec.default = Some(Value::Double(8.0));
    });
    assert_fresh(&mut f, x, b, Time::Default);
}

#[test]
fn failed_transactions_bounded_causes_and_nan_stability() {
    let mut f = fixture();
    let q = f.session.observe(Query::ShadingValue(f.input));
    let revision = f.session.poll(q).unwrap().revision;
    assert!(
        f.session
            .apply(&set(f.layer, f.a, Value::Bool(true)))
            .is_err(),
        "wrong type is rejected"
    );
    let update = f.session.poll(q).unwrap();
    assert!(
        !update.evaluated,
        "failure does not dirty a retained result"
    );
    assert_eq!(
        update.revision, revision,
        "failure does not advance revision"
    );
    f.session
        .apply(&set(f.layer, f.a, Value::Float(f32::NAN)))
        .unwrap();
    assert!(
        f.session.poll(q).unwrap().answer_changed,
        "NaN replaces previous value"
    );
    f.session
        .apply(&set(f.layer, f.a, Value::Float(f32::NAN)))
        .unwrap();
    assert!(
        !f.session.poll(q).unwrap().answer_changed,
        "an unchanged NaN is stable"
    );
    let b = f.session.observe(Query::WorldBound(f.root));
    f.session.poll(b).unwrap();
    for i in 0..20 {
        let path = f.session.path(&format!("/World/New{i}")).unwrap();
        let mut edit = f.session.edit(EditTarget::for_layer(f.layer));
        layerstack_schemas::usd_geom::Cube::define(&mut edit, path);
        let transaction = edit.finish();
        f.session.apply(&transaction).unwrap();
    }
    let update = f.session.poll(b).unwrap();
    assert_eq!(
        update.causes.len(),
        8,
        "cause history has an explicit memory budget"
    );
    assert!(update.omitted_causes > 0, "truncation is observable");
}

#[test]
#[cfg(panic = "unwind")]
fn interrupted_external_edit_is_refreshed_before_polling() {
    let mut f = fixture();
    let b = f.session.observe(Query::WorldBound(f.root));
    f.session.poll(b).unwrap();
    let layer = f.layer;
    let size = f.size;
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.session.edit_sources(|store| {
            store
                .layers
                .get_mut(&layer)
                .unwrap()
                .property_mut(size)
                .unwrap()
                .default = Some(Value::Double(10.0));
            panic!("interrupted host edit");
        });
    }));
    assert!(
        failed.is_err(),
        "exercise unwinding through external editing"
    );
    assert!(
        f.session.poll(b).unwrap().answer_changed,
        "pending source mutations cannot expose stale answers"
    );
}

#[test]
fn public_cpp_notices_and_computations_match_edit_sequence() {
    use layerstack::{ListOp, TargetPath};
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/retained_queries.json")).unwrap();
    assert_eq!(
        oracle["version"],
        layerstack_schemas::OPENUSD_VERSION,
        "oracle version is pinned"
    );
    let mut f = fixture();
    let x = f.session.observe(Query::WorldTransform(f.child));
    let b = f.session.observe(Query::WorldBound(f.root));
    let q = f.session.observe(Query::ShadingValue(f.input));
    for record in oracle["records"].as_array().unwrap() {
        let name = record["name"].as_str().unwrap();
        let txn = match name {
            "unrelated" => Some(set(f.layer, f.unrelated, Value::Double(2.0))),
            "translated" => Some(set(f.layer, f.translate, Value::Vec3d([4.0, 0.0, 0.0]))),
            "undo_translate" => Some(set(f.layer, f.translate, Value::Vec3d([0.0; 3]))),
            "size_4" => Some(set(f.layer, f.size, Value::Double(4.0))),
            "undo_size" => Some(set(f.layer, f.size, Value::Double(2.0))),
            "provider_b" => {
                f.session.set_time(Time::Default);
                let mut txn = Transaction::new();
                txn.set_targets(
                    EditTarget::for_layer(f.layer).property(f.input),
                    ListOp::explicit(vec![TargetPath::Property(f.b)]),
                );
                Some(txn)
            }
            "time_1" => {
                f.session.set_time(Time::at(1.0));
                None
            }
            "initial" => None,
            _ => panic!("unknown oracle step"),
        };
        if let Some(txn) = txn {
            let applied = f.session.apply(&txn).unwrap();
            let mut actual = serde_json::Map::new();
            for prim in &applied.changes.property_changes {
                for field in &prim.fields {
                    let path = PropertyPath::new(prim.prim, field.name)
                        .display(&f.session.store().paths, &f.session.store().tokens);
                    let key = match field.field {
                        layerstack::PropertyField::Default => "default",
                        layerstack::PropertyField::Targets => "connectionPaths",
                        _ => panic!("unexpected field in oracle"),
                    };
                    actual.insert(path, serde_json::json!([key]));
                }
            }
            assert_eq!(
                serde_json::Value::Object(actual),
                record["notices"][0]["info"],
                "C++ field evidence at {name}"
            );
        }
        let QueryAnswer::WorldTransform(Some(matrix)) = f.session.poll(x).unwrap().answer else {
            panic!("transform result");
        };
        assert_eq!(
            serde_json::json!(matrix),
            record["matrix"],
            "C++ transform at {name}"
        );
        let QueryAnswer::WorldBound(Ok(bbox)) = f.session.poll(b).unwrap().answer else {
            panic!("bound result");
        };
        let range = bbox.aligned_range();
        assert_eq!(
            serde_json::json!(range.min),
            record["min"],
            "C++ bound minimum at {name}"
        );
        assert_eq!(
            serde_json::json!(range.max),
            record["max"],
            "C++ bound maximum at {name}"
        );
        let QueryAnswer::ShadingValue(value) = f.session.poll(q).unwrap().answer else {
            panic!("shading result");
        };
        let paths: Vec<_> = value
            .providers
            .sources
            .iter()
            .map(|s| s.attribute)
            .collect();
        let values: Vec<_> = value
            .values
            .iter()
            .map(|v| match v {
                Some(Value::Float(v)) => *v,
                _ => panic!("float provider"),
            })
            .collect();
        let store = f.session.store();
        let paths: Vec<_> = paths
            .iter()
            .map(|p| p.display(&store.paths, &store.tokens))
            .collect();
        assert_eq!(
            serde_json::json!(paths),
            record["providers"],
            "C++ providers at {name}"
        );
        assert_eq!(
            serde_json::json!(values),
            record["values"],
            "C++ shading values at {name}"
        );
    }
}

#[test]
fn equal_values_from_a_new_authored_layer_change_provenance() {
    let mut f = fixture();
    let root = f.layer;
    let weak = LayerId(999);
    f.session.edit_sources(|store| {
        let mut source = store.layers.remove(&root).unwrap();
        source.id = weak;
        store.insert_layer(source);
        let mut strong = layerstack::Layer::new(root);
        strong.sublayers.push(layerstack::SublayerEntry::new(weak));
        store.insert_layer(strong);
    });
    let q = f.session.observe(Query::ShadingValue(f.input));
    f.session.poll(q).unwrap();
    let applied = f
        .session
        .apply(&set(root, f.a, Value::Float(0.25)))
        .unwrap();
    let update = f.session.poll(q).unwrap();
    assert!(
        !update.answer_changed,
        "same provider and value remain equal"
    );
    assert!(
        update.provenance_changed,
        "a new winning authored layer is observable"
    );
    f.session.apply(&applied.inverse).unwrap();
    assert!(
        f.session.poll(q).unwrap().provenance_changed,
        "undo restores the weaker source"
    );
}

#[test]
fn geometry_queries_share_transform_work_and_time_preserves_recipes() {
    let mut f = fixture();
    let x = f.session.observe(Query::WorldTransform(f.child));
    let b = f.session.observe(Query::WorldBound(f.child));
    f.session.poll(x).unwrap();
    let update = f.session.poll(b).unwrap();
    assert_eq!(
        update.work.local_transforms, 0,
        "bound query reuses prepared local transforms"
    );
    assert_eq!(
        update.work.world_transforms, 0,
        "bound query reuses composed world transforms"
    );
    f.session.set_time(Time::at(1.0));
    assert!(
        f.session.poll(x).unwrap().answer_changed,
        "animated transform advances"
    );
    let update = f.session.poll(b).unwrap();
    assert_eq!(
        update.work.local_transforms, 0,
        "frame evaluation is shared across query kinds"
    );
    assert_eq!(
        update.work.world_transforms, 0,
        "world composition is shared across query kinds"
    );
    assert!(
        !update.dependencies_changed,
        "time does not rebuild dependency recipes"
    );
}

#[test]
fn prepared_transform_evidence_excludes_unused_animation_and_handles_root() {
    let mut f = fixture();
    let layer = f.layer;
    let child = f.reset_child;
    f.session.edit_sources(|store| {
        let unused = store.tokens.intern("xformOp:translate:unused");
        store.layers.get_mut(&layer).unwrap().set_property(
            PropertyPath::new(child, unused),
            layerstack::PropertySpec::typed_attribute(layerstack::PropertyType::new(
                "double3",
                false,
                Value::Vec3d([0.0; 3]),
            ))
            .with_time_samples(vec![
                (0.0, Value::Vec3d([0.0; 3])),
                (2.0, Value::Vec3d([2.0; 3])),
            ]),
        );
    });
    let x = f.session.observe(Query::WorldTransform(child));
    f.session.poll(x).unwrap();
    f.session.set_time(Time::at(1.0));
    assert!(
        !f.session.poll(x).unwrap().evaluated,
        "unused animation outside xformOpOrder is not a dependency"
    );
    let root = f.session.path("/").unwrap();
    let b = f.session.observe(Query::WorldBound(root));
    f.session.poll(b).unwrap();
    let x = f.session.observe(Query::WorldTransform(root));
    assert!(
        matches!(
            f.session.poll(x).unwrap().answer,
            QueryAnswer::WorldTransform(Some(_))
        ),
        "pseudo-root retains its identity transform"
    );
}
