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
    /// A stronger sparse or unresolved clip set may advance this set's query.
    StrongerSourceComposition,
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
    /// Clip eligibility from prepared metadata, without projecting sample values.
    pub(crate) fn authoring_sources(
        &self,
        prim: PathId,
        field: TokenId,
    ) -> Vec<(OpinionKey, Arc<str>, crate::SourceComposability)> {
        use crate::SourceComposability;
        let entries = self.properties.get(&(prim, field));
        let mut sources: Vec<_> = entries
            .into_iter()
            .flatten()
            .map(|entry| {
                // Non-array composable kinds and incomplete residency need not
                // have the same shape at every queried bracket. Do not infer a
                // dense cutoff from the array-only sparse-samples optimization.
                (
                    entry.key.clone(),
                    entry.set_name.clone(),
                    SourceComposability::Unknown,
                )
            })
            .collect();
        for set in self.demands.get(&prim).into_iter().flatten() {
            if sources
                .iter()
                .any(|(key, name, _)| key == &set.key && name == &set.name)
            {
                continue;
            }
            let unknown = |request: &ClipAssetRequest| {
                matches!(
                    request.status,
                    ClipAssetStatus::Unavailable(
                        ClipAssetUnavailable::Unresolved | ClipAssetUnavailable::NotResident(_)
                    )
                )
            };
            let unresolved = match &set.manifest {
                Some(manifest) => unknown(manifest),
                None => set.template_unresolved || set.requests.iter().any(unknown),
            };
            if unresolved {
                sources.push((
                    set.key.clone(),
                    set.name.clone(),
                    SourceComposability::Unknown,
                ));
            }
        }
        sources
    }

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
        let Some(index) = index else {
            return plan;
        };
        let ordinary = index
            .property_opinions(property.property())
            .unwrap_or_default();
        let mut sets: Vec<_> = self
            .demands
            .get(&property.prim_path())
            .into_iter()
            .flatten()
            .collect();
        // Stable ties preserve the composed clipSets order. Ancestor discovery
        // order alone does not establish composition strength.
        sets.sort_by(|a, b| {
            index
                .graph
                .cmp_nodes(a.key.node, b.key.node)
                .then_with(|| a.key.layer_strength.cmp(&b.key.layer_strength))
        });
        let mut stronger_candidate = false;
        let mut masked_from = sets.len();
        for (position, set) in sets.iter().enumerate() {
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
                        || opinion.value.has_default_value())
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
            if let Some(manifest) = &set.manifest {
                add_demand(&mut plan.demands, manifest, ClipDemandReason::Manifest);
                if !resident(manifest) {
                    stronger_candidate = true;
                    continue;
                }
                if !set.declarations.get(&property.property()).is_some_and(|p| {
                    p.kind == crate::PropertyKind::Attribute
                        && p.variability == Variability::Varying
                }) {
                    continue;
                }
            }
            let entry = self
                .properties
                .get(&(property.prim_path(), property.property()))
                .into_iter()
                .flatten()
                .find(|p| p.owner == set.owner && p.set_name == set.name && p.key == set.key);
            if set.manifest.is_none()
                && !set.template_unresolved
                && set.requests.iter().all(resident)
                && entry.is_none()
            {
                continue;
            }
            let may_mask = !stronger_candidate;
            stronger_candidate = true;
            if let Err(error) = append_value_demands(
                set,
                entry,
                time,
                interpolation,
                !may_mask,
                &mut plan.demands,
            ) {
                plan.error = Some(error);
                continue;
            }
            // AOUSD Core §12.3.2 and §12.5: the first eligible dense source,
            // including a complete gap, masks weaker sets. Only required assets
            // must be resident. Sparse/unknown stronger sets remain conservative;
            // their query advancement must not be guessed from numeric values.
            if may_mask
                && plan.error.is_none()
                && entry.is_some_and(|p| !p.has_sparse_samples)
                && plan
                    .demands
                    .iter()
                    .filter(|d| set.requests.contains(&d.request))
                    .all(|d| resident(&d.request))
            {
                masked_from = position + 1;
                break;
            }
        }
        for issue in &self.issues {
            if issue.kind != ClipIssueKind::UnresolvedTemplate
                && !sets[masked_from..].iter().any(|set| {
                    set.owner == issue.owner
                        && set.name == issue.set_name
                        && set.key.layer_id == issue.anchor
                        && set.key.spec_path == issue.source
                })
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

fn resident(request: &ClipAssetRequest) -> bool {
    matches!(
        request.status,
        ClipAssetStatus::Loaded(_) | ClipAssetStatus::Unavailable(ClipAssetUnavailable::Missing)
    )
}
fn add_demand(demands: &mut Vec<ClipDemand>, request: &ClipAssetRequest, reason: ClipDemandReason) {
    if !demands.iter().any(|d| d.request == *request) {
        demands.push(ClipDemand {
            request: request.clone(),
            reason,
        });
    }
}
fn append_value_demands(
    set: &DemandSet,
    entry: Option<&ClipProperty>,
    time: f64,
    interpolation: InterpolationType,
    stronger_source: bool,
    demands: &mut Vec<ClipDemand>,
) -> Result<(), ClipEvalError> {
    if stronger_source
        || set.template_unresolved
        || set.manifest.is_none()
        || set.interpolate_missing
    {
        let reason = if set.template_unresolved {
            ClipDemandReason::TemplateSchedule
        } else if set.manifest.is_none() {
            ClipDemandReason::AutomaticManifest
        } else if stronger_source {
            ClipDemandReason::StrongerSourceComposition
        } else {
            ClipDemandReason::MissingValueSearch
        };
        for request in &set.requests {
            if request.role != ClipAssetRole::Manifest {
                add_demand(demands, request, reason);
            }
        }
        return Ok(());
    }
    let slot = set
        .active
        .partition_point(|(start, _)| *start <= time)
        .saturating_sub(1);
    let Some(&(_, active)) = set.active.get(slot) else {
        return Ok(());
    };
    let request_for = |index: usize| {
        set.identifiers.get(index).and_then(|name| {
            set.requests
                .iter()
                .find(|r| r.role != ClipAssetRole::Manifest && r.identifier == *name)
        })
    };
    if let Some(request) = request_for(active) {
        add_demand(demands, request, ClipDemandReason::ActiveInterval);
        if !resident(request) {
            return Ok(());
        }
    }
    if let Some(entry) = entry {
        let (lower, upper) = entry.evaluator.required_clips(time)?;
        if let Some(request) = request_for(lower) {
            add_demand(demands, request, ClipDemandReason::LowerInterpolationBound);
        }
        if interpolation == InterpolationType::Linear
            && entry.evaluator.lower_has_value(time)?
            && let Some(request) = request_for(upper)
        {
            add_demand(demands, request, ClipDemandReason::UpperInterpolationBound);
        }
    }
    Ok(())
}
