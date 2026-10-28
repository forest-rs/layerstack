// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resumable query-specific residency planning; performs no I/O or decoding.
use super::*;
use crate::sort;

/// Why a query may need this asset, independently of whether it is resident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
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
#[non_exhaustive]
pub struct ClipDemand {
    /// Source identity, role and current availability.
    pub request: ClipAssetRequest,
    /// Query-specific scheduling reason.
    pub reason: ClipDemandReason,
}
/// Query preparation evidence. Replan after satisfying demands and recomposing;
/// a manifest read can reveal further demands. No numeric query performs I/O.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ClipQueryPlan {
    /// Queried property in the stage namespace.
    pub property: PropertyPath,
    /// Requested stage time and interpolation.
    pub time: crate::Time,
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
    /// A plan of `property` at `time` that demands nothing.
    pub(crate) fn empty(property: PropertyPath, time: crate::Time) -> Self {
        Self {
            property,
            time,
            demands: Vec::new(),
            issues: Vec::new(),
            error: None,
            budget_exhausted: false,
        }
    }
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
                    ClipAssetUnavailable::Unanswered | ClipAssetUnavailable::NotResident(_)
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
    /// Whether a clip (not the manifest) awaits resolution or loading.
    pub(super) fn has_unresolved_clips(&self) -> bool {
        self.requests
            .iter()
            .any(|request| request.role != ClipAssetRole::Manifest && !resident(request))
    }
    pub(super) fn new(
        set: &ReadySet,
        path: PathId,
        node: NodeId,
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
        sort_by_time(&mut active);
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
/// A clip set that may source one attribute, as authoring inspection sees it.
#[derive(Clone, Debug)]
pub(crate) struct AuthoringClipSet {
    /// The metadata anchor, at the node of the prim the set applies to.
    pub(crate) key: OpinionKey,
    /// The clip-set name on the metadata owner.
    pub(crate) set_name: Arc<str>,
    /// The prepared property entry, when resident metadata declares the
    /// attribute varying; `None` while `pending` assets are unread.
    pub(crate) entry: Option<usize>,
    /// The assets whose metadata would settle whether the set sources the
    /// attribute: an unresolved or nonresident manifest, or, for a set
    /// without one, the clips its manifest is generated from.
    pub(crate) pending: Arc<[ClipAssetRequest]>,
}
impl Catalog {
    /// The clip sets that source `field` of `prim` by prepared metadata, and
    /// those whose metadata is not resident yet, without reading values.
    /// OpenUSD `Usd_ClipSet::ContainsValueForAttribute`: a set sources an
    /// attribute its manifest declares varying, at every time.
    pub(crate) fn authoring_sets(&self, prim: PathId, field: TokenId) -> Vec<AuthoringClipSet> {
        let mut sets: Vec<_> = self
            .properties
            .get(&(prim, field))
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, entry)| AuthoringClipSet {
                key: entry.key.clone(),
                set_name: entry.set_name.clone(),
                entry: Some(index),
                pending: Arc::from([]),
            })
            .collect();
        for set in self.demands.get(&prim).into_iter().flatten() {
            if sets
                .iter()
                .any(|known| known.key == set.key && known.set_name == set.name)
            {
                continue;
            }
            let pending: Vec<_> = match &set.manifest {
                Some(manifest) => (!resident(manifest))
                    .then(|| manifest.clone())
                    .into_iter()
                    .collect(),
                None => set
                    .requests
                    .iter()
                    .filter(|request| !resident(request))
                    .cloned()
                    .collect(),
            };
            if !pending.is_empty() {
                sets.push(AuthoringClipSet {
                    key: set.key.clone(),
                    set_name: set.name.clone(),
                    entry: None,
                    pending: pending.into(),
                });
            }
        }
        sets
    }

    /// Whether a weaker value can show through entry `entry` of `field` at
    /// `time`, from sample kinds and resident metadata only. A sparse array
    /// edit at a bracketing sample composes
    /// (OpenUSD `_GetSamplesComposability`); an array kind whose clips are
    /// not all resident, or a path expression, is unknown.
    pub(crate) fn authoring_composability(
        &self,
        prim: PathId,
        field: TokenId,
        entry: usize,
        time: f64,
        interpolation: InterpolationType,
    ) -> crate::SourceComposability {
        use crate::SourceComposability;
        let Some(entry) = self
            .properties
            .get(&(prim, field))
            .and_then(|entries| entries.get(entry))
        else {
            return SourceComposability::Unknown;
        };
        if entry
            .property_type
            .as_ref()
            .is_some_and(|ty| ty.type_name() == "pathExpression")
        {
            return SourceComposability::Unknown;
        }
        if entry.evaluator.is_spline() {
            return SourceComposability::Dense;
        }
        if entry.has_sparse_samples {
            let times = entry.evaluator.sample_times();
            let next = times.partition_point(|t| *t < time);
            let (lower, upper) = if next == times.len() || times[next] == time || next == 0 {
                let at = next.min(times.len().saturating_sub(1));
                (at, at)
            } else {
                (next - 1, next)
            };
            let sparse = |at: usize| {
                times
                    .get(at)
                    .is_some_and(|&t| entry.evaluator.sample_is_sparse(t))
            };
            return if sparse(lower) || interpolation == InterpolationType::Linear && sparse(upper) {
                SourceComposability::Composable
            } else {
                SourceComposability::Dense
            };
        }
        if entry.unseen_clips
            && entry
                .property_type
                .as_ref()
                .is_none_or(PropertyType::is_array)
        {
            SourceComposability::Unknown
        } else {
            SourceComposability::Dense
        }
    }

    /// The stage times at which entry `entry`'s sample kinds may change:
    /// its sample times when any is a sparse array edit, else none.
    pub(crate) fn authoring_breakpoints(
        &self,
        prim: PathId,
        field: TokenId,
        entry: usize,
    ) -> &[f64] {
        self.properties
            .get(&(prim, field))
            .and_then(|entries| entries.get(entry))
            .filter(|entry| entry.has_sparse_samples)
            .map_or(&[], |entry| entry.evaluator.sample_times())
    }

    pub(crate) fn query_plan(
        &self,
        property: PropertyPath,
        time: f64,
        interpolation: InterpolationType,
        max_demands: usize,
        index: Option<&PrimIndex>,
    ) -> ClipQueryPlan {
        let mut plan = ClipQueryPlan::empty(
            property,
            crate::Time::At {
                code: time,
                interpolation,
            },
        );
        plan.error = (!time.is_finite()).then_some(ClipEvalError::InvalidQuery);
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
        sort::sort_by(&mut sets, |a, b| {
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
                let strength = index.graph.cmp_keys(&opinion.source().key, &set.key);
                let value = opinion.value();
                if strength.is_le() && value.blocks_animation() {
                    masked = true;
                }
                if strength.is_eq()
                    && (value.time_samples().is_some()
                        || value.spline().is_some()
                        || value.has_default_value())
                {
                    masked = true;
                }
                if strength.is_le() {
                    prefix.push(opinion.opinion().clone());
                }
            }
            if masked {
                continue;
            }
            let Some(time) =
                crate::value_resolution::query_time_for_weaker_source_with_composability(
                    ordinary.with_records(&prefix),
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
