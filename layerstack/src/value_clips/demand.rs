// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resumable query-specific residency planning; performs no I/O or decoding.
use super::*;

/// Why a query may need this asset, independently of whether it is resident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipDemandReason {
    /// Determine whether the manifest declares this property eligible.
    Manifest,
    /// Without an explicit manifest, every clip contributes to declaration discovery.
    AutomaticManifest,
    /// The clip active at the requested time.
    ActiveInterval,
    /// Clip supplying the earlier bracketing sample.
    LowerInterpolationBound,
    /// Clip supplying the later sample, possibly at the next activation.
    UpperInterpolationBound,
    /// Template scheduling needs every candidate confirmed present or missing.
    TemplateSchedule,
    /// Missing-value interpolation can search across other activation intervals.
    MissingValueSearch,
}
/// One anchored demand and its reason. Loaded/missing results are retained so
/// consumers can distinguish a complete gap from unresolved residency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipDemand {
    /// Source identity, role and current availability.
    pub request: ClipAssetRequest,
    /// Query-specific scheduling reason.
    pub reason: ClipDemandReason,
}
/// Query preparation evidence. Replan after satisfying demands and recomposing;
/// a manifest read can reveal further demands. No numeric query performs I/O.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipQueryPlan {
    /// Queried property in the stage namespace.
    pub property: PropertyPath,
    /// Requested stage time.
    pub time: f64,
    /// Requested interpolation policy.
    pub interpolation: InterpolationType,
    /// Required assets, including resident and confirmed-missing uses.
    pub demands: Vec<ClipDemand>,
    /// Relevant preparation diagnostics, excluding mere unresolved-template state.
    pub issues: Vec<ClipIssue>,
    /// Invalid query or selected evaluator failure.
    pub error: Option<ClipEvalError>,
    /// The explicit returned-demand limit was exceeded; resume with a larger limit.
    pub budget_exhausted: bool,
}
impl ClipQueryPlan {
    /// True when planning found no unknown residency, error or exhausted budget.
    /// A confirmed missing clip can supply a complete gap; completeness does not
    /// promise a numeric value or validate deferred numeric payloads.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.budget_exhausted
            && self.error.is_none()
            && self.issues.is_empty()
            && self.demands.iter().all(|d| {
                matches!(
                    d.request.status,
                    ClipAssetStatus::Loaded(_)
                        | ClipAssetStatus::Unavailable(ClipAssetUnavailable::Missing)
                )
            })
    }
    /// Unresolved/nonresident requests to satisfy explicitly before replanning.
    pub fn pending(&self) -> impl Iterator<Item = &ClipDemand> {
        self.demands.iter().filter(|d| {
            matches!(
                d.request.status,
                ClipAssetStatus::Unavailable(
                    ClipAssetUnavailable::Unresolved | ClipAssetUnavailable::NotResident(_)
                )
            )
        })
    }
}
#[derive(Clone, Debug)]
pub(super) struct DemandSet {
    key: OpinionKey,
    owner: PathId,
    name: Arc<str>,
    manifest: Option<ClipAssetRequest>,
    declarations: BTreeMap<TokenId, Arc<PropertySpec>>,
    requests: Vec<ClipAssetRequest>,
    active: Vec<(f64, usize)>,
    identifiers: Vec<Arc<str>>,
    template_unresolved: bool,
    interpolate_missing: bool,
}
impl DemandSet {
    pub(super) fn new(
        set: &ReadySet,
        path: PathId,
        node: crate::NodeId,
        inventory: &[ClipAssetRequest],
    ) -> Self {
        let requests: Vec<_> = inventory
            .iter()
            .filter(|r| {
                r.owner == set.definition.owner
                    && r.set_name == set.definition.name
                    && r.anchor == set.definition.anchor.layer_id
                    && r.source == set.definition.anchor.spec_path
            })
            .cloned()
            .collect();
        let mut active = set.active.clone();
        active.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut key = set.definition.anchor.clone();
        key.node = node;
        Self {
            key,
            owner: set.definition.owner,
            name: set.definition.name.clone(),
            manifest: requests
                .iter()
                .find(|r| r.role == ClipAssetRole::Manifest)
                .cloned(),
            declarations: set
                .manifest
                .as_ref()
                .and_then(|(_, m)| m.at(path))
                .cloned()
                .unwrap_or_default(),
            requests,
            active,
            identifiers: set.identifiers.clone(),
            template_unresolved: set.template_unresolved,
            interpolate_missing: set.interpolate_missing,
        }
    }
}
impl Catalog {
    pub(crate) fn query_plan(
        &self,
        property: PropertyPath,
        time: f64,
        interpolation: InterpolationType,
        max_demands: usize,
        index: Option<&PrimIndex>,
    ) -> ClipQueryPlan {
        let mut plan = ClipQueryPlan {
            property,
            time,
            interpolation,
            demands: Vec::new(),
            issues: Vec::new(),
            error: (!time.is_finite()).then_some(ClipEvalError::InvalidQuery),
            budget_exhausted: false,
        };
        if plan.error.is_some() {
            return plan;
        }
        for set in self
            .demands
            .get(&property.prim_path())
            .into_iter()
            .flatten()
        {
            let Some(index) = index else {
                continue;
            };
            let ordinary = index
                .property_opinions(property.property())
                .unwrap_or_default();
            let mut prefix = Vec::new();
            let mut masked = false;
            for opinion in ordinary {
                let strength = index
                    .graph
                    .cmp_nodes(opinion.key.node, set.key.node)
                    .then_with(|| opinion.key.layer_strength.cmp(&set.key.layer_strength));
                if strength.is_le() && opinion.value.blocks_animation() {
                    masked = true;
                }
                if strength.is_eq()
                    && (opinion.value.time_samples().is_some()
                        || opinion.value.spline().is_some()
                        || opinion.value.default_value().is_some())
                {
                    masked = true;
                }
                if strength.is_le() {
                    prefix.push(opinion.clone());
                }
            }
            if masked {
                continue;
            }
            let Some(time) =
                crate::value_resolution::query_time_for_weaker_source_with_composability(
                    &prefix,
                    time,
                    interpolation,
                    None,
                )
            else {
                continue;
            };
            let mut add = |request: &ClipAssetRequest, reason| {
                if !plan.demands.iter().any(|d| d.request == *request) {
                    plan.demands.push(ClipDemand {
                        request: request.clone(),
                        reason,
                    });
                }
            };
            if let Some(manifest) = &set.manifest {
                add(manifest, ClipDemandReason::Manifest);
                if !matches!(manifest.status, ClipAssetStatus::Loaded(_)) {
                    continue;
                }
                if !set.declarations.get(&property.property()).is_some_and(|p| {
                    p.kind == crate::PropertyKind::Attribute
                        && p.variability == Variability::Varying
                }) {
                    continue;
                }
            }
            if set.template_unresolved || set.manifest.is_none() || set.interpolate_missing {
                let reason = if set.template_unresolved {
                    ClipDemandReason::TemplateSchedule
                } else if set.manifest.is_none() {
                    ClipDemandReason::AutomaticManifest
                } else {
                    ClipDemandReason::MissingValueSearch
                };
                for request in &set.requests {
                    if request.role != ClipAssetRole::Manifest {
                        add(request, reason);
                    }
                }
                continue;
            }
            let slot = set
                .active
                .partition_point(|(start, _)| *start <= time)
                .saturating_sub(1);
            let Some(&(_, active)) = set.active.get(slot) else {
                continue;
            };
            let request_for = |index: usize| {
                set.identifiers.get(index).and_then(|name| {
                    set.requests
                        .iter()
                        .find(|r| r.role != ClipAssetRole::Manifest && r.identifier == *name)
                })
            };
            if let Some(request) = request_for(active) {
                add(request, ClipDemandReason::ActiveInterval);
                if !matches!(
                    request.status,
                    ClipAssetStatus::Loaded(_)
                        | ClipAssetStatus::Unavailable(ClipAssetUnavailable::Missing)
                ) {
                    continue;
                }
            }
            if let Some(entry) = self
                .properties
                .get(&(property.prim_path(), property.property()))
                .into_iter()
                .flatten()
                .find(|p| p.owner == set.owner && p.set_name == set.name)
            {
                match entry.evaluator.evaluate(time, interpolation) {
                    Ok(value) => {
                        if let Some(request) = request_for(value.lower_clip) {
                            add(request, ClipDemandReason::LowerInterpolationBound);
                        }
                        if interpolation == InterpolationType::Linear
                            && let Some(request) = request_for(value.upper_clip)
                        {
                            add(request, ClipDemandReason::UpperInterpolationBound);
                        }
                    }
                    Err(error) => plan.error = Some(error),
                }
            }
        }
        for issue in &self.issues {
            if issue.kind != ClipIssueKind::UnresolvedTemplate
                && self
                    .owners
                    .get(&property.prim_path())
                    .is_some_and(|owners| owners.contains(&issue.owner))
            {
                plan.issues.push(issue.clone());
            }
        }
        plan.budget_exhausted = plan.demands.len() > max_demands;
        plan.demands.truncate(max_demands);
        plan
    }
}
