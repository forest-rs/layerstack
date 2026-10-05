// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Caller-driven evaluation of `UsdProc` recipes with retained input dependencies.
//!
//! This module owns composed input reads and result reuse. The application owns
//! its evaluator, invocation order, publication targets and output lifecycle.
//! There is no plugin loader, scheduler or implicit stage mutation. Outputs are
//! generic: meshes, model descriptions and texture-generation requests can use
//! the same input contract without teaching composition how to execute them.
//!
//! OpenUSD `UsdProcGenerativeProcedural` defines `proceduralSystem` and inputs in
//! the `primvars:` namespace, but does not execute them. This is a `LayerStack`
//! application adapter, **not** an implementation of Hydra's `HdGp` runtime.
//! Inputs use AOUSD Core §12.3 (value resolution), §12.4 (relationships), §12.5
//! (interpolation), and §13.3.2.4 (schema fallbacks).

use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
};
use layerstack::{
    ArrayReadError, AttributeQuery, LayerStore, PathId, PrimSnapshot, PropertyKind, PropertyPath,
    RelationshipQuery, StoreIdentity, TargetPath, Time, TokenInterner, Value,
};

mod evidence;
pub use evidence::{Evaluated, EvaluationEvidence, EvidenceApplyError, EvidenceError};

/// Explicit producer dependencies, resource revisions and guarded publication.
pub mod graph;

use crate::{Scene, usd_proc::GenerativeProcedural};

/// Application-supplied evaluator for one procedural system.
///
/// Results must be determined by reads through [`ProceduralInputs`] and this
/// evaluator's configuration. Call [`Procedural::invalidate`] when external
/// resources, generator code or untracked state change. Configuration changes
/// through [`Procedural::evaluator_mut`] invalidate automatically. Evaluation
/// must not publish side effects: returning an output and publishing it are
/// separate steps, so failed validation or publication can be retried safely.
pub trait ProceduralEvaluator {
    /// Retained generated output; publication remains the caller's responsibility.
    type Output;
    /// Generator-specific failure, including invalid or unsupported parameters.
    type Error;

    /// Exact `proceduralSystem` token this evaluator understands.
    fn system(&self) -> &str;

    /// Evaluates a recipe using tracked, composed inputs.
    fn evaluate(&self, inputs: &mut ProceduralInputs<'_>) -> Result<Self::Output, Self::Error>;
}

/// Failure to read one procedural input from the composed stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProceduralInputError {
    /// The named property exists with a different kind.
    WrongKind {
        /// Prim containing the input.
        prim: PathId,
        /// USD property name.
        property: String,
        /// Kind the evaluator requested.
        expected: PropertyKind,
        /// Composed kind of the existing property.
        actual: PropertyKind,
    },
    /// A selected numeric array could not be decoded.
    Array {
        /// Prim containing the input.
        prim: PathId,
        /// USD property name.
        property: String,
        /// Original storage/decoding failure.
        error: ArrayReadError,
    },
}
impl fmt::Display for ProceduralInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongKind {
                property,
                expected,
                actual,
                ..
            } => write!(
                f,
                "procedural input {property} is {actual:?}, expected {expected:?}"
            ),
            Self::Array {
                property, error, ..
            } => write!(f, "cannot read procedural array {property}: {error}"),
        }
    }
}
impl core::error::Error for ProceduralInputError {}

/// Failure to evaluate a procedural recipe; no output is published implicitly.
#[derive(Debug)]
pub enum ProceduralError<E> {
    /// The supplied store or stage has different token/path domain affinity.
    DifferentStore,
    /// The recipe is absent or is not a `GenerativeProcedural`.
    NotProcedural(PathId),
    /// The recipe selects a system this evaluator does not understand.
    System {
        /// Evaluator's supported system.
        expected: String,
        /// Composed token, or `None` for a missing/incompatible value.
        actual: Option<String>,
    },
    /// Dependency revalidation or system input could not be read.
    Input(ProceduralInputError),
    /// Application evaluator failed; its previous result is not returned.
    Evaluation(E),
}
impl<E: fmt::Display> fmt::Display for ProceduralError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DifferentStore => {
                f.write_str("procedural binding belongs to different store interners")
            }
            Self::NotProcedural(path) => write!(f, "no procedural recipe at {path:?}"),
            Self::System { expected, actual } => {
                write!(f, "procedural system {actual:?}, expected {expected}")
            }
            Self::Input(error) => error.fmt(f),
            Self::Evaluation(error) => write!(f, "procedural evaluation failed: {error}"),
        }
    }
}
impl<E: core::error::Error + 'static> core::error::Error for ProceduralError<E> {}

/// One composed property read by the last successful evaluation.
#[derive(Clone, Copy, Debug)]
pub struct ProceduralDependency<'a> {
    /// Concrete stage prim; identities remain local to one store.
    pub prim: PathId,
    /// USD property name, including `primvars:` for recipe parameters.
    pub property: &'a str,
    /// Requested kind. Missing properties are tracked too.
    pub kind: PropertyKind,
}

#[derive(Clone, Debug)]
enum InputValue {
    Attribute(Option<Value>),
    Targets(Option<Vec<TargetPath>>),
}
impl InputValue {
    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Attribute(Some(a)), Self::Attribute(Some(b))) => a.same_representation(b),
            (Self::Attribute(None), Self::Attribute(None)) => true,
            (Self::Targets(a), Self::Targets(b)) => a == b,
            _ => false,
        }
    }
    fn kind(&self) -> PropertyKind {
        match self {
            Self::Attribute(_) => PropertyKind::Attribute,
            Self::Targets(_) => PropertyKind::Relationship,
        }
    }
}

#[derive(Clone, Debug)]
struct Input {
    prim: PathId,
    property: String,
    value: InputValue,
    query: InputQuery,
}
#[derive(Clone, Debug)]
enum InputQuery {
    Attribute(AttributeQuery),
    Relationship(RelationshipQuery),
    Missing(PrimSnapshot),
}
impl Input {
    fn query(scene: &Scene<'_>, prim: PathId, property: &str, kind: PropertyKind) -> InputQuery {
        match scene.store().tokens().lookup(property) {
            Some(token) => {
                let path = PropertyPath::new(prim, token);
                match kind {
                    PropertyKind::Attribute => InputQuery::Attribute(AttributeQuery::new(path)),
                    PropertyKind::Relationship => {
                        InputQuery::Relationship(RelationshipQuery::new(path))
                    }
                }
            }
            None => InputQuery::Missing(scene.stage().prim_snapshot(prim)),
        }
    }
    fn new(
        scene: &Scene<'_>,
        time: Time,
        prim: PathId,
        property: &str,
        kind: PropertyKind,
    ) -> Result<Self, ProceduralInputError> {
        let mut input = Self {
            prim,
            property: property.into(),
            value: match kind {
                PropertyKind::Attribute => InputValue::Attribute(None),
                PropertyKind::Relationship => InputValue::Targets(None),
            },
            query: Self::query(scene, prim, property, kind),
        };
        input.refresh(scene, time)?;
        Ok(input)
    }
    fn is_current(&self, scene: &Scene<'_>, time: Time) -> bool {
        match &self.query {
            InputQuery::Attribute(query) => query.is_current(scene.stage(), time),
            InputQuery::Relationship(query) => query.is_current(scene.stage()),
            InputQuery::Missing(snapshot) => {
                snapshot.is_current(scene.stage())
                    && scene.store().tokens().lookup(&self.property).is_none()
            }
        }
    }
    fn refresh(&mut self, scene: &Scene<'_>, time: Time) -> Result<(), ProceduralInputError> {
        let kind = self.value.kind();
        if let Some(token) = scene.store().tokens().lookup(&self.property)
            && let Some(actual) = scene
                .stage()
                .property_kind(PropertyPath::new(self.prim, token))
            && actual != kind
        {
            return Err(ProceduralInputError::WrongKind {
                prim: self.prim,
                property: self.property.clone(),
                expected: kind,
                actual,
            });
        }
        if matches!(self.query, InputQuery::Missing(_)) {
            self.query = Self::query(scene, self.prim, &self.property, kind);
        }
        self.value = match &mut self.query {
            InputQuery::Attribute(query) => InputValue::Attribute(
                query
                    .try_get(scene.stage(), time)
                    .map_err(|error| ProceduralInputError::Array {
                        prim: self.prim,
                        property: self.property.clone(),
                        error,
                    })?
                    .map(|r| r.value),
            ),
            InputQuery::Relationship(query) => InputValue::Targets(query.get(scene.stage())),
            InputQuery::Missing(_) => match kind {
                PropertyKind::Attribute => InputValue::Attribute(None),
                PropertyKind::Relationship => InputValue::Targets(None),
            },
        };
        Ok(())
    }
}

/// Read-only evaluation context that records every property read, including absence.
///
/// Attribute reads retain native numeric owners. Selected deferred storage may
/// decode, and interpolation may materialize a new buffer; failures are explicit.
/// A failed read prevents accepting a successful evaluator result even when the
/// evaluator ignores that error. Missing values are ordinary tracked inputs.
/// Use `crate::value::borrow_*_array` or `read_*_array_shared` to consume arrays
/// without copying. Relationships resolve composed terminal targets; evaluating
/// their target attributes requires separate tracked [`Self::attribute`] reads.
#[derive(Debug)]
pub struct ProceduralInputs<'a> {
    scene: Scene<'a>,
    recipe: PathId,
    time: Time,
    inputs: Vec<Input>,
    reads: u64,
    failure: Option<ProceduralInputError>,
}
impl ProceduralInputs<'_> {
    /// Concrete recipe prim in the supplied stage.
    pub fn recipe(&self) -> PathId {
        self.recipe
    }
    /// Requested stage time. Changing it always invalidates a retained result.
    pub fn time(&self) -> Time {
        self.time
    }
    /// Store-local interner for interpreting token and asset values.
    pub fn tokens(&self) -> &TokenInterner {
        self.scene.store().tokens()
    }
    /// Reads the recipe attribute `primvars:{name}`.
    pub fn parameter(&mut self, name: &str) -> Result<Option<Value>, ProceduralInputError> {
        self.attribute(self.recipe, &format!("primvars:{name}"))
    }
    /// Reads the recipe relationship `primvars:{name}`, including forwarding.
    pub fn parameter_targets(
        &mut self,
        name: &str,
    ) -> Result<Option<Vec<TargetPath>>, ProceduralInputError> {
        self.relationship_targets(self.recipe, &format!("primvars:{name}"))
    }
    /// Reads an attribute on any input prim, including schema fallback and samples.
    pub fn attribute(
        &mut self,
        prim: PathId,
        property: &str,
    ) -> Result<Option<Value>, ProceduralInputError> {
        let InputValue::Attribute(value) = self.record(prim, property, PropertyKind::Attribute)?
        else {
            unreachable!()
        };
        Ok(value)
    }
    /// Reads terminal relationship targets on any input prim. `None` is absence;
    /// `Some([])` is an existing relationship with no terminal targets.
    pub fn relationship_targets(
        &mut self,
        prim: PathId,
        property: &str,
    ) -> Result<Option<Vec<TargetPath>>, ProceduralInputError> {
        let InputValue::Targets(value) = self.record(prim, property, PropertyKind::Relationship)?
        else {
            unreachable!()
        };
        Ok(value)
    }
    fn record(
        &mut self,
        prim: PathId,
        property: &str,
        kind: PropertyKind,
    ) -> Result<InputValue, ProceduralInputError> {
        self.reads = self.reads.saturating_add(1);
        let input = match Input::new(&self.scene, self.time, prim, property, kind) {
            Ok(input) => input,
            Err(error) => {
                self.failure = Some(error.clone());
                return Err(error);
            }
        };
        let value = input.value.clone();
        if !self
            .inputs
            .iter()
            .any(|i| i.prim == prim && i.property == property && i.value.kind() == kind)
        {
            self.inputs.push(input);
        }
        Ok(value)
    }
}

/// Cumulative work of one caller-owned procedural, including failed attempts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProceduralWork {
    /// Calls into the application evaluator.
    pub evaluations: u64,
    /// Successful reuse without running the evaluator.
    pub cache_hits: u64,
    /// Composed input reads during evaluation and dependency revalidation.
    pub input_reads: u64,
    /// Input reads skipped because retained query identities are still current.
    pub query_cache_hits: u64,
}

/// Caller-owned evaluator and one retained result for a concrete `UsdProc` recipe.
///
/// Each call checks captured schema/input query identities; only changed inputs
/// need composed resolution. Equal values reuse the output after unrelated edits;
/// there is no requirement to deliver every change notice. Dynamic dependencies
/// are replaced on successful evaluation. Native owner equality avoids element
/// comparisons; different owners can require O(elements) comparison. Storage is
/// bounded to one output plus the values/targets read by that evaluation.
///
/// Construction captures store affinity; another store is rejected before path
/// lookup. Create a new binding for another store. Invalidate after
/// external resource changes. A result at numeric time is an evaluation snapshot, not an authored
/// time sample: the publisher must explicitly choose defaults or time samples.
/// If a recipe disappears or evaluation fails, no previous output is returned;
/// the caller chooses whether to retain or remove previously published content.
pub struct Procedural<E: ProceduralEvaluator> {
    store: StoreIdentity,
    recipe: PathId,
    evaluator: E,
    output: Option<Arc<E::Output>>,
    time: Option<Time>,
    inputs: Vec<Input>,
    work: ProceduralWork,
    recipe_snapshot: Option<PrimSnapshot>,
    epoch: Arc<AtomicBool>,
}
impl<E: ProceduralEvaluator + fmt::Debug> fmt::Debug for Procedural<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Procedural")
            .field("recipe", &self.recipe)
            .field("evaluator", &self.evaluator)
            .field("has_output", &self.output.is_some())
            .field("time", &self.time)
            .field("inputs", &self.inputs)
            .field("work", &self.work)
            .finish_non_exhaustive()
    }
}
impl<E: ProceduralEvaluator> Procedural<E> {
    /// Binds a caller-supplied evaluator to a store-local recipe path.
    pub fn new(store: &dyn LayerStore, recipe: PathId, evaluator: E) -> Self {
        Self {
            store: store.identity(),
            recipe,
            evaluator,
            output: None,
            time: None,
            inputs: Vec::new(),
            work: ProceduralWork::default(),
            recipe_snapshot: None,
            epoch: Arc::new(AtomicBool::new(true)),
        }
    }
    /// Recipe path; changing stores requires a new binding.
    pub fn recipe(&self) -> PathId {
        self.recipe
    }
    /// Drops result/dependency state and invalidates detached evidence, preserving
    /// store affinity and cumulative work counters. Use after external resources
    /// or generator code change; old delayed results must not be published.
    pub fn invalidate(&mut self) {
        self.epoch.store(false, Ordering::Release);
        self.epoch = Arc::new(AtomicBool::new(true));
        self.clear_result();
    }
    fn clear_result(&mut self) {
        self.output = None;
        self.time = None;
        self.inputs.clear();
        self.recipe_snapshot = None;
    }
    /// Mutates evaluator configuration after dropping its retained result.
    pub fn evaluator_mut(&mut self) -> &mut E {
        self.invalidate();
        &mut self.evaluator
    }
    /// Properties read by the last successful evaluation, including the system.
    pub fn dependencies(&self) -> impl Iterator<Item = ProceduralDependency<'_>> {
        self.inputs.iter().map(|i| ProceduralDependency {
            prim: i.prim,
            property: &i.property,
            kind: i.value.kind(),
        })
    }
    /// Work performed since construction; wall-clock timing belongs to the host.
    pub fn work(&self) -> ProceduralWork {
        self.work
    }
    /// Resolves inputs and evaluates only when its retained result cannot be reused.
    /// Nothing is authored; publish the returned output through an explicit target.
    pub fn evaluate(
        &mut self,
        scene: &Scene<'_>,
        time: Time,
    ) -> Result<&E::Output, ProceduralError<E::Error>> {
        let identity = scene.store().identity();
        if self.store != identity || scene.stage().store_identity() != Some(&identity) {
            return Err(ProceduralError::DifferentStore);
        }
        if self.output.is_some()
            && self.time == Some(time)
            && self
                .recipe_snapshot
                .as_ref()
                .is_some_and(|s| s.is_current(scene.stage()))
            && self
                .inputs
                .iter()
                .all(|input| input.is_current(scene, time))
        {
            self.work.cache_hits = self.work.cache_hits.saturating_add(1);
            self.work.query_cache_hits = self
                .work
                .query_cache_hits
                .saturating_add(u64::try_from(self.inputs.len()).unwrap_or(u64::MAX));
            return Ok(self.output.as_deref().expect("retained output"));
        }
        if GenerativeProcedural::new(scene, self.recipe).is_none() {
            self.clear_result();
            return Err(ProceduralError::NotProcedural(self.recipe));
        }
        let mut inputs = ProceduralInputs {
            scene: *scene,
            recipe: self.recipe,
            time,
            inputs: Vec::new(),
            reads: 0,
            failure: None,
        };
        let system = inputs.attribute(self.recipe, "proceduralSystem");
        self.work.input_reads = self.work.input_reads.saturating_add(inputs.reads);
        inputs.reads = 0;
        let actual = system
            .map_err(ProceduralError::Input)?
            .as_ref()
            .and_then(|v| crate::value::read_token(v, inputs.tokens()))
            .map(String::from);
        if actual.as_deref() != Some(self.evaluator.system()) {
            self.clear_result();
            return Err(ProceduralError::System {
                expected: self.evaluator.system().into(),
                actual,
            });
        }
        let mut unchanged = self.output.is_some() && self.time == Some(time);
        if unchanged {
            for input in &mut self.inputs {
                if input.is_current(scene, time) {
                    self.work.query_cache_hits = self.work.query_cache_hits.saturating_add(1);
                    continue;
                }
                self.work.input_reads = self.work.input_reads.saturating_add(1);
                let mut refreshed = input.clone();
                refreshed
                    .refresh(scene, time)
                    .map_err(ProceduralError::Input)?;
                if !input.value.matches(&refreshed.value) {
                    unchanged = false;
                    break;
                }
                *input = refreshed;
            }
        }
        if unchanged {
            self.work.cache_hits = self.work.cache_hits.saturating_add(1);
            self.recipe_snapshot = Some(scene.stage().prim_snapshot(self.recipe));
        } else {
            self.work.evaluations = self.work.evaluations.saturating_add(1);
            let output = self.evaluator.evaluate(&mut inputs);
            self.work.input_reads = self.work.input_reads.saturating_add(inputs.reads);
            match output {
                Ok(output) => {
                    if let Some(error) = inputs.failure {
                        self.clear_result();
                        return Err(ProceduralError::Input(error));
                    }
                    self.output = Some(Arc::new(output));
                    self.time = Some(time);
                    self.inputs = inputs.inputs;
                    self.recipe_snapshot = Some(scene.stage().prim_snapshot(self.recipe));
                }
                Err(error) => {
                    self.clear_result();
                    return Err(ProceduralError::Evaluation(error));
                }
            }
        }
        Ok(self
            .output
            .as_deref()
            .expect("successful evaluation or retained output"))
    }

    /// Evaluates and detaches a shared result with evidence of its consumed USD
    /// inputs. Use its evidence to apply delayed publication immediately after
    /// checking the current synchronized scene. No output copy is required.
    pub fn snapshot(
        &mut self,
        scene: &Scene<'_>,
        time: Time,
    ) -> Result<Evaluated<E::Output>, ProceduralError<E::Error>> {
        self.evaluate(scene, time)?;
        Ok(Evaluated::new(
            self.output.as_ref().expect("successful evaluation").clone(),
            EvaluationEvidence::capture(
                self.store.clone(),
                self.recipe,
                time,
                self.recipe_snapshot
                    .as_ref()
                    .expect("successful recipe snapshot")
                    .clone(),
                self.inputs.clone(),
                self.epoch.clone(),
            ),
        ))
    }
}

#[cfg(test)]
mod tests;
