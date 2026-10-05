// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit, bounded spline queries and tangent containment.
use super::{CurveType, Extrapolation, Knot, KnotInterp, SplineData, cubic_bezier_deriv};
use alloc::{vec, vec::Vec};

/// Invalid input or a spline feature outside these bounded utilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplineQueryError {
    /// Nonfinite input, unordered knots, negative tangent widths or bad tolerance.
    InvalidInput,
    /// Adaptive sampling and change intervals require loops to be baked.
    UnsupportedLoops,
    /// The query spans 2^53 or more extrapolation periods; adjacent cycle
    /// indices and their boundary sides cannot be distinguished in `f64`.
    /// Also returned when finite inner-loop echo times alias in `f64`.
    UnsupportedLoopRange,
    /// A Bézier time curve reverses direction, making inversion ambiguous.
    RegressiveTangents,
    /// The time derivative vanishes at the query, so the slope ratio is singular.
    SingularDerivative,
    /// The requested approximation exceeds its explicit vertex/work budget.
    SampleBudgetExceeded,
}
impl core::fmt::Display for SplineQueryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid spline query: {self:?}")
    }
}
impl core::error::Error for SplineQueryError {}
/// One vertex of a piecewise-linear spline approximation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplineSample {
    /// Time in the spline's coordinate system.
    pub time: f64,
    /// Value in the spline's units.
    pub value: f64,
}
/// A continuous part of a sampled spline. Separate polylines preserve jumps and
/// blocked regions; consumers must not connect the end of one to the next.
#[derive(Clone, Debug, PartialEq)]
pub struct SplinePolyline {
    /// Ordered vertices, including both endpoints of a nonempty segment.
    pub samples: Vec<SplineSample>,
}
/// Conservative interval in which two unlooped spline descriptions may differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplineChangeInterval {
    /// Inclusive start, possibly negative infinity for pre-extrapolation edits.
    pub start: f64,
    /// Inclusive end, possibly positive infinity for post-extrapolation edits.
    pub end: f64,
}
fn looped(spline: &SplineData) -> bool {
    spline.active_inner_loops().unwrap_or(true)
        || [spline.pre_extrapolation, spline.post_extrapolation]
            .iter()
            .any(|v| {
                matches!(
                    v,
                    Extrapolation::LoopRepeat
                        | Extrapolation::LoopReset
                        | Extrapolation::LoopOscillate
                )
            })
}
pub(super) fn validate(spline: &SplineData) -> Result<(), SplineQueryError> {
    spline.active_inner_loops()?;
    if spline
        .knots
        .windows(2)
        .any(|p| p[0].time >= p[1].time || !(p[1].time - p[0].time).is_finite())
        || spline.knots.iter().any(|k| {
            [
                k.time,
                k.value,
                k.pre_tan_width,
                k.post_tan_width,
                k.pre_tan_slope,
                k.post_tan_slope,
            ]
            .into_iter()
            .any(|v| !v.is_finite())
                || k.pre_value.is_some_and(|v| !v.is_finite())
                || k.pre_tan_width < 0.
                || k.post_tan_width < 0.
        })
        || [spline.pre_extrapolation, spline.post_extrapolation]
            .iter()
            .any(|e| matches!(e,Extrapolation::Sloped(s) if !s.is_finite()))
    {
        return Err(SplineQueryError::InvalidInput);
    }
    for pair in spline.knots.windows(2) {
        if !slope(&pair[0], &pair[1]).is_finite()
            || controls(spline, &pair[0], &pair[1])
                .iter()
                .flatten()
                .any(|v| !v.is_finite())
        {
            return Err(SplineQueryError::InvalidInput);
        }
    }
    Ok(())
}
fn finite(value: Option<f64>) -> Result<Option<f64>, SplineQueryError> {
    if value.is_some_and(|v| !v.is_finite()) {
        Err(SplineQueryError::InvalidInput)
    } else {
        Ok(value)
    }
}
fn slope(a: &Knot, b: &Knot) -> f64 {
    (b.pre_value.unwrap_or(b.value) - a.value) / (b.time - a.time)
}
pub(super) fn regressive(a: &Knot, b: &Knot) -> bool {
    // Test the minimum of x'(u)/3 over [0,1]. AOUSD §12.5; Ts Bézier time curves.
    let span = b.time - a.time;
    // Scale all lengths together; width/span may overflow on tiny segments.
    let scale = span.max(a.post_tan_width).max(b.pre_tan_width);
    let p = a.post_tan_width / scale;
    let q = b.pre_tan_width / scale;
    let dt = span / scale;
    let aa = 3. * (p + q) - 2. * dt;
    let bb = 2. * dt - 4. * p - 2. * q;
    let at = if aa > 0. {
        (-bb / (2. * aa)).clamp(0., 1.)
    } else {
        0.
    };
    p < 0. || q < 0. || (aa * at + bb) * at + p < 0.
}
impl SplineData {
    /// Evaluates a finite time with explicit invalid-data and unsupported-feature
    /// errors. Both loop kinds use bounded virtual evaluation; regressive Bézier
    /// tangents require containment.
    /// AOUSD Core §12.3.3, §12.5.
    pub fn evaluate_checked(&self, time: f64) -> Result<Option<f64>, SplineQueryError> {
        if self.active_inner_loops()? {
            return self.evaluate_inner_loops(time, false, false);
        }
        validate(self)?;
        if !time.is_finite() {
            return Err(SplineQueryError::InvalidInput);
        }
        if self.has_regressive_tangents()? {
            return Err(SplineQueryError::RegressiveTangents);
        }
        let Some(mapping) = self.map_extrapolation(time, false)? else {
            return Ok(None);
        };
        finite(self.evaluate_mapped(mapping))
    }
    pub(super) fn linear_extrapolation_slope(&self, pre: bool) -> f64 {
        // OpenUSD ts/eval.cpp::_GetExtrapolationSlope; tangent faces inward.
        if self.knots.len() < 2 {
            return 0.;
        }
        let (a, b) = if pre {
            (&self.knots[0], &self.knots[1])
        } else {
            let n = self.knots.len();
            (&self.knots[n - 2], &self.knots[n - 1])
        };
        if (if pre { a } else { b }).pre_value.is_some() {
            return 0.;
        }
        match a.next_interp {
            KnotInterp::Block | KnotInterp::Held => 0.,
            KnotInterp::Linear => slope(a, b),
            KnotInterp::Curve => {
                if pre {
                    a.post_tan_slope
                } else {
                    b.pre_tan_slope
                }
            }
        }
    }
    fn extrapolation_slope(&self, pre: bool) -> Option<f64> {
        match if pre {
            self.pre_extrapolation
        } else {
            self.post_extrapolation
        } {
            Extrapolation::Block => None,
            Extrapolation::Held => Some(0.),
            Extrapolation::Linear => Some(self.linear_extrapolation_slope(pre)),
            Extrapolation::Sloped(v) => Some(v),
            _ => None,
        }
    }
    /// Evaluates the left-hand limit, including held segments and blocked
    /// regions. At a dual-valued knot this uses its pre-value unless the preceding
    /// segment is held (previous value) or blocked (no value). Extrapolation loops
    /// preserve boundary sides, including finite inner-loop echoes. Inner loops
    /// use the effective curve's left limit; OpenUSD 26.8 can instead consult
    /// shadowed knots at exact held boundaries. Invalid data errors.
    /// AOUSD Core §12.3.3, §12.5; OpenUSD `TsSpline::EvalPreValue`.
    pub fn evaluate_pre_value(&self, time: f64) -> Result<Option<f64>, SplineQueryError> {
        if self.active_inner_loops()? {
            return self.evaluate_inner_loops(time, true, false);
        }
        validate(self)?;
        if self.has_regressive_tangents()? {
            return Err(SplineQueryError::RegressiveTangents);
        }
        if !time.is_finite() {
            return Err(SplineQueryError::InvalidInput);
        }
        let Some(mapping) = self.map_extrapolation(time, true)? else {
            return Ok(None);
        };
        let result = if mapping.held {
            Some(if mapping.pre {
                self.knots[0].pre_value.unwrap_or(self.knots[0].value)
            } else {
                self.knots.last().expect("nonempty").value
            })
        } else if mapping.pre {
            self.pre_value_unlooped(mapping.time)
        } else {
            self.evaluate_unlooped(mapping.time)
        };
        finite(result.map(|v| v + mapping.offset))
    }
    pub(super) fn pre_value_unlooped(&self, time: f64) -> Option<f64> {
        match self
            .knots
            .binary_search_by(|k| k.time.partial_cmp(&time).expect("finite time"))
        {
            Ok(0) => self
                .extrapolation_slope(true)
                .map(|_| self.knots[0].pre_value.unwrap_or(self.knots[0].value)),
            Ok(i) => match self.knots[i - 1].next_interp {
                KnotInterp::Block => None,
                KnotInterp::Held => Some(self.knots[i - 1].value),
                _ => Some(self.knots[i].pre_value.unwrap_or(self.knots[i].value)),
            },
            Err(_) => self.evaluate_unlooped(time),
        }
    }
    /// Analytic right-hand derivative, including linear/sloped extrapolation.
    /// Blocked regions return `None`. At a curved knot the authored right slope
    /// is used, including zero-width tangents, as `TsSpline::EvalDerivative` does.
    /// Regressive Bézier segments require containment; a
    /// vanishing interior time derivative returns `SingularDerivative`.
    /// Inner-loop derivatives use the echoed curve, consistently with baking;
    /// OpenUSD 26.8 can instead use shadowed authored knots at loop boundaries.
    /// AOUSD Core §12.5; OpenUSD `ts/eval.cpp`.
    pub fn evaluate_derivative(&self, time: f64) -> Result<Option<f64>, SplineQueryError> {
        self.derivative(time, false)
    }
    /// Analytic left-hand derivative. At knots this inspects the preceding
    /// segment; at the first knot it uses pre-extrapolation. Limits on supported
    /// data match [`Self::evaluate_derivative`].
    pub fn evaluate_pre_derivative(&self, time: f64) -> Result<Option<f64>, SplineQueryError> {
        self.derivative(time, true)
    }
    fn derivative(&self, time: f64, pre: bool) -> Result<Option<f64>, SplineQueryError> {
        if self.active_inner_loops()? {
            return self.evaluate_inner_loops(time, pre, true);
        }
        validate(self)?;
        if !time.is_finite() {
            return Err(SplineQueryError::InvalidInput);
        }
        if self.knots.is_empty() {
            return Ok(None);
        }
        let Some(mapping) = self.map_extrapolation(time, pre)? else {
            return Ok(None);
        };
        if mapping.held {
            return Ok(Some(0.));
        }
        finite(
            self.derivative_unlooped(mapping.time, mapping.pre)?
                .map(|v| v * mapping.sign),
        )
    }
    pub(super) fn derivative_unlooped(
        &self,
        time: f64,
        pre: bool,
    ) -> Result<Option<f64>, SplineQueryError> {
        let segment = match self
            .knots
            .binary_search_by(|k| k.time.partial_cmp(&time).expect("finite time"))
        {
            Ok(i) => {
                if pre && i == 0 {
                    return finite(self.extrapolation_slope(true));
                }
                if !pre && i + 1 == self.knots.len() {
                    return finite(self.extrapolation_slope(false));
                }
                let (a, b) = if pre {
                    (&self.knots[i - 1], &self.knots[i])
                } else {
                    (&self.knots[i], &self.knots[i + 1])
                };
                if a.next_interp == KnotInterp::Curve
                    && self.default_curve_type == CurveType::Bezier
                    && regressive(a, b)
                {
                    return Err(SplineQueryError::RegressiveTangents);
                }
                return finite(match a.next_interp {
                    KnotInterp::Block => None,
                    KnotInterp::Held => Some(0.),
                    KnotInterp::Linear => Some(slope(a, b)),
                    KnotInterp::Curve => Some(if pre {
                        b.pre_tan_slope
                    } else {
                        a.post_tan_slope
                    }),
                });
            }
            Err(0) => return finite(self.extrapolation_slope(true)),
            Err(i) if i == self.knots.len() => return finite(self.extrapolation_slope(false)),
            Err(i) => i - 1,
        };
        let a = &self.knots[segment];
        let b = &self.knots[segment + 1];
        let value = match a.next_interp {
            KnotInterp::Block => return Ok(None),
            KnotInterp::Held => 0.,
            KnotInterp::Linear => slope(a, b),
            KnotInterp::Curve => {
                if self.default_curve_type == CurveType::Bezier && regressive(a, b) {
                    return Err(SplineQueryError::RegressiveTangents);
                }
                let c = controls(self, a, b);
                let u = if self.default_curve_type == CurveType::Hermite {
                    (time - a.time) / (b.time - a.time)
                } else {
                    self.solve_bezier_time(c[0][0], c[1][0], c[2][0], c[3][0], time)
                        .ok_or(SplineQueryError::InvalidInput)?
                };
                let dx = cubic_bezier_deriv(c[0][0], c[1][0], c[2][0], c[3][0], u);
                if dx == 0. {
                    return Err(SplineQueryError::SingularDerivative);
                }
                cubic_bezier_deriv(c[0][1], c[1][1], c[2][1], c[3][1], u) / dx
            }
        };
        if !value.is_finite() {
            return Err(SplineQueryError::InvalidInput);
        }
        Ok(Some(value))
    }
    /// Whether any curved Bézier segment has a negative time derivative.
    /// This tests actual regression, without OpenUSD authoring-mode padding.
    /// Hermite curves never regress in time. Invalid/looped descriptions error.
    pub fn has_regressive_tangents(&self) -> Result<bool, SplineQueryError> {
        validate(self)?;
        Ok(self.default_curve_type == CurveType::Bezier
            && self
                .knots
                .windows(2)
                .any(|p| p[0].next_interp == KnotInterp::Curve && regressive(&p[0], &p[1])))
    }
    /// Contains Bézier tangent handles within their adjacent segment's time
    /// range. Both widths are independently clamped to the span, preserving
    /// slopes; this guarantees nonregression without choosing an active handle.
    /// Returns the number of changed widths. Validation precedes all mutation.
    /// This is the bounded `TsAntiRegressionContain` policy, not interactive
    /// keep-ratio/keep-start tangent editing. AOUSD Core §12.5.
    pub fn contain_tangents(&mut self) -> Result<usize, SplineQueryError> {
        validate(self)?;
        if self.default_curve_type != CurveType::Bezier {
            return Ok(0);
        }
        let mut count = 0;
        for i in 0..self.knots.len().saturating_sub(1) {
            if self.knots[i].next_interp != KnotInterp::Curve {
                continue;
            }
            let span = self.knots[i + 1].time - self.knots[i].time;
            if self.knots[i].post_tan_width > span {
                self.knots[i].post_tan_width = span;
                count += 1;
            }
            if self.knots[i + 1].pre_tan_width > span {
                self.knots[i + 1].pre_tan_width = span;
                count += 1;
            }
        }
        Ok(count)
    }
    /// Finds a conservative change interval between two unlooped descriptions.
    /// Changed/inserted/deleted knots include their adjacent segments in both
    /// versions; edits at endpoints include extrapolation. Precision/curve-type
    /// changes conservatively cover all time. Equal descriptions return `None`.
    /// This compares authored structure, not numerical equivalence.
    pub fn change_interval(
        &self,
        other: &Self,
    ) -> Result<Option<SplineChangeInterval>, SplineQueryError> {
        validate(self)?;
        validate(other)?;
        if looped(self) || looped(other) {
            return Err(SplineQueryError::UnsupportedLoops);
        }
        if self == other {
            return Ok(None);
        }
        if self.knots.is_empty()
            || other.knots.is_empty()
            || self.data_type != other.data_type
            || self.default_curve_type != other.default_curve_type
        {
            return Ok(Some(SplineChangeInterval {
                start: f64::NEG_INFINITY,
                end: f64::INFINITY,
            }));
        }
        let mut start = f64::INFINITY;
        let mut end = f64::NEG_INFINITY;
        for (a, b) in [(self, other), (other, self)] {
            for (i, k) in a.knots.iter().enumerate() {
                if b.knots
                    .binary_search_by(|v| v.time.partial_cmp(&k.time).expect("finite time"))
                    .ok()
                    .is_some_and(|j| b.knots[j] == *k)
                {
                    continue;
                }
                start = start.min(if i == 0 {
                    f64::NEG_INFINITY
                } else {
                    a.knots[i - 1].time
                });
                end = end.max(if i + 1 == a.knots.len() {
                    f64::INFINITY
                } else {
                    a.knots[i + 1].time
                });
            }
        }
        // Linear extrapolation can depend on the neighboring knot even when
        // the endpoint itself is unchanged (linear segment secants).
        if (self.pre_extrapolation == Extrapolation::Linear
            || other.pre_extrapolation == Extrapolation::Linear)
            && self.knots.get(..2) != other.knots.get(..2)
        {
            start = f64::NEG_INFINITY;
            end = end.max(self.knots[0].time.max(other.knots[0].time));
        }
        if (self.post_extrapolation == Extrapolation::Linear
            || other.post_extrapolation == Extrapolation::Linear)
            && self.knots.get(self.knots.len().saturating_sub(2)..)
                != other.knots.get(other.knots.len().saturating_sub(2)..)
        {
            start = start.min(
                self.knots
                    .last()
                    .expect("nonempty")
                    .time
                    .min(other.knots.last().expect("nonempty").time),
            );
            end = f64::INFINITY;
        }
        if self.pre_extrapolation != other.pre_extrapolation {
            start = f64::NEG_INFINITY;
            end = end.max(self.knots[0].time.max(other.knots[0].time));
        }
        if self.post_extrapolation != other.post_extrapolation {
            start = start.min(
                self.knots
                    .last()
                    .expect("nonempty")
                    .time
                    .min(other.knots.last().expect("nonempty").time),
            );
            end = f64::INFINITY;
        }
        Ok(Some(SplineChangeInterval { start, end }))
    }
    /// Approximates a finite, increasing interval with continuous polylines.
    /// `tolerance` bounds absolute vertical error (up to floating-point roundoff)
    /// in value units, using a cubic
    /// control-hull test rather than midpoint-only heuristics. Held jumps and
    /// value blocks split polylines. `max_samples` bounds total returned vertices
    /// and subdivision work; failure returns no partial approximation.
    /// Loops and regressive Bézier segments must be handled before sampling.
    /// AOUSD Core §12.5; this uses explicit value-error units, unlike `TsSpline`'s
    /// display-space distance metric with separate time/value scales.
    pub fn sample_adaptive(
        &self,
        start: f64,
        end: f64,
        tolerance: f64,
        max_samples: usize,
    ) -> Result<Vec<SplinePolyline>, SplineQueryError> {
        validate(self)?;
        if looped(self) {
            return Err(SplineQueryError::UnsupportedLoops);
        }
        if !start.is_finite()
            || !end.is_finite()
            || start >= end
            || !tolerance.is_finite()
            || tolerance <= 0.
        {
            return Err(SplineQueryError::InvalidInput);
        }
        if self.has_regressive_tangents()? {
            return Err(SplineQueryError::RegressiveTangents);
        }
        let mut result = Vec::new();
        if self.knots.is_empty() {
            return Ok(result);
        }
        let mut times = vec![start];
        times.extend(
            self.knots
                .iter()
                .map(|k| k.time)
                .filter(|&t| t > start && t < end),
        );
        times.push(end);
        let mut work = max_samples.saturating_mul(2);
        let mut used = 0;
        for window in times.windows(2) {
            let (a, b) = (window[0], window[1]);
            let Some(va) = self.evaluate(a) else {
                continue;
            };
            let Some(vb) = self.evaluate_pre_value(b)? else {
                continue;
            };
            finite(Some(va))?;
            finite(Some(vb))?;
            let index = self.knots.partition_point(|k| k.time <= a);
            let mut samples = vec![SplineSample { time: a, value: va }];
            if index > 0
                && index < self.knots.len()
                && self.knots[index - 1].next_interp == KnotInterp::Curve
            {
                let ka = &self.knots[index - 1];
                let kb = &self.knots[index];
                let c = controls(self, ka, kb);
                let param = |time| {
                    self.solve_bezier_time(c[0][0], c[1][0], c[2][0], c[3][0], time)
                        .ok_or(SplineQueryError::InvalidInput)
                };
                let u = param(a)?;
                let v = param(b)?;
                let (_, right) = split(c, u);
                let (piece, _) = split(right, if u == 1. { 0. } else { (v - u) / (1. - u) });
                let mut pending = vec![(piece, 0_u32)];
                while let Some((c, depth)) = pending.pop() {
                    if work == 0 || used + samples.len() >= max_samples {
                        return Err(SplineQueryError::SampleBudgetExceeded);
                    }
                    work -= 1;
                    if flat(c, tolerance) {
                        samples.push(SplineSample {
                            time: c[3][0],
                            value: c[3][1],
                        });
                    } else {
                        if depth >= 64 {
                            return Err(SplineQueryError::SampleBudgetExceeded);
                        }
                        let (left, right) = split(c, 0.5);
                        if left[3][0] <= c[0][0] || left[3][0] >= c[3][0] {
                            return Err(SplineQueryError::SampleBudgetExceeded);
                        }
                        pending.push((right, depth + 1));
                        pending.push((left, depth + 1));
                    }
                }
                // Preserve requested boundary times despite inverse-solver rounding.
                samples[0] = SplineSample { time: a, value: va };
                *samples.last_mut().expect("curve endpoint") = SplineSample { time: b, value: vb };
            } else {
                samples.push(SplineSample { time: b, value: vb });
            }
            used += samples.len();
            if used > max_samples {
                return Err(SplineQueryError::SampleBudgetExceeded);
            }
            result.push(SplinePolyline { samples });
        }
        Ok(result)
    }
}
fn controls(s: &SplineData, a: &Knot, b: &Knot) -> [[f64; 2]; 4] {
    let span = b.time - a.time;
    let aw = if s.default_curve_type == CurveType::Hermite {
        span / 3.
    } else {
        a.post_tan_width
    };
    let bw = if s.default_curve_type == CurveType::Hermite {
        span / 3.
    } else {
        b.pre_tan_width
    };
    let bv = b.pre_value.unwrap_or(b.value);
    [
        [a.time, a.value],
        [a.time + aw, a.value + aw * a.post_tan_slope],
        [b.time - bw, bv - bw * b.pre_tan_slope],
        [b.time, bv],
    ]
}
fn split(c: [[f64; 2]; 4], u: f64) -> ([[f64; 2]; 4], [[f64; 2]; 4]) {
    let mix = |a: [f64; 2], b: [f64; 2]| core::array::from_fn(|i| a[i] * (1. - u) + b[i] * u);
    let a = mix(c[0], c[1]);
    let b = mix(c[1], c[2]);
    let d = mix(c[2], c[3]);
    let e = mix(a, b);
    let f = mix(b, d);
    let g = mix(e, f);
    ([c[0], a, e, g], [g, f, d, c[3]])
}
fn flat(c: [[f64; 2]; 4], tolerance: f64) -> bool {
    let slope = (c[3][1] - c[0][1]) / (c[3][0] - c[0][0]);
    c[1..3]
        .iter()
        .all(|p| (p[1] - (c[0][1] + slope * (p[0] - c[0][0]))).abs() <= tolerance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spline::SplineDataType;

    fn knot(time: f64, value: f64) -> Knot {
        Knot {
            custom_data: Vec::new(),
            pre_tan_algorithm: crate::spline::TangentAlgorithm::None,
            post_tan_algorithm: crate::spline::TangentAlgorithm::None,

            time,
            value,
            pre_value: None,
            next_interp: KnotInterp::Linear,
            curve_type: CurveType::Bezier,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 0.5,
            post_tan_width: 0.5,
            pre_tan_slope: 99.,
            post_tan_slope: 99.,
        }
    }
    fn spline() -> SplineData {
        let mut a = knot(0., 2.);
        a.post_tan_slope = 3.;
        let mut b = knot(2., 8.);
        b.pre_tan_slope = 4.;
        SplineData {
            pre_loop_boundary: None,
            post_loop_boundary: None,

            data_type: SplineDataType::Double,
            default_curve_type: CurveType::Bezier,
            pre_extrapolation: Extrapolation::Linear,
            post_extrapolation: Extrapolation::Linear,
            loop_params: None,
            knots: vec![a, b],
        }
    }
    fn close(actual: Option<f64>, expected: Option<f64>) {
        match (actual, expected) {
            (Some(a), Some(b)) => assert!((a - b).abs() < 1e-9, "{a} != {b}"),
            (None, None) => (),
            pair => panic!("{pair:?}"),
        }
    }
    #[test]
    fn cpp_one_sided_evaluation_and_extrapolation() {
        // Golden rows from OpenUSD 26.08 TsSpline Eval/EvalPreValue and both
        // derivative methods. Outward tangent slopes deliberately differ.
        let cases = [
            (
                KnotInterp::Linear,
                [Some(-1.), Some(2.), Some(5.), Some(8.), Some(11.)],
                [Some(3.); 5],
                Some(8.),
                Some(3.),
            ),
            (
                KnotInterp::Held,
                [Some(2.), Some(2.), Some(2.), Some(8.), Some(8.)],
                [Some(0.); 5],
                Some(2.),
                Some(0.),
            ),
            (
                KnotInterp::Curve,
                [Some(-1.), Some(2.), Some(4.8125), Some(8.), Some(12.)],
                [Some(3.), Some(3.), Some(17. / 6.), Some(4.), Some(4.)],
                Some(8.),
                Some(4.),
            ),
            (
                KnotInterp::Block,
                [Some(2.), None, None, Some(8.), Some(8.)],
                [Some(0.), None, None, Some(0.), Some(0.)],
                None,
                None,
            ),
        ];
        for (mode, values, derivatives, pre_value, pre_derivative) in cases {
            let mut s = spline();
            s.knots[0].next_interp = mode;
            for (i, t) in [-1., 0., 1., 2., 3.].into_iter().enumerate() {
                close(s.evaluate(t), values[i]);
                close(s.evaluate_derivative(t).unwrap(), derivatives[i]);
            }
            close(s.evaluate_pre_value(2.).unwrap(), pre_value);
            close(s.evaluate_pre_derivative(2.).unwrap(), pre_derivative);
            close(s.evaluate_pre_value(-0.).unwrap(), Some(2.));
            s.post_extrapolation = Extrapolation::Block;
            assert_eq!(s.evaluate(2.), None);
            close(s.evaluate_pre_value(2.).unwrap(), pre_value);
        }
    }
    #[test]
    fn dual_endpoints_flatten_linear_extrapolation() {
        let mut s = spline();
        s.knots[0].pre_value = Some(-4.);
        s.knots[1].pre_value = Some(5.);
        assert_eq!(s.evaluate(-1.), Some(-4.));
        assert_eq!(s.evaluate(3.), Some(8.));
        assert_eq!(s.evaluate_pre_value(2.).unwrap(), Some(5.));
        assert_eq!(s.evaluate_derivative(2.).unwrap(), Some(0.));
        assert_eq!(s.evaluate_pre_derivative(2.).unwrap(), Some(1.5));
    }
    #[test]
    fn subframe_bezier_and_hermite_use_relative_time_precision() {
        for curve in [CurveType::Bezier, CurveType::Hermite] {
            let mut s = spline();
            s.default_curve_type = curve;
            s.knots[0].next_interp = KnotInterp::Curve;
            s.knots[0].post_tan_slope = 0.;
            s.knots[1].pre_tan_slope = 0.;
            let tiny = s
                .retimed(crate::doc::LayerOffset {
                    offset: 0.,
                    scale: 1e-18,
                })
                .unwrap();
            for t in [0.1, 0.5, 1., 1.5, 1.9] {
                close(tiny.evaluate(t * 1e-18), s.evaluate(t));
            }
        }
    }
    #[test]
    fn analytic_derivatives_match_finite_differences() {
        for curve in [CurveType::Bezier, CurveType::Hermite] {
            let mut s = spline();
            s.default_curve_type = curve;
            s.knots[0].next_interp = KnotInterp::Curve;
            for t in [0.2, 0.75, 1.2, 1.8] {
                let delta = 1e-5;
                let finite = (s.evaluate(t + delta).unwrap() - s.evaluate(t - delta).unwrap())
                    / (2. * delta);
                assert!((s.evaluate_derivative(t).unwrap().unwrap() - finite).abs() < 1e-7);
            }
        }
    }
    #[test]
    fn adaptive_control_hull_catches_symmetric_curve_and_preserves_jumps() {
        let mut s = spline();
        s.knots[0].value = 0.;
        s.knots[1].value = 0.;
        s.knots[0].next_interp = KnotInterp::Curve;
        s.knots[0].post_tan_slope = 10.;
        s.knots[1].pre_tan_slope = 10.;
        let polylines = s.sample_adaptive(0., 2., 0.001, 1000).unwrap();
        let points = &polylines[0].samples;
        assert!(points.len() > 8); // A midpoint-only flatness test misses this S curve.
        for i in 0..1001 {
            let t = 2. * f64::from(i) / 1000.;
            let j = points
                .partition_point(|p| p.time < t)
                .clamp(1, points.len() - 1);
            let (a, b) = (points[j - 1], points[j]);
            let v = a.value + (b.value - a.value) * (t - a.time) / (b.time - a.time);
            assert!((s.evaluate(t).unwrap() - v).abs() <= 0.001 + 1e-10);
        }
        assert_eq!(
            s.sample_adaptive(0., 2., 1e-10, 2),
            Err(SplineQueryError::SampleBudgetExceeded)
        );
        s.knots[0].next_interp = KnotInterp::Held;
        s.knots[1].value = 8.;
        let p = s.sample_adaptive(-1., 3., 0.01, 10).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[1].samples.last().unwrap().value, 0.);
        assert_eq!(p[2].samples[0].value, 8.);
        s.knots[0].next_interp = KnotInterp::Block;
        assert_eq!(s.sample_adaptive(-1., 3., 0.01, 10).unwrap().len(), 2);
    }
    #[test]
    fn invalid_and_unsupported_inputs_and_tangent_containment() {
        let mut s = spline();
        s.knots[0].next_interp = KnotInterp::Curve;
        s.knots[0].post_tan_width = 3.;
        s.knots[1].pre_tan_width = 3.;
        assert!(s.has_regressive_tangents().unwrap());
        assert_eq!(
            s.evaluate_derivative(0.),
            Err(SplineQueryError::RegressiveTangents)
        );
        assert_eq!(
            s.sample_adaptive(0., 2., 0.1, 100),
            Err(SplineQueryError::RegressiveTangents)
        );
        assert_eq!(s.contain_tangents().unwrap(), 2);
        assert!(!s.has_regressive_tangents().unwrap());
        assert_eq!(s.contain_tangents().unwrap(), 0);
        assert_eq!(
            s.evaluate_derivative(f64::NAN),
            Err(SplineQueryError::InvalidInput)
        );
        assert_eq!(s.evaluate(f64::NAN), None);
        s.knots[1].time = 0.;
        let original = s.clone();
        assert_eq!(s.contain_tangents(), Err(SplineQueryError::InvalidInput));
        assert_eq!(s, original);
        s = spline();
        s.pre_extrapolation = Extrapolation::LoopRepeat;
        assert_eq!(s.evaluate_pre_value(0.), Ok(Some(2.)));
        s.loop_params = Some(super::super::LoopParams {
            proto_start: 0.,
            proto_end: 2.,
            num_pre_loops: 1,
            num_post_loops: 1,
            value_offset: 0.,
        });
        let mut baked = s.clone();
        baked.bake_inner_loops(16).unwrap();
        assert_eq!(s.evaluate_checked(1.), baked.evaluate_checked(1.));
        s = spline();
        s.knots[1].value = f64::MAX;
        s.knots[0].value = -f64::MAX;
        assert_eq!(
            s.evaluate_derivative(1.),
            Err(SplineQueryError::InvalidInput)
        );
    }
    #[test]
    fn change_interval_covers_neighbors_and_extrapolation() {
        let mut a = spline();
        a.pre_extrapolation = Extrapolation::Held;
        a.post_extrapolation = Extrapolation::Held;
        a.knots = vec![knot(0., 0.), knot(1., 1.), knot(2., 2.), knot(3., 3.)];
        assert_eq!(a.change_interval(&a).unwrap(), None);
        let mut b = a.clone();
        b.knots[1].value = 4.;
        assert_eq!(
            a.change_interval(&b).unwrap(),
            Some(SplineChangeInterval { start: 0., end: 2. })
        );
        b = a.clone();
        b.knots.remove(2);
        assert_eq!(
            a.change_interval(&b).unwrap(),
            Some(SplineChangeInterval { start: 1., end: 3. })
        );
        a.pre_extrapolation = Extrapolation::Linear;
        b = a.clone();
        b.knots[1].value = 4.;
        assert_eq!(
            a.change_interval(&b).unwrap(),
            Some(SplineChangeInterval {
                start: f64::NEG_INFINITY,
                end: 2.
            })
        );
        b = a.clone();
        b.knots[0].value = 4.;
        assert_eq!(
            a.change_interval(&b).unwrap(),
            Some(SplineChangeInterval {
                start: f64::NEG_INFINITY,
                end: 1.
            })
        );
    }
}
