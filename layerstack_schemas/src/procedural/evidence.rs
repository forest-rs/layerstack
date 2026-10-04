// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Detached evaluation evidence and guarded application of delayed work.

use super::*;
use layerstack::{Applied, EditError, LiveStage, Transaction};

/// Why an evaluated result cannot be published against the supplied scene.
#[derive(Clone, Debug)]
pub enum EvidenceError {
    /// Token/path domains do not match the binding's original store.
    DifferentStore,
    /// Explicit generator/resource invalidation retired this evaluation epoch.
    Invalidated,
    /// The recipe is absent or no longer a `GenerativeProcedural`.
    NotProcedural(PathId),
    /// A consumed input has a different composed value or terminal targets.
    Changed {
        /// Prim containing the changed input.
        prim: PathId,
        /// Consumed USD property name.
        property: String,
    },
    /// An input no longer has its expected kind or cannot be decoded.
    Input(ProceduralInputError),
}
impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DifferentStore => f.write_str("evaluation belongs to different store interners"),
            Self::Invalidated => f.write_str("evaluation was explicitly invalidated"),
            Self::NotProcedural(path) => write!(f, "recipe is no longer procedural at {path:?}"),
            Self::Changed { prim, property } => {
                write!(f, "consumed input changed at {prim:?}.{property}")
            }
            Self::Input(error) => error.fmt(f),
        }
    }
}
impl core::error::Error for EvidenceError {}

/// Failure before or during guarded publication of delayed work.
#[derive(Debug)]
pub enum EvidenceApplyError {
    /// The current inputs do not justify this result; no transaction was applied.
    Evidence(EvidenceError),
    /// Authoring failed, including a stale target-layer generation guard.
    Edit(EditError),
}
impl fmt::Display for EvidenceApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evidence(error) => error.fmt(f),
            Self::Edit(error) => error.fmt(f),
        }
    }
}
impl core::error::Error for EvidenceApplyError {}

/// Consumed composed inputs of one evaluation at an explicit time.
///
/// Unchanged identities avoid resolution. Changed records are checked for equal
/// values/targets; different numeric owners can require element comparison.
/// Evidence retains query records and values, so it can pin other opinions on
/// consumed prims too. Applications own how many pending results they retain.
/// External resource contents are outside USD evidence: call the binding's
/// `invalidate` before publication when resources/configuration change.
#[derive(Clone, Debug)]
pub struct EvaluationEvidence {
    store: StoreIdentity,
    recipe: PathId,
    time: Time,
    recipe_snapshot: PrimSnapshot,
    inputs: Vec<Input>,
    epoch: Arc<AtomicBool>,
}
impl EvaluationEvidence {
    pub(super) fn capture(
        store: StoreIdentity,
        recipe: PathId,
        time: Time,
        recipe_snapshot: PrimSnapshot,
        inputs: Vec<Input>,
        epoch: Arc<AtomicBool>,
    ) -> Self {
        Self {
            store,
            recipe,
            time,
            recipe_snapshot,
            inputs,
            epoch,
        }
    }
    /// Evaluation time; this does not author a time sample or choose a current frame.
    pub fn time(&self) -> Time {
        self.time
    }
    /// Properties consumed by this result, including missing inputs and system.
    pub fn dependencies(&self) -> impl Iterator<Item = ProceduralDependency<'_>> {
        self.inputs.iter().map(|i| ProceduralDependency {
            prim: i.prim,
            property: &i.property,
            kind: i.value.kind(),
        })
    }
    /// Checks against an already synchronized scene. This is evidence for the
    /// supplied snapshot only; later edits require another check. Prefer `apply`
    /// for delayed publication. AOUSD Core §12.3–§12.5 (composed inputs).
    pub fn verify(&self, scene: &Scene<'_>) -> Result<(), EvidenceError> {
        let identity = scene.store().identity();
        if self.store != identity || scene.stage().store_identity() != Some(&identity) {
            return Err(EvidenceError::DifferentStore);
        }
        if !self.epoch.load(Ordering::Acquire) {
            return Err(EvidenceError::Invalidated);
        }
        if !self.recipe_snapshot.is_current(scene.stage())
            && GenerativeProcedural::new(scene, self.recipe).is_none()
        {
            return Err(EvidenceError::NotProcedural(self.recipe));
        }
        for input in &self.inputs {
            if input.is_current(scene, self.time) {
                continue;
            }
            let mut current = input.clone();
            current
                .refresh(scene, self.time)
                .map_err(EvidenceError::Input)?;
            if !input.value.matches(&current.value) {
                return Err(EvidenceError::Changed {
                    prim: input.prim,
                    property: input.property.clone(),
                });
            }
        }
        Ok(())
    }
    /// Synchronizes source edits, verifies consumed inputs, then applies one
    /// transaction without releasing the mutable stage/store between steps.
    /// Stale work does not author anything; synchronization can still notify
    /// observers. Target-layer guards remain independent and are checked by apply.
    /// The application owns generator/resource concurrency and output lifecycle.
    pub fn apply(
        &self,
        live: &mut LiveStage,
        store: &mut dyn LayerStore,
        transaction: &Transaction,
    ) -> Result<Applied, EvidenceApplyError> {
        let identity = store.identity();
        if self.store != identity || live.stage().store_identity() != Some(&identity) {
            return Err(EvidenceApplyError::Evidence(EvidenceError::DifferentStore));
        }
        live.synchronize(store);
        self.verify(&Scene::new(live.stage(), store))
            .map_err(EvidenceApplyError::Evidence)?;
        live.apply(store, transaction)
            .map_err(EvidenceApplyError::Edit)
    }
}

/// Shared generator output and its independently retained input evidence.
/// Output copying, publication targets and pending-work lifetimes remain with
/// the application. Cloning this handle does not clone the generated payload.
#[derive(Debug)]
pub struct Evaluated<T> {
    output: Arc<T>,
    evidence: EvaluationEvidence,
}
impl<T> Clone for Evaluated<T> {
    fn clone(&self) -> Self {
        Self {
            output: self.output.clone(),
            evidence: self.evidence.clone(),
        }
    }
}
impl<T> Evaluated<T> {
    pub(super) fn new(output: Arc<T>, evidence: EvaluationEvidence) -> Self {
        Self { output, evidence }
    }
    /// Borrows the result produced by the application evaluator.
    pub fn output(&self) -> &T {
        &self.output
    }
    /// Retains the generated payload without copying it.
    pub fn shared_output(&self) -> Arc<T> {
        self.output.clone()
    }
    /// Evidence to verify or guard application of a publication transaction.
    pub fn evidence(&self) -> &EvaluationEvidence {
        &self.evidence
    }
}
