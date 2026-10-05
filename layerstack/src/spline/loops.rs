// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Virtual inner-loop knots and explicitly bounded baking (AOUSD Core §12.5).
//! OpenUSD `Ts_SplineData::HasInnerLoops` and `ts/eval.cpp::_LoopResolver`.
//!
//! Queries describe the echoed curve, including its one-sided values and
//! derivatives. OpenUSD 26.8 sometimes consults shadowed raw knots for exact
//! held incoming values or derivatives; these utilities use the effective
//! curve consistently, matching evaluation after baking.
use super::{Knot, SplineData, SplineQueryError, queries};
use alloc::vec::Vec;
use core::ops::Range;

struct InnerLoops {
    prototype: Range<usize>,
    before: Range<usize>,
    after: Range<usize>,
    span: f64,
    start: f64,
    pre: i64,
    post: i64,
    offset: f64,
}
impl SplineData {
    fn inner_loops(&self) -> Result<Option<InnerLoops>, SplineQueryError> {
        let Some(lp) = self.loop_params else {
            return Ok(None);
        };
        if ![lp.proto_start, lp.proto_end, lp.value_offset]
            .into_iter()
            .all(f64::is_finite)
        {
            return Err(SplineQueryError::InvalidInput);
        }
        let pre = i64::from(lp.num_pre_loops.max(0));
        let post = i64::from(lp.num_post_loops.max(0));
        // Native authoring clamps negative counts; missing start knots and
        // zero-count/empty prototypes disable inner looping.
        if lp.proto_end <= lp.proto_start || pre + post == 0 {
            return Ok(None);
        }
        let Ok(first) = self
            .knots
            .binary_search_by(|k| k.time.total_cmp(&lp.proto_start))
        else {
            return Ok(None);
        };
        let span = lp.proto_end - lp.proto_start;
        let low = lp.proto_start - pre as f64 * span;
        let high = lp.proto_end + post as f64 * span;
        if !span.is_finite() || !low.is_finite() || !high.is_finite() {
            return Err(SplineQueryError::InvalidInput);
        }
        if low + span == low || high - span == high {
            return Err(SplineQueryError::UnsupportedLoopRange);
        }
        if self.pre_loop_boundary.is_some() || self.post_loop_boundary.is_some() {
            return Err(SplineQueryError::InvalidInput);
        }
        Ok(Some(InnerLoops {
            prototype: first..self.knots.partition_point(|k| k.time < lp.proto_end),
            before: 0..self.knots.partition_point(|k| k.time < low),
            after: self.knots.partition_point(|k| k.time <= high)..self.knots.len(),
            span,
            start: lp.proto_start,
            pre,
            post,
            offset: lp.value_offset,
        }))
    }
    pub(super) fn active_inner_loops(&self) -> Result<bool, SplineQueryError> {
        Ok(self.inner_loops()?.is_some())
    }
    fn echo(&self, loops: &InnerLoops, index: usize, cycle: i64) -> Result<Knot, SplineQueryError> {
        let mut knot = self.knots[index].clone();
        knot.time += cycle as f64 * loops.span;
        knot.value += cycle as f64 * loops.offset;
        knot.pre_value = knot.pre_value.map(|v| v + cycle as f64 * loops.offset);
        if !knot.time.is_finite()
            || !knot.value.is_finite()
            || knot.pre_value.is_some_and(|v| !v.is_finite())
        {
            return Err(SplineQueryError::InvalidInput);
        }
        Ok(knot)
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "floor result is clamped to exact signed 32-bit authored loop counts"
    )]
    fn window(&self, loops: &InnerLoops, time: f64) -> Result<Self, SplineQueryError> {
        let first = if loops.before.is_empty() {
            self.echo(loops, loops.prototype.start, -loops.pre)?.time
        } else {
            self.knots[0].time
        };
        let last = if loops.after.is_empty() {
            self.echo(loops, loops.prototype.start, loops.post + 1)?
                .time
        } else {
            self.knots.last().expect("nonempty loop prototype").time
        };
        // Three local windows also include first/last adjacent knots, preserving
        // inward extrapolation slopes. Work/storage is independent of loop count.
        let mut knots: Vec<(Knot, (usize, i64))> = Vec::new();
        for query in [time, first, last] {
            for range in [&loops.before, &loops.after] {
                for index in neighbors(&self.knots, range, query) {
                    knots.push((self.knots[index].clone(), (index, i64::MIN)));
                }
            }
            let cycle = libm::floor((query - loops.start) / loops.span)
                .clamp(-loops.pre as f64, loops.post as f64) as i64;
            for cycle in [cycle.saturating_sub(1), cycle, cycle.saturating_add(1)] {
                if cycle < -loops.pre || cycle > loops.post {
                    continue;
                }
                let local_time = query - cycle as f64 * loops.span;
                for index in neighbors(&self.knots, &loops.prototype, local_time) {
                    knots.push((self.echo(loops, index, cycle)?, (index, cycle)));
                }
            }
        }
        knots.push((
            self.echo(loops, loops.prototype.start, loops.post + 1)?,
            (loops.prototype.start, loops.post + 1),
        ));
        knots.sort_by(|a, b| a.0.time.total_cmp(&b.0.time));
        if knots
            .windows(2)
            .any(|pair| pair[0].0.time == pair[1].0.time && pair[0].1 != pair[1].1)
        {
            return Err(SplineQueryError::UnsupportedLoopRange);
        }
        knots.dedup_by(|a, b| a.0.time == b.0.time);
        Ok(Self {
            knots: knots.into_iter().map(|(k, _)| k).collect(),
            loop_params: None,
            pre_loop_boundary: self.pre_loop_boundary,
            post_loop_boundary: self.post_loop_boundary,
            data_type: self.data_type,
            default_curve_type: self.default_curve_type,
            pre_extrapolation: self.pre_extrapolation,
            post_extrapolation: self.post_extrapolation,
        })
    }
    pub(super) fn evaluate_inner_loops(
        &self,
        time: f64,
        pre: bool,
        derivative: bool,
    ) -> Result<Option<f64>, SplineQueryError> {
        queries::validate(self)?;
        if !time.is_finite() {
            return Err(SplineQueryError::InvalidInput);
        }
        let loops = self.inner_loops()?.expect("active inner loops");
        let edges = self.window(&loops, time)?;
        let Some(mapping) = edges.map_extrapolation(time, pre)? else {
            return Ok(None);
        };
        let window = self.window(&loops, mapping.time)?;
        if window.has_regressive_tangents()? {
            return Err(SplineQueryError::RegressiveTangents);
        }
        let value = if derivative {
            if mapping.held {
                Some(0.)
            } else {
                window
                    .derivative_unlooped(mapping.time, mapping.pre)?
                    .map(|v| v * mapping.sign)
            }
        } else {
            window.evaluate_mapped(mapping)
        };
        if value.is_some_and(|v| !v.is_finite()) {
            return Err(SplineQueryError::InvalidInput);
        }
        Ok(value)
    }
    /// Replaces finite inner-loop echoes with ordinary authored knots.
    ///
    /// `max_knots` bounds the complete resulting knot count before allocation.
    /// Failure leaves the spline unchanged. Echoes replace shadowed knots;
    /// prototype-start copies carry value offsets and both boundary sides.
    /// Extrapolation loops remain authored. Evaluation uses bounded virtual
    /// windows and does not require this materialization step. Both sides of
    /// the first echoed knot receive the cycle offset, preserving runtime
    /// evaluation even when the prototype start is dual-valued. OpenUSD 26.8's
    /// baking utility leaves that first incoming value unshifted.
    /// AOUSD Core §12.5; OpenUSD `TsSpline::BakeInnerLoops`.
    pub fn bake_inner_loops(&mut self, max_knots: usize) -> Result<usize, SplineQueryError> {
        queries::validate(self)?;
        let Some(loops) = self.inner_loops()? else {
            if self.knots.len() > max_knots {
                return Err(SplineQueryError::SampleBudgetExceeded);
            }
            self.loop_params = None;
            return Ok(self.knots.len());
        };
        let cycles = usize::try_from(loops.pre + loops.post + 1)
            .map_err(|_| SplineQueryError::SampleBudgetExceeded)?;
        let count = loops
            .prototype
            .len()
            .checked_mul(cycles)
            .and_then(|n| n.checked_add(loops.before.len()))
            .and_then(|n| n.checked_add(loops.after.len()))
            .and_then(|n| n.checked_add(1))
            .filter(|&n| n <= max_knots)
            .ok_or(SplineQueryError::SampleBudgetExceeded)?;
        let mut knots = Vec::with_capacity(count);
        knots.extend_from_slice(&self.knots[loops.before.clone()]);
        for cycle in -loops.pre..=loops.post {
            for index in loops.prototype.clone() {
                knots.push(self.echo(&loops, index, cycle)?);
            }
        }
        knots.push(self.echo(&loops, loops.prototype.start, loops.post + 1)?);
        knots.extend_from_slice(&self.knots[loops.after.clone()]);
        if knots.windows(2).any(|pair| pair[0].time >= pair[1].time) {
            return Err(SplineQueryError::UnsupportedLoopRange);
        }
        self.knots = knots;
        self.loop_params = None;
        Ok(count)
    }
}

fn neighbors(knots: &[Knot], range: &Range<usize>, time: f64) -> Range<usize> {
    let split = knots[range.clone()].partition_point(|k| k.time <= time) + range.start;
    split.saturating_sub(2).max(range.start)..split.saturating_add(2).min(range.end)
}
