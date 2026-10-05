// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plans sparse temporal composition without inspecting or cloning host values.
//!
//! The host supplies each source's bracketing samples, strongest source first,
//! at the time requested by [`TemporalPlanner::query`]. The planner merges their
//! times and classifications, retaining recipes for materializing the composed
//! brackets. Materialization uses the ordinary opinion-family fold; interpolation
//! happens afterward in the host.
//!
//! The merge follows `SdfComposeTimeSampleSeries` and the sparse-array-edits
//! proposal's "Composing and Evaluating Time-Varying Sparse Opinions". No USD
//! sampling rules are built in: hosts choose time keys, equivalence during merge,
//! sample discovery, value classification, and interpolation. In particular, a
//! default can be represented by a host-defined earliest time key.

use alloc::{vec, vec::Vec};
use core::fmt;

/// Which sample of a participating source a recipe selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplePick {
    /// The source's lower sample (also used for a single sample).
    Lower,
    /// The source's upper sample.
    Upper,
}

/// A host sample's time and whether it needs weaker contributions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemporalSample<T> {
    /// Time in the common domain of this query, after any host time mapping.
    pub time: T,
    /// `true` for sparse contributions; `false` for dense values, blocks, or
    /// other host-defined contributions that stop the fold.
    pub composes: bool,
}

/// How many composed brackets the caller needs to materialize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporalMode {
    /// Stop once the lower bracket no longer needs weaker contributions.
    Held,
    /// Resolve both brackets so the host can interpolate between their values.
    Bracketing,
}

/// A recipe for one sample of the composed series.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalSelection<T> {
    /// Time of this composed sample.
    pub time: T,
    /// Whether this sample still needs weaker contributions (or a host seed).
    pub composes: bool,
    /// One pick per accepted source, strongest first. To materialize this
    /// sample, fold the picked values with the host's ordinary family rules.
    /// A dense value or block stops that fold; later picks may be irrelevant.
    pub picks: Vec<SamplePick>,
}

/// Incremental planner for one query over an already ordered source chain.
///
/// Ask [`Self::query`] before reading each weaker source. If that source has no
/// value, skip it without calling [`Self::push`]. Retain accepted source brackets
/// alongside the planner: selection index `i` identifies the `i`th accepted
/// source, not necessarily the `i`th source visited.
///
/// A terminal lower bracket can move the requested source time to the composed
/// upper bracket. Once the needed brackets are terminal, `query()` returns
/// `None`, so hidden weaker sources need not be read at all. If the sources run
/// out first, materialize remaining sparse recipes over the host's fallback.
///
/// Time keys must have a consistent ordering. Brackets must be ordered, with
/// identical times denoting one sample. A non-reflexive query such as floating
/// point NaN has no meaningful interpolation result; it does not cause a panic.
///
/// `equivalent` decides when samples from different sources count as the same
/// key, and which sample is held when they merge. It is fixed for the whole
/// query. It may use a tolerance and need not be transitive. Bracketing the
/// merged series at the query time uses exact ordering instead.
///
/// ```
/// use opinionated::{TemporalMode, TemporalPlanner, TemporalSample};
/// let mut plan = TemporalPlanner::new(5_u64, TemporalMode::Bracketing, |a, b| a == b);
/// assert_eq!(plan.query(), Some(5));
/// plan.push(
///     TemporalSample { time: 0, composes: false },
///     TemporalSample { time: 10, composes: true },
/// );
/// assert_eq!(plan.query(), Some(10)); // only the upper bracket needs a base
/// plan.push(
///     TemporalSample { time: 10, composes: false },
///     TemporalSample { time: 10, composes: false },
/// );
/// assert_eq!(plan.query(), None);
/// assert_eq!(plan.samples().len(), 2);
/// ```
#[derive(Clone)]
pub struct TemporalPlanner<T, E> {
    time: T,
    query: Option<T>,
    mode: TemporalMode,
    equivalent: E,
    samples: Vec<TemporalSelection<T>>,
}

impl<T: fmt::Debug, E> fmt::Debug for TemporalPlanner<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TemporalPlanner")
            .field("time", &self.time)
            .field("query", &self.query)
            .field("mode", &self.mode)
            .field("samples", &self.samples)
            .finish_non_exhaustive()
    }
}

impl<T: Copy + PartialOrd, E: Fn(T, T) -> bool> TemporalPlanner<T, E> {
    /// Starts an empty plan at `time`, merging keys with `equivalent`.
    #[must_use]
    pub fn new(time: T, mode: TemporalMode, equivalent: E) -> Self {
        Self {
            time,
            query: Some(time),
            mode,
            equivalent,
            samples: Vec::new(),
        }
    }

    /// Time at which to sample the next weaker source, or `None` when done.
    #[must_use]
    pub fn query(&self) -> Option<T> {
        self.query
    }

    /// Current composed brackets, in time order, at most two.
    ///
    /// An empty slice means no source has contributed yet, or merging near
    /// equivalent times removed all samples. The host can distinguish those
    /// cases by tracking accepted sources; the latter also ends the query.
    #[must_use]
    pub fn samples(&self) -> &[TemporalSelection<T>] {
        &self.samples
    }

    /// Takes the recipes without cloning their source selections.
    #[must_use]
    pub fn into_samples(self) -> Vec<TemporalSelection<T>> {
        self.samples
    }

    /// Composes a weaker source's brackets into the plan. Returns `false` and
    /// leaves the plan unchanged if it was already complete.
    ///
    /// Supply the same sample twice for a single sample. With equal times the
    /// lower sample wins. Samples contain no values and require no allocation.
    pub fn push(&mut self, lower: TemporalSample<T>, upper: TemporalSample<T>) -> bool {
        if self.query.is_none() {
            return false;
        }
        let weak = [lower, upper];
        let weak = &weak[..if lower.time == upper.time { 1 } else { 2 }];
        let pick = |i| {
            if i == 0 {
                SamplePick::Lower
            } else {
                SamplePick::Upper
            }
        };
        if self.samples.is_empty() {
            self.samples = weak
                .iter()
                .enumerate()
                .map(|(i, s)| TemporalSelection {
                    time: s.time,
                    composes: s.composes,
                    picks: vec![pick(i)],
                })
                .collect();
        } else {
            let strong = &self.samples;
            let equivalent = &self.equivalent;
            // Merge only metadata first: at most four candidates exist, and
            // only two can survive bracketing. Do not copy recipes discarded
            // by that selection, or copy recipes that can be moved intact.
            let mut merged = [(0, lower.time, false, SamplePick::Lower); 4];
            let mut len = 0;
            let (mut i, mut j) = (0, 0);
            while i < strong.len() || j < weak.len() {
                let s = strong.get(i);
                let w = weak.get(j);
                // Explicit exhaustion avoids treating a real maximum time
                // (including +infinity) as an end-of-series sentinel.
                if let Some(s) = s.filter(|s| w.is_none_or(|w| s.time <= w.time)) {
                    let held = held_index(weak.len(), j, s.time, |k| weak[k].time, equivalent);
                    merged[len] = (i, s.time, s.composes && weak[held].composes, pick(held));
                    len += 1;
                } else if let Some(w) = w {
                    let held = held_index(strong.len(), i, w.time, |k| strong[k].time, equivalent);
                    if strong[held].composes {
                        merged[len] = (held, w.time, w.composes, pick(j));
                        len += 1;
                    }
                }
                match (s, w) {
                    (None, _) => j += 1,
                    (_, None) => i += 1,
                    (Some(s), Some(w)) if equivalent(s.time, w.time) => {
                        i += 1;
                        j += 1;
                    }
                    (Some(s), Some(w)) if s.time < w.time => i += 1,
                    _ => j += 1,
                }
            }
            let (from, to) = bracket_range(len, self.time, |i| merged[i].1);
            let mut old = [None, None];
            for (slot, sample) in old.iter_mut().zip(self.samples.drain(..)) {
                *slot = Some(sample);
            }
            for k in from..to {
                let (source, time, composes, pick) = merged[k];
                let previous = old[source].as_mut().expect("candidate source exists");
                let reused = merged[k + 1..to].iter().any(|entry| entry.0 == source);
                let mut picks = if reused {
                    let mut fork = Vec::with_capacity(previous.picks.len() + 1);
                    fork.extend_from_slice(&previous.picks);
                    fork
                } else {
                    core::mem::take(&mut previous.picks)
                };
                picks.push(pick);
                self.samples.push(TemporalSelection {
                    time,
                    composes,
                    picks,
                });
            }
        }
        trim(&mut self.samples, self.time);
        match (self.samples.first(), self.samples.last()) {
            (Some(lower), Some(upper)) if !lower.composes => {
                self.query = if !upper.composes || self.mode == TemporalMode::Held {
                    None
                } else {
                    Some(upper.time)
                };
            }
            (None, _) => self.query = None,
            _ => {}
        }
        true
    }
}

fn held_index<T: Copy>(
    len: usize,
    next: usize,
    time: T,
    at: impl Fn(usize) -> T,
    equivalent: &impl Fn(T, T) -> bool,
) -> usize {
    if next == len || (next != 0 && !equivalent(at(next), time)) {
        next - 1
    } else {
        next
    }
}

fn bracket_range<T: Copy + PartialOrd>(
    len: usize,
    time: T,
    at: impl Fn(usize) -> T,
) -> (usize, usize) {
    let Some(last) = len.checked_sub(1) else {
        return (0, 0);
    };
    let (from, to) = if last == 0 || time <= at(0) {
        (0, 0)
    } else if time >= at(last) {
        (last, last)
    } else {
        // There are at most four merged candidates.
        let next = (0..len)
            .take_while(|&i| at(i) < time)
            .count()
            .clamp(1, last);
        if at(next) == time {
            (next, next)
        } else {
            (next - 1, next)
        }
    };
    (from, to + 1)
}

fn trim<T: Copy + PartialOrd>(samples: &mut Vec<TemporalSelection<T>>, time: T) {
    let (from, to) = bracket_range(samples.len(), time, |i| samples[i].time);
    samples.truncate(to);
    samples.drain(..from);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, FamilyMember, OpinionFamily,
        resolve_family_chain,
    };
    use core::cell::Cell;

    fn sample<T>(time: T, composes: bool) -> TemporalSample<T> {
        TemporalSample { time, composes }
    }

    #[test]
    fn shared_recipe_forks_before_either_branch_is_extended() {
        let mut plan = TemporalPlanner::new(5, TemporalMode::Bracketing, |a, b| a == b);
        plan.push(sample(0, true), sample(10, true));
        plan.push(sample(3, true), sample(7, true));
        assert_eq!(plan.samples()[0].time, 3);
        assert_eq!(plan.samples()[1].time, 7);
        assert_eq!(
            plan.samples()[0].picks,
            [SamplePick::Lower, SamplePick::Lower]
        );
        assert_eq!(
            plan.samples()[1].picks,
            [SamplePick::Lower, SamplePick::Upper]
        );
        plan.push(sample(3, false), sample(7, false));
        assert_eq!(plan.query(), None);
        assert_eq!(plan.samples()[0].picks, [SamplePick::Lower; 3]);
        assert_eq!(
            plan.samples()[1].picks,
            [SamplePick::Lower, SamplePick::Upper, SamplePick::Upper]
        );
    }

    #[test]
    fn aligned_sources_reuse_recipe_storage() {
        let mut plan = TemporalPlanner::new(5, TemporalMode::Bracketing, |a, b| a == b);
        plan.push(sample(0, true), sample(10, true));
        for selection in &mut plan.samples {
            selection.picks.reserve(100);
        }
        let buffers = [
            plan.samples[0].picks.as_ptr(),
            plan.samples[1].picks.as_ptr(),
        ];
        for _ in 0..99 {
            plan.push(sample(0, true), sample(10, true));
            assert_eq!(plan.samples[0].picks.as_ptr(), buffers[0]);
            assert_eq!(plan.samples[1].picks.as_ptr(), buffers[1]);
        }
        assert_eq!(plan.samples[0].picks, vec![SamplePick::Lower; 100]);
        assert_eq!(plan.samples[1].picks, vec![SamplePick::Upper; 100]);
    }

    #[test]
    fn exact_keys_endpoints_and_held_cutoff() {
        for (time, expected) in [
            (-5, vec![0]),
            (0, vec![0]),
            (5, vec![0, 10]),
            (10, vec![10]),
            (20, vec![10]),
        ] {
            let mut plan = TemporalPlanner::new(time, TemporalMode::Bracketing, |a, b| a == b);
            plan.push(sample(0, false), sample(10, false));
            assert_eq!(
                plan.samples().iter().map(|s| s.time).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(plan.query(), None);
        }
        let mut held = TemporalPlanner::new(5, TemporalMode::Held, |a, b| a == b);
        held.push(sample(0, false), sample(10, true));
        assert_eq!(held.query(), None);
        let before = held.samples().to_vec();
        assert!(!held.push(sample(5, false), sample(5, false)));
        assert_eq!(held.samples(), before);
    }

    #[test]
    fn only_upper_needs_a_weaker_source_and_its_lower_sample_must_continue() {
        let mut plan = TemporalPlanner::new(5, TemporalMode::Bracketing, |a, b| a == b);
        plan.push(sample(0, false), sample(10, true));
        assert_eq!(plan.query(), Some(10));
        // The weak upper is terminal, but its lower contributes at time 10.
        // Stopping on the weak upper here would lose the eventual base.
        plan.push(sample(8, true), sample(12, false));
        assert_eq!(plan.query(), Some(10));
        plan.push(sample(10, false), sample(10, false));
        assert_eq!(plan.query(), None);
        assert_eq!(
            plan.samples()[1].picks,
            [SamplePick::Upper, SamplePick::Lower, SamplePick::Lower]
        );
    }

    #[test]
    fn sparse_chain_runs_out_and_keeps_recipes_for_the_host_seed() {
        let mut plan = TemporalPlanner::new(5, TemporalMode::Bracketing, |a, b| a == b);
        plan.push(sample(0, true), sample(10, true));
        plan.push(sample(3, true), sample(7, true));
        assert_eq!(plan.query(), Some(5));
        assert_eq!(plan.samples()[0].time, 3);
        assert_eq!(plan.samples()[1].time, 7);
        assert_eq!(
            plan.samples()[0].picks,
            [SamplePick::Lower, SamplePick::Lower]
        );
        assert_eq!(
            plan.samples()[1].picks,
            [SamplePick::Lower, SamplePick::Upper]
        );
        assert!(plan.samples().iter().all(|s| s.composes));
    }

    #[test]
    fn host_tolerance_preserves_near_collision_rules() {
        let close = |a: f64, b: f64| a == b || (a - b).abs() < 1e-6;
        let mut plan = TemporalPlanner::new(1.5, TemporalMode::Bracketing, close);
        plan.push(sample(1.000_000_4, false), sample(2.0, true));
        plan.push(sample(1.0, false), sample(2.0, false));
        assert_eq!(plan.samples().len(), 1);
        assert_eq!(plan.samples()[0].time, 2.0);
        assert_eq!(
            plan.samples()[0].picks,
            [SamplePick::Upper, SamplePick::Upper]
        );
        assert_eq!(plan.query(), None);
    }

    #[test]
    fn maximum_keys_and_infinities_are_samples_not_sentinels() {
        let mut ticks = TemporalPlanner::new(u64::MAX - 1, TemporalMode::Bracketing, |a, b| a == b);
        ticks.push(sample(0, true), sample(u64::MAX, true));
        ticks.push(sample(0, false), sample(0, false));
        assert_eq!(ticks.samples()[1].time, u64::MAX);
        assert_eq!(ticks.query(), None);
        let mut exhausted = TemporalPlanner::new(5.0, TemporalMode::Bracketing, |a, b| a == b);
        exhausted.push(sample(0.0, true), sample(0.0, true));
        exhausted.push(sample(0.0, true), sample(f64::INFINITY, false));
        assert_eq!(exhausted.samples()[1].time, f64::INFINITY);
        for time in [0.0, f64::INFINITY, f64::NAN] {
            let mut plan = TemporalPlanner::new(time, TemporalMode::Bracketing, |a, b| a == b);
            plan.push(sample(f64::NEG_INFINITY, true), sample(f64::INFINITY, true));
            plan.push(sample(0.0, false), sample(0.0, false));
            assert_eq!(
                plan.samples().last().unwrap().time,
                if time == f64::INFINITY {
                    f64::INFINITY
                } else {
                    0.0
                }
            );
            assert_eq!(plan.query(), None);
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Point {
        x: i32,
        y: i32,
    }

    type Value = FamilyMember<Vec<Point>, ArrayEdit<Point>>;
    struct Points<'a>(&'a Cell<usize>);
    impl OpinionFamily<Value> for Points<'_> {
        type Value = Vec<Point>;
        type Edit<'op> = ArrayEdit<Point>;
        fn classify(&self, value: &Value) -> Value {
            self.0.set(self.0.get() + 1);
            value.clone()
        }
        fn apply<'op>(&self, edit: Self::Edit<'op>, mut base: Self::Value) -> Self::Value
        where
            Value: 'op,
        {
            edit.apply_in_place(&mut base, None);
            base
        }
        fn seed(&self) -> Self::Value {
            vec![Point { x: 0, y: 0 }]
        }
    }

    // A non-USD host: animation ticks, typed points, and typed sparse corrections.
    // The planner selects samples; the existing family fold materializes them.
    #[test]
    fn typed_animation_uses_existing_array_and_family_kernels() {
        let dense = |x| Value::Dense(vec![Point { x, y: 0 }, Point { x: x + 1, y: 2 }]);
        let correction = |x| {
            Value::Sparse(ArrayEdit {
                ops: vec![ArrayEditOp::Write {
                    index: ArrayIndex::Position(0),
                    src: ArrayEditOperand::Literal(Point { x, y: 4 }),
                }],
            })
        };
        let sources = [(correction(10), correction(30)), (dense(0), dense(20))];
        let mut plan = TemporalPlanner::new(5_u64, TemporalMode::Bracketing, |a, b| a == b);
        let read = Cell::new(0);
        let materialized = Cell::new(0);
        let mut inputs = sources
            .iter()
            .chain(core::iter::once_with(|| panic!("hidden source read")));
        while plan.query().is_some() {
            let (lower, upper) = inputs.next().unwrap();
            read.set(read.get() + 1);
            plan.push(
                sample(0, matches!(lower, Value::Sparse(_))),
                sample(10, matches!(upper, Value::Sparse(_))),
            );
        }
        assert_eq!(read.get(), 2);
        assert_eq!(materialized.get(), 0, "planning never interprets values");
        let materialize = |recipe: &TemporalSelection<u64>| {
            let selected: Vec<_> = recipe
                .picks
                .iter()
                .enumerate()
                .map(|(i, pick)| {
                    let value = match pick {
                        SamplePick::Lower => &sources[i].0,
                        SamplePick::Upper => &sources[i].1,
                    };
                    ((i, *pick), value)
                })
                .collect();
            resolve_family_chain(
                &Points(&materialized),
                selected.iter().map(|(source, value)| (*value, source)),
            )
            .resolved()
            .unwrap()
        };
        let (lower, lower_source) = materialize(&plan.samples()[0]);
        let (upper, upper_source) = materialize(&plan.samples()[1]);
        assert_eq!(lower, [Point { x: 10, y: 4 }, Point { x: 1, y: 2 }]);
        assert_eq!(upper, [Point { x: 30, y: 4 }, Point { x: 21, y: 2 }]);
        assert_eq!(lower_source, (0, SamplePick::Lower));
        assert_eq!(upper_source, (0, SamplePick::Upper));
        let midpoint: Vec<_> = lower
            .iter()
            .zip(&upper)
            .map(|(a, b)| Point {
                x: (a.x + b.x) / 2,
                y: (a.y + b.y) / 2,
            })
            .collect();
        assert_eq!(midpoint, [Point { x: 20, y: 4 }, Point { x: 11, y: 2 }]);
        assert_eq!(materialized.get(), 4);
    }

    #[test]
    fn block_cuts_off_materialization_and_sparse_edits_use_the_seed() {
        let sources = [
            Value::Sparse(ArrayEdit {
                ops: vec![ArrayEditOp::Write {
                    index: ArrayIndex::Position(0),
                    src: ArrayEditOperand::Literal(Point { x: 7, y: 8 }),
                }],
            }),
            Value::Block,
        ];
        let mut plan = TemporalPlanner::new(4, TemporalMode::Bracketing, |a, b| a == b);
        plan.push(sample(0, true), sample(0, true));
        plan.push(sample(0, false), sample(0, false));
        assert_eq!(plan.query(), None);
        let count = Cell::new(0);
        let result = resolve_family_chain(
            &Points(&count),
            sources
                .iter()
                .zip([0, 1].iter())
                .chain(core::iter::once_with(|| {
                    panic!("hidden value materialized")
                })),
        );
        assert_eq!(result.resolved().unwrap().0, [Point { x: 7, y: 8 }]);
        assert_eq!(count.get(), 2);
    }
}
