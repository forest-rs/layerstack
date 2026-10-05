// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Caller-driven builds over an explicit registry of procedural output sites.
//!
//! This adapter owns dependency ordering and declared resource invalidation.
//! Evaluators own generated values; publishers own authored sites and property
//! manifests. Requests are synchronous and publish each successful producer in
//! dependency order. A downstream failure leaves earlier successful publications
//! in place and exposes them in the failure report. There is no background
//! scheduler, automatic producer discovery or cross-producer atomicity.
//!
//! Dependencies name registered output prims, not persisted numeric IDs. All IDs
//! belong to the constructor's store. The application declares output dependencies
//! and external resource revisions; composed USD inputs still use the dynamic
//! tracked reads and evidence of [`crate::procedural::Procedural`]. Resource
//! revisions must identify immutable content/configuration snapshots throughout a request.
//! These are application build semantics above `UsdProc`, not Hydra execution.
//! USD reads/publication follow AOUSD Core §12.3–§12.5 and §13.3.2.4.

use super::{
    Evaluated, EvaluationEvidence, EvidenceApplyError, EvidenceError, Procedural, ProceduralError,
    ProceduralEvaluator,
};
use crate::Scene;
use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::fmt;
use layerstack::{
    Applied, Changes, LayerStore, LiveStage, PathId, Stage, StoreIdentity, Time, Transaction,
};

/// Explicit ownership and prerequisite declarations for one producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducerSpec {
    /// Store-local `GenerativeProcedural` recipe path.
    pub recipe: PathId,
    /// Exact output prim sites owned by this producer; at least one is required.
    /// Descendant ownership is not inferred and transactions are not sandboxed.
    pub outputs: Vec<PathId>,
    /// Registered output sites which must be published before evaluating this recipe.
    pub inputs: Vec<PathId>,
    /// Host resource/configuration keys whose revisions govern evaluation reuse.
    pub resources: Vec<String>,
}

/// Application resource versions. Revisions are opaque equality tokens, not clocks.
pub type ResourceRevisions = BTreeMap<String, u64>;

/// One external resource snapshot consumed by a producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceVersion {
    /// Application-defined resource or configuration identity.
    pub key: String,
    /// Revision supplied when the result was captured.
    pub revision: u64,
}

/// Invalid registry, dependency request or external resource declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// Supplied stage/store domains differ from the registry's original domain.
    DifferentStore,
    /// A recipe is already registered; remove it explicitly before replacement.
    AlreadyRegistered(PathId),
    /// A producer has no declared output site.
    NoOutputs(PathId),
    /// Two producers claim the same exact output site.
    OutputClaimed {
        /// Claimed output prim.
        output: PathId,
        /// Already registered recipe owning the site.
        owner: PathId,
    },
    /// A requested or prerequisite output has no registered producer.
    MissingProducer {
        /// Producer requesting the input, or `None` for the top-level request.
        consumer: Option<PathId>,
        /// Missing registered output site.
        output: PathId,
    },
    /// Cycle in declared dependencies; the first recipe is repeated at the end.
    Cycle(Vec<PathId>),
    /// A declared external resource has no supplied revision.
    MissingResource {
        /// Recipe declaring the resource.
        recipe: PathId,
        /// Application resource key.
        key: String,
    },
}
impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid procedural build: {self:?}")
    }
}
impl core::error::Error for GraphError {}

/// Host-owned preparation and acknowledgement of an authored output publication.
///
/// `prepare` must only construct a transaction; it must not publish output edits.
/// It can intern names and inspect the current authored site. Retained output
/// reuse still calls it, so missing/replaced authored outputs can be repaired.
/// Use target generation guards and update owned property manifests only after
/// `committed`. The declaration is routing evidence, not a restriction on which
/// sites the returned transaction can edit; the host enforces ownership.
pub trait ProducerPublisher<T> {
    /// Host validation/preparation failure.
    type Error;

    /// Constructs a guarded publication from the exact evaluated output.
    fn prepare(
        &mut self,
        producer: &ProducerSpec,
        output: &T,
        stage: &Stage,
        store: &mut dyn LayerStore,
    ) -> Result<Transaction, Self::Error>;

    /// Acknowledges a successful application, including an empty transaction.
    /// Called after publication; panics do not roll the authored state back.
    fn committed(&mut self, _producer: &ProducerSpec, _applied: &Applied) {}
}

/// Failure before or during one producer's build.
#[derive(Debug)]
pub enum BuildError<E, P> {
    /// Dependency/affinity preflight failed before any producer was run.
    Graph(GraphError),
    /// Composed input resolution or application evaluation failed.
    Evaluation {
        /// Failed producer recipe.
        recipe: PathId,
        /// Original evaluation error.
        error: ProceduralError<E>,
    },
    /// The host rejected generated data or could not prepare its publication.
    Preparation {
        /// Failed producer recipe.
        recipe: PathId,
        /// Original host error.
        error: P,
    },
    /// Input evidence or transaction guards rejected publication.
    Publication {
        /// Failed producer recipe.
        recipe: PathId,
        /// Original evidence/authoring error.
        error: BuildEvidenceError,
    },
}
impl<E: fmt::Display, P: fmt::Display> fmt::Display for BuildError<E, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Graph(error) => error.fmt(f),
            Self::Evaluation { recipe, error } => {
                write!(f, "producer {recipe:?} evaluation failed: {error}")
            }
            Self::Preparation { recipe, error } => write!(
                f,
                "producer {recipe:?} publication preparation failed: {error}"
            ),
            Self::Publication { recipe, error } => {
                write!(f, "producer {recipe:?} guarded publication failed: {error}")
            }
        }
    }
}
impl<E: core::error::Error, P: core::error::Error> core::error::Error for BuildError<E, P> {}

/// Why a detached build result cannot justify a later publication.
#[derive(Debug)]
pub enum BuildEvidenceError {
    /// A declared resource revision differs or is now absent.
    Resource {
        /// Resource identity.
        key: String,
        /// Revision used to evaluate the result.
        expected: u64,
        /// Current supplied revision, absent if the resource was removed.
        actual: Option<u64>,
    },
    /// Consumed USD inputs or evaluator epoch are no longer current.
    Input(EvidenceError),
    /// A prerequisite's inputs or evaluator epoch no longer justify downstream work.
    Prerequisite {
        /// Registered recipe whose evidence became stale.
        recipe: PathId,
        /// Original composed-input or invalidation failure.
        error: EvidenceError,
    },
    /// Transaction/evidence application failed after resource verification.
    Publication(EvidenceApplyError),
}
impl fmt::Display for BuildEvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stale procedural build result: {self:?}")
    }
}
impl core::error::Error for BuildEvidenceError {}

// Retain a shared dependency DAG rather than copying every ancestor's input
// evidence into each downstream step. Traversal is iterative for long chains.
struct Guard {
    store: StoreIdentity,
    recipe: PathId,
    evidence: EvaluationEvidence,
    prerequisites: Vec<Arc<Self>>,
}
impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guard")
            .field("recipe", &self.recipe)
            .field("prerequisites", &self.prerequisites.len())
            .finish_non_exhaustive()
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        // The final descendant can be the last owner of a long prerequisite
        // chain. Release exclusive ancestors iteratively, just like verification.
        let mut pending = core::mem::take(&mut self.prerequisites);
        while let Some(guard) = pending.pop() {
            if let Ok(mut guard) = Arc::try_unwrap(guard) {
                pending.append(&mut guard.prerequisites);
            }
        }
    }
}
impl Guard {
    fn verify(&self, scene: &Scene<'_>) -> Result<(), BuildEvidenceError> {
        self.evidence
            .verify(scene)
            .map_err(BuildEvidenceError::Input)?;
        let mut seen = BTreeSet::from([self.recipe]);
        let mut pending: Vec<_> = self.prerequisites.iter().map(Arc::as_ref).collect();
        while let Some(guard) = pending.pop() {
            if !seen.insert(guard.recipe) {
                continue;
            }
            guard
                .evidence
                .verify(scene)
                .map_err(|error| BuildEvidenceError::Prerequisite {
                    recipe: guard.recipe,
                    error,
                })?;
            pending.extend(guard.prerequisites.iter().map(Arc::as_ref));
        }
        Ok(())
    }
}

/// One successfully published producer, retaining exact output and input evidence.
#[derive(Debug)]
pub struct BuildStep<T> {
    /// Producer recipe.
    pub recipe: PathId,
    /// Whether the application evaluator ran rather than reusing its result.
    pub evaluated: bool,
    /// Changes from this publication; empty when the authored site was already current.
    pub changes: Changes,
    result: Evaluated<T>,
    resources: Vec<ResourceVersion>,
    guard: Arc<Guard>,
}
impl<T> BuildStep<T> {
    /// Exact evaluated output, independent of current authored output state.
    pub fn output(&self) -> &T {
        self.result.output()
    }
    /// Shared immutable output owner; cloning does not copy the generated payload.
    pub fn shared_output(&self) -> Arc<T> {
        self.result.shared_output()
    }
    /// External resource revisions used by this producer and its prerequisites.
    pub fn resources(&self) -> &[ResourceVersion] {
        &self.resources
    }
    fn check_resources(&self, current: &ResourceRevisions) -> Result<(), BuildEvidenceError> {
        for resource in &self.resources {
            let actual = current.get(&resource.key).copied();
            if actual != Some(resource.revision) {
                return Err(BuildEvidenceError::Resource {
                    key: resource.key.clone(),
                    expected: resource.revision,
                    actual,
                });
            }
        }
        Ok(())
    }
    /// Checks this producer's and its prerequisites' captured USD inputs,
    /// evaluator epochs and declared resources against a current scene.
    /// This does not check whether the already published output still exists.
    pub fn verify(
        &self,
        scene: &Scene<'_>,
        resources: &ResourceRevisions,
    ) -> Result<(), BuildEvidenceError> {
        self.check_resources(resources)?;
        self.guard.verify(scene)
    }
    /// Publishes delayed work only while resource and composed-input evidence agree.
    /// The host must keep resource contents stable for these revision tokens.
    /// Synchronizes sources and checks the complete prerequisite evidence before
    /// applying the transaction under the same mutable stage/store access.
    pub fn apply(
        &self,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
        resources: &ResourceRevisions,
        transaction: &Transaction,
    ) -> Result<Applied, BuildEvidenceError> {
        self.check_resources(resources)?;
        if self.guard.store != store.identity()
            || live.stage().store_identity() != Some(&self.guard.store)
        {
            return Err(BuildEvidenceError::Input(EvidenceError::DifferentStore));
        }
        live.synchronize(store);
        self.guard.verify(&Scene::new(live.stage(), store))?;
        live.apply(store, transaction)
            .map_err(|error| BuildEvidenceError::Publication(EvidenceApplyError::Edit(error)))
    }
}

/// Completed producer publications in deterministic dependency order.
#[derive(Debug)]
pub struct BuildResult<T> {
    /// Each producer runs at most once per request, including shared prerequisites.
    pub steps: Vec<BuildStep<T>>,
}
impl<T> Default for BuildResult<T> {
    fn default() -> Self {
        Self { steps: Vec::new() }
    }
}

/// A failed request and every earlier successful publication from that request.
#[derive(Debug)]
pub struct BuildFailure<T, E, P> {
    /// Concrete failure; boxed to keep the result's error representation small.
    pub error: Box<BuildError<E, P>>,
    /// Already applied prerequisite publications. They are not rolled back.
    pub completed: BuildResult<T>,
}
impl<T, E: fmt::Display, P: fmt::Display> fmt::Display for BuildFailure<T, E, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}; {} prerequisite publications completed",
            self.error,
            self.completed.steps.len()
        )
    }
}
impl<T: fmt::Debug, E: core::error::Error + 'static, P: core::error::Error + 'static>
    core::error::Error for BuildFailure<T, E, P>
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

/// A completed build or a failure retaining earlier successful publications.
pub type BuildOutcome<T, E, P> = Result<BuildResult<T>, BuildFailure<T, E, P>>;

/// Cumulative registry work; elapsed time, payload bytes and scheduling belong to the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphWork {
    /// Top-level build requests, including preflight failures.
    pub requests: u64,
    /// Producers in successful dependency plans, before attempted evaluation.
    pub planned: u64,
    /// Successful application evaluations during build requests.
    pub evaluated: u64,
    /// Successful reuse of a producer's tracked result.
    pub reused: u64,
    /// Successful publication transactions, including empty transactions.
    pub publications: u64,
    /// Producer invalidations caused by changed external resource revisions.
    pub resource_invalidations: u64,
}

struct Node<E: ProceduralEvaluator> {
    spec: ProducerSpec,
    binding: Procedural<E>,
    resources: Option<Vec<ResourceVersion>>,
}

/// Explicit, store-bound producer registry above single-recipe evaluation.
///
/// `E` may be a host enum covering several procedural systems. Dependencies name
/// output sites explicitly; this adapter does not infer a producer from an
/// arbitrary missing USD input. Registration and build order are deterministic.
/// Each build revalidates tracked inputs and prepares authored output even on a
/// cache hit. This separates computation reuse from publication-site freshness.
pub struct ProceduralGraph<E: ProceduralEvaluator> {
    store: StoreIdentity,
    nodes: BTreeMap<PathId, Node<E>>,
    owners: BTreeMap<PathId, PathId>,
    work: GraphWork,
}
impl<E: ProceduralEvaluator> fmt::Debug for ProceduralGraph<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProceduralGraph")
            .field("store", &self.store)
            .field("producers", &self.nodes.len())
            .field("outputs", &self.owners)
            .field("work", &self.work)
            .finish_non_exhaustive()
    }
}
impl<E: ProceduralEvaluator> ProceduralGraph<E> {
    /// Creates an empty registry bound to one token/path domain.
    pub fn new(store: &dyn LayerStore) -> Self {
        Self {
            store: store.identity(),
            nodes: BTreeMap::new(),
            owners: BTreeMap::new(),
            work: GraphWork::default(),
        }
    }
    /// Registers a producer atomically, rejecting duplicate recipes/output claims.
    /// Dependency existence and cycles are checked for the requested closure at build time.
    pub fn register(
        &mut self,
        store: &dyn LayerStore,
        mut spec: ProducerSpec,
        evaluator: E,
    ) -> Result<(), GraphError> {
        if self.store != store.identity() {
            return Err(GraphError::DifferentStore);
        }
        if self.nodes.contains_key(&spec.recipe) {
            return Err(GraphError::AlreadyRegistered(spec.recipe));
        }
        spec.outputs.sort_unstable();
        spec.outputs.dedup();
        spec.inputs.sort_unstable();
        spec.inputs.dedup();
        spec.resources.sort();
        spec.resources.dedup();
        if spec.outputs.is_empty() {
            return Err(GraphError::NoOutputs(spec.recipe));
        }
        for &output in &spec.outputs {
            if let Some(&owner) = self.owners.get(&output) {
                return Err(GraphError::OutputClaimed { output, owner });
            }
        }
        for &output in &spec.outputs {
            self.owners.insert(output, spec.recipe);
        }
        let binding = Procedural::new(store, spec.recipe, evaluator);
        self.nodes.insert(
            spec.recipe,
            Node {
                spec,
                binding,
                resources: None,
            },
        );
        Ok(())
    }
    /// Retires a producer and its detached evidence. Dependents remain registered;
    /// requesting them reports a missing producer until the output is registered again.
    pub fn remove(&mut self, recipe: PathId) -> bool {
        let Some(mut node) = self.nodes.remove(&recipe) else {
            return false;
        };
        node.binding.invalidate();
        for output in node.spec.outputs {
            self.owners.remove(&output);
        }
        true
    }
    /// Registered producer declarations in store-local recipe order.
    pub fn producers(&self) -> impl Iterator<Item = &ProducerSpec> {
        self.nodes.values().map(|n| &n.spec)
    }
    /// Exact declared owner of an output prim.
    pub fn owner(&self, output: PathId) -> Option<PathId> {
        self.owners.get(&output).copied()
    }
    /// Invalidates one producer after untracked generator/configuration changes.
    pub fn invalidate(&mut self, recipe: PathId) -> bool {
        let Some(node) = self.nodes.get_mut(&recipe) else {
            return false;
        };
        node.binding.invalidate();
        true
    }
    /// Cumulative build work; successful cached outputs can still require authoring repair.
    pub fn work(&self) -> GraphWork {
        self.work
    }

    /// Inspects dependency order without evaluation, publication or source synchronization.
    pub fn plan(&self, output: PathId) -> Result<Vec<PathId>, GraphError> {
        let recipe = self
            .owners
            .get(&output)
            .copied()
            .ok_or(GraphError::MissingProducer {
                consumer: None,
                output,
            })?;
        let mut marks = BTreeMap::new();
        let mut result = Vec::new();
        marks.insert(recipe, false);
        let mut pending = Vec::from([(recipe, 0_usize)]);
        while let Some(&(recipe, index)) = pending.last() {
            if let Some(&input) = self.nodes[&recipe].spec.inputs.get(index) {
                pending.last_mut().expect("active frame").1 += 1;
                let dependency =
                    self.owners
                        .get(&input)
                        .copied()
                        .ok_or(GraphError::MissingProducer {
                            consumer: Some(recipe),
                            output: input,
                        })?;
                match marks.get(&dependency) {
                    Some(true) => {}
                    Some(false) => {
                        let start = pending
                            .iter()
                            .position(|(p, _)| *p == dependency)
                            .expect("active dependency");
                        let mut cycle: Vec<_> = pending[start..].iter().map(|(p, _)| *p).collect();
                        cycle.push(dependency);
                        return Err(GraphError::Cycle(cycle));
                    }
                    None => {
                        marks.insert(dependency, false);
                        pending.push((dependency, 0));
                    }
                }
            } else {
                pending.pop();
                marks.insert(recipe, true);
                result.push(recipe);
            }
        }
        Ok(result)
    }
    /// Builds a requested output's dependency closure, publishing prerequisites first.
    ///
    /// All resource keys and graph edges are checked before any producer runs.
    /// Each successful publication synchronizes the scene before downstream reads.
    /// External resources must match their declared revision snapshots throughout
    /// the request. A later resource replacement is rejected by detached step evidence.
    pub fn request<P: ProducerPublisher<E::Output>>(
        &mut self,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
        output: PathId,
        time: Time,
        resources: &ResourceRevisions,
        publisher: &mut P,
    ) -> BuildOutcome<E::Output, E::Error, P::Error> {
        self.work.requests = self.work.requests.saturating_add(1);
        let mut completed = BuildResult::default();
        macro_rules! fail {
            ($error:expr) => {{
                return Err(BuildFailure {
                    error: Box::new($error),
                    completed,
                });
            }};
        }
        if self.store != store.identity() || live.stage().store_identity() != Some(&self.store) {
            fail!(BuildError::Graph(GraphError::DifferentStore));
        }
        let plan = match self.plan(output) {
            Ok(plan) => plan,
            Err(error) => fail!(BuildError::Graph(error)),
        };
        let mut versions = BTreeMap::new();
        let mut inherited_versions = BTreeMap::<PathId, Vec<ResourceVersion>>::new();
        for &recipe in &plan {
            let mut captured = Vec::new();
            for key in &self.nodes[&recipe].spec.resources {
                let Some(&revision) = resources.get(key) else {
                    fail!(BuildError::Graph(GraphError::MissingResource {
                        recipe,
                        key: key.clone()
                    }));
                };
                captured.push(ResourceVersion {
                    key: key.clone(),
                    revision,
                });
            }
            let mut inherited: ResourceRevisions = captured
                .iter()
                .map(|v| (v.key.clone(), v.revision))
                .collect();
            for input in &self.nodes[&recipe].spec.inputs {
                let owner = self.owners[input];
                for version in &inherited_versions[&owner] {
                    inherited.insert(version.key.clone(), version.revision);
                }
            }
            inherited_versions.insert(
                recipe,
                inherited
                    .into_iter()
                    .map(|(key, revision)| ResourceVersion { key, revision })
                    .collect(),
            );
            versions.insert(recipe, captured);
        }
        self.work.planned = self.work.planned.saturating_add(plan.len() as u64);
        live.synchronize(store);
        let mut guards = BTreeMap::<PathId, Arc<Guard>>::new();
        for recipe in plan {
            let node = self
                .nodes
                .get_mut(&recipe)
                .expect("planned registered producer");
            let captured = versions.remove(&recipe).expect("preflight resources");
            if node.resources.as_ref().is_some_and(|old| old != &captured) {
                node.binding.invalidate();
                self.work.resource_invalidations =
                    self.work.resource_invalidations.saturating_add(1);
            }
            let before = node.binding.work().evaluations;
            let result = match node
                .binding
                .snapshot(&Scene::new(live.stage(), store), time)
            {
                Ok(result) => result,
                Err(error) => fail!(BuildError::Evaluation { recipe, error }),
            };
            node.resources = Some(captured.clone());
            let evaluated = node.binding.work().evaluations != before;
            if evaluated {
                self.work.evaluated = self.work.evaluated.saturating_add(1);
            } else {
                self.work.reused = self.work.reused.saturating_add(1);
            }
            let transaction =
                match publisher.prepare(&node.spec, result.output(), live.stage(), store) {
                    Ok(transaction) => transaction,
                    Err(error) => fail!(BuildError::Preparation { recipe, error }),
                };
            let guard = Arc::new(Guard {
                store: self.store.clone(),
                recipe,
                evidence: result.evidence().clone(),
                prerequisites: node
                    .spec
                    .inputs
                    .iter()
                    .map(|input| guards[&self.owners[input]].clone())
                    .collect(),
            });
            let mut step = BuildStep {
                recipe,
                evaluated,
                changes: Changes::default(),
                result,
                resources: inherited_versions
                    .remove(&recipe)
                    .expect("preflight inherited resources"),
                guard: guard.clone(),
            };
            let applied = match step.apply(live, store, resources, &transaction) {
                Ok(applied) => applied,
                Err(error) => fail!(BuildError::Publication { recipe, error }),
            };
            self.work.publications = self.work.publications.saturating_add(1);
            publisher.committed(&node.spec, &applied);
            step.changes = applied.changes;
            guards.insert(recipe, guard);
            completed.steps.push(step);
        }
        Ok(completed)
    }
}

#[cfg(test)]
mod tests;
