// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plans sparse temporal composition without inspecting or cloning host values.
//! User-facing docs live on [`TemporalPlanner`], since this module is private.

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

/// One key from a layer, as the planner sees it: its time, and whether it is
/// an edit that needs a value from a weaker layer.
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

/// Works out which keyframes from each layer make up an animated value at one
/// time.
///
/// Use this when layers hold keyframes rather than single values, and some
/// keys are edits (an offset, an [`ArrayEdit`](crate::ArrayEdit)) rather than
/// whole values. Resolve the layers first and interpolate afterwards: an
/// edit has no value of its own to interpolate, and each layer's keys can sit
/// at different times. The planner lines the layers up so that each
/// interpolation endpoint can be resolved with
/// [`resolve_family_chain`](crate::resolve_family_chain).
///
/// The planner never sees your values, only key times and whether each key
/// is an edit. You keep the keys, decide how times compare, and interpolate.
///
/// # Using it
///
/// 1. Create a planner for the time you want.
/// 2. Walk your layers strongest first. Before reading each one, ask
///    [`query`](Self::query) which time to read it at; stop when it says
///    `None`, because nothing weaker can affect the result. Find the keys on
///    either side of that time and [`push`](Self::push) them as
///    [`TemporalSample`]s. Skip layers with no keys without pushing, and
///    remember which layers you pushed.
/// 3. Read [`samples`](Self::samples): at most two [`TemporalSelection`]s,
///    in time order. Each one's `picks` says which of the two keys to take
///    from each layer you pushed, in order. Resolve those keys with your family:
///    - No selections: there is no composed sample; use your host's absence
///      policy. [`samples`](Self::samples) explains when this can happen.
///    - One selection: use its resolved value directly, including at an exact
///      key time or outside the keyed interval. There is nothing to interpolate.
///    - Two selections: in [`TemporalMode::Held`], resolve only the lower one.
///      In [`TemporalMode::Bracketing`], resolve both and interpolate their values.
///
/// A selected block can resolve to [`FamilyResolution::Blocked`](crate::FamilyResolution::Blocked).
/// The host decides how to represent that missing value rather than treating
/// it as an interpolation endpoint. The example below returns `None` for
/// absence or a blocked endpoint.
///
/// If the layers run out while a selection still `composes`, its edits
/// apply to your fallback, as the family's [`seed`](crate::OpinionFamily::seed).
///
/// ```
/// use opinionated::{
///     FamilyMember, OpinionFamily, SamplePick, TemporalMode, TemporalPlanner, TemporalSample,
///     TemporalSelection, resolve_family_chain,
/// };
///
/// // A keyframe holds a height, or an offset added to whatever is below it.
/// enum Key {
///     Height(f32),
///     Offset(f32),
///     Block,
/// }
///
/// struct Heights;
///
/// impl OpinionFamily<Key> for Heights {
///     type Value = f32;
///     type Edit<'op> = f32;
///     fn classify<'op>(&self, key: &'op Key) -> FamilyMember<f32, f32> {
///         match *key {
///             Key::Height(height) => FamilyMember::Dense(height),
///             Key::Offset(offset) => FamilyMember::Sparse(offset),
///             Key::Block => FamilyMember::Block,
///         }
///     }
///     fn apply<'op>(&self, offset: f32, base: f32) -> f32
///     where
///         Key: 'op,
///     {
///         base + offset
///     }
///     fn seed(&self) -> f32 {
///         0.0
///     }
/// }
///
/// // The keys on either side of `frame`, or one key twice at either end.
/// // The caller skips empty layers before calling this helper.
/// fn bracket(keys: &[(u32, Key)], frame: u32) -> (usize, usize) {
///     let upper = keys
///         .iter()
///         .position(|(time, _)| *time >= frame)
///         .unwrap_or(keys.len() - 1);
///     let lower = if keys[upper].0 > frame && upper > 0 { upper - 1 } else { upper };
///     (lower, upper)
/// }
///
/// fn height_at(layers: &[&[(u32, Key)]], frame: u32, mode: TemporalMode) -> Option<f32> {
///     let mut plan = TemporalPlanner::new(frame, mode, |a, b| a == b);
///     let mut pushed = Vec::new(); // (layer, lower key, upper key)
///     for (layer, keys) in layers.iter().enumerate() {
///         let Some(query) = plan.query() else { break };
///         if keys.is_empty() {
///             continue;
///         }
///         let (lower, upper) = bracket(keys, query);
///         let sample = |i: usize| TemporalSample {
///             time: keys[i].0,
///             composes: matches!(keys[i].1, Key::Offset(_)),
///         };
///         plan.push(sample(lower), sample(upper));
///         pushed.push((layer, lower, upper));
///     }
///
///     let resolve = |selection: &TemporalSelection<u32>| {
///         let chain = selection.picks.iter().zip(&pushed).map(|(pick, entry)| {
///             let (layer, lower, upper) = *entry;
///             let key = match pick {
///                 SamplePick::Lower => lower,
///                 SamplePick::Upper => upper,
///             };
///             (&layers[layer][key].1, &entry.0)
///         });
///         resolve_family_chain(&Heights, chain).resolved().map(|(height, _)| height)
///     };
///
///     match plan.samples() {
///         [] => None,
///         [only] => resolve(only),
///         [lower, upper] => {
///             let h0 = resolve(lower)?;
///             if mode == TemporalMode::Held {
///                 return Some(h0);
///             }
///             let h1 = resolve(upper)?;
///             let s = (frame - lower.time) as f32 / (upper.time - lower.time) as f32;
///             Some(h0 + (h1 - h0) * s)
///         }
///         _ => unreachable!("the planner returns at most two selections"),
///     }
/// }
///
/// // Each layer's keys by frame, strongest layer first. Empty layers are skipped.
/// let layers: [&[(u32, Key)]; 3] = [
///     &[],
///     &[(0, Key::Offset(1.0)), (10, Key::Offset(3.0))],
///     &[(0, Key::Height(0.0)), (10, Key::Height(10.0))],
/// ];
/// assert_eq!(height_at(&layers, 5, TemporalMode::Bracketing), Some(7.0));
/// assert_eq!(height_at(&layers, 5, TemporalMode::Held), Some(1.0));
/// assert_eq!(height_at(&layers, 0, TemporalMode::Bracketing), Some(1.0));
/// assert_eq!(height_at(&layers, 20, TemporalMode::Bracketing), Some(13.0));
/// let single: [&[(u32, Key)]; 1] = [&[(4, Key::Height(9.0))]];
/// assert_eq!(height_at(&single, 0, TemporalMode::Bracketing), Some(9.0));
/// assert_eq!(height_at(&[], 5, TemporalMode::Bracketing), None);
/// assert_eq!(height_at(&[&[]], 5, TemporalMode::Bracketing), None);
/// // Sparse edits without a dense base use Heights::seed().
/// assert_eq!(height_at(&layers[..2], 5, TemporalMode::Bracketing), Some(2.0));
/// // This host treats a blocked endpoint as no height.
/// let blocked: [&[(u32, Key)]; 1] = [&[(0, Key::Height(1.0)), (10, Key::Block)]];
/// assert_eq!(height_at(&blocked, 5, TemporalMode::Bracketing), None);
/// assert_eq!(height_at(&blocked, 5, TemporalMode::Held), Some(1.0));
/// ```
///
/// # Rules
///
/// A layer's keys must be in time order. Pushing the same key as both lower
/// and upper means a single key. Times only need [`PartialOrd`]; a query at a
/// NaN time gives no meaningful result but does not panic.
///
/// `equivalent` decides when keys from different layers count as the same
/// time, and which key is kept when they merge. It may use a tolerance and
/// need not be transitive. Choosing the endpoints around the query time uses
/// exact ordering.
///
/// The merge follows OpenUSD's `SdfComposeTimeSampleSeries` and the
/// sparse-array-edits proposal ("Composing and Evaluating Time-Varying Sparse
/// Opinions"), without USD's time tolerance or sampling rules.
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
