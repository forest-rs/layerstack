// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use alloc::{sync::Arc, vec};
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, ListOp, LiveStage, PrimSpec, PropertySpec,
    PropertyType, StageOptions, Transaction,
};

#[derive(Debug)]
struct Pick {
    factor: i32,
}
impl ProceduralEvaluator for Pick {
    type Output = Option<i32>;
    type Error = ProceduralInputError;
    fn system(&self) -> &str {
        "test:pick"
    }
    fn evaluate(
        &self,
        inputs: &mut ProceduralInputs<'_>,
    ) -> Result<Option<i32>, ProceduralInputError> {
        let targets = inputs.parameter_targets("source")?.unwrap_or_default();
        let [TargetPath::Property(target)] = targets.as_slice() else {
            return Ok(None);
        };
        let name = String::from(inputs.tokens().resolve(target.property()));
        let value = inputs.attribute(target.prim_path(), &name)?;
        let bias = inputs
            .parameter("bias")?
            .as_ref()
            .and_then(|v| crate::value::read_int(v, inputs.tokens()))
            .unwrap_or(0);
        Ok(value
            .as_ref()
            .and_then(|v| crate::value::read_int(v, inputs.tokens()))
            .map(|v| v * self.factor + bias))
    }
}
const ROOT: LayerId = LayerId(1);
fn setup() -> (InMemoryStore, LiveStage, Procedural<Pick>) {
    let mut store = InMemoryStore::default();
    let recipe = store.path("/Recipe");
    let input = store.path("/Inputs");
    let ty = store.tokens.intern("GenerativeProcedural");
    let system = store.tokens.intern("test:pick");
    let system_property = store.property_path("/Recipe.proceduralSystem");
    let source = store.property_path("/Recipe.primvars:source");
    let a = store.property_path("/Inputs.a");
    let b = store.property_path("/Inputs.b");
    let mut layer = Layer::new(ROOT);
    let mut spec = PrimSpec::def();
    spec.type_name = Some(ty);
    layer.insert_prim(recipe, spec);
    layer.insert_prim(input, PrimSpec::def());
    layer.set_property(
        system_property,
        PropertySpec::typed_attribute(PropertyType::new("token", false, Value::Token(system)))
            .with_default(Value::Token(system)),
    );
    layer.set_property(
        source,
        PropertySpec::relationship().with_targets(ListOp::explicit(vec![TargetPath::Property(a)])),
    );
    for (path, value) in [(a, 1), (b, 2)] {
        layer.set_property(
            path,
            PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                .with_default(Value::Int(value)),
        );
    }
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
        ..Default::default()
    };
    let live = LiveStage::compose(&mut store, ROOT, options);
    let binding = Procedural::new(&store, recipe, Pick { factor: 1 });
    (store, live, binding)
}
fn set(store: &mut InMemoryStore, live: &mut LiveStage, property: &str, value: Value) {
    let path = store.property_path(property);
    let mut txn = Transaction::new();
    txn.set_default(EditTarget::for_layer(ROOT).property(path), value);
    live.apply(store, &txn).unwrap();
}
#[test]
fn bindings_and_detached_results_reject_foreign_domains_even_with_equal_ids() {
    let (store, live, mut binding) = setup();
    let (other, other_live, _) = setup();
    assert!(matches!(
        binding.evaluate(&Scene::new(other_live.stage(), &other), Time::Default),
        Err(ProceduralError::DifferentStore)
    ));
    assert!(matches!(
        binding.evaluate(&Scene::new(live.stage(), &other), Time::Default),
        Err(ProceduralError::DifferentStore)
    ));
    let result = binding
        .snapshot(&Scene::new(live.stage(), &store), Time::Default)
        .unwrap();
    assert!(matches!(
        result
            .evidence()
            .verify(&Scene::new(other_live.stage(), &other)),
        Err(EvidenceError::DifferentStore)
    ));
    let moved = store;
    assert!(
        result
            .evidence()
            .verify(&Scene::new(live.stage(), &moved))
            .is_ok()
    );
}
#[test]
fn unchanged_queries_skip_resolution_and_detached_work_tracks_source_and_epoch() {
    let (mut store, mut live, mut binding) = setup();
    let result = binding
        .snapshot(&Scene::new(live.stage(), &store), Time::Default)
        .unwrap();
    let before = binding.work();
    let repeated = binding
        .snapshot(&Scene::new(live.stage(), &store), Time::Default)
        .unwrap();
    assert!(Arc::ptr_eq(
        &result.shared_output(),
        &repeated.shared_output()
    ));
    assert_eq!(binding.work().input_reads, before.input_reads);
    assert_eq!(binding.work().query_cache_hits - before.query_cache_hits, 4);

    // Bypass the LiveStage: guarded application must synchronize before checking.
    let a = store.property_path("/Inputs.a");
    store.layers.get_mut(&ROOT).unwrap().set_property(
        a,
        PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
            .with_default(Value::Int(9)),
    );
    let published = store.property_path("/Published.value");
    let mut transaction = Transaction::new();
    transaction.create_prim(
        EditTarget::for_layer(ROOT).prim(published.prim_path()),
        layerstack::Specifier::Def,
        None,
    );
    transaction.create_property(
        EditTarget::for_layer(ROOT).property(published),
        PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
            .with_default(Value::Int(1)),
    );
    assert!(matches!(
        result.evidence().apply(&mut live, &mut store, &transaction),
        Err(EvidenceApplyError::Evidence(EvidenceError::Changed { .. }))
    ));
    assert!(store.layers[&ROOT].property(published).is_none());
    let fresh = binding
        .snapshot(&Scene::new(live.stage(), &store), Time::Default)
        .unwrap();
    assert_eq!(*fresh.output(), Some(9));
    fresh
        .evidence()
        .apply(&mut live, &mut store, &transaction)
        .unwrap();
    binding.invalidate();
    assert!(matches!(
        fresh.evidence().verify(&Scene::new(live.stage(), &store)),
        Err(EvidenceError::Invalidated)
    ));
}
#[test]
fn composed_reads_track_absence_and_replace_dynamic_dependencies() {
    let (mut store, mut live, mut procedural) = setup();
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(1)
    );
    assert_eq!(procedural.dependencies().count(), 4);
    assert!(
        procedural
            .dependencies()
            .any(|d| d.property == "primvars:bias")
    );
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(1)
    );
    assert_eq!(procedural.work().evaluations, 1);
    set(&mut store, &mut live, "/Inputs.b", Value::Int(7));
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        procedural.work().evaluations,
        1,
        "unread properties do not execute the generator"
    );
    let bias = store.property_path("/Recipe.primvars:bias");
    let mut edit = Transaction::new();
    edit.create_property(
        EditTarget::for_layer(ROOT).property(bias),
        PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
            .with_default(Value::Int(3)),
    );
    live.apply(&mut store, &edit).unwrap();
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(4)
    );
    let b = store.property_path("/Inputs.b");
    let source = store.property_path("/Recipe.primvars:source");
    let mut edit = Transaction::new();
    edit.set_targets(
        EditTarget::for_layer(ROOT).property(source),
        ListOp::explicit(vec![TargetPath::Property(b)]),
    );
    live.apply(&mut store, &edit).unwrap();
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(10)
    );
    let a = store.tokens.lookup("a").unwrap();
    assert!(
        !procedural
            .dependencies()
            .any(|d| store.tokens.lookup(d.property) == Some(a))
    );
    let before = procedural.work().evaluations;
    set(&mut store, &mut live, "/Inputs.a", Value::Int(20));
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(10)
    );
    assert_eq!(procedural.work().evaluations, before);
    procedural.evaluator_mut().factor = 2;
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(17)
    );
    procedural.invalidate(); // Generator code or an external file changed.
    procedural
        .evaluate(&Scene::new(live.stage(), &store), Time::Default)
        .unwrap();
    assert_eq!(procedural.work().evaluations, before + 2);
}
#[test]
fn missing_recipes_systems_and_wrong_kinds_never_return_stale_outputs() {
    let (mut store, mut live, mut procedural) = setup();
    procedural
        .evaluate(&Scene::new(live.stage(), &store), Time::Default)
        .unwrap();
    let recipe = procedural.recipe();
    let mut remove = Transaction::new();
    remove.remove_spec(EditTarget::for_layer(ROOT).prim(recipe));
    let removed = live.apply(&mut store, &remove).unwrap();
    assert!(
        matches!(procedural.evaluate(&Scene::new(live.stage(), &store), Time::Default), Err(ProceduralError::NotProcedural(p)) if p == recipe)
    );
    assert_eq!(procedural.dependencies().count(), 0);
    live.apply(&mut store, &removed.inverse).unwrap();
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(1)
    );
    let unsupported = store.tokens.intern("other:system");
    set(
        &mut store,
        &mut live,
        "/Recipe.proceduralSystem",
        Value::Token(unsupported),
    );
    assert!(matches!(
        procedural.evaluate(&Scene::new(live.stage(), &store), Time::Default),
        Err(ProceduralError::System { .. })
    ));
    let expected = store.tokens.intern("test:pick");
    set(
        &mut store,
        &mut live,
        "/Recipe.proceduralSystem",
        Value::Token(expected),
    );
    let source = store.property_path("/Recipe.primvars:source");
    let mut wrong = Transaction::new();
    wrong.remove_spec(EditTarget::for_layer(ROOT).property(source));
    wrong.create_property(
        EditTarget::for_layer(ROOT).property(source),
        PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
            .with_default(Value::Int(1)),
    );
    live.apply(&mut store, &wrong).unwrap();
    assert!(matches!(
        procedural.evaluate(&Scene::new(live.stage(), &store), Time::Default),
        Err(ProceduralError::Evaluation(
            ProceduralInputError::WrongKind { .. }
        ))
    ));
    assert_eq!(
        procedural.dependencies().count(),
        0,
        "failed evaluations are not retained"
    );
}
#[test]
fn forwarded_target_edits_and_target_recreation_refresh_the_result() {
    let (mut store, mut live, mut procedural) = setup();
    let source = store.property_path("/Recipe.primvars:source");
    let a = store.property_path("/Inputs.a");
    let b = store.property_path("/Inputs.b");
    let forwarding = store.property_path("/Inputs.forward");
    let mut edit = Transaction::new();
    edit.create_property(
        EditTarget::for_layer(ROOT).property(forwarding),
        PropertySpec::relationship().with_targets(ListOp::explicit(vec![TargetPath::Property(a)])),
    );
    edit.set_targets(
        EditTarget::for_layer(ROOT).property(source),
        ListOp::explicit(vec![TargetPath::Property(forwarding)]),
    );
    live.apply(&mut store, &edit).unwrap();
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(1)
    );
    let mut edit = Transaction::new();
    edit.set_targets(
        EditTarget::for_layer(ROOT).property(forwarding),
        ListOp::explicit(vec![TargetPath::Property(b)]),
    );
    live.apply(&mut store, &edit).unwrap();
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(2)
    );
    let mut edit = Transaction::new();
    edit.remove_spec(EditTarget::for_layer(ROOT).property(b));
    let removed = live.apply(&mut store, &edit).unwrap();
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        None
    );
    live.apply(&mut store, &removed.inverse).unwrap();
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        Some(2)
    );
    let before = procedural.work().evaluations;
    procedural
        .evaluate(&Scene::new(live.stage(), &store), Time::at(1.))
        .unwrap();
    assert_eq!(
        procedural.work().evaluations,
        before + 1,
        "evaluation time is part of the cache key"
    );
}
#[test]
fn ignored_input_errors_cannot_produce_a_cached_success() {
    #[derive(Debug)]
    struct Ignoring;
    impl ProceduralEvaluator for Ignoring {
        type Output = i32;
        type Error = ProceduralInputError;
        fn system(&self) -> &str {
            "test:pick"
        }
        fn evaluate(&self, inputs: &mut ProceduralInputs<'_>) -> Result<i32, ProceduralInputError> {
            let _ = inputs.parameter("source"); // Deliberately asks for the wrong kind.
            Ok(17)
        }
    }
    let (store, live, binding) = setup();
    let mut procedural = Procedural::new(&store, binding.recipe(), Ignoring);
    for _ in 0..2 {
        assert!(matches!(
            procedural.evaluate(&Scene::new(live.stage(), &store), Time::Default),
            Err(ProceduralError::Input(
                ProceduralInputError::WrongKind { .. }
            ))
        ));
    }
    assert_eq!(procedural.work().evaluations, 2);
    assert_eq!(procedural.work().cache_hits, 0);
    assert_eq!(procedural.dependencies().count(), 0);
}
#[cfg(feature = "usd-hydra")]
#[test]
fn api_schema_system_fallback_dispatches_without_an_authored_token() {
    struct Hydra;
    impl ProceduralEvaluator for Hydra {
        type Output = i32;
        type Error = ProceduralInputError;
        fn system(&self) -> &str {
            "hydraGenerativeProcedural"
        }
        fn evaluate(&self, _: &mut ProceduralInputs<'_>) -> Result<i32, ProceduralInputError> {
            Ok(17)
        }
    }
    let (mut store, mut live, binding) = setup();
    let property = store.property_path("/Recipe.proceduralSystem");
    let mut edit = Transaction::new();
    edit.remove_spec(EditTarget::for_layer(ROOT).property(property));
    live.apply(&mut store, &edit).unwrap();
    let mut edit = crate::SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
    crate::usd_hydra::HydraGenerativeProceduralApi::apply(&mut edit, binding.recipe()).unwrap();
    let transaction = edit.finish();
    live.apply(&mut store, &transaction).unwrap();
    let mut procedural = Procedural::new(&store, binding.recipe(), Hydra);
    assert_eq!(
        *procedural
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap(),
        17
    );
}
