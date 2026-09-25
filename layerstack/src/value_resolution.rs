// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Internal value-resolution helpers shared by [`crate::stage`].
//!
//! This module owns sparse-family detection and chain construction for
//! attribute value families that require composed sparse opinions. The
//! strong-over-weak fold itself is delegated to [`opinionated`]'s family
//! kernel via [`resolve_family_chain`], which is time-agnostic:
//!
//! - default-time queries fold authored defaults through [`ArrayFamily`];
//! - time queries first plan the composed series' bracketing samples
//!   ([`plan_brackets`]), then fold the chain once per bracketing sample
//!   through [`PickedArrayFamily`] and interpolate the composed results.

use alloc::{vec, vec::Vec};

use opinionated::{
    FamilyMember, FamilyResolution, IgnoreReason, OpinionFamily, OpinionKind, resolve_family_chain,
};

use crate::{
    array_edit::{ArrayEdit, PropertyTypeFill, apply_to_array},
    doc::{InterpolationType, Value},
    half::{from_f32 as f32_to_half, to_f32 as half_to_f32},
    prim_index::{Opinion, OpinionValue},
    property::PropertyType,
};

/// Internal query modes for sparse value resolution.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SparseQuery<'a> {
    /// Resolve default/fallback opinions without a specific sample time.
    Default {
        /// Optional weakest dense seed, such as a schema fallback.
        fallback: Option<&'a Value>,
    },
    /// Resolve a time-varying query at a specific time.
    AtTime {
        /// Query time, in stage time.
        time: f64,
        /// Interpolation mode.
        interp: InterpolationType,
        /// Optional weakest dense seed, such as a schema fallback.
        fallback: Option<&'a Value>,
    },
}

/// Result of attempting sparse-family resolution.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SparseResolveResult {
    /// No sparse-composable family applied to this query.
    NotApplicable,
    /// A blocking opinion suppressed the value.
    Blocked,
    /// Sparse composition produced a dense resolved value.
    Resolved(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SparseValueFamily {
    Array,
}

/// The sparse array-edit family over authored defaults, expressed over
/// [`opinionated`]'s kernel.
///
/// [`Value::Array`] is the dense member, [`Value::ArrayEdit`] the sparse edit,
/// and [`Value::Blocked`] the block. Time samples do not participate in a
/// default-time query. The seed is the schema fallback when one is present
/// (materialized over the empty array if it is itself an edit), otherwise the
/// empty array. The carried [`PropertyType`] lets edits perform typed
/// materialization (`minsize`/`resize` fill values) during apply.
///
/// Because the kernel pulls opinions lazily and stops at the first dense
/// member or block, opinions hidden behind them are never cloned, and each
/// participating value is cloned exactly once.
#[derive(Clone, Copy, Debug)]
struct ArrayFamily<'a> {
    /// Typed property metadata for edit materialization.
    property_type: Option<&'a PropertyType>,
    /// Optional weakest dense seed, such as a schema fallback.
    fallback: Option<&'a Value>,
}

impl ArrayFamily<'_> {
    fn classify_value(value: &Value) -> FamilyMember<Vec<Value>, ArrayEdit> {
        match value {
            Value::Array(items) => FamilyMember::Dense(items.clone()),
            Value::ArrayEdit(edit) => FamilyMember::Sparse(edit.clone()),
            // A sampled block blocks exactly like an authored default block,
            // wherever it is the held sample. OpenUSD 26.08 lets opinions
            // weaker than a held sampled block show through when the block's
            // series composes at its next sample: the named divergence
            // `transparent-sampled-block`.
            //
            // Spec: AOUSD Core §12.3.6 (blocked attributes: individual time
            // samples can be blocked).
            Value::Blocked => FamilyMember::Block,
            _ => Self::foreign(),
        }
    }

    fn foreign() -> FamilyMember<Vec<Value>, ArrayEdit> {
        // `OpinionKind` cannot name domain families, so the reason degrades
        // to a set-over-set mismatch. The reason is never observed: the lean
        // kernel entry point records no events.
        FamilyMember::Foreign(IgnoreReason::IncompatibleOperation {
            resolved: OpinionKind::Set,
            ignored: OpinionKind::Set,
        })
    }
}

impl OpinionFamily<Opinion> for ArrayFamily<'_> {
    type Value = Vec<Value>;
    type Edit = ArrayEdit;

    fn classify(&self, opinion: &Opinion) -> FamilyMember<Self::Value, Self::Edit> {
        // A default-time query reads only the default slot.
        //
        // Spec: AOUSD Core §12.3.1 (default values).
        match opinion.value.default_value() {
            Some(value) => Self::classify_value(value),
            None => Self::foreign(),
        }
    }

    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value {
        let mut value = base;
        edit.apply_in_place(&mut value, PropertyTypeFill(self.property_type));
        value
    }

    /// The weakest dense value: the schema fallback, else the empty array.
    ///
    /// Edits over a block, default or sampled, compose over it too: a block
    /// resolves to the fallback (Core §12.3.6), and a blocked time sample
    /// blocks exactly like a blocked default (Core §16.2.16.3). OpenUSD 26.08
    /// composes edits over a sampled block over the empty array instead: the
    /// named divergence `sampled-block-drops-fallback`.
    fn seed(&self) -> Self::Value {
        match self.fallback {
            Some(Value::Array(items)) => items.clone(),
            Some(Value::ArrayEdit(edit)) => apply_to_array(edit, &[], self.property_type),
            _ => Vec::new(),
        }
    }
}

/// Attempts sparse-family resolution for the given opinion chain.
///
/// This keeps sparse-family detection and strong-over-weak folding out of
/// [`crate::stage::Stage`]. If no sparse family applies, returns
/// [`SparseResolveResult::NotApplicable`].
///
/// Block semantics follow AOUSD Core §12.3.6 (blocked attributes) and the
/// sparse-array-edits proposal (`OpenUSD-proposals/proposals/sparse-array-edits`,
/// "Generalized Value Resolution with Composed Sparse Opinions" and "Value
/// Resolution"). The fold runs strongest to weakest and stops at the first
/// dense value or block. A block discards every weaker authored opinion; when
/// nothing stronger contributed, the result is [`SparseResolveResult::Blocked`]
/// and callers fall back to the schema fallback. Sparse edits stronger than
/// the block still compose over the weakest dense value that survives it: the
/// fallback seed if one is supplied, otherwise the empty array, since the
/// proposal requires a resolved array value to always be dense.
///
/// Time queries compose the bracketing samples of the chain before
/// interpolating; see [`resolve_array_at_time`].
pub(crate) fn resolve_sparse_value(
    opinions: &[Opinion],
    query: SparseQuery<'_>,
    property_type: Option<&PropertyType>,
) -> SparseResolveResult {
    let Some(family) = SparseValueFamily::for_query(opinions, query) else {
        return SparseResolveResult::NotApplicable;
    };
    family.resolve(opinions, query, property_type)
}

impl SparseValueFamily {
    fn for_query(opinions: &[Opinion], query: SparseQuery<'_>) -> Option<Self> {
        match query {
            SparseQuery::Default { fallback } => {
                match opinions
                    .iter()
                    .find_map(|opinion| opinion.value.default_value())
                {
                    Some(value) => Self::for_value(value),
                    None => fallback.and_then(Self::for_value),
                }
            }
            SparseQuery::AtTime { .. } => opinions.iter().find_map(Self::for_time_opinion),
        }
    }

    fn for_time_opinion(opinion: &Opinion) -> Option<Self> {
        match opinion.value.time_samples() {
            Some(samples) => samples.iter().find_map(|(_, value)| Self::for_value(value)),
            None => opinion.value.default_value().and_then(Self::for_value),
        }
    }

    fn for_value(value: &Value) -> Option<Self> {
        match value {
            Value::Array(_) | Value::ArrayEdit(_) => Some(Self::Array),
            _ => None,
        }
    }

    fn resolve(
        self,
        opinions: &[Opinion],
        query: SparseQuery<'_>,
        property_type: Option<&PropertyType>,
    ) -> SparseResolveResult {
        match self {
            Self::Array => match query {
                SparseQuery::Default { fallback } => {
                    let family = ArrayFamily {
                        property_type,
                        fallback,
                    };
                    // A stronger dense default outside the family ends the
                    // chain: weaker family members stay hidden behind it, and
                    // any accumulated edits materialize over the seed.
                    fold_array_chain(
                        &family,
                        opinions
                            .iter()
                            .take_while(|opinion| !is_foreign_default(&opinion.value)),
                    )
                }
                SparseQuery::AtTime {
                    time,
                    interp,
                    fallback,
                } => resolve_array_at_time(
                    opinions,
                    time,
                    interp,
                    ArrayFamily {
                        property_type,
                        fallback,
                    },
                ),
            },
        }
    }
}

/// Returns `true` for a dense default value outside the array family.
fn is_foreign_default(value: &OpinionValue) -> bool {
    value.default_value().is_some_and(|value| {
        !matches!(
            value,
            Value::Array(_) | Value::ArrayEdit(_) | Value::Blocked
        )
    })
}

/// Folds a strongest-to-weakest opinion chain through [`opinionated`]'s
/// family kernel.
///
/// `opinions` is consumed lazily: the kernel stops pulling at the first dense
/// member or block, so weaker opinions are never cloned.
fn fold_array_chain<'o>(
    family: &ArrayFamily<'_>,
    opinions: impl Iterator<Item = &'o Opinion>,
) -> SparseResolveResult {
    match resolve_family_chain(family, opinions.map(|opinion| (opinion, &()))) {
        FamilyResolution::Resolved { value, .. } => {
            SparseResolveResult::Resolved(Value::Array(value))
        }
        FamilyResolution::Blocked { .. } => SparseResolveResult::Blocked,
        // No opinion contributed: an array-family fallback still resolves on
        // its own; otherwise the family does not apply to this chain.
        FamilyResolution::Absent if family.fallback.is_some() => {
            SparseResolveResult::Resolved(Value::Array(family.seed()))
        }
        FamilyResolution::Absent => SparseResolveResult::NotApplicable,
    }
}

// ── Time queries ─────────────────────────────────────────────────────────
//
// A time query cannot fold each opinion's own interpolated value: an edit
// sampled at one time and a dense value sampled at another must compose at
// matching times, and interpolation must see composed values. OpenUSD and the
// sparse-array-edits proposal ("Evaluating a Strength-Ordering of Samples at
// a Specific Time", `OpenUSD-proposals/proposals/sparse-array-edits/README.md`)
// therefore compose each opinion's bracketing samples into a composed series
// trimmed to the (at most two) samples bracketing the query time, and
// interpolate only those. See `_GetValueFromResolveInfoImpl` and
// `_ResolveInfoResolver::ProcessLayerAtTime` in `pxr/usd/usd/stage.cpp`
// (OpenUSD 26.08) and `SdfComposeTimeSampleSeries` in
// `pxr/usd/sdf/composeTimeSampleSeries.h`.
//
// Layerstack splits that walk in two. [`plan_brackets`] runs the series
// composition without touching values: each composed sample records its time,
// whether it still composes (it is sparse), and which of every participating
// opinion's bracketing samples it is made of. The existing time-agnostic
// kernel then folds the chain once for each composed bracketing sample,
// reading exactly those samples ([`PickedArrayFamily`]), so values are only
// composed (and cloned) for the at most two samples that are interpolated.

/// Sample times closer than this are "close" in OpenUSD's two tolerance
/// rules, which apply to different times and must stay separate:
///
/// - composing two series treats their samples as one time
///   (`Sdf_timesEqualDefaultFn` in `pxr/usd/sdf/composeTimeSampleSeries.h`),
///   see [`Entry::compose_under`];
/// - within one series, two bracketing samples this close in layer time do
///   not interpolate: the lower one holds (`_GetInterpolatingSamplesImpl` in
///   `pxr/usd/usd/interpolators.cpp`), see [`Bracket::of`] and
///   [`interpolate_samples`].
///
/// Both use `GfIsClose(a, b, 1e-6)`, a strict `|a - b| < 1e-6`.
const TIME_EPSILON: f64 = 1e-6;

/// `GfIsClose(a, b, TIME_EPSILON)`; also true for equal infinities.
fn times_close(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() < TIME_EPSILON
}

/// Which of an opinion's bracketing samples a composed sample is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Lower,
    Upper,
}

/// One opinion's bracketing samples for a time query, in stage time.
///
/// A default value is a single sample at `-inf`, as in the proposal ("a
/// Series with a single sample at the earliest time") and OpenUSD. A spline
/// is a default-like sample with no array value (`None`). Splines and
/// non-array values end the fold, as a dense value outside the family ends a
/// default-time fold.
#[derive(Clone, Copy, Debug)]
struct Bracket<'o> {
    /// Position of the opinion among the participating opinions.
    index: usize,
    lower: (f64, Option<&'o Value>),
    upper: (f64, Option<&'o Value>),
}

impl<'o> Bracket<'o> {
    /// Returns `opinion`'s samples bracketing stage time `query`, or `None`
    /// when the opinion has no value to contribute.
    ///
    /// Time-sample brackets follow `SdfLayer::GetBracketingTimeSamples`
    /// (exact layer times): the sample at the query time, else the samples on
    /// either side, clamped to the first or last sample outside the authored
    /// range. As in `_GetInterpolatingSamplesImpl`, only the lower sample
    /// contributes under held interpolation, or when the two samples are
    /// close ([`TIME_EPSILON`]) in layer time, even under linear
    /// interpolation.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (layer offset and scale), §12.3.2.2 (time
    /// samples), §12.5 (interpolation).
    fn of(
        opinion: &'o Opinion,
        index: usize,
        query: f64,
        interp: InterpolationType,
    ) -> Option<Self> {
        let single = |time, value| {
            Some(Self {
                index,
                lower: (time, value),
                upper: (time, value),
            })
        };
        // Per spec: time samples, then a spline, then the default (Core
        // §12.3.2; OpenUSD `ProcessLayerAtTime`).
        if let Some(samples) = opinion.value.time_samples() {
            {
                let offset = opinion.layer_offset;
                let to_stage = |index: usize| {
                    let (local, ref value) = samples[index];
                    (local * offset.scale + offset.offset, Some(value))
                };
                let local = offset.map_time(query);
                let last = samples.len().checked_sub(1)?;
                let (lower, upper) = if local <= samples[0].0 {
                    (0, 0)
                } else if local >= samples[last].0 {
                    (last, last)
                } else {
                    // Strictly inside the authored range, so `1..=last`
                    // (the clamp only guards a NaN query time).
                    let next = samples.partition_point(|(t, _)| *t < local).clamp(1, last);
                    if samples[next].0 == local {
                        (next, next)
                    } else if interp == InterpolationType::Held
                        || times_close(samples[next - 1].0, samples[next].0)
                    {
                        (next - 1, next - 1)
                    } else {
                        (next - 1, next)
                    }
                };
                Some(Self {
                    index,
                    lower: to_stage(lower),
                    upper: to_stage(upper),
                })
            }
        } else if opinion.value.spline().is_some() {
            single(f64::NEG_INFINITY, None)
        } else {
            opinion
                .value
                .default_value()
                .and_then(|value| single(f64::NEG_INFINITY, Some(value)))
        }
    }

    fn sample(&self, pick: Pick) -> Option<&'o Value> {
        match pick {
            Pick::Lower => self.lower.1,
            Pick::Upper => self.upper.1,
        }
    }

    /// The bracket's distinct samples, as a series of its own.
    fn entries(&self) -> Vec<Entry> {
        let own = |(time, value): (f64, Option<&Value>), pick| Entry {
            time,
            composes: Composes::of(value),
            picks: vec![pick],
        };
        let mut entries = vec![own(self.lower, Pick::Lower)];
        if self.upper.0 != self.lower.0 {
            entries.push(own(self.upper, Pick::Upper));
        }
        entries
    }
}

/// Whether a (composed) sample still composes over weaker opinions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Composes {
    /// A sparse edit: weaker opinions still contribute.
    Yes,
    /// A dense value, a block, or a value outside the family: the fold ends.
    No,
}

impl Composes {
    fn of(value: Option<&Value>) -> Self {
        if matches!(value, Some(Value::ArrayEdit(_))) {
            Self::Yes
        } else {
            Self::No
        }
    }

    /// Composes a stronger sample over a weaker one: the result composes only
    /// if both do (edit over edit is an edit; an edit over a dense value or a
    /// block materializes).
    fn over(self, weaker: Self) -> Self {
        if self == Self::Yes && weaker == Self::Yes {
            Self::Yes
        } else {
            Self::No
        }
    }
}

/// One sample of a composed series, without its value.
#[derive(Clone, Debug)]
struct Entry {
    /// Stage time of the composed sample.
    time: f64,
    /// Whether the composed sample still composes over weaker opinions.
    composes: Composes,
    /// For each opinion composed so far (by [`Bracket::index`]), the
    /// bracketing sample this composed sample is made of.
    picks: Vec<Pick>,
}

impl Entry {
    /// The entry of `series` held at `time` while merging, where `next` is
    /// the next unmerged index: the entry at (within [`TIME_EPSILON`] of)
    /// `time`, else the previous entry, else the first (the `held` helper of
    /// `SdfComposeTimeSampleSeries`).
    fn held_while_merging(series: &[Self], next: usize, time: f64) -> &Self {
        if next == series.len() || (next != 0 && !times_close(series[next].time, time)) {
            &series[next - 1]
        } else {
            &series[next]
        }
    }

    fn joined(&self, weaker: &Self, time: f64, composes: Composes) -> Self {
        let mut picks = Vec::with_capacity(self.picks.len() + weaker.picks.len());
        picks.extend_from_slice(&self.picks);
        picks.extend_from_slice(&weaker.picks);
        Self {
            time,
            composes,
            picks,
        }
    }

    /// Composes `strong` over `weak`, following `SdfComposeTimeSampleSeries`:
    /// every stronger sample composes over the weaker sample held at its
    /// time, a weaker sample appears in the result only where the stronger
    /// sample held at its time composes, and samples of the two series closer
    /// than [`TIME_EPSILON`] merge into one.
    fn compose_under(strong: &[Self], weak: &[Self]) -> Vec<Self> {
        if strong.is_empty() {
            return weak.to_vec();
        }
        let mut out = Vec::with_capacity(strong.len() + weak.len());
        let (mut i, mut j) = (0, 0);
        while i < strong.len() || j < weak.len() {
            let strong_time = strong.get(i).map_or(f64::INFINITY, |e| e.time);
            let weak_time = weak.get(j).map_or(f64::INFINITY, |e| e.time);
            if strong_time <= weak_time {
                let held = Self::held_while_merging(weak, j, strong_time);
                let composes = strong[i].composes.over(held.composes);
                out.push(strong[i].joined(held, strong_time, composes));
            } else {
                let held = Self::held_while_merging(strong, i, weak_time);
                if held.composes == Composes::Yes {
                    out.push(held.joined(&weak[j], weak_time, weak[j].composes));
                }
            }
            if i == strong.len() {
                j += 1;
            } else if j == weak.len() {
                i += 1;
            } else if times_close(strong_time, weak_time) {
                i += 1;
                j += 1;
            } else if strong_time < weak_time {
                i += 1;
            } else {
                j += 1;
            }
        }
        out
    }

    /// Trims `series` to the entries bracketing `time`: the entry at `time`,
    /// else the entries on either side, else the first or last entry (the
    /// trimming step of `composeSamples` in `stage.cpp`, with exact times).
    fn bracketing(mut series: Vec<Self>, time: f64) -> Vec<Self> {
        let Some(last) = series.len().checked_sub(1) else {
            return series;
        };
        let (from, to) = if last == 0 || time <= series[0].time {
            (0, 0)
        } else if time >= series[last].time {
            (last, last)
        } else {
            // Strictly inside the series, so `1..=last` (the clamp only
            // guards a NaN query time).
            let next = series.partition_point(|e| e.time < time).clamp(1, last);
            if series[next].time == time {
                (next, next)
            } else {
                (next - 1, next)
            }
        };
        series.truncate(to + 1);
        series.drain(..from);
        series
    }
}

/// The participating opinions of a time query and the composed series'
/// bracketing samples.
#[derive(Debug)]
struct BracketPlan<'o> {
    /// Participating opinions' brackets, strongest first.
    brackets: Vec<Bracket<'o>>,
    /// The composed series' samples bracketing the query time. Empty when no
    /// opinion participates, or when composing left no sample at all.
    composed: Vec<Entry>,
}

/// Composes the opinions' bracketing samples, strongest first, into the
/// composed series' bracketing samples at `time`.
///
/// This is the proposal's `Evaluate` loop over sample times and
/// composability. Each opinion contributes its samples bracketing the query
/// time; the composed series is trimmed back to the samples bracketing `time`
/// after each opinion. When the composed lower sample no longer composes, it
/// hides weaker opinions until the composed upper sample, so weaker opinions
/// are bracketed at that sample's time instead. The walk stops once neither
/// bracketing sample composes (or, for held interpolation, once the lower one
/// does not), so opinions hidden behind dense values and blocks are never
/// visited.
///
/// The walk keeps composing after the query moves to the upper sample even
/// when a weaker opinion's own upper sample does not compose, as the
/// proposal does: its held sample at that time still does. OpenUSD 26.08
/// stops there instead; this is the named divergence `override-early-stop`
/// (`docs/generic-sparse-composition.md`, "Divergences From OpenUSD").
fn plan_brackets<'o>(
    opinions: impl IntoIterator<Item = &'o Opinion>,
    time: f64,
    interp: InterpolationType,
) -> BracketPlan<'o> {
    let mut brackets: Vec<Bracket<'o>> = Vec::new();
    let mut composed: Vec<Entry> = Vec::new();
    let mut query = time;
    for opinion in opinions {
        let Some(bracket) = Bracket::of(opinion, brackets.len(), query, interp) else {
            continue;
        };
        composed = Entry::bracketing(Entry::compose_under(&composed, &bracket.entries()), time);
        brackets.push(bracket);
        // Composing can swallow every sample (a stronger sample that does not
        // compose, merged with an earlier weaker one); OpenUSD then has no
        // value.
        let (Some(lower), Some(upper)) = (composed.first(), composed.last()) else {
            break;
        };
        if lower.composes == Composes::No {
            if upper.composes == Composes::No || interp == InterpolationType::Held {
                break;
            }
            query = upper.time;
        }
    }
    BracketPlan { brackets, composed }
}

/// [`ArrayFamily`] reading, for one composed sample, the bracketing sample
/// each opinion contributes to it.
#[derive(Clone, Copy, Debug)]
struct PickedArrayFamily<'a> {
    array: ArrayFamily<'a>,
    /// [`Entry::picks`] of the composed sample being folded.
    picks: &'a [Pick],
}

impl<'o> OpinionFamily<Bracket<'o>> for PickedArrayFamily<'_> {
    type Value = Vec<Value>;
    type Edit = ArrayEdit;

    fn classify(&self, bracket: &Bracket<'o>) -> FamilyMember<Self::Value, Self::Edit> {
        bracket
            .sample(self.picks[bracket.index])
            .map_or_else(ArrayFamily::foreign, ArrayFamily::classify_value)
    }

    fn apply(&self, edit: Self::Edit, base: Self::Value) -> Self::Value {
        self.array.apply(edit, base)
    }

    fn seed(&self) -> Self::Value {
        self.array.seed()
    }
}

/// Resolves an array-family attribute at stage time `time`.
///
/// Composes the bracketing samples of every participating opinion
/// ([`plan_brackets`]), folds the chain for the composed lower (and, for
/// linear interpolation, upper) sample with the time-agnostic kernel, then
/// interpolates or holds the composed values.
///
/// Before the first composed time sample, a default or fallback is the lower
/// bracketing sample at `-inf`; the composed lower value holds there (Core
/// §12.5.1). OpenUSD 26.08 interpolates towards the upper sample with
/// `alpha = inf / inf`, which yields NaN for interpolating element types:
/// the named divergence `nan-before-first-sample`.
///
/// Spec: AOUSD Core §12.3.2.2 (time samples), §12.3.6 (blocked samples),
/// §12.5 (interpolation); sparse-array-edits proposal, "Composing and
/// Evaluating Time-Varying Sparse Opinions".
fn resolve_array_at_time(
    opinions: &[Opinion],
    time: f64,
    interp: InterpolationType,
    array: ArrayFamily<'_>,
) -> SparseResolveResult {
    let plan = plan_brackets(opinions, time, interp);
    let (Some(lower_entry), Some(upper_entry)) = (plan.composed.first(), plan.composed.last())
    else {
        return if plan.brackets.is_empty() {
            fold_array_chain(&array, core::iter::empty())
        } else {
            SparseResolveResult::Blocked
        };
    };
    let lower = match fold_entry(&plan.brackets, array, lower_entry) {
        SparseResolveResult::Resolved(Value::Array(lower)) => lower,
        other => return other,
    };
    let (lower_time, upper_time) = (lower_entry.time, upper_entry.time);
    if interp == InterpolationType::Held || upper_time == lower_time || lower_time.is_infinite() {
        return SparseResolveResult::Resolved(Value::Array(lower));
    }
    let alpha = (time - lower_time) / (upper_time - lower_time);
    let value = match fold_entry(&plan.brackets, array, upper_entry) {
        SparseResolveResult::Resolved(Value::Array(upper)) => {
            lerp_arrays(&lower, &upper, alpha).unwrap_or(lower)
        }
        // A blocked or absent upper sample holds the lower one
        // (`_GetInterpolatingSamplesImpl` in `interpolators.cpp`).
        _ => lower,
    };
    SparseResolveResult::Resolved(Value::Array(value))
}

/// Folds the participating brackets into the value of one composed sample.
///
/// The fold stops at the first picked sample outside the array family, which
/// ends the chain as a dense value would.
fn fold_entry(
    brackets: &[Bracket<'_>],
    array: ArrayFamily<'_>,
    entry: &Entry,
) -> SparseResolveResult {
    let family = PickedArrayFamily {
        array,
        picks: &entry.picks,
    };
    let chain = brackets
        .iter()
        .take(entry.picks.len())
        .take_while(|bracket| {
            matches!(
                bracket.sample(entry.picks[bracket.index]),
                Some(Value::Array(_) | Value::ArrayEdit(_) | Value::Blocked)
            )
        })
        .map(|bracket| (bracket, &()));
    match resolve_family_chain(&family, chain) {
        FamilyResolution::Resolved { value, .. } => {
            SparseResolveResult::Resolved(Value::Array(value))
        }
        FamilyResolution::Blocked { .. } => SparseResolveResult::Blocked,
        FamilyResolution::Absent if array.fallback.is_some() => {
            SparseResolveResult::Resolved(Value::Array(array.seed()))
        }
        FamilyResolution::Absent => SparseResolveResult::NotApplicable,
    }
}

/// Linearly interpolates two composed arrays element by element.
///
/// Returns `None`, meaning hold the lower array, when the sizes differ or an
/// element type does not interpolate. As in OpenUSD, floating-point scalars
/// (half precision included), vectors, matrices and time codes interpolate,
/// and integers do not (`USD_LINEAR_INTERPOLATION_TYPES` in
/// `pxr/usd/usd/interpolation.h`, `_LerpVisitor` in
/// `pxr/usd/usd/interpolators.cpp`); quaternions interpolate by slerp
/// ([`gf_slerp`]).
///
/// Spec: AOUSD Core §12.5.2 (the linearly interpolating types; others hold).
fn lerp_arrays(lower: &[Value], upper: &[Value], alpha: f64) -> Option<Vec<Value>> {
    if lower.len() != upper.len() {
        return None;
    }
    lower
        .iter()
        .zip(upper)
        .map(|(a, b)| lerp_element(a, b, alpha))
        .collect()
}

// `GfLerp(alpha, a, b)` is `(1 - alpha) * a + alpha * b` in the arithmetic
// of the value type (`pxr/base/gf/math.h`), so the rounding differs by type:
//
// - a scalar promotes to `double`, and the sum narrows once;
// - a vector scales and adds with its `GfVec` operators, which narrow each
//   scaled component to the element type before a sum in that type;
// - `double` vectors and matrices round like `double` scalars.
//
// Rounding once instead of per term is not a tolerance matter: for
// `(1e10, ..)` to `(-1e10, ..)` at `0.500000001` the two float terms cancel
// exactly in OpenUSD, and a double-precision sum leaves `-20`.

/// `GfLerp` over `double`, and `float` promoted to `double`.
fn gf_lerp(a: f64, b: f64, alpha: f64) -> f64 {
    (1.0 - alpha) * a + alpha * b
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a float scalar interpolates in double precision and narrows once, as `GfLerp` does"
)]
fn lerp_f32(a: f32, b: f32, alpha: f64) -> f32 {
    gf_lerp(f64::from(a), f64::from(b), alpha) as f32
}

/// `GfLerp` over `GfVec2f`, `GfVec3f` and `GfVec4f`: each component scales
/// in double precision and narrows to `float` (`GfVec3f::operator*=(double)`),
/// and the two scaled vectors add in `float`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "each scaled component narrows to float, as `GfVec3f` arithmetic does"
)]
fn lerp_f32s<const N: usize>(a: &[f32; N], b: &[f32; N], alpha: f64) -> [f32; N] {
    let scaled = |value: f32, scale: f64| (f64::from(value) * scale) as f32;
    core::array::from_fn(|i| scaled(a[i], 1.0 - alpha) + scaled(b[i], alpha))
}

fn lerp_f64s<const N: usize>(a: &[f64; N], b: &[f64; N], alpha: f64) -> [f64; N] {
    core::array::from_fn(|i| gf_lerp(a[i], b[i], alpha))
}

/// `GfLerp` over `GfHalf`: computed in double precision, then narrowed
/// through `float` to half.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the result narrows to half precision, as `GfHalf` does"
)]
fn lerp_half(a: u16, b: u16, alpha: f64) -> u16 {
    f32_to_half(gf_lerp(f64::from(half_to_f32(a)), f64::from(half_to_f32(b)), alpha) as f32)
}

/// `GfLerp` over `GfVec2h`, `GfVec3h` and `GfVec4h`, as their operators
/// compute it: the scale narrows to `float` (`GfHalf *= double` goes through
/// `float`), each component scales in `float` and narrows to half, and the two
/// halves add in `float` and narrow to half.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the scale narrows to float, as `GfHalf` arithmetic does"
)]
fn lerp_halves<const N: usize>(a: &[u16; N], b: &[u16; N], alpha: f64) -> [u16; N] {
    let scaled = |bits: u16, scale: f64| half_to_f32(f32_to_half(half_to_f32(bits) * scale as f32));
    core::array::from_fn(|i| f32_to_half(scaled(a[i], 1.0 - alpha) + scaled(b[i], alpha)))
}

/// `GfSlerp(alpha, q0, q1)` (`pxr/base/gf/quat.template.cpp`) over
/// quaternion components stored `[i, j, k, r]` and widened to `f64`.
///
/// OpenUSD evaluates it in the quaternion's own scalar type, so the steps
/// round where `GfQuath`, `GfQuatf` and `GfQuatd` arithmetic rounds: `narrow`
/// rounds to that scalar type, and `accumulate` to the precision the dot
/// product accumulates in (`float` for `quath` and `quatf`). The angle, its
/// sine and both scales are scalars; each scaled component narrows before
/// the two add. The dot product's sign picks the shorter arc, and rotations
/// within `1e-5` of each other lerp.
///
/// Spec: AOUSD Core §12.5.2 (quaternions interpolate "via quaternion
/// slerp"); OpenUSD's `Usd_Lerp` in `pxr/usd/usd/interpolators.cpp`.
fn gf_slerp(
    alpha: f64,
    q0: [f64; 4],
    q1: [f64; 4],
    narrow: fn(f64) -> f64,
    accumulate: fn(f64) -> f64,
) -> [f64; 4] {
    let imaginary = narrow(accumulate(
        accumulate(accumulate(q0[0] * q1[0]) + accumulate(q0[1] * q1[1]))
            + accumulate(q0[2] * q1[2]),
    ));
    let mut cos_theta = accumulate(imaginary + accumulate(q0[3] * q1[3]));
    let flip = cos_theta < 0.0;
    if flip {
        cos_theta = -cos_theta;
    }
    let (scale0, mut scale1) = if 1.0 - cos_theta > 0.00001 {
        let theta = narrow(libm::acos(cos_theta));
        let sin_theta = narrow(libm::sin(theta));
        (
            narrow(libm::sin((1.0 - alpha) * theta) / sin_theta),
            narrow(libm::sin(alpha * theta) / sin_theta),
        )
    } else {
        (narrow(1.0 - alpha), narrow(alpha))
    };
    if flip {
        scale1 = -scale1;
    }
    core::array::from_fn(|c| narrow(narrow(scale0 * q0[c]) + narrow(scale1 * q1[c])))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "rounds to single precision, as `float` arithmetic does"
)]
fn round_f32(value: f64) -> f64 {
    f64::from(value as f32)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "rounds to half precision, as `GfHalf` arithmetic does"
)]
fn round_half(value: f64) -> f64 {
    f64::from(half_to_f32(f32_to_half(value as f32)))
}

fn slerp_quath(a: &[u16; 4], b: &[u16; 4], alpha: f64) -> [u16; 4] {
    let widen = |q: &[u16; 4]| q.map(|c| f64::from(half_to_f32(c)));
    let q = gf_slerp(alpha, widen(a), widen(b), round_half, round_f32);
    #[allow(clippy::cast_possible_truncation, reason = "already half precision")]
    q.map(|c| f32_to_half(c as f32))
}

fn slerp_quatf(a: &[f32; 4], b: &[f32; 4], alpha: f64) -> [f32; 4] {
    let q = gf_slerp(
        alpha,
        a.map(f64::from),
        b.map(f64::from),
        round_f32,
        round_f32,
    );
    #[allow(clippy::cast_possible_truncation, reason = "already single precision")]
    q.map(|c| c as f32)
}

/// Interpolates one pair of elements or scalars, or returns `None` when the
/// pair holds (`_LerpVisitor` in `pxr/usd/usd/interpolators.cpp`).
///
/// Spec: AOUSD Core §12.5.2 (the linearly interpolating types; others hold).
fn lerp_element(a: &Value, b: &Value, alpha: f64) -> Option<Value> {
    Some(match (a, b) {
        (Value::Half(a), Value::Half(b)) => Value::Half(lerp_half(*a, *b, alpha)),
        (Value::Vec2h(a), Value::Vec2h(b)) => Value::Vec2h(lerp_halves(a, b, alpha)),
        (Value::Vec3h(a), Value::Vec3h(b)) => Value::Vec3h(lerp_halves(a, b, alpha)),
        (Value::Vec4h(a), Value::Vec4h(b)) => Value::Vec4h(lerp_halves(a, b, alpha)),
        (Value::Float(a), Value::Float(b)) => Value::Float(lerp_f32(*a, *b, alpha)),
        (Value::Double(a), Value::Double(b)) => Value::Double(gf_lerp(*a, *b, alpha)),
        (Value::TimeCode(a), Value::TimeCode(b)) => Value::TimeCode(gf_lerp(*a, *b, alpha)),
        (Value::Vec2f(a), Value::Vec2f(b)) => Value::Vec2f(lerp_f32s(a, b, alpha)),
        (Value::Vec3f(a), Value::Vec3f(b)) => Value::Vec3f(lerp_f32s(a, b, alpha)),
        (Value::Vec4f(a), Value::Vec4f(b)) => Value::Vec4f(lerp_f32s(a, b, alpha)),
        (Value::Vec2d(a), Value::Vec2d(b)) => Value::Vec2d(lerp_f64s(a, b, alpha)),
        (Value::Vec3d(a), Value::Vec3d(b)) => Value::Vec3d(lerp_f64s(a, b, alpha)),
        (Value::Vec4d(a), Value::Vec4d(b)) => Value::Vec4d(lerp_f64s(a, b, alpha)),
        (Value::Matrix2d(a), Value::Matrix2d(b)) => {
            Value::Matrix2d(alloc::boxed::Box::new(lerp_f64s(a, b, alpha)))
        }
        (Value::Matrix3d(a), Value::Matrix3d(b)) => {
            Value::Matrix3d(alloc::boxed::Box::new(lerp_f64s(a, b, alpha)))
        }
        (Value::Matrix4d(a), Value::Matrix4d(b)) => {
            Value::Matrix4d(alloc::boxed::Box::new(lerp_f64s(a, b, alpha)))
        }
        (Value::Quath(a), Value::Quath(b)) => Value::Quath(slerp_quath(a, b, alpha)),
        (Value::Quatf(a), Value::Quatf(b)) => Value::Quatf(slerp_quatf(a, b, alpha)),
        (Value::Quatd(a), Value::Quatd(b)) => Value::Quatd(gf_slerp(alpha, *a, *b, |x| x, |x| x)),
        _ => return None,
    })
}

/// Holds or interpolates one opinion's sorted scalar time samples at layer
/// time `time`.
///
/// The samples bracketing `time` follow `SdfLayer::GetBracketingTimeSamples`,
/// clamped to the first or last sample outside the authored range. Held
/// interpolation, and two bracketing samples closer than [`TIME_EPSILON`],
/// hold the lower sample (`_GetInterpolatingSamplesImpl` in
/// `pxr/usd/usd/interpolators.cpp`). Otherwise the pair interpolates with the
/// element rules of composed arrays ([`lerp_element`]): integers, and values
/// of different or non-interpolating types, hold the lower sample, and so does
/// a blocked upper sample.
///
/// Spec: AOUSD Core §12.3.2.2 (time samples), §12.5 (interpolation methods).
pub(crate) fn interpolate_samples(
    samples: &[(f64, Value)],
    time: f64,
    interp: InterpolationType,
) -> Option<Value> {
    if samples.is_empty() {
        return None;
    }

    match samples
        .binary_search_by(|(t, _)| t.partial_cmp(&time).unwrap_or(core::cmp::Ordering::Equal))
    {
        Ok(idx) => Some(samples[idx].1.clone()),
        Err(0) => Some(samples[0].1.clone()),
        Err(idx) if idx >= samples.len() => Some(samples[samples.len() - 1].1.clone()),
        Err(idx) => {
            let ((lower_time, lower), (upper_time, upper)) = (&samples[idx - 1], &samples[idx]);
            if interp == InterpolationType::Held || times_close(*lower_time, *upper_time) {
                return Some(lower.clone());
            }
            let alpha = (time - lower_time) / (upper_time - lower_time);
            Some(lerp_element(lower, upper, alpha).unwrap_or_else(|| lower.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FieldValue, LayerId, LayerOffset, OpinionKey, TokenId,
        array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex},
        interner::TokenInterner,
        path::{Path, PathId, PathInterner},
        prim_index::{ArcKind, Opinion},
    };
    use alloc::sync::Arc;
    use alloc::vec;

    fn array_value(values: &[i32]) -> Value {
        Value::Array(values.iter().copied().map(Value::Int).collect())
    }

    fn int_array_type() -> PropertyType {
        PropertyType::new(Arc::<str>::from("int"), true, Value::Int(0))
    }

    fn test_key(layer: LayerId, lookup_path: PathId) -> OpinionKey {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let spec_path =
            crate::spec_path::SpecPath::parse("/A", &mut tokens, &mut paths).expect("spec path");
        OpinionKey {
            is_local: true,
            specializes: Vec::new(),
            arc_kind: ArcKind::Local,
            nested_arc_kind: None,
            namespace_depth: 1,
            authored: true,
            arc_list_index: 0,
            layer_strength: 0,
            layer_id: layer,
            lookup_path,
            spec_path,
        }
    }

    fn test_ids() -> (PathId, TokenId) {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let spec_path = paths.intern(Path::parse_absolute("/A", &mut tokens).expect("valid path"));
        let field = tokens.intern("x");
        (spec_path, field)
    }

    fn array_opinion(
        spec_path: PathId,
        field: TokenId,
        value: impl Into<OpinionValue>,
        layer_strength: u16,
    ) -> Opinion {
        Opinion {
            key: OpinionKey {
                layer_strength,
                ..test_key(LayerId(1), spec_path)
            },
            field,
            value: value.into(),
            layer_offset: LayerOffset::IDENTITY,
        }
    }

    /// Test-only opinion payload: a property authoring only time samples.
    fn samples(samples: Vec<(f64, Value)>) -> OpinionValue {
        OpinionValue::from(crate::property::PropertySpec {
            time_samples: Some(samples),
            ..crate::property::PropertySpec::default()
        })
    }

    #[test]
    fn dense_value_terminates_sparse_fold() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                    ops: vec![ArrayEditOp::Write {
                        src: ArrayEditOperand::Literal(Value::Int(9)),
                        index: ArrayIndex::Position(0),
                    }],
                })),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                    ops: vec![ArrayEditOp::Insert {
                        src: ArrayEditOperand::Literal(Value::Int(7)),
                        index: ArrayIndex::End,
                    }],
                })),
                2,
            ),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::Default { fallback: None },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[9, 2])),
            "once the fold reaches a dense array, weaker sparse opinions must not affect the result"
        );
    }

    #[test]
    fn fallback_seeds_sparse_default_resolution() {
        let (spec_path, field) = test_ids();
        let opinions = vec![array_opinion(
            spec_path,
            field,
            FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                ops: vec![ArrayEditOp::Write {
                    src: ArrayEditOperand::Literal(Value::Int(9)),
                    index: ArrayIndex::Position(0),
                }],
            })),
            0,
        )];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::Default {
                fallback: Some(&array_value(&[1, 2])),
            },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[9, 2])),
            "schema fallback should act as the weakest dense seed for sparse array resolution"
        );
    }

    #[test]
    fn held_time_samples_use_same_sparse_fold_as_defaults() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                samples(vec![
                    (
                        0.0,
                        Value::ArrayEdit(ArrayEdit {
                            ops: vec![ArrayEditOp::Write {
                                src: ArrayEditOperand::Literal(Value::Int(9)),
                                index: ArrayIndex::Position(0),
                            }],
                        }),
                    ),
                    (2.0, Value::ArrayEdit(ArrayEdit::default())),
                ]),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::AtTime {
                time: 1.0,
                interp: InterpolationType::Held,
                fallback: None,
            },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[9, 2])),
            "time-sampled sparse opinions should fold over weaker dense values using the same family logic"
        );
    }

    #[test]
    fn blocking_value_still_blocks_sparse_resolution() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 0),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::AtTime {
                time: 1.0,
                interp: InterpolationType::Held,
                fallback: None,
            },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Blocked,
            "blocking opinions must suppress weaker sparse-family values"
        );
    }

    #[test]
    fn stronger_edits_materialize_over_seed_when_block_cuts_chain() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(ArrayEdit {
                    ops: vec![ArrayEditOp::Insert {
                        src: ArrayEditOperand::Literal(Value::Int(7)),
                        index: ArrayIndex::End,
                    }],
                })),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 1),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::Default { fallback: None },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[7])),
            "a block hides weaker opinions, but stronger sparse edits still materialize over the seed"
        );
    }

    #[test]
    fn sparse_over_sparse_fold_matches_grouped_composition() {
        let (spec_path, field) = test_ids();
        let strong = ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Int(8)),
                index: ArrayIndex::Position(1),
            }],
        };
        let weak = ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(7)),
                index: ArrayIndex::End,
            }],
        };

        let full_chain = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(strong.clone())),
                0,
            ),
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(weak.clone())),
                1,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
        ];

        let grouped_chain = vec![
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(Value::ArrayEdit(strong.compose_over(&weak))),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
        ];

        let full = resolve_sparse_value(
            &full_chain,
            SparseQuery::Default { fallback: None },
            Some(&int_array_type()),
        );
        let grouped = resolve_sparse_value(
            &grouped_chain,
            SparseQuery::Default { fallback: None },
            Some(&int_array_type()),
        );
        assert_eq!(
            full, grouped,
            "sparse family folding should preserve associative grouped composition"
        );
    }

    fn write_edit(value: i32, index: i64) -> Value {
        Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(Value::Int(value)),
                index: ArrayIndex::Position(index),
            }],
        })
    }

    /// Folds `opinions` through the production adapter and reports how many
    /// opinions the kernel pulled (and therefore classified or sampled).
    fn fold_counting(
        family: &ArrayFamily<'_>,
        opinions: &[Opinion],
    ) -> (SparseResolveResult, usize) {
        let mut visited = 0;
        let resolved = fold_array_chain(family, opinions.iter().inspect(|_| visited += 1));
        (resolved, visited)
    }

    /// Plans a time query and reports how many opinions the walk visited
    /// (and therefore bracketed).
    fn plan_counting(
        opinions: &[Opinion],
        time: f64,
        interp: InterpolationType,
    ) -> (Vec<f64>, usize) {
        let mut visited = 0;
        let plan = plan_brackets(opinions.iter().inspect(|_| visited += 1), time, interp);
        let times = plan.composed.iter().map(|e| e.time).collect();
        (times, visited)
    }

    #[test]
    fn opinions_hidden_by_a_dense_value_are_not_sampled() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(spec_path, field, samples(vec![(0.0, write_edit(9, 0))]), 0),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
            array_opinion(
                spec_path,
                field,
                samples(vec![(0.0, array_value(&[5, 5, 5]))]),
                2,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[3])), 3),
        ];
        for interp in [InterpolationType::Held, InterpolationType::Linear] {
            assert_eq!(
                resolve_at(&opinions, 0.0, interp),
                SparseResolveResult::Resolved(array_value(&[9, 2]))
            );
            let (_, visited) = plan_counting(&opinions, 0.0, interp);
            assert_eq!(
                visited, 2,
                "the walk must stop at the first dense value without sampling weaker opinions ({interp:?})"
            );
        }
    }

    #[test]
    fn dense_lower_sample_hides_weaker_opinions_until_the_upper_sample() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                samples(vec![(0.0, array_value(&[0, 0])), (2.0, write_edit(9, 0))]),
                0,
            ),
            array_opinion(
                spec_path,
                field,
                samples(vec![
                    (1.0, array_value(&[1, 1])),
                    (3.0, array_value(&[3, 3])),
                ]),
                1,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[7])), 2),
        ];

        // Held: only the lower sample contributes, and being dense it ends the
        // walk at once.
        let (times, visited) = plan_counting(&opinions, 1.5, InterpolationType::Held);
        assert_eq!((times, visited), (vec![0.0], 1));

        // Linear: the weaker series is bracketed at the upper sample's time
        // (2), where its held sample is dense, so the walk ends there. Its
        // sample at 1 is hidden by the stronger dense sample held at 1.
        let (times, visited) = plan_counting(&opinions, 1.5, InterpolationType::Linear);
        assert_eq!((times, visited), (vec![0.0, 2.0], 2));
        assert_eq!(
            resolve_at(&opinions, 1.5, InterpolationType::Held),
            SparseResolveResult::Resolved(array_value(&[0, 0]))
        );
        assert_eq!(
            resolve_at(&opinions, 2.0, InterpolationType::Held),
            SparseResolveResult::Resolved(array_value(&[9, 1]))
        );
    }

    #[test]
    fn opinions_hidden_by_a_block_are_not_visited() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(spec_path, field, FieldValue::Value(write_edit(9, 0)), 0),
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 1),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
            array_opinion(spec_path, field, FieldValue::Value(write_edit(7, 1)), 3),
        ];
        let property_type = int_array_type();
        let family = ArrayFamily {
            property_type: Some(&property_type),
            fallback: None,
        };

        let (_, visited) = fold_counting(&family, &opinions);
        assert_eq!(
            visited, 2,
            "the fold must stop at the first block without visiting weaker opinions"
        );
    }

    fn resolve_at(
        opinions: &[Opinion],
        time: f64,
        interp: InterpolationType,
    ) -> SparseResolveResult {
        resolve_sparse_value(
            opinions,
            SparseQuery::AtTime {
                time,
                interp,
                fallback: None,
            },
            Some(&int_array_type()),
        )
    }

    #[test]
    fn sampled_block_blocks_weaker_dense_array() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(spec_path, field, samples(vec![(0.0, Value::Blocked)]), 0),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[42])), 1),
        ];

        for time in [-1.0, 0.0, 5.0] {
            for interp in [InterpolationType::Held, InterpolationType::Linear] {
                assert_eq!(
                    resolve_at(&opinions, time, interp),
                    SparseResolveResult::Blocked,
                    "a sampled block must hide the weaker array at t={time} ({interp:?})"
                );
            }
        }
    }

    #[test]
    fn sampled_block_applies_only_where_it_is_the_held_sample() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                samples(vec![
                    (0.0, write_edit(9, 0)),
                    (2.0, Value::Blocked),
                    (4.0, array_value(&[7])),
                ]),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 1),
        ];

        let held = InterpolationType::Held;
        assert_eq!(
            resolve_at(&opinions, 1.0, held),
            SparseResolveResult::Resolved(array_value(&[9, 2])),
            "held edit sample before the block composes over the weaker array"
        );
        assert_eq!(
            resolve_at(&opinions, 2.0, held),
            SparseResolveResult::Blocked,
            "exact block sample"
        );
        assert_eq!(
            resolve_at(&opinions, 3.0, held),
            SparseResolveResult::Blocked,
            "held block sample"
        );
        assert_eq!(
            resolve_at(&opinions, 3.0, InterpolationType::Linear),
            SparseResolveResult::Blocked,
            "arrays do not interpolate, so a block holds under linear interpolation too"
        );
        assert_eq!(
            resolve_at(&opinions, 4.0, held),
            SparseResolveResult::Resolved(array_value(&[7])),
            "exact dense sample after the block"
        );
    }

    #[test]
    fn layer_offset_maps_query_time_before_sampling_a_block() {
        let (spec_path, field) = test_ids();
        let mut strong = array_opinion(
            spec_path,
            field,
            samples(vec![(0.0, array_value(&[1])), (1.0, Value::Blocked)]),
            0,
        );
        // Stage time 11 maps to layer time 1, the block sample.
        strong.layer_offset = LayerOffset {
            offset: 10.0,
            scale: 1.0,
        };
        let opinions = vec![
            strong,
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[42])), 1),
        ];

        assert_eq!(
            resolve_at(&opinions, 10.0, InterpolationType::Held),
            SparseResolveResult::Resolved(array_value(&[1]))
        );
        assert_eq!(
            resolve_at(&opinions, 11.0, InterpolationType::Held),
            SparseResolveResult::Blocked
        );
    }

    #[test]
    fn stronger_edits_materialize_over_fallback_seed_when_block_cuts_chain() {
        let (spec_path, field) = test_ids();
        let append_seven = Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(7)),
                index: ArrayIndex::End,
            }],
        });
        let opinions = vec![
            array_opinion(spec_path, field, FieldValue::Value(append_seven), 0),
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 1),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
        ];

        let resolved = resolve_sparse_value(
            &opinions,
            SparseQuery::Default {
                fallback: Some(&array_value(&[5, 6])),
            },
            Some(&int_array_type()),
        );
        assert_eq!(
            resolved,
            SparseResolveResult::Resolved(array_value(&[5, 6, 7])),
            "the fallback survives the block and seeds the stronger edit; [1, 2] does not"
        );
    }

    #[test]
    fn stronger_sampled_edit_materializes_over_seed_when_sampled_block_cuts_chain() {
        let (spec_path, field) = test_ids();
        let append_seven = Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(Value::Int(7)),
                index: ArrayIndex::End,
            }],
        });
        let opinions = vec![
            array_opinion(spec_path, field, samples(vec![(0.0, append_seven)]), 0),
            array_opinion(
                spec_path,
                field,
                samples(vec![(0.0, Value::Blocked), (2.0, array_value(&[3]))]),
                1,
            ),
            array_opinion(spec_path, field, FieldValue::Value(array_value(&[1, 2])), 2),
        ];

        let held = InterpolationType::Held;
        assert_eq!(
            resolve_at(&opinions, 0.0, held),
            SparseResolveResult::Resolved(array_value(&[7])),
            "exact block sample: the stronger edit materializes over the empty seed"
        );
        assert_eq!(
            resolve_at(&opinions, 1.0, held),
            SparseResolveResult::Resolved(array_value(&[7])),
            "held block sample: the stronger edit materializes over the empty seed"
        );
        assert_eq!(
            resolve_at(&opinions, 2.0, held),
            SparseResolveResult::Resolved(array_value(&[3, 7])),
            "once the dense sample takes over, the edit composes over it"
        );
    }

    fn float_array(values: &[f32]) -> Value {
        Value::Array(values.iter().copied().map(Value::Float).collect())
    }

    fn float_array_type() -> PropertyType {
        PropertyType::new(Arc::<str>::from("float"), true, Value::Float(0.0))
    }

    fn float3_array(values: &[[f32; 3]]) -> Value {
        Value::Array(values.iter().copied().map(Value::Vec3f).collect())
    }

    fn edit(op: ArrayEditOp) -> Value {
        Value::ArrayEdit(ArrayEdit { ops: vec![op] })
    }

    fn append(value: Value) -> ArrayEditOp {
        ArrayEditOp::Insert {
            src: ArrayEditOperand::Literal(value),
            index: ArrayIndex::End,
        }
    }

    fn write(value: Value, index: i64) -> ArrayEditOp {
        ArrayEditOp::Write {
            src: ArrayEditOperand::Literal(value),
            index: ArrayIndex::Position(index),
        }
    }

    fn resolve_float_at(
        opinions: &[Opinion],
        time: f64,
        interp: InterpolationType,
        fallback: Option<&Value>,
    ) -> SparseResolveResult {
        resolve_sparse_value(
            opinions,
            SparseQuery::AtTime {
                time,
                interp,
                fallback,
            },
            Some(&float_array_type()),
        )
    }

    /// `usd_interpolation_session_edit` (a port of `TestInterpolation` in
    /// `testUsdAttributeArrayEdits.cpp`): the stronger edit composes over both
    /// bracketing samples before they interpolate.
    #[test]
    fn stronger_edit_composes_over_both_bracketing_samples_before_interpolating() {
        let (spec_path, field) = test_ids();
        let resize_four = Value::ArrayEdit(ArrayEdit {
            ops: vec![ArrayEditOp::Resize { len: 4 }],
        });
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                samples(vec![(2.0, edit(write(Value::Float(8.0), 1)))]),
                0,
            ),
            array_opinion(
                spec_path,
                field,
                samples(vec![
                    (1.0, resize_four),
                    (3.0, float_array(&[2.0, 4.0, 6.0, 8.0])),
                ]),
                1,
            ),
        ];
        let linear = InterpolationType::Linear;
        for (time, expected) in [
            (0.0, [0.0, 8.0, 0.0, 0.0]),
            (2.0, [0.0, 8.0, 0.0, 0.0]),
            (2.5, [1.0, 8.0, 3.0, 4.0]),
            (3.0, [2.0, 8.0, 6.0, 8.0]),
        ] {
            assert_eq!(
                resolve_float_at(&opinions, time, linear, None),
                SparseResolveResult::Resolved(float_array(&expected)),
                "t={time}"
            );
        }
        assert_eq!(
            resolve_float_at(&opinions, 2.5, InterpolationType::Held, None),
            SparseResolveResult::Resolved(float_array(&[0.0, 8.0, 0.0, 0.0]))
        );
    }

    #[test]
    fn composed_arrays_interpolate_by_element_type() {
        let lerp = |a: Value, b: Value| lerp_arrays(&[a], &[b], 0.25).map(|v| v[0].clone());
        assert_eq!(
            lerp(Value::Float(0.0), Value::Float(4.0)),
            Some(Value::Float(1.0))
        );
        assert_eq!(
            lerp(Value::Double(0.0), Value::Double(4.0)),
            Some(Value::Double(1.0))
        );
        assert_eq!(
            lerp(Value::TimeCode(0.0), Value::TimeCode(4.0)),
            Some(Value::TimeCode(1.0))
        );
        assert_eq!(
            lerp(Value::Vec3f([0.0; 3]), Value::Vec3f([4.0, 8.0, 12.0])),
            Some(Value::Vec3f([1.0, 2.0, 3.0]))
        );
        assert_eq!(
            lerp(
                Value::Matrix2d(alloc::boxed::Box::new([0.0; 4])),
                Value::Matrix2d(alloc::boxed::Box::new([4.0; 4]))
            ),
            Some(Value::Matrix2d(alloc::boxed::Box::new([1.0; 4])))
        );
        assert_eq!(
            lerp_arrays(
                &[Value::Vec3f([1e10; 3])],
                &[Value::Vec3f([-1e10; 3])],
                0.500_000_001
            ),
            Some(vec![Value::Vec3f([0.0; 3])]),
            "float vector terms narrow to float before they cancel, as in OpenUSD"
        );
        assert_eq!(
            lerp(Value::Half(0x0000), Value::Half(0x4400)),
            Some(Value::Half(0x3c00)),
            "half 0 to 4 at a quarter is 1"
        );
        assert_eq!(
            lerp(Value::Vec2h([0x0000; 2]), Value::Vec2h([0x4400; 2])),
            Some(Value::Vec2h([0x3c00; 2]))
        );
        assert_eq!(
            lerp(Value::Int(0), Value::Int(4)),
            None,
            "integers hold, as in OpenUSD"
        );
        assert_eq!(
            lerp_arrays(
                &[Value::Float(0.0)],
                &[Value::Float(1.0), Value::Float(2.0)],
                0.5
            ),
            None,
            "arrays of different sizes hold"
        );
    }

    #[test]
    fn quaternions_slerp_along_the_shorter_arc() {
        let close = |a: [f64; 4], b: [f64; 4]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-12);
        let half_turn = core::f64::consts::FRAC_1_SQRT_2;
        // Identity to a half turn about z: halfway is a quarter turn.
        let Some(Value::Quatd(q)) = lerp_element(
            &Value::Quatd([0.0, 0.0, 0.0, 1.0]),
            &Value::Quatd([0.0, 0.0, 1.0, 0.0]),
            0.5,
        ) else {
            panic!("quatd slerps");
        };
        assert!(close(q, [0.0, 0.0, half_turn, half_turn]), "{q:?}");
        // `-q` is the same rotation: the negative dot product flips it onto
        // the shorter arc.
        let identity = Value::Quatd([0.0, 0.0, 0.0, 1.0]);
        let q = lerp_element(&identity, &Value::Quatd([0.0, 0.0, 0.8, 0.6]), 0.25);
        let negated = lerp_element(&identity, &Value::Quatd([0.0, 0.0, -0.8, -0.6]), 0.25);
        let (Some(Value::Quatd(q)), Some(Value::Quatd(negated))) = (q, negated) else {
            panic!("quatd slerps");
        };
        assert!(close(q, negated), "{q:?} != {negated:?}");
        // Nearly equal rotations lerp.
        let Some(Value::Quatd(q)) = lerp_element(
            &Value::Quatd([0.0, 0.0, 0.0, 1.0]),
            &Value::Quatd([0.002, 0.0, 0.0, 0.999_998]),
            0.5,
        ) else {
            panic!("quatd slerps");
        };
        assert!(close(q, [0.001, 0.0, 0.0, 0.999_999]), "{q:?}");
        // Half quaternions round to half precision: 0x39a8 is the half
        // nearest to 0.7071.
        assert_eq!(
            lerp_element(
                &Value::Quath([0, 0, 0, 0x3c00]),
                &Value::Quath([0, 0, 0x3c00, 0]),
                0.5
            ),
            Some(Value::Quath([0, 0, 0x39a8, 0x39a8]))
        );
    }

    #[test]
    fn scalar_samples_follow_the_element_rules() {
        let linear = InterpolationType::Linear;
        let ints = [(0.0, Value::Int(0)), (2.0, Value::Int(4))];
        assert_eq!(
            interpolate_samples(&ints, 1.0, linear),
            Some(Value::Int(0)),
            "integers hold, as in OpenUSD"
        );
        let vectors = [(0.0, Value::Vec3d([0.0; 3])), (2.0, Value::Vec3d([2.0; 3]))];
        assert_eq!(
            interpolate_samples(&vectors, 0.5, linear),
            Some(Value::Vec3d([0.5; 3]))
        );
        let close = [(0.0, Value::Double(0.0)), (5e-7, Value::Double(10.0))];
        assert_eq!(
            interpolate_samples(&close, 2.5e-7, linear),
            Some(Value::Double(0.0)),
            "samples closer than 1e-6 hold the lower one"
        );
        let blocked = [(0.0, Value::Double(1.0)), (2.0, Value::Blocked)];
        assert_eq!(
            interpolate_samples(&blocked, 1.0, linear),
            Some(Value::Double(1.0)),
            "a blocked upper sample holds the lower one"
        );
    }

    /// OpenUSD 26.08 resolved values for a `Cube`'s `extent`, whose schema
    /// fallback is `[(-1, -1, -1), (1, 1, 1)]`: time-sampled edits compose
    /// over the fallback, and so do edits above a default block. The
    /// `schema_*` cases of `layerstack_conformance/tests/temporal_sparse.rs`
    /// check the same through `Stage::resolve_value_at_time_with_schema`;
    /// these run against the resolver directly.
    #[test]
    fn time_sampled_edits_compose_over_the_fallback_seed() {
        let (spec_path, field) = test_ids();
        let fallback = float3_array(&[[-1.0; 3], [1.0; 3]]);
        let float3_type =
            PropertyType::new(Arc::<str>::from("float3"), true, Value::Vec3f([0.0; 3]));
        let resolve = |opinions: &[Opinion], time: f64, interp| {
            resolve_sparse_value(
                opinions,
                SparseQuery::AtTime {
                    time,
                    interp,
                    fallback: Some(&fallback),
                },
                Some(&float3_type),
            )
        };
        let edits = vec![array_opinion(
            spec_path,
            field,
            samples(vec![
                (1.0, edit(write(Value::Vec3f([0.0; 3]), 0))),
                (3.0, edit(append(Value::Vec3f([5.0; 3])))),
            ]),
            0,
        )];
        let written = float3_array(&[[0.0; 3], [1.0; 3]]);
        let appended = float3_array(&[[-1.0; 3], [1.0; 3], [5.0; 3]]);
        for interp in [InterpolationType::Held, InterpolationType::Linear] {
            // OpenUSD resolves NaN before the first sample (`alpha = inf/inf`
            // against the fallback's `-inf` sample); the first sample holds.
            for (time, expected) in [
                (0.0, &written),
                (1.0, &written),
                (2.0, &written),
                (3.0, &appended),
                (4.0, &appended),
            ] {
                assert_eq!(
                    resolve(&edits, time, interp),
                    SparseResolveResult::Resolved(expected.clone()),
                    "t={time} {interp:?}"
                );
            }
        }

        let over_default_block = vec![
            array_opinion(
                spec_path,
                field,
                samples(vec![(1.0, edit(append(Value::Vec3f([5.0; 3]))))]),
                0,
            ),
            array_opinion(spec_path, field, FieldValue::Value(Value::Blocked), 1),
            array_opinion(
                spec_path,
                field,
                FieldValue::Value(float3_array(&[[2.0; 3]])),
                2,
            ),
        ];
        for time in [0.0, 1.0, 2.0] {
            assert_eq!(
                resolve(&over_default_block, time, InterpolationType::Linear),
                SparseResolveResult::Resolved(appended.clone()),
                "the fallback survives the block and seeds the edit at t={time}"
            );
        }
    }

    /// Composes sample values the way OpenUSD 26.08's
    /// `_GetValueFromResolveInfoImpl` does, rather than planning sample times
    /// first: each opinion's interpolating samples (`_GetInterpolatingSamplesImpl`),
    /// `SdfComposeTimeSampleSeries` over values, and trimming to the samples
    /// bracketing the query time, with the proposal's `Evaluate` deciding
    /// when to move the query and stop (`OpenUSD-proposals/proposals/sparse-array-edits/README.md`).
    mod reference {
        use super::*;

        #[derive(Clone, Debug)]
        enum Sample {
            Dense(Vec<Value>),
            Sparse(ArrayEdit),
            Block,
        }

        type Series = Vec<(f64, Sample)>;

        fn sample(value: &Value) -> Sample {
            match value {
                Value::Array(items) => Sample::Dense(items.clone()),
                Value::ArrayEdit(edit) => Sample::Sparse(edit.clone()),
                Value::Blocked => Sample::Block,
                other => panic!("unexpected sample {other:?}"),
            }
        }

        fn close(a: f64, b: f64) -> bool {
            a == b || (a - b).abs() < 1e-6
        }

        /// The opinion's interpolating samples at stage time `query`.
        fn interpolating(
            opinion: &Opinion,
            query: f64,
            interp: InterpolationType,
        ) -> Option<Series> {
            let offset = opinion.layer_offset;
            let samples = match (opinion.value.time_samples(), opinion.value.default_value()) {
                (Some(samples), _) => samples,
                (None, Some(value)) => return Some(vec![(f64::NEG_INFINITY, sample(value))]),
                (None, None) => return None,
            };
            let local = (query - offset.offset) / offset.scale;
            let lower = samples.iter().rposition(|(t, _)| *t <= local).unwrap_or(0);
            let upper = if samples[lower].0 >= local {
                lower
            } else {
                (lower + 1).min(samples.len() - 1)
            };
            let stage = |i: usize| {
                (
                    samples[i].0 * offset.scale + offset.offset,
                    sample(&samples[i].1),
                )
            };
            if interp == InterpolationType::Held || close(samples[lower].0, samples[upper].0) {
                Some(vec![stage(lower)])
            } else {
                Some(vec![stage(lower), stage(upper)])
            }
        }

        fn compose(
            strong: &Sample,
            weak: &Sample,
            ty: &PropertyType,
            seed: &[Value],
        ) -> Option<Sample> {
            let Sample::Sparse(s) = strong else {
                return None;
            };
            Some(match weak {
                Sample::Sparse(w) => Sample::Sparse(s.compose_over(w)),
                Sample::Dense(d) => Sample::Dense(apply_to_array(s, d, Some(ty))),
                Sample::Block => Sample::Dense(apply_to_array(s, seed, Some(ty))),
            })
        }

        fn held(series: &Series, next: usize, time: f64) -> &Sample {
            if next == series.len() || (next != 0 && !close(series[next].0, time)) {
                &series[next - 1].1
            } else {
                &series[next].1
            }
        }

        fn over(strong: &Series, weak: &Series, ty: &PropertyType, seed: &[Value]) -> Series {
            if strong.is_empty() {
                return weak.clone();
            }
            let (mut i, mut j) = (0, 0);
            let mut out = Series::new();
            while i < strong.len() || j < weak.len() {
                let st = strong.get(i).map_or(f64::INFINITY, |e| e.0);
                let wt = weak.get(j).map_or(f64::INFINITY, |e| e.0);
                if st <= wt {
                    let composed = compose(&strong[i].1, held(weak, j, st), ty, seed);
                    out.push((st, composed.unwrap_or_else(|| strong[i].1.clone())));
                } else if let Some(composed) = compose(held(strong, i, wt), &weak[j].1, ty, seed) {
                    out.push((wt, composed));
                }
                if i == strong.len() {
                    j += 1;
                } else if j == weak.len() {
                    i += 1;
                } else if close(st, wt) {
                    i += 1;
                    j += 1;
                } else if st < wt {
                    i += 1;
                } else {
                    j += 1;
                }
            }
            out
        }

        fn trim(series: Series, time: f64) -> Series {
            let last = series.len() - 1;
            if series.len() == 1 || time <= series[0].0 {
                return vec![series[0].clone()];
            }
            if time >= series[last].0 {
                return vec![series[last].clone()];
            }
            let next = series.iter().position(|(t, _)| *t >= time).expect("inside");
            if series[next].0 == time {
                vec![series[next].clone()]
            } else {
                series[next - 1..=next].to_vec()
            }
        }

        pub(super) fn evaluate(
            opinions: &[Opinion],
            time: f64,
            interp: InterpolationType,
            ty: &PropertyType,
            fallback: Option<&Vec<Value>>,
        ) -> SparseResolveResult {
            let seed = fallback.cloned().unwrap_or_default();
            let mut composed = Series::new();
            let mut query = time;
            for opinion in opinions {
                let Some(weaker) = interpolating(opinion, query, interp) else {
                    continue;
                };
                let merged = over(&composed, &weaker, ty, &seed);
                if merged.is_empty() {
                    return SparseResolveResult::Blocked;
                }
                composed = trim(merged, time);
                let dense = |s: &Sample| !matches!(s, Sample::Sparse(_));
                if dense(&composed[0].1) {
                    if dense(&composed[composed.len() - 1].1) {
                        break;
                    }
                    query = composed[composed.len() - 1].0;
                }
            }
            if composed.is_empty() {
                return match fallback {
                    Some(seed) => SparseResolveResult::Resolved(Value::Array(seed.clone())),
                    None => SparseResolveResult::NotApplicable,
                };
            }
            let finish = |s: &Sample| match s {
                Sample::Dense(d) => Some(d.clone()),
                Sample::Sparse(e) => Some(apply_to_array(e, &seed, Some(ty))),
                Sample::Block => None,
            };
            let (lo_time, lo) = &composed[0];
            let (hi_time, hi) = &composed[composed.len() - 1];
            let Some(lower) = finish(lo) else {
                return SparseResolveResult::Blocked;
            };
            if interp == InterpolationType::Held || hi_time == lo_time || lo_time.is_infinite() {
                return SparseResolveResult::Resolved(Value::Array(lower));
            }
            let alpha = (time - lo_time) / (hi_time - lo_time);
            let value = finish(hi)
                .and_then(|upper| lerp_arrays(&lower, &upper, alpha))
                .unwrap_or(lower);
            SparseResolveResult::Resolved(Value::Array(value))
        }
    }

    /// Deterministic xorshift generator for the randomized cross-check.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[allow(clippy::cast_precision_loss, reason = "small test values")]
    fn random_value(rng: &mut Rng, allow_block: bool) -> Value {
        let k = rng.below(10) as f32;
        match rng.below(if allow_block { 7 } else { 6 }) {
            0 | 1 => float_array(&[k, k + 1.0]),
            2 => float_array(&[k]),
            3 => edit(write(Value::Float(k), 0)),
            4 => edit(append(Value::Float(k))),
            5 => Value::ArrayEdit(ArrayEdit {
                ops: vec![ArrayEditOp::Insert {
                    src: ArrayEditOperand::Literal(Value::Float(k)),
                    index: ArrayIndex::Position(0),
                }],
            }),
            _ => Value::Blocked,
        }
    }

    /// How [`random_chain`] spaces sample times.
    #[derive(Clone, Copy, Debug)]
    enum Spacing {
        /// Whole frames.
        Coarse,
        /// Whole frames nudged by amounts below, at and above the 1e-6
        /// closeness tolerance, and layer scales that compress or spread them.
        NearCoincident,
    }

    const NUDGES: [f64; 6] = [0.0, 3e-7, 5e-7, 9e-7, 1e-6, 1.5e-6];

    #[allow(clippy::cast_precision_loss, reason = "small test values")]
    fn random_time(rng: &mut Rng, spacing: Spacing) -> f64 {
        let frame = rng.below(if matches!(spacing, Spacing::Coarse) {
            6
        } else {
            3
        }) as f64;
        match spacing {
            Spacing::Coarse => frame,
            Spacing::NearCoincident => {
                frame + NUDGES[usize::try_from(rng.below(6)).expect("small index")]
            }
        }
    }

    fn random_chain(
        rng: &mut Rng,
        spacing: Spacing,
        spec_path: PathId,
        field: TokenId,
    ) -> Vec<Opinion> {
        let count = 1 + rng.below(4);
        (0..count)
            .map(|strength| {
                let value = if rng.below(4) == 0 {
                    OpinionValue::from(
                        crate::property::PropertySpec::attribute()
                            .with_default(random_value(rng, true)),
                    )
                } else {
                    let mut times: Vec<f64> = (0..1 + rng.below(3))
                        .map(|_| random_time(rng, spacing))
                        .collect();
                    times.sort_by(f64::total_cmp);
                    times.dedup();
                    samples(
                        times
                            .into_iter()
                            .map(|t| (t, random_value(rng, true)))
                            .collect(),
                    )
                };
                let mut opinion = array_opinion(
                    spec_path,
                    field,
                    value,
                    u16::try_from(strength).expect("small chain"),
                );
                let (offset, scale) = match (spacing, rng.below(3)) {
                    (_, 0) => (0.0, 1.0),
                    (Spacing::Coarse, 1) => (1.0, 1.0),
                    (Spacing::Coarse, _) => (0.5, 2.0),
                    (Spacing::NearCoincident, 1) => (5e-7, 1e-7),
                    (Spacing::NearCoincident, _) => (0.0, 10.0),
                };
                opinion.layer_offset = LayerOffset { offset, scale };
                opinion
            })
            .collect()
    }

    fn check_planning_matches_series_composition(spacing: Spacing, seed: u64) {
        let (spec_path, field) = test_ids();
        let ty = float_array_type();
        let fallback = vec![Value::Float(100.0)];
        let mut rng = Rng(seed);
        let times: Vec<f64> = match spacing {
            Spacing::Coarse => (0..=36).map(|step| -1.0 + f64::from(step) * 0.25).collect(),
            Spacing::NearCoincident => [-1.0, 0.0, 1.0, 2.0, 3.0]
                .iter()
                .flat_map(|frame| {
                    [0.0, 1e-7, 2.5e-7, 5e-7, 7e-7, 1e-6, 1.2e-6, 2e-6, 0.5]
                        .iter()
                        .map(move |nudge| frame + nudge)
                })
                .collect(),
        };
        for case in 0..2000 {
            let opinions = random_chain(&mut rng, spacing, spec_path, field);
            let seed = (rng.below(2) == 0).then_some(&fallback);
            let seed_value = seed.map(|s| Value::Array(s.clone()));
            for &time in &times {
                for interp in [InterpolationType::Held, InterpolationType::Linear] {
                    let actual = resolve_array_at_time(
                        &opinions,
                        time,
                        interp,
                        ArrayFamily {
                            property_type: Some(&ty),
                            fallback: seed_value.as_ref(),
                        },
                    );
                    let expected = reference::evaluate(&opinions, time, interp, &ty, seed);
                    assert_eq!(
                        actual, expected,
                        "{spacing:?} case {case} at t={time} ({interp:?}), fallback {seed:?}:\n{opinions:#?}"
                    );
                }
            }
        }
    }

    /// Planning sample times first and folding once per bracketing time gives
    /// the same results as composing the sample values themselves, including
    /// the lazy stops.
    #[test]
    fn bracket_planning_matches_series_composition() {
        check_planning_matches_series_composition(Spacing::Coarse, 0x9e37_79b9_7f4a_7c15);
    }

    /// As [`bracket_planning_matches_series_composition`], with sample times
    /// closer than the 1e-6 tolerance within and across series.
    #[test]
    fn bracket_planning_matches_series_composition_near_coincident_times() {
        check_planning_matches_series_composition(Spacing::NearCoincident, 0x2545_f491_4f6c_dd1d);
    }

    #[test]
    fn nan_query_time_resolves_without_panicking() {
        let (spec_path, field) = test_ids();
        let opinions = vec![
            array_opinion(
                spec_path,
                field,
                samples(vec![
                    (0.0, write_edit(9, 0)),
                    (1.0, write_edit(8, 0)),
                    (2.0, write_edit(7, 0)),
                ]),
                0,
            ),
            array_opinion(
                spec_path,
                field,
                samples(vec![(0.0, array_value(&[1])), (2.0, array_value(&[2]))]),
                1,
            ),
        ];
        for interp in [InterpolationType::Held, InterpolationType::Linear] {
            let _ = resolve_at(&opinions, f64::NAN, interp);
        }
    }
}
