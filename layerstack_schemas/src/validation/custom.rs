// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use layerstack::Value;

/// Stable identity of a selected check. Built-ins retain their typed identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValidatorId {
    /// A compiled built-in rule.
    Builtin(BuiltinRule),
    /// A caller-registered namespaced identifier, such as `studio:prim-names`.
    Custom(Arc<str>),
}
/// Targets scheduled for a custom validator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidatorDomain {
    /// Once, only for stage scope.
    Stage,
    /// Once per selected prim, in canonical namespace order.
    Prim,
}
/// Frozen, inspectable declaration captured when a validator is registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatorMetadata {
    /// Stable `namespace:name` identifier. Both parts must be nonempty and the
    /// identifier must contain no whitespace. IDs are unique within a context.
    pub id: Arc<str>,
    /// Human-readable purpose; consumers identify rules by `id`.
    pub description: Arc<str>,
    /// Whether the scheduler supplies stage or prim targets.
    pub domain: ValidatorDomain,
    /// Declared temporal coverage. This does not cause repeated invocations:
    /// the callback receives the complete canonical requested-time slice once.
    pub time_domain: RuleTimeDomain,
}
/// One scheduler-selected target, independent of any validator's payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidatorTarget {
    /// The composed stage and root-layer metadata.
    Stage,
    /// One selected composed prim.
    Prim(PathId),
}
/// One custom violated invariant. The producing ID is attached by the context.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomValidationProblem {
    /// Exact composed location and authored source, when the rule knows it.
    /// Path and token IDs must belong to the supplied scene.
    pub site: ValidationSite,
    /// Value-sample time; `None` for structural findings. Numeric times must be finite.
    pub time: Option<Time>,
    /// Stable domain-specific diagnostic code.
    pub code: Arc<str>,
    /// Human-readable explanation of the violation.
    pub message: Arc<str>,
    /// Optional structured USD-value payload, interpreted by the rule's consumer.
    /// This payload is not an erased Rust domain enum.
    pub details: Option<Value>,
}
/// A check could not complete; this is distinct from a violated scene invariant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatorFailure {
    /// Stable execution-failure code defined by the validator.
    pub code: Arc<str>,
    /// Explanation of why the check could not complete.
    pub message: Arc<str>,
}
/// An execution failure with its scheduled rule and target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationExecutionFailure {
    /// Producing validator, assigned by the context.
    pub rule: ValidatorId,
    /// Target on which the callback failed.
    pub target: ValidatorTarget,
    /// Structured execution failure.
    pub failure: ValidatorFailure,
}
/// Caller-owned domain checks, registered explicitly into a validation context.
///
/// The context owns namespace/time scheduling and report assembly. Implementors
/// own their checks and source attribution. Each callback runs synchronously
/// once per selected target with sorted, deduplicated requested times. Structural
/// checks may ignore those times; `AuthoredTimes` checks enumerate their own
/// authored samples and report the actual failure times. Implementations must
/// produce deterministic results for identical scene/target/time inputs, without
/// editing the scene. Metadata is read only at registration and retained by the
/// context, even if subsequent calls to `metadata` would differ.
///
/// Nonfinite diagnostic times are rejected as execution failures; findings from
/// that invocation are discarded before report sorting.
///
/// Return `Err` when execution cannot complete. Any findings appended by that
/// invocation are discarded, its failure is recorded, and other checks continue.
/// No thread-safety bound is imposed: the pipeline does not execute in parallel.
pub trait Validator {
    /// Declares stable identity, target domain and temporal coverage.
    fn metadata(&self) -> ValidatorMetadata;
    /// Checks one scheduled target, appending domain-specific violations.
    fn validate(
        &self,
        scene: &Scene<'_>,
        target: ValidatorTarget,
        times: &[Time],
        output: &mut Vec<CustomValidationProblem>,
    ) -> Result<(), ValidatorFailure>;
}
/// Invalid registration leaves the context unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidatorRegistrationError {
    /// Empty ID, missing namespace/name, or whitespace in an ID.
    InvalidId(Arc<str>),
    /// Another custom validator already has this ID.
    DuplicateId(Arc<str>),
}
impl core::fmt::Display for ValidatorRegistrationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid validator registration: {self:?}")
    }
}
impl core::error::Error for ValidatorRegistrationError {}
#[derive(Clone)]
pub(super) struct RegisteredValidator {
    pub(super) metadata: ValidatorMetadata,
    pub(super) validator: Arc<dyn Validator>,
}
impl core::fmt::Debug for RegisteredValidator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.metadata.fmt(f)
    }
}
impl ValidationContext {
    /// Registers a caller-owned validator, freezing its metadata. IDs must be
    /// `namespace:name` and unique; failure leaves existing registration intact.
    /// Registration order does not affect execution or report order. Cloned
    /// contexts share implementations through `Arc` but retain independent sets.
    pub fn add_validator(
        &mut self,
        validator: Arc<dyn Validator>,
    ) -> Result<(), ValidatorRegistrationError> {
        let metadata = validator.metadata();
        if metadata.id.split_once(':').is_none_or(|(namespace, name)| {
            namespace.is_empty() || name.is_empty() || metadata.id.chars().any(char::is_whitespace)
        }) {
            return Err(ValidatorRegistrationError::InvalidId(metadata.id));
        }
        let at = match self
            .custom
            .binary_search_by(|v| v.metadata.id.cmp(&metadata.id))
        {
            Ok(_) => return Err(ValidatorRegistrationError::DuplicateId(metadata.id)),
            Err(at) => at,
        };
        self.custom.insert(
            at,
            RegisteredValidator {
                metadata,
                validator,
            },
        );
        Ok(())
    }
    /// Frozen custom declarations, in canonical ID/execution order. Built-in
    /// declarations remain discoverable through `builtins` and `rules`.
    pub fn registered_validators(&self) -> impl ExactSizeIterator<Item = &ValidatorMetadata> {
        self.custom.iter().map(|v| &v.metadata)
    }
}
pub(super) fn run(
    registered: &RegisteredValidator,
    scene: &Scene<'_>,
    target: ValidatorTarget,
    out: &mut ValidationReport,
) {
    out.work.rule_invocations += 1;
    out.work.custom_invocations += 1;
    let rule = ValidatorId::Custom(registered.metadata.id.clone());
    let mut findings = Vec::new();
    if let Err(failure) = registered
        .validator
        .validate(scene, target, &out.times, &mut findings)
    {
        out.failures.push(ValidationExecutionFailure {
            rule,
            target,
            failure,
        });
        return;
    }
    if findings
        .iter()
        .any(|p| matches!(p.time, Some(Time::At { code, .. }) if !code.is_finite()))
    {
        out.failures.push(ValidationExecutionFailure {
            rule,
            target,
            failure: ValidatorFailure {
                code: Arc::from("validation:invalid-diagnostic-time"),
                message: Arc::from("validator emitted a nonfinite diagnostic time"),
            },
        });
        return;
    }
    out.problems
        .extend(findings.into_iter().map(|finding| ValidationProblem {
            rule: rule.clone(),
            site: finding.site,
            time: finding.time,
            kind: ValidationProblemKind::Custom {
                code: finding.code,
                message: finding.message,
                details: finding.details,
            },
        }));
}
