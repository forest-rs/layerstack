// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Selectable, immutable scene checks with structured diagnostics.
//!
//! [`ValidationContext::builtins`] lists the rules actually compiled in. Custom
//! checks implement [`Validator`] and register explicitly with
//! [`ValidationContext::add_validator`]; clones share their implementations. This
//! covers selected OpenUSD validators, not plugin discovery or full validator
//! parity. Stage metadata rules run only for [`ValidationScope::Stage`]. Prim
//! rules run on the selected prims, including inactive/abstract prims retained
//! by the composed stage; unloaded and unselected branches are not inspected.
//! Mesh topology uses exactly the requested times. Subset-family validation
//! follows `UsdGeomSubset::ValidateFamily`: default time and all authored index
//! samples, independently of requested times (see [`RuleTimeDomain`]).
//!
//! ```
//! # fn check(scene: &layerstack_schemas::Scene<'_>) {
//! use layerstack_schemas::validation::{BuiltinRule, ValidationContext, ValidationScope};
//! use layerstack::Time;
//! let checks = ValidationContext::new([BuiltinRule::AttributeTypes]);
//! let report = checks.validate(scene, ValidationScope::Stage, &[Time::Default]).unwrap();
//! for problem in &report.problems {
//!     // Typed problems and source sites are suitable for editor navigation.
//!     let _ = (&problem.kind, &problem.site.source);
//! }
//! # }
//! ```
//!
//! Custom checks use the same run and report:
//!
//! ```
//! # extern crate alloc;
//! # fn check(scene: &layerstack_schemas::Scene<'_>, validator: alloc::sync::Arc<dyn layerstack_schemas::validation::Validator>) {
//! use layerstack_schemas::validation::{ValidationContext, ValidationScope};
//! let mut checks = ValidationContext::new([]);
//! checks.add_validator(validator).unwrap();
//! let registered = checks.registered_validators().next().unwrap();
//! let _ = (&registered.id, registered.domain, registered.time_domain);
//! let report = checks.validate(scene, ValidationScope::Stage, &[layerstack::Time::Default]).unwrap();
//! if !report.is_complete() {
//!     // Execution failures identify the scheduled rule and target.
//!     let _ = &report.failures;
//! }
//! # }
//! ```
//!
//! AOUSD Core §7 (scene description), §10.6 (composition errors), §12
//! (resolution); OpenUSD `pxr/usdValidation/*Validators/validators.cpp` and
//! `usdValidation/coreValidators.cpp`. No scene edits or global registration.

use crate::Scene;
use alloc::{sync::Arc, vec::Vec};
use layerstack::{CompositionError, LayerId, PathId, SpecPath, Time, TokenId};

mod custom;
pub use custom::{
    CustomValidationProblem, ValidationExecutionFailure, Validator, ValidatorDomain,
    ValidatorFailure, ValidatorId, ValidatorMetadata, ValidatorRegistrationError, ValidatorTarget,
};
mod rules;
#[cfg(test)]
mod tests;

/// A built-in rule; variants exist only when their schema domains are enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuiltinRule {
    /// Surface composition errors already recorded by the stage.
    CompositionErrors,
    /// The root layer's defaultPrim must identify an existing composed prim.
    DefaultPrim,
    /// Every authored attribute spec must agree with its composed type name.
    AttributeTypes,
    /// Root-layer metersPerUnit and upAxis must be authored (fallbacks do not count).
    #[cfg(feature = "usd-geom")]
    GeometryStageMetadata,
    /// Mesh point/count/index consistency at the requested times.
    #[cfg(feature = "usd-geom")]
    MeshTopology,
    /// All subset families of each selected imageable, at their authored samples.
    #[cfg(feature = "usd-geom")]
    SubsetFamilies,
    /// A subset's direct parent must be imageable.
    #[cfg(feature = "usd-geom")]
    SubsetParent,
    /// Boundable prims must not have a Gprim ancestor.
    #[cfg(feature = "usd-geom")]
    GeometryEncapsulation,
    /// Material-binding relationships require `MaterialBindingAPI`.
    #[cfg(feature = "usd-shade")]
    MaterialBindingApi,
    /// Properties in the material:binding namespace must be relationships.
    #[cfg(feature = "usd-shade")]
    MaterialBindingPropertyKinds,
    /// A bound subset must author familyName; materialBind families must be restricted.
    #[cfg(all(feature = "usd-shade", feature = "usd-geom"))]
    MaterialSubsetFamilies,
    /// Validate authored shading connections with built-in connectability policies.
    #[cfg(feature = "usd-shade")]
    ShadingConnections,
}
/// How a rule uses time. This is inspectable before a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleTimeDomain {
    /// Structural rule, independent of value sampling.
    Structural,
    /// Every caller-requested default/numeric time.
    RequestedTimes,
    /// Default time and all authored subset-index samples, matching `ValidateFamily`.
    AllSubsetSamples,
    /// Custom check enumerates its own authored samples; the context supplies
    /// canonical requested times but does not gather domain-specific samples.
    AuthoredTimes,
}
impl BuiltinRule {
    /// The actual temporal coverage of this rule.
    #[must_use]
    pub fn time_domain(self) -> RuleTimeDomain {
        match self {
            #[cfg(feature = "usd-geom")]
            Self::MeshTopology => RuleTimeDomain::RequestedTimes,
            #[cfg(feature = "usd-geom")]
            Self::SubsetFamilies => RuleTimeDomain::AllSubsetSamples,
            _ => RuleTimeDomain::Structural,
        }
    }
}
/// Namespace coverage. A subtree includes its root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationScope {
    /// Composed stage and root-layer metadata.
    Stage,
    /// Exactly one prim; its relationships may refer outside the scope.
    Prim(PathId),
    /// A prim and its composed descendants.
    Subtree(PathId),
}
/// An authored source for navigation; absent for computed/fallback-only sites.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationSource {
    /// Source layer, not necessarily the root layer.
    pub layer: LayerId,
    /// Variant-qualified authored spec; none for layer metadata.
    pub spec: Option<SpecPath>,
}
/// The composed location and, where known, the authored source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationSite {
    /// None denotes the stage/root-layer metadata.
    pub prim: Option<PathId>,
    /// Property token for a property-specific violation.
    pub property: Option<TokenId>,
    /// Authored origin when known without guessing a contributing spec.
    pub source: Option<ValidationSource>,
}
/// A type name, retaining array-ness without incidental default-value storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeTypeName {
    /// Scalar USD type name.
    pub name: Arc<str>,
    /// Whether this is an array declaration.
    pub is_array: bool,
}
/// Typed violated invariants. Built-in diagnostics do not require prose parsing.
#[derive(Clone, Debug, PartialEq)]
pub enum ValidationProblemKind {
    /// Domain-specific custom finding, with common site/time attribution.
    Custom {
        /// Stable diagnostic code within the producing validator's domain.
        code: Arc<str>,
        /// Human-readable invariant violation.
        message: Arc<str>,
        /// Optional USD-value details; consumers interpret the rule's payload.
        details: Option<layerstack::Value>,
    },
    /// A composition operator was ignored; original structured details are retained.
    Composition(CompositionError),
    /// Root defaultPrim is absent, malformed, or points outside the composed stage.
    InvalidDefaultPrim,
    /// One authored spec disagrees with the composed attribute type.
    AttributeTypeMismatch {
        /// Composed declaration.
        expected: Option<AttributeTypeName>,
        /// This source's authored declaration.
        authored: Option<AttributeTypeName>,
    },
    /// Missing root-layer metadata; schema fallbacks do not satisfy authoring.
    #[cfg(feature = "usd-geom")]
    MissingGeometryMetadata(&'static str),
    /// A mesh topology failure at the diagnostic's time.
    #[cfg(feature = "usd-geom")]
    MeshTopology(crate::subset::MeshTopologyError),
    /// Subset family validation, including its original sample time and site.
    #[cfg(feature = "usd-geom")]
    SubsetFamily {
        /// Family name.
        family: alloc::string::String,
        /// The violated family invariant.
        problem: crate::subset::SubsetProblemKind,
    },
    /// A subset's direct parent is not imageable.
    #[cfg(feature = "usd-geom")]
    SubsetParentNotImageable,
    /// The offending Gprim ancestor of a boundable.
    #[cfg(feature = "usd-geom")]
    GprimAncestor(PathId),
    /// Binding relationships exist without the applied API.
    #[cfg(feature = "usd-shade")]
    MissingMaterialBindingApi,
    /// A binding-namespace property is an attribute.
    #[cfg(feature = "usd-shade")]
    MaterialBindingNotRelationship,
    /// A bound subset has no authored familyName value.
    #[cfg(all(feature = "usd-shade", feature = "usd-geom"))]
    MissingMaterialSubsetFamilyName,
    /// A materialBind family has the unrestricted family type.
    #[cfg(all(feature = "usd-shade", feature = "usd-geom"))]
    UnrestrictedMaterialSubsetFamily,
    /// A connection rejected by the existing built-in shading policy.
    #[cfg(feature = "usd-shade")]
    ShadingConnection(crate::shading::ConnectionError),
    /// A shading connection points at a prim rather than a property.
    #[cfg(feature = "usd-shade")]
    ShadingSourceNotProperty(layerstack::TargetPath),
}
/// One error emitted by a selected rule.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationProblem {
    /// Producing rule.
    pub rule: ValidatorId,
    /// Exact location, including authored source when known.
    pub site: ValidationSite,
    /// Some only for value-time-dependent failures.
    pub time: Option<Time>,
    /// Machine-readable error details.
    pub kind: ValidationProblemKind,
}
/// Work counters count actual explicit operations, not estimated elapsed time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValidationWork {
    /// Selected composed prims visited, excluding the pseudo-root.
    pub prims: usize,
    /// All rule invocations, including failed custom callbacks; stage checks
    /// run once, prim checks once per selected prim.
    pub rule_invocations: usize,
    /// Custom callback invocations, including failures.
    pub custom_invocations: usize,
    /// Authored property declarations inspected for type conflicts.
    pub property_specs: usize,
    /// Mesh topology evaluations at distinct requested times.
    pub mesh_time_evaluations: usize,
    /// Calls into the subset-family validator; each may read multiple times.
    pub subset_families: usize,
    /// Individual connection targets examined.
    pub connections: usize,
}
/// Result for selected built-in/custom checks, scope and times. Execution
/// failures are separate from scene violations; inspect `is_complete` as well.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationReport {
    /// Rule selection in canonical order.
    pub rules: Vec<ValidatorId>,
    /// Scope used for the run.
    pub scope: ValidationScope,
    /// Requested times, sorted and deduplicated; see each rule's time domain.
    pub times: Vec<Time>,
    /// Deterministic rule/namespace/property/source/sample order.
    pub problems: Vec<ValidationProblem>,
    /// Custom callbacks that could not complete, in rule/target execution order.
    /// Findings from those failed invocations are discarded.
    pub failures: Vec<ValidationExecutionFailure>,
    /// Observable work performed.
    pub work: ValidationWork,
}
impl ValidationReport {
    /// Whether every scheduled check completed successfully.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }
    /// Whether all selected checks found no errors in their documented coverage.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.is_complete() && self.problems.is_empty()
    }
}
/// Invalid request; no partial report is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationError {
    /// The explicit time list is empty or contains a nonfinite numeric time.
    InvalidTimes,
    /// The prim/subtree root is absent from the composed stage.
    MissingScopePrim(PathId),
}
impl core::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid validation request: {self:?}")
    }
}
impl core::error::Error for ValidationError {}
/// Explicit, reusable built-in selection and caller-owned custom validators.
/// No hidden registry, dynamic loading or cache.
#[derive(Clone, Debug)]
pub struct ValidationContext {
    rules: Vec<BuiltinRule>,
    custom: Vec<custom::RegisteredValidator>,
}
impl Default for ValidationContext {
    fn default() -> Self {
        Self::new(Self::builtins().iter().copied())
    }
}
impl ValidationContext {
    /// Selects rules; duplicates are removed and declaration order is canonical.
    #[must_use]
    pub fn new(rules: impl IntoIterator<Item = BuiltinRule>) -> Self {
        let mut rules: Vec<_> = rules.into_iter().collect();
        rules.sort_unstable();
        rules.dedup();
        Self {
            rules,
            custom: Vec::new(),
        }
    }
    /// Every built-in rule available with the current feature set.
    #[must_use]
    pub const fn builtins() -> &'static [BuiltinRule] {
        &[
            BuiltinRule::CompositionErrors,
            BuiltinRule::DefaultPrim,
            BuiltinRule::AttributeTypes,
            #[cfg(feature = "usd-geom")]
            BuiltinRule::GeometryStageMetadata,
            #[cfg(feature = "usd-geom")]
            BuiltinRule::MeshTopology,
            #[cfg(feature = "usd-geom")]
            BuiltinRule::SubsetFamilies,
            #[cfg(feature = "usd-geom")]
            BuiltinRule::SubsetParent,
            #[cfg(feature = "usd-geom")]
            BuiltinRule::GeometryEncapsulation,
            #[cfg(feature = "usd-shade")]
            BuiltinRule::MaterialBindingApi,
            #[cfg(feature = "usd-shade")]
            BuiltinRule::MaterialBindingPropertyKinds,
            #[cfg(all(feature = "usd-shade", feature = "usd-geom"))]
            BuiltinRule::MaterialSubsetFamilies,
            #[cfg(feature = "usd-shade")]
            BuiltinRule::ShadingConnections,
        ]
    }
    /// The selected built-in rules in their execution order.
    #[must_use]
    pub fn rules(&self) -> &[BuiltinRule] {
        &self.rules
    }
    /// Runs selected checks against an immutable scene snapshot. Numeric times
    /// must be finite; at least one time is required even for structural-only
    /// selections. Empty rule selections are valid and perform no rule work.
    pub fn validate(
        &self,
        scene: &Scene<'_>,
        scope: ValidationScope,
        times: &[Time],
    ) -> Result<ValidationReport, ValidationError> {
        rules::validate(&self.rules, &self.custom, scene, scope, times)
    }
}
