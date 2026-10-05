// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::procedural::{ProceduralInputError, ProceduralInputs};
use alloc::{sync::Arc, vec};
use core::{
    convert::Infallible,
    sync::atomic::{AtomicI32, AtomicUsize, Ordering},
};
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, PrimSpec, PropertyPath, PropertySpec, PropertyType,
    StageOptions, Value,
};

const ROOT: LayerId = LayerId(1);

#[derive(Debug)]
struct Sum {
    sources: Vec<PathId>,
    calls: Arc<AtomicUsize>,
    multiplier: Arc<AtomicI32>,
}
impl ProceduralEvaluator for Sum {
    type Output = i32;
    type Error = ProceduralInputError;
    fn system(&self) -> &str {
        "test:sum"
    }
    fn evaluate(&self, input: &mut ProceduralInputs<'_>) -> Result<i32, Self::Error> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let seed = input.parameter("seed")?;
        let mut sum = seed
            .as_ref()
            .and_then(|v| crate::value::read_int(v, input.tokens()))
            .unwrap_or(0);
        for &source in &self.sources {
            let value = input.attribute(source, "value")?;
            sum += value
                .as_ref()
                .and_then(|v| crate::value::read_int(v, input.tokens()))
                .unwrap_or(0);
        }
        Ok(sum * self.multiplier.load(Ordering::Relaxed))
    }
}

#[derive(Default)]
struct Publish {
    acknowledged: Vec<PathId>,
    fail: Option<PathId>,
}
impl ProducerPublisher<i32> for Publish {
    type Error = &'static str;
    fn prepare(
        &mut self,
        spec: &ProducerSpec,
        output: &i32,
        _stage: &Stage,
        store: &mut dyn LayerStore,
    ) -> Result<Transaction, Self::Error> {
        if self.fail == Some(spec.recipe) {
            return Err("host rejected output");
        }
        let token = store.tokens_mut().intern("value");
        let mut transaction = Transaction::new();
        transaction.expect_generation(ROOT, store.layer(ROOT).unwrap().generation());
        for &site in &spec.outputs {
            let path = PropertyPath::new(site, token);
            if store
                .layer(ROOT)
                .unwrap()
                .property(path)
                .and_then(|p| p.default.as_ref())
                != Some(&Value::Int(*output))
            {
                transaction.set_default(
                    EditTarget::for_layer(ROOT).property(path),
                    Value::Int(*output),
                );
            }
        }
        Ok(transaction)
    }
    fn committed(&mut self, spec: &ProducerSpec, _: &Applied) {
        self.acknowledged.push(spec.recipe);
    }
}

struct Fixture {
    store: InMemoryStore,
    live: LiveStage,
    graph: ProceduralGraph<Sum>,
    recipes: [PathId; 3],
    outputs: [PathId; 3],
    calls: [Arc<AtomicUsize>; 3],
    multiplier: Arc<AtomicI32>,
}
fn fixture() -> Fixture {
    let mut store = InMemoryStore::default();
    let recipes = [
        store.path("/Terrain"),
        store.path("/Scatter"),
        store.path("/Unrelated"),
    ];
    let outputs = [
        store.path("/TerrainOutput"),
        store.path("/ScatterOutput"),
        store.path("/UnrelatedOutput"),
    ];
    let ty = store.tokens.intern("GenerativeProcedural");
    let system = store.tokens.intern("test:sum");
    let system_name = store.tokens.intern("proceduralSystem");
    let seed_name = store.tokens.intern("primvars:seed");
    let value = store.tokens.intern("value");
    let mut layer = Layer::new(ROOT);
    for (i, &recipe) in recipes.iter().enumerate() {
        let mut prim = PrimSpec::def();
        prim.type_name = Some(ty);
        layer.insert_prim(recipe, prim);
        layer.set_property(
            PropertyPath::new(recipe, system_name),
            PropertySpec::typed_attribute(PropertyType::new("token", false, Value::Token(system)))
                .with_default(Value::Token(system)),
        );
        layer.set_property(
            PropertyPath::new(recipe, seed_name),
            PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                .with_default(Value::Int(i32::try_from(i).unwrap() + 1)),
        );
    }
    for &site in &outputs {
        layer.insert_prim(site, PrimSpec::def());
        layer.set_property(
            PropertyPath::new(site, value),
            PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                .with_default(Value::Int(0)),
        );
    }
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
        ..Default::default()
    };
    let live = LiveStage::compose(&mut store, ROOT, options);
    let calls = core::array::from_fn(|_| Arc::new(AtomicUsize::new(0)));
    let multiplier = Arc::new(AtomicI32::new(1));
    let mut graph = ProceduralGraph::new(&store);
    for (i, &recipe) in recipes.iter().enumerate() {
        let inputs = if i == 1 { vec![outputs[0]] } else { Vec::new() };
        graph
            .register(
                &store,
                ProducerSpec {
                    recipe,
                    outputs: vec![outputs[i]],
                    inputs: inputs.clone(),
                    resources: if i == 0 {
                        vec!["heightmap".into()]
                    } else {
                        Vec::new()
                    },
                },
                Sum {
                    sources: inputs,
                    calls: calls[i].clone(),
                    multiplier: if i == 0 {
                        multiplier.clone()
                    } else {
                        Arc::new(AtomicI32::new(1))
                    },
                },
            )
            .unwrap();
    }
    Fixture {
        store,
        live,
        graph,
        recipes,
        outputs,
        calls,
        multiplier,
    }
}
fn resources() -> ResourceRevisions {
    ResourceRevisions::from([("heightmap".into(), 1)])
}
fn set(f: &mut Fixture, path: &str, value: i32) {
    let path = f.store.property_path(path);
    let mut transaction = Transaction::new();
    transaction.set_default(
        EditTarget::for_layer(ROOT).property(path),
        Value::Int(value),
    );
    f.live.apply(&mut f.store, &transaction).unwrap();
}
fn build(f: &mut Fixture, publisher: &mut Publish) -> BuildResult<i32> {
    f.graph
        .request(
            &mut f.live,
            &mut f.store,
            f.outputs[1],
            Time::Default,
            &resources(),
            publisher,
        )
        .unwrap()
}

#[test]
fn dependency_order_reuse_branch_edits_and_publication_repair() {
    let mut f = fixture();
    let mut publisher = Publish::default();
    let initial = build(&mut f, &mut publisher);
    assert_eq!(
        initial.steps.iter().map(|s| s.recipe).collect::<Vec<_>>(),
        f.recipes[..2]
    );
    assert_eq!(*initial.steps[0].output(), 1);
    assert_eq!(
        *initial.steps[1].output(),
        3,
        "downstream sees the newly published prerequisite"
    );
    assert_eq!(
        f.calls[2].load(Ordering::Relaxed),
        0,
        "unrequested branch never evaluates"
    );
    let repeated = build(&mut f, &mut publisher);
    assert!(
        repeated
            .steps
            .iter()
            .all(|s| !s.evaluated && s.changes == Changes::default())
    );
    assert!(Arc::ptr_eq(
        &initial.steps[1].shared_output(),
        &repeated.steps[1].shared_output()
    ));
    set(&mut f, "/Unrelated.primvars:seed", 99);
    assert!(
        build(&mut f, &mut publisher)
            .steps
            .iter()
            .all(|s| !s.evaluated)
    );
    set(&mut f, "/Scatter.primvars:seed", 7);
    let edited = build(&mut f, &mut publisher);
    assert!(!edited.steps[0].evaluated);
    assert!(edited.steps[1].evaluated);
    assert_eq!(*edited.steps[1].output(), 8);
    set(&mut f, "/TerrainOutput.value", 100);
    let repaired = build(&mut f, &mut publisher);
    assert!(!repaired.steps[0].evaluated);
    assert_ne!(
        repaired.steps[0].changes,
        Changes::default(),
        "cached evaluation still repairs output-site edits"
    );
    assert!(
        !repaired.steps[1].evaluated,
        "repair restores the same consumed value"
    );
}

#[test]
fn resource_revisions_invalidate_only_owners_and_guard_transitive_delayed_work() {
    let mut f = fixture();
    let initial = build(&mut f, &mut Publish::default());
    let mut versions = resources();
    versions.insert("heightmap".into(), 2);
    let mut delayed = Transaction::new();
    let path = f.store.property_path("/ScatterOutput.value");
    delayed.set_default(EditTarget::for_layer(ROOT).property(path), Value::Int(99));
    assert!(
        matches!(
            initial.steps[1].apply(&mut f.live, &mut f.store, &versions, &delayed),
            Err(BuildEvidenceError::Resource { .. })
        ),
        "downstream evidence includes prerequisite resources before USD outputs change"
    );
    f.multiplier.store(2, Ordering::Relaxed);
    let changed = f
        .graph
        .request(
            &mut f.live,
            &mut f.store,
            f.outputs[1],
            Time::Default,
            &versions,
            &mut Publish::default(),
        )
        .unwrap();
    assert_eq!(*changed.steps[0].output(), 2);
    assert_eq!(*changed.steps[1].output(), 4);
    assert!(changed.steps.iter().all(|s| s.evaluated));
    assert_eq!(f.graph.work().resource_invalidations, 1);
    assert!(matches!(
        initial.steps[0].verify(&Scene::new(f.live.stage(), &f.store), &resources()),
        Err(BuildEvidenceError::Input(EvidenceError::Invalidated))
    ));
}

#[test]
fn prerequisite_input_edits_invalidation_and_removal_retire_delayed_downstream_work() {
    for mode in 0..3 {
        let mut f = fixture();
        let initial = build(&mut f, &mut Publish::default());
        match mode {
            0 => {
                // Direct authored edits have not yet synchronized the stage.
                let path = f.store.property_path("/Terrain.primvars:seed");
                f.store
                    .layers
                    .get_mut(&ROOT)
                    .unwrap()
                    .set_property(path, PropertySpec::attribute().with_default(Value::Int(9)));
            }
            1 => {
                assert!(f.graph.invalidate(f.recipes[0]));
            }
            _ => {
                assert!(f.graph.remove(f.recipes[0]));
            }
        }
        let before = f.store.layers[&ROOT].generation();
        let path = f.store.property_path("/ScatterOutput.value");
        let mut delayed = Transaction::new();
        delayed.set_default(EditTarget::for_layer(ROOT).property(path), Value::Int(99));
        assert!(
            matches!(initial.steps[1].apply(
                &mut f.live, &mut f.store, &resources(), &delayed),
                Err(BuildEvidenceError::Prerequisite { recipe, .. }) if recipe == f.recipes[0]
            ),
            "upstream staleness rejects downstream work before output changes"
        );
        assert_eq!(f.store.layers[&ROOT].generation(), before);
        assert_eq!(
            f.live.stage().resolve_field_path(path).unwrap().value,
            Value::Int(3)
        );
        assert!(matches!(
            initial.steps[1].verify(&Scene::new(f.live.stage(), &f.store), &resources()),
            Err(BuildEvidenceError::Prerequisite { .. })
        ));
    }
}

#[test]
fn rejected_publication_guards_leave_all_outputs_and_acknowledgements_unchanged() {
    struct StalePublisher {
        acknowledged: bool,
    }
    impl ProducerPublisher<i32> for StalePublisher {
        type Error = Infallible;
        fn prepare(
            &mut self,
            spec: &ProducerSpec,
            _: &i32,
            _: &Stage,
            store: &mut dyn LayerStore,
        ) -> Result<Transaction, Infallible> {
            let token = store.tokens_mut().intern("value");
            let mut edit = Transaction::new();
            edit.expect_generation(ROOT, store.layer(ROOT).unwrap().generation() + 1);
            for &site in &spec.outputs {
                edit.set_default(
                    EditTarget::for_layer(ROOT).property(PropertyPath::new(site, token)),
                    Value::Int(99),
                );
            }
            Ok(edit)
        }
        fn committed(&mut self, _: &ProducerSpec, _: &Applied) {
            self.acknowledged = true;
        }
    }
    let mut f = fixture();
    let before = f.store.layers[&ROOT].generation();
    let mut publisher = StalePublisher {
        acknowledged: false,
    };
    let failure = f
        .graph
        .request(
            &mut f.live,
            &mut f.store,
            f.outputs[1],
            Time::Default,
            &resources(),
            &mut publisher,
        )
        .unwrap_err();
    assert!(matches!(
        *failure.error,
        BuildError::Publication {
            error: BuildEvidenceError::Publication(EvidenceApplyError::Edit(_)),
            ..
        }
    ));
    assert!(failure.completed.steps.is_empty());
    assert!(!publisher.acknowledged);
    assert_eq!(f.store.layers[&ROOT].generation(), before);
    for site in f.outputs {
        let value = f.store.tokens.lookup("value").unwrap();
        assert_eq!(
            f.live
                .stage()
                .resolve_field_path(PropertyPath::new(site, value))
                .unwrap()
                .value,
            Value::Int(0)
        );
    }
}

#[test]
fn delayed_evidence_reaches_indirect_prerequisites() {
    let mut f = fixture();
    assert!(f.graph.remove(f.recipes[2]));
    f.graph
        .register(
            &f.store,
            ProducerSpec {
                recipe: f.recipes[2],
                outputs: vec![f.outputs[2]],
                inputs: vec![f.outputs[1]],
                resources: Vec::new(),
            },
            Sum {
                sources: vec![f.outputs[1]],
                calls: f.calls[2].clone(),
                multiplier: Arc::new(AtomicI32::new(1)),
            },
        )
        .unwrap();
    let built = f
        .graph
        .request(
            &mut f.live,
            &mut f.store,
            f.outputs[2],
            Time::Default,
            &resources(),
            &mut Publish::default(),
        )
        .unwrap();
    assert_eq!(built.steps.len(), 3);
    assert_eq!(*built.steps[2].output(), 6);
    assert!(f.graph.invalidate(f.recipes[0]));
    assert!(
        matches!(built.steps[2].verify(&Scene::new(f.live.stage(), &f.store), &resources()),
        Err(BuildEvidenceError::Prerequisite { recipe, error: EvidenceError::Invalidated })
        if recipe == f.recipes[0])
    );
}

#[test]
fn releasing_deep_detached_guard_chains_does_not_recurse() {
    let mut f = fixture();
    let built = build(&mut f, &mut Publish::default());
    let base = built.steps[0].guard.clone();
    let mut tail = base.clone();
    for _ in 0..16_384 {
        tail = Arc::new(Guard {
            store: base.store.clone(),
            recipe: base.recipe,
            evidence: base.evidence.clone(),
            prerequisites: vec![tail],
        });
    }
    drop(tail);
    assert!(
        base.evidence
            .verify(&Scene::new(f.live.stage(), &f.store))
            .is_ok()
    );
}

#[test]
fn failed_downstream_reports_completed_publications_and_preserves_previous_output() {
    let mut f = fixture();
    let initial = build(&mut f, &mut Publish::default());
    set(&mut f, "/Terrain.primvars:seed", 10);
    let mut publisher = Publish {
        fail: Some(f.recipes[1]),
        ..Default::default()
    };
    let failure = f
        .graph
        .request(
            &mut f.live,
            &mut f.store,
            f.outputs[1],
            Time::Default,
            &resources(),
            &mut publisher,
        )
        .unwrap_err();
    assert_eq!(failure.completed.steps.len(), 1);
    assert_eq!(*failure.completed.steps[0].output(), 10);
    assert!(matches!(*failure.error, BuildError::Preparation { .. }));
    assert_eq!(publisher.acknowledged, [f.recipes[0]]);
    let path = f.store.property_path("/ScatterOutput.value");
    assert_eq!(
        f.live.stage().resolve_field_path(path).unwrap().value,
        Value::Int(3)
    );
    assert_eq!(
        *initial.steps[1].output(),
        3,
        "old shared output remains valid"
    );
    assert_eq!(
        *build(&mut f, &mut Publish::default()).steps[1].output(),
        12
    );
}

#[test]
fn registration_conflicts_and_preflight_failures_do_not_publish() {
    let mut f = fixture();
    let before = f.store.layers[&ROOT].generation();
    let mut publisher = Publish::default();
    let failure = f
        .graph
        .request(
            &mut f.live,
            &mut f.store,
            f.outputs[1],
            Time::Default,
            &ResourceRevisions::new(),
            &mut publisher,
        )
        .unwrap_err();
    assert!(matches!(
        *failure.error,
        BuildError::Graph(GraphError::MissingResource { .. })
    ));
    assert!(failure.completed.steps.is_empty());
    assert_eq!(f.store.layers[&ROOT].generation(), before);
    let extra = f.store.path("/Extra");
    let evaluator = || Sum {
        sources: Vec::new(),
        calls: Arc::new(AtomicUsize::new(0)),
        multiplier: Arc::new(AtomicI32::new(1)),
    };
    assert!(matches!(
        f.graph.register(
            &f.store,
            ProducerSpec {
                recipe: extra,
                outputs: vec![f.outputs[0]],
                inputs: Vec::new(),
                resources: Vec::new()
            },
            evaluator()
        ),
        Err(GraphError::OutputClaimed { .. })
    ));
    assert_eq!(f.graph.producers().count(), 3);
    let absent = f.store.path("/AbsentOutput");
    f.graph
        .register(
            &f.store,
            ProducerSpec {
                recipe: extra,
                outputs: vec![extra],
                inputs: vec![absent],
                resources: Vec::new(),
            },
            evaluator(),
        )
        .unwrap();
    assert_eq!(
        f.graph.plan(extra).unwrap_err(),
        GraphError::MissingProducer {
            consumer: Some(extra),
            output: absent
        }
    );
    assert!(f.graph.remove(f.recipes[0]));
    assert!(matches!(
        f.graph.plan(f.outputs[1]),
        Err(GraphError::MissingProducer { .. })
    ));
}

#[test]
fn cycle_preflight_and_foreign_domains_are_explicit() {
    let mut f = fixture();
    assert!(f.graph.remove(f.recipes[0]));
    f.graph
        .register(
            &f.store,
            ProducerSpec {
                recipe: f.recipes[0],
                outputs: vec![f.outputs[0]],
                inputs: vec![f.outputs[1]],
                resources: Vec::new(),
            },
            Sum {
                sources: Vec::new(),
                calls: f.calls[0].clone(),
                multiplier: f.multiplier.clone(),
            },
        )
        .unwrap();
    let GraphError::Cycle(cycle) = f.graph.plan(f.outputs[0]).unwrap_err() else {
        panic!("expected cycle");
    };
    assert_eq!(cycle.first(), cycle.last());
    assert_eq!(cycle.len(), 3);
    let mut other = fixture();
    let failure = f
        .graph
        .request(
            &mut other.live,
            &mut other.store,
            f.outputs[0],
            Time::Default,
            &resources(),
            &mut Publish::default(),
        )
        .unwrap_err();
    assert!(matches!(
        *failure.error,
        BuildError::Graph(GraphError::DifferentStore)
    ));
}

#[test]
fn removed_producer_retires_detached_evidence() {
    let mut f = fixture();
    let initial = build(&mut f, &mut Publish::default());
    assert!(f.graph.remove(f.recipes[1]));
    assert!(matches!(
        initial.steps[1].verify(&Scene::new(f.live.stage(), &f.store), &resources()),
        Err(BuildEvidenceError::Input(EvidenceError::Invalidated))
    ));
}

#[derive(Debug)]
struct Never;
impl ProceduralEvaluator for Never {
    type Output = ();
    type Error = Infallible;
    fn system(&self) -> &str {
        "never"
    }
    fn evaluate(&self, _: &mut ProceduralInputs<'_>) -> Result<(), Infallible> {
        unreachable!("planner does not evaluate")
    }
}
#[test]
fn long_dependency_plans_use_a_heap_stack_and_shared_dependencies_run_once() {
    let mut store = InMemoryStore::default();
    let mut graph = ProceduralGraph::new(&store);
    let mut previous = None;
    for i in 0..4096 {
        let recipe = store.path(&alloc::format!("/Producer{i}"));
        graph
            .register(
                &store,
                ProducerSpec {
                    recipe,
                    outputs: vec![recipe],
                    inputs: previous.into_iter().collect(),
                    resources: Vec::new(),
                },
                Never,
            )
            .unwrap();
        previous = Some(recipe);
    }
    assert_eq!(graph.plan(previous.unwrap()).unwrap().len(), 4096);
    let shared = previous.unwrap();
    let left = store.path("/Left");
    let right = store.path("/Right");
    let top = store.path("/Top");
    for (recipe, inputs) in [
        (left, vec![shared]),
        (right, vec![shared]),
        (top, vec![left, right]),
    ] {
        graph
            .register(
                &store,
                ProducerSpec {
                    recipe,
                    outputs: vec![recipe],
                    inputs,
                    resources: Vec::new(),
                },
                Never,
            )
            .unwrap();
    }
    assert_eq!(graph.plan(top).unwrap().len(), 4099);
}
