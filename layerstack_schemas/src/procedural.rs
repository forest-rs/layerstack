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

use alloc::{format, string::String, vec::Vec};
use core::fmt;
use layerstack::{ArrayReadError, PathId, PropertyKind, TargetPath, Time, TokenInterner, Value};

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
}
fn read(
    scene: &Scene<'_>,
    time: Time,
    prim: PathId,
    property: &str,
    kind: PropertyKind,
) -> Result<InputValue, ProceduralInputError> {
    let view = scene.stage().prim(prim, scene.store());
    if let Some(token) = scene.store().tokens().lookup(property)
        && let Some(actual) = scene
            .stage()
            .property_kind(layerstack::PropertyPath::new(prim, token))
        && actual != kind
    {
        return Err(ProceduralInputError::WrongKind {
            prim,
            property: property.into(),
            expected: kind,
            actual,
        });
    }
    match kind {
        PropertyKind::Attribute => Ok(InputValue::Attribute(
            view.and_then(|p| p.attribute(property))
                .map(|a| a.try_get(time))
                .transpose()
                .map_err(|error| ProceduralInputError::Array {
                    prim,
                    property: property.into(),
                    error,
                })?
                .flatten()
                .map(|r| r.value),
        )),
        PropertyKind::Relationship => Ok(InputValue::Targets(
            view.and_then(|p| p.relationship(property))
                .map(|r| r.forwarded_targets()),
        )),
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
        let value = match read(&self.scene, self.time, prim, property, kind) {
            Ok(value) => value,
            Err(error) => {
                self.failure = Some(error.clone());
                return Err(error);
            }
        };
        if !self
            .inputs
            .iter()
            .any(|i| i.prim == prim && i.property == property && i.value.kind() == kind)
        {
            self.inputs.push(Input {
                prim,
                property: property.into(),
                value: value.clone(),
            });
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
}

/// Caller-owned evaluator and one retained result for a concrete `UsdProc` recipe.
///
/// Each call checks the recipe's schema/system and re-reads previously consumed
/// inputs. Equal composed values reuse the output even after unrelated edits;
/// there is no requirement to deliver every change notice. Dynamic dependencies
/// are replaced on successful evaluation. Native owner equality avoids element
/// comparisons; different owners can require O(elements) comparison. Storage is
/// bounded to one output plus the values/targets read by that evaluation.
///
/// Use within one store; create a new binding for another store. Invalidate after
/// external resource changes. A result at numeric time is an evaluation snapshot, not an authored
/// time sample: the publisher must explicitly choose defaults or time samples.
/// If a recipe disappears or evaluation fails, no previous output is returned;
/// the caller chooses whether to retain or remove previously published content.
pub struct Procedural<E: ProceduralEvaluator> {
    recipe: PathId,
    evaluator: E,
    output: Option<E::Output>,
    time: Option<Time>,
    inputs: Vec<Input>,
    work: ProceduralWork,
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
            .finish()
    }
}
impl<E: ProceduralEvaluator> Procedural<E> {
    /// Binds a caller-supplied evaluator to a store-local recipe path.
    pub fn new(recipe: PathId, evaluator: E) -> Self {
        Self {
            recipe,
            evaluator,
            output: None,
            time: None,
            inputs: Vec::new(),
            work: ProceduralWork::default(),
        }
    }
    /// Recipe path; changing stores requires a new binding.
    pub fn recipe(&self) -> PathId {
        self.recipe
    }
    /// Drops result/dependency state, preserving cumulative work counters.
    pub fn invalidate(&mut self) {
        self.output = None;
        self.time = None;
        self.inputs.clear();
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
        if GenerativeProcedural::new(scene, self.recipe).is_none() {
            self.invalidate();
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
            self.invalidate();
            return Err(ProceduralError::System {
                expected: self.evaluator.system().into(),
                actual,
            });
        }
        let mut unchanged = self.output.is_some() && self.time == Some(time);
        if unchanged {
            for input in &self.inputs {
                self.work.input_reads = self.work.input_reads.saturating_add(1);
                let value = read(scene, time, input.prim, &input.property, input.value.kind())
                    .map_err(ProceduralError::Input)?;
                if !input.value.matches(&value) {
                    unchanged = false;
                    break;
                }
            }
        }
        if unchanged {
            self.work.cache_hits = self.work.cache_hits.saturating_add(1);
        } else {
            self.work.evaluations = self.work.evaluations.saturating_add(1);
            let output = self.evaluator.evaluate(&mut inputs);
            self.work.input_reads = self.work.input_reads.saturating_add(inputs.reads);
            match output {
                Ok(output) => {
                    if let Some(error) = inputs.failure {
                        self.invalidate();
                        return Err(ProceduralError::Input(error));
                    }
                    self.output = Some(output);
                    self.time = Some(time);
                    self.inputs = inputs.inputs;
                }
                Err(error) => {
                    self.invalidate();
                    return Err(ProceduralError::Evaluation(error));
                }
            }
        }
        Ok(self
            .output
            .as_ref()
            .expect("successful evaluation or retained output"))
    }
}

#[cfg(test)]
mod tests;
