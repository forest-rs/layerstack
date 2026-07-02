// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Spline data model and evaluation for time-varying attribute resolution.
//!
//! Splines provide smooth interpolation between knots using Bézier or Hermite
//! curves. They sit between `TimeSamples` and default values in the resolution
//! priority order (§12.3).
//!
//! Spec: AOUSD Core §12.3.3 (spline opinions), §12.5 (interpolation methods),
//! §16.3.10.33 (binary encoding).

use alloc::vec::Vec;
use core::fmt;

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

/// Curve type for a spline segment (§16.3.10.33).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CurveType {
    /// Cubic Bézier curve.
    Bezier = 0,
    /// Cubic Hermite curve.
    Hermite = 1,
}

/// Per-knot interpolation mode for the segment *following* a knot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum KnotInterp {
    /// No value in this segment.
    Block = 0,
    /// Step function: hold the knot value.
    Held = 1,
    /// Linear interpolation to the next knot.
    Linear = 2,
    /// Smooth curve interpolation (Bézier or Hermite).
    Curve = 3,
}

/// Extrapolation mode outside the knot range (§12.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extrapolation {
    /// No value outside the range.
    Block,
    /// Hold the nearest knot value.
    Held,
    /// Extend with a linear tangent from the nearest knot.
    Linear,
    /// Extend with an explicit slope.
    Sloped(f64),
    /// Loop: repeat the prototype region.
    LoopRepeat,
    /// Loop: reset value at each period boundary.
    LoopReset,
    /// Loop: oscillate (ping-pong).
    LoopOscillate,
}

/// Numeric precision of knot values in the binary encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SplineDataType {
    /// Not specified (empty spline).
    Unspecified = 0,
    /// 64-bit double.
    Double = 1,
    /// 32-bit float.
    Float = 2,
    /// 16-bit half.
    Half = 3,
}

// ---------------------------------------------------------------------------
// Loop parameters
// ---------------------------------------------------------------------------

/// Loop parameters for repeating extrapolation modes (§12.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoopParams {
    /// Start of the prototype region (in time).
    pub proto_start: f64,
    /// End of the prototype region (in time).
    pub proto_end: f64,
    /// Number of pre-loops (before the prototype region).
    pub num_pre_loops: i32,
    /// Number of post-loops (after the prototype region).
    pub num_post_loops: i32,
    /// Value offset applied per loop iteration.
    pub value_offset: f64,
}

// ---------------------------------------------------------------------------
// Knot
// ---------------------------------------------------------------------------

/// A single knot on a spline curve.
///
/// Each knot specifies a time/value pair plus tangent information that
/// controls the shape of the curve segment to the *next* knot.
#[derive(Clone, Debug, PartialEq)]
pub struct Knot {
    /// Time position of this knot.
    pub time: f64,
    /// Value at this knot (approaching from the right, or single-valued).
    pub value: f64,
    /// Value approaching from the left (dual-valued knots only).
    pub pre_value: Option<f64>,
    /// How the segment *after* this knot is interpolated.
    pub next_interp: KnotInterp,
    /// Curve type for this knot's segment (can override the spline default).
    pub curve_type: CurveType,
    /// Whether the pre-tangent uses Maya form.
    pub pre_tan_maya_form: bool,
    /// Whether the post-tangent uses Maya form.
    pub post_tan_maya_form: bool,
    /// Width of the incoming tangent (Bézier only; 0 for Hermite).
    pub pre_tan_width: f64,
    /// Width of the outgoing tangent (Bézier only; 0 for Hermite).
    pub post_tan_width: f64,
    /// Slope of the incoming tangent.
    pub pre_tan_slope: f64,
    /// Slope of the outgoing tangent.
    pub post_tan_slope: f64,
}

// ---------------------------------------------------------------------------
// SplineData
// ---------------------------------------------------------------------------

/// Complete spline data for a single attribute (§16.3.10.33).
///
/// A spline consists of an ordered set of [`Knot`]s with tangent information,
/// plus extrapolation behavior outside the knot range.
#[derive(Clone, Debug, PartialEq)]
pub struct SplineData {
    /// Numeric precision of knot values.
    pub data_type: SplineDataType,
    /// Default curve type for segments.
    pub default_curve_type: CurveType,
    /// Extrapolation mode before the first knot.
    pub pre_extrapolation: Extrapolation,
    /// Extrapolation mode after the last knot.
    pub post_extrapolation: Extrapolation,
    /// Optional loop parameters for repeating extrapolation.
    pub loop_params: Option<LoopParams>,
    /// Knots sorted by time.
    pub knots: Vec<Knot>,
}

impl SplineData {
    /// Evaluate the spline at the given time, returning the interpolated value.
    ///
    /// Returns `None` for empty splines or `Block` extrapolation regions.
    ///
    /// Spec: §12.5 (interpolation methods).
    #[must_use]
    pub fn evaluate(&self, time: f64) -> Option<f64> {
        if self.knots.is_empty() {
            return None;
        }

        let first = &self.knots[0];
        let last = &self.knots[self.knots.len() - 1];

        // Before first knot → pre-extrapolation.
        if time < first.time {
            return self.extrapolate_pre(time);
        }

        // After last knot → post-extrapolation.
        if time > last.time {
            return self.extrapolate_post(time);
        }

        // Find the segment containing `time`.
        self.evaluate_inner(time)
    }

    /// Evaluate within the knot range (first.time <= time <= last.time).
    fn evaluate_inner(&self, time: f64) -> Option<f64> {
        // Binary search for the knot at or just before `time`.
        let idx = match self.knots.binary_search_by(|k| {
            k.time
                .partial_cmp(&time)
                .unwrap_or(core::cmp::Ordering::Equal)
        }) {
            Ok(i) => return Some(self.knots[i].value),
            Err(i) => i,
        };

        // `idx` is the insertion point: knots[idx-1].time < time < knots[idx].time.
        if idx == 0 {
            return Some(self.knots[0].value);
        }
        if idx >= self.knots.len() {
            return Some(self.knots[self.knots.len() - 1].value);
        }

        let k0 = &self.knots[idx - 1];
        let k1 = &self.knots[idx];

        match k0.next_interp {
            KnotInterp::Block => None,
            KnotInterp::Held => Some(k0.value),
            KnotInterp::Linear => {
                let alpha = (time - k0.time) / (k1.time - k0.time);
                // Use pre_value of k1 if it's dual-valued.
                let v1 = k1.pre_value.unwrap_or(k1.value);
                Some(k0.value + (v1 - k0.value) * alpha)
            }
            KnotInterp::Curve => match k0.curve_type {
                CurveType::Bezier => self.eval_bezier(k0, k1, time),
                CurveType::Hermite => Some(self.eval_hermite(k0, k1, time)),
            },
        }
    }

    /// Evaluate a cubic Bézier segment between two knots.
    ///
    /// The tangent (width, slope) representation is converted to control points:
    /// - `P0 = (t0, v0)`
    /// - `P1 = (t0 + post_width, v0 + post_width * post_slope)`
    /// - `P2 = (t1 - pre_width, v1 - pre_width * pre_slope)`
    /// - `P3 = (t1, v1)`
    ///
    /// We solve for the Bézier parameter `u` such that `B_x(u) = time`
    /// using Newton-Raphson, then evaluate `B_y(u)`.
    fn eval_bezier(&self, k0: &Knot, k1: &Knot, time: f64) -> Option<f64> {
        let t0 = k0.time;
        let t1 = k1.time;
        let v0 = k0.value;
        let v1 = k1.pre_value.unwrap_or(k1.value);

        // Control points in time.
        let tx0 = t0;
        let tx1 = t0 + k0.post_tan_width;
        let tx2 = t1 - k1.pre_tan_width;
        let tx3 = t1;

        // Control points in value.
        let vy0 = v0;
        let vy1 = v0 + k0.post_tan_width * k0.post_tan_slope;
        let vy2 = v1 - k1.pre_tan_width * k1.pre_tan_slope;
        let vy3 = v1;

        // Find parameter u where B_x(u) = time using Newton-Raphson.
        let u = self.solve_bezier_time(tx0, tx1, tx2, tx3, time)?;

        // Evaluate B_y(u).
        Some(cubic_bezier(vy0, vy1, vy2, vy3, u))
    }

    /// Find the Bézier parameter `u` such that the time-axis cubic
    /// `B_x(u) = target` using Newton-Raphson iteration.
    fn solve_bezier_time(
        &self,
        tx0: f64,
        tx1: f64,
        tx2: f64,
        tx3: f64,
        target: f64,
    ) -> Option<f64> {
        // Initial guess: linear proportion.
        let span = tx3 - tx0;
        if span.abs() < f64::EPSILON {
            return Some(0.0);
        }
        let mut u = (target - tx0) / span;
        u = u.clamp(0.0, 1.0);

        // Newton-Raphson iterations.
        const MAX_ITER: usize = 20;
        const TOLERANCE: f64 = 1e-12;

        for _ in 0..MAX_ITER {
            let bx = cubic_bezier(tx0, tx1, tx2, tx3, u);
            let err = bx - target;
            if err.abs() < TOLERANCE {
                return Some(u);
            }

            let dbx = cubic_bezier_deriv(tx0, tx1, tx2, tx3, u);
            if dbx.abs() < f64::EPSILON {
                // Derivative too small — fall back to bisection.
                return Some(bisect_bezier_time(tx0, tx1, tx2, tx3, target));
            }

            u -= err / dbx;
            u = u.clamp(0.0, 1.0);
        }

        // If Newton didn't converge, fall back to bisection.
        Some(bisect_bezier_time(tx0, tx1, tx2, tx3, target))
    }

    /// Evaluate a cubic Hermite segment between two knots.
    ///
    /// Uses standard Hermite basis functions with the knot slopes.
    fn eval_hermite(&self, k0: &Knot, k1: &Knot, time: f64) -> f64 {
        let dt = k1.time - k0.time;
        if dt.abs() < f64::EPSILON {
            return k0.value;
        }

        let t = (time - k0.time) / dt;
        let v0 = k0.value;
        let v1 = k1.pre_value.unwrap_or(k1.value);
        let m0 = k0.post_tan_slope * dt;
        let m1 = k1.pre_tan_slope * dt;

        // Hermite basis functions.
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;

        h00 * v0 + h10 * m0 + h01 * v1 + h11 * m1
    }

    /// Pre-extrapolation: evaluate before the first knot.
    fn extrapolate_pre(&self, time: f64) -> Option<f64> {
        let first = &self.knots[0];
        match &self.pre_extrapolation {
            Extrapolation::Block => None,
            Extrapolation::Held => Some(first.pre_value.unwrap_or(first.value)),
            Extrapolation::Linear => {
                let slope = first.pre_tan_slope;
                let dt = time - first.time;
                Some(first.pre_value.unwrap_or(first.value) + slope * dt)
            }
            Extrapolation::Sloped(slope) => {
                let dt = time - first.time;
                Some(first.pre_value.unwrap_or(first.value) + slope * dt)
            }
            Extrapolation::LoopRepeat | Extrapolation::LoopReset | Extrapolation::LoopOscillate => {
                self.extrapolate_loop_pre(time)
            }
        }
    }

    /// Post-extrapolation: evaluate after the last knot.
    fn extrapolate_post(&self, time: f64) -> Option<f64> {
        let last = &self.knots[self.knots.len() - 1];
        match &self.post_extrapolation {
            Extrapolation::Block => None,
            Extrapolation::Held => Some(last.value),
            Extrapolation::Linear => {
                let slope = last.post_tan_slope;
                let dt = time - last.time;
                Some(last.value + slope * dt)
            }
            Extrapolation::Sloped(slope) => {
                let dt = time - last.time;
                Some(last.value + slope * dt)
            }
            Extrapolation::LoopRepeat | Extrapolation::LoopReset | Extrapolation::LoopOscillate => {
                self.extrapolate_loop_post(time)
            }
        }
    }

    /// Loop-based pre-extrapolation.
    fn extrapolate_loop_pre(&self, time: f64) -> Option<f64> {
        let params = self.loop_params.as_ref()?;
        let period = params.proto_end - params.proto_start;
        if period <= 0.0 {
            return Some(self.knots[0].pre_value.unwrap_or(self.knots[0].value));
        }

        let dt = params.proto_start - time;
        let cycles = ceil_f64(dt / period);
        let loop_count = if params.num_pre_loops > 0 {
            cycles.min(params.num_pre_loops as f64)
        } else {
            cycles
        };

        let (mapped_time, value_offset) = match self.pre_extrapolation {
            Extrapolation::LoopRepeat => {
                let rem = ((time - params.proto_start) % period + period) % period;
                (params.proto_start + rem, -loop_count * params.value_offset)
            }
            Extrapolation::LoopReset => {
                let rem = ((time - params.proto_start) % period + period) % period;
                (params.proto_start + rem, 0.0)
            }
            Extrapolation::LoopOscillate => {
                let full_cycle = period * 2.0;
                let rem = ((time - params.proto_start) % full_cycle + full_cycle) % full_cycle;
                if rem > period {
                    (
                        params.proto_end - (rem - period),
                        -loop_count * params.value_offset,
                    )
                } else {
                    (params.proto_start + rem, -loop_count * params.value_offset)
                }
            }
            _ => return None,
        };

        self.evaluate_inner(mapped_time).map(|v| v + value_offset)
    }

    /// Loop-based post-extrapolation.
    fn extrapolate_loop_post(&self, time: f64) -> Option<f64> {
        let params = self.loop_params.as_ref()?;
        let period = params.proto_end - params.proto_start;
        if period <= 0.0 {
            return Some(self.knots[self.knots.len() - 1].value);
        }

        let dt = time - params.proto_end;
        let cycles = ceil_f64(dt / period);
        let loop_count = if params.num_post_loops > 0 {
            cycles.min(params.num_post_loops as f64)
        } else {
            cycles
        };

        let (mapped_time, value_offset) = match self.post_extrapolation {
            Extrapolation::LoopRepeat => {
                let rem = (time - params.proto_start) % period;
                (params.proto_start + rem, loop_count * params.value_offset)
            }
            Extrapolation::LoopReset => {
                let rem = (time - params.proto_start) % period;
                (params.proto_start + rem, 0.0)
            }
            Extrapolation::LoopOscillate => {
                let full_cycle = period * 2.0;
                let rem = (time - params.proto_start) % full_cycle;
                if rem > period {
                    (
                        params.proto_end - (rem - period),
                        loop_count * params.value_offset,
                    )
                } else {
                    (params.proto_start + rem, loop_count * params.value_offset)
                }
            }
            _ => return None,
        };

        self.evaluate_inner(mapped_time).map(|v| v + value_offset)
    }
}

impl fmt::Display for CurveType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bezier => f.write_str("Bezier"),
            Self::Hermite => f.write_str("Hermite"),
        }
    }
}

impl fmt::Display for SplineDataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unspecified => f.write_str("Unspecified"),
            Self::Double => f.write_str("Double"),
            Self::Float => f.write_str("Float"),
            Self::Half => f.write_str("Half"),
        }
    }
}

// ---------------------------------------------------------------------------
// no_std float helpers
// ---------------------------------------------------------------------------

/// Ceiling function for `f64` (`no_std`-compatible).
#[allow(
    clippy::cast_possible_truncation,
    reason = "intentional f64→i64 for integer part extraction"
)]
fn ceil_f64(x: f64) -> f64 {
    let i = x as i64 as f64;
    if x > i { i + 1.0 } else { i }
}

// ---------------------------------------------------------------------------
// Cubic Bézier math
// ---------------------------------------------------------------------------

/// Evaluate a cubic Bézier at parameter `t ∈ [0, 1]`.
fn cubic_bezier(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let mt = 1.0 - t;
    let mt2 = mt * mt;
    let t2 = t * t;
    mt2 * mt * p0 + 3.0 * mt2 * t * p1 + 3.0 * mt * t2 * p2 + t2 * t * p3
}

/// Derivative of a cubic Bézier at parameter `t`.
fn cubic_bezier_deriv(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * (p1 - p0) + 6.0 * mt * t * (p2 - p1) + 3.0 * t * t * (p3 - p2)
}

/// Bisection fallback for finding `u` such that `B_x(u) = target`.
fn bisect_bezier_time(tx0: f64, tx1: f64, tx2: f64, tx3: f64, target: f64) -> f64 {
    let mut lo = 0.0_f64;
    let mut hi = 1.0_f64;
    const MAX_ITER: usize = 64;
    const TOLERANCE: f64 = 1e-12;

    for _ in 0..MAX_ITER {
        let mid = (lo + hi) * 0.5;
        let val = cubic_bezier(tx0, tx1, tx2, tx3, mid);
        if (val - target).abs() < TOLERANCE {
            return mid;
        }
        if val < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) * 0.5
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    /// Helper to build a simple spline with given knots.
    fn simple_spline(knots: Vec<Knot>) -> SplineData {
        SplineData {
            data_type: SplineDataType::Double,
            default_curve_type: CurveType::Bezier,
            pre_extrapolation: Extrapolation::Held,
            post_extrapolation: Extrapolation::Held,
            loop_params: None,
            knots,
        }
    }

    fn linear_knot(time: f64, value: f64) -> Knot {
        Knot {
            time,
            value,
            pre_value: None,
            next_interp: KnotInterp::Linear,
            curve_type: CurveType::Bezier,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 0.0,
            post_tan_width: 0.0,
            pre_tan_slope: 0.0,
            post_tan_slope: 0.0,
        }
    }

    fn held_knot(time: f64, value: f64) -> Knot {
        Knot {
            time,
            value,
            pre_value: None,
            next_interp: KnotInterp::Held,
            curve_type: CurveType::Bezier,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 0.0,
            post_tan_width: 0.0,
            pre_tan_slope: 0.0,
            post_tan_slope: 0.0,
        }
    }

    #[test]
    fn empty_spline_returns_none() {
        let s = simple_spline(vec![]);
        assert_eq!(s.evaluate(0.0), None);
    }

    #[test]
    fn single_knot_held() {
        let s = simple_spline(vec![held_knot(5.0, 42.0)]);
        assert_eq!(s.evaluate(5.0), Some(42.0));
        // Before and after: held extrapolation.
        assert_eq!(s.evaluate(0.0), Some(42.0));
        assert_eq!(s.evaluate(10.0), Some(42.0));
    }

    #[test]
    fn exact_knot_values() {
        let s = simple_spline(vec![linear_knot(0.0, 0.0), linear_knot(10.0, 100.0)]);
        assert_eq!(s.evaluate(0.0), Some(0.0));
        assert_eq!(s.evaluate(10.0), Some(100.0));
    }

    #[test]
    fn linear_interpolation_midpoint() {
        let s = simple_spline(vec![linear_knot(0.0, 0.0), linear_knot(10.0, 100.0)]);
        assert_eq!(s.evaluate(5.0), Some(50.0));
    }

    #[test]
    fn linear_interpolation_quarter() {
        let s = simple_spline(vec![linear_knot(0.0, 0.0), linear_knot(10.0, 100.0)]);
        let v = s.evaluate(2.5).unwrap();
        assert!((v - 25.0).abs() < 1e-10);
    }

    #[test]
    fn held_interpolation() {
        let s = simple_spline(vec![held_knot(0.0, 10.0), held_knot(10.0, 20.0)]);
        assert_eq!(s.evaluate(5.0), Some(10.0));
        assert_eq!(s.evaluate(9.999), Some(10.0));
    }

    #[test]
    fn held_extrapolation() {
        let s = simple_spline(vec![linear_knot(5.0, 50.0), linear_knot(15.0, 150.0)]);
        // Before first → held.
        assert_eq!(s.evaluate(0.0), Some(50.0));
        // After last → held.
        assert_eq!(s.evaluate(20.0), Some(150.0));
    }

    #[test]
    fn linear_extrapolation() {
        let mut s = simple_spline(vec![linear_knot(0.0, 0.0), linear_knot(10.0, 100.0)]);
        // Give the first knot a pre-slope of 10 and last knot a post-slope of 10.
        s.knots[0].pre_tan_slope = 10.0;
        s.knots[1].post_tan_slope = 10.0;
        s.pre_extrapolation = Extrapolation::Linear;
        s.post_extrapolation = Extrapolation::Linear;

        let pre = s.evaluate(-5.0).unwrap();
        assert!((pre - (-50.0)).abs() < 1e-10);

        let post = s.evaluate(15.0).unwrap();
        assert!((post - 150.0).abs() < 1e-10);
    }

    #[test]
    fn sloped_extrapolation() {
        let mut s = simple_spline(vec![linear_knot(0.0, 100.0), linear_knot(10.0, 200.0)]);
        s.pre_extrapolation = Extrapolation::Sloped(5.0);
        s.post_extrapolation = Extrapolation::Sloped(-3.0);

        let pre = s.evaluate(-10.0).unwrap();
        assert!((pre - 50.0).abs() < 1e-10); // 100 + 5 * (-10)

        let post = s.evaluate(20.0).unwrap();
        assert!((post - 170.0).abs() < 1e-10); // 200 + (-3) * 10
    }

    #[test]
    fn block_extrapolation() {
        let mut s = simple_spline(vec![linear_knot(0.0, 0.0), linear_knot(10.0, 100.0)]);
        s.pre_extrapolation = Extrapolation::Block;
        s.post_extrapolation = Extrapolation::Block;

        assert_eq!(s.evaluate(-5.0), None);
        assert_eq!(s.evaluate(15.0), None);
        // Inside range should still work.
        assert_eq!(s.evaluate(5.0), Some(50.0));
    }

    #[test]
    fn hermite_straight_line() {
        // Hermite with zero slopes → should still interpolate linearly
        // when slopes are set to match the linear slope.
        let k0 = Knot {
            time: 0.0,
            value: 0.0,
            pre_value: None,
            next_interp: KnotInterp::Curve,
            curve_type: CurveType::Hermite,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 0.0,
            post_tan_width: 0.0,
            pre_tan_slope: 1.0,
            post_tan_slope: 1.0,
        };
        let k1 = Knot {
            time: 10.0,
            value: 10.0,
            pre_value: None,
            next_interp: KnotInterp::Held,
            curve_type: CurveType::Hermite,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 0.0,
            post_tan_width: 0.0,
            pre_tan_slope: 1.0,
            post_tan_slope: 1.0,
        };
        let s = simple_spline(vec![k0, k1]);
        let v = s.evaluate(5.0).unwrap();
        assert!((v - 5.0).abs() < 1e-10, "hermite linear: got {v}");
    }

    #[test]
    fn bezier_straight_line() {
        // Bézier with tangent widths set to 1/3 of the span and slope=1
        // should produce a straight line y=x.
        let k0 = Knot {
            time: 0.0,
            value: 0.0,
            pre_value: None,
            next_interp: KnotInterp::Curve,
            curve_type: CurveType::Bezier,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 10.0 / 3.0,
            post_tan_width: 10.0 / 3.0,
            pre_tan_slope: 1.0,
            post_tan_slope: 1.0,
        };
        let k1 = Knot {
            time: 10.0,
            value: 10.0,
            pre_value: None,
            next_interp: KnotInterp::Held,
            curve_type: CurveType::Bezier,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 10.0 / 3.0,
            post_tan_width: 10.0 / 3.0,
            pre_tan_slope: 1.0,
            post_tan_slope: 1.0,
        };
        let s = simple_spline(vec![k0, k1]);
        for i in 0..=10 {
            let t = i as f64;
            let v = s.evaluate(t).unwrap();
            assert!((v - t).abs() < 1e-6, "bezier linear at t={t}: got {v}");
        }
    }

    #[test]
    fn dual_valued_knot() {
        // Knot at t=5 has value=10 (right limit) and pre_value=5 (left limit).
        // Linear segments approaching and leaving should use the appropriate values.
        let s = simple_spline(vec![
            linear_knot(0.0, 0.0),
            Knot {
                time: 5.0,
                value: 10.0,
                pre_value: Some(5.0),
                next_interp: KnotInterp::Linear,
                curve_type: CurveType::Bezier,
                pre_tan_maya_form: false,
                post_tan_maya_form: false,
                pre_tan_width: 0.0,
                post_tan_width: 0.0,
                pre_tan_slope: 0.0,
                post_tan_slope: 0.0,
            },
            linear_knot(10.0, 20.0),
        ]);

        // At t=5, the knot value is 10 (approaching from right / at the knot).
        assert_eq!(s.evaluate(5.0), Some(10.0));

        // Approaching t=5 from the left: linear from (0,0) to (5, pre_value=5).
        let v = s.evaluate(2.5).unwrap();
        assert!((v - 2.5).abs() < 1e-10, "pre-value approach: got {v}");

        // Leaving t=5: linear from (5, 10) to (10, pre_value_of_10=20).
        let v = s.evaluate(7.5).unwrap();
        assert!((v - 15.0).abs() < 1e-10, "post-value approach: got {v}");
    }

    #[test]
    fn multiple_segments() {
        let s = simple_spline(vec![
            linear_knot(0.0, 0.0),
            linear_knot(10.0, 100.0),
            linear_knot(20.0, 50.0),
        ]);
        // First segment.
        assert_eq!(s.evaluate(5.0), Some(50.0));
        // Second segment.
        let v = s.evaluate(15.0).unwrap();
        assert!((v - 75.0).abs() < 1e-10);
    }
}
