// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Point motion for meshes, curves and points; simulation and rendering belong to callers.
//! OpenUSD: `UsdGeomPointBased::ComputePointsAtTime(s)` and `samplingUtils.cpp`.
//! Source selection and interpolation follow AOUSD Core §12.3–12.5.
use crate::{
    Time,
    motion_sampling::{aligned, anchor, rate, vectors},
    usd_geom::PointBased,
};
use alloc::vec::Vec;

/// Invalid required point data or query configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointMotionError {
    /// The required `points` array cannot be read.
    MissingPoints,
    /// Numeric times must be finite, of matching kind, and use a positive finite rate.
    InvalidTime,
    /// A nonempty motion array has a different length from points.
    LengthMismatch,
    /// An array motion source cannot be anchored as discrete samples.
    UnsupportedSource(&'static str),
}
impl core::fmt::Display for PointMotionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid point motion: {self:?}")
    }
}
impl core::error::Error for PointMotionError {}

/// Caller-owned motion inputs anchored to one base time.
///
/// Motion arrays use distance units per second and per second squared. Schema
/// gathering ignores misaligned or incorrectly sized optional motion arrays,
/// as OpenUSD does. `evaluate` integrates these inputs without scene reads.
/// Without velocities it returns these points; the schema computation performs
/// ordinary interpolation separately. Keep inputs only while their source
/// scene snapshot remains valid, or gather again after edits.
#[derive(Clone, Debug, PartialEq)]
pub struct PointMotionInputs {
    /// Points at the base time's lower sample, or default if unsampled.
    pub points: Vec<[f32; 3]>,
    /// Aligned velocities; empty disables integration.
    pub velocities: Vec<[f32; 3]>,
    /// Aligned accelerations; empty disables the quadratic term.
    pub accelerations: Vec<[f32; 3]>,
    /// The base time used to gather the inputs.
    pub base_time: Time,
    /// Numeric velocity sample time, if velocities are enabled.
    pub velocity_sample: Option<f64>,
    /// Stage time codes per second.
    pub time_codes_per_second: f64,
}
fn compatible(time: Time, base: Time) -> bool {
    match (time, base) {
        (Time::Default, Time::Default) => true,
        (Time::At { code, .. }, Time::At { code: base, .. }) => {
            code.is_finite() && base.is_finite()
        }
        _ => false,
    }
}
impl PointMotionInputs {
    /// Evaluates these arrays at `time`, retaining float32 vector rounding as in Gf.
    ///
    /// OpenUSD 26.8 ignores the deprecated velocity-scale argument; inherited
    /// motion settings do not multiply this computation implicitly.
    /// Returns an error for incompatible times, array lengths or sample anchors.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "GfVec3f vector operations round to float32"
    )]
    pub fn evaluate(&self, time: Time) -> Result<Vec<[f32; 3]>, PointMotionError> {
        if !compatible(time, self.base_time)
            || !self.time_codes_per_second.is_finite()
            || self.time_codes_per_second <= 0.
        {
            return Err(PointMotionError::InvalidTime);
        }
        let n = self.points.len();
        if (!self.velocities.is_empty() && self.velocities.len() != n)
            || (!self.accelerations.is_empty()
                && (self.accelerations.len() != n || self.velocities.is_empty()))
        {
            return Err(PointMotionError::LengthMismatch);
        }
        if self.velocities.is_empty() {
            return Ok(self.points.clone());
        }
        let delta = match (time, self.velocity_sample) {
            (Time::At { code, .. }, Some(sample)) if sample.is_finite() => {
                (code - sample) / self.time_codes_per_second
            }
            _ => return Err(PointMotionError::InvalidTime),
        };
        let mut points = self.points.clone();
        for (i, point) in points.iter_mut().enumerate() {
            for (axis, coordinate) in point.iter_mut().enumerate() {
                let mut velocity = self.velocities[i][axis];
                if !self.accelerations.is_empty() {
                    velocity += ((delta * f64::from(self.accelerations[i][axis])) as f32) * 0.5;
                }
                *coordinate += (delta * f64::from(velocity)) as f32;
            }
        }
        Ok(points)
    }
}
impl PointBased<'_> {
    /// Gathers point and aligned motion arrays once at `base_time`.
    /// Returns errors for unreadable points, unsupported sample sources or invalid times.
    pub fn motion_inputs(&self, base_time: Time) -> Result<PointMotionInputs, PointMotionError> {
        if !compatible(base_time, base_time) {
            return Err(PointMotionError::InvalidTime);
        }
        let p = anchor(self, "points", base_time).map_err(PointMotionError::UnsupportedSource)?;
        let points = vectors(self, "points", p.time).ok_or(PointMotionError::MissingPoints)?;
        let v =
            anchor(self, "velocities", base_time).map_err(PointMotionError::UnsupportedSource)?;
        let velocities = vectors(self, "velocities", v.time)
            .filter(|data| data.len() == points.len() && aligned(p, v))
            .unwrap_or_default();
        let a = anchor(self, "accelerations", base_time)
            .map_err(PointMotionError::UnsupportedSource)?;
        let accelerations = vectors(self, "accelerations", a.time)
            .filter(|data| !velocities.is_empty() && data.len() == points.len() && aligned(v, a))
            .unwrap_or_default();
        Ok(PointMotionInputs {
            points,
            velocities,
            accelerations,
            base_time,
            velocity_sample: v.sample,
            time_codes_per_second: rate(&self.scene()),
        })
    }
    /// Computes points at one time using `base_time` for motion sample anchoring.
    /// Without valid velocities, interpolates points with the requested time's
    /// interpolation mode; mismatched topology falls back to the anchored points.
    /// Returns errors for invalid times or missing required data.
    pub fn compute_points_at_time(
        &self,
        time: Time,
        base_time: Time,
    ) -> Result<Vec<[f32; 3]>, PointMotionError> {
        Ok(self
            .compute_points_at_times(&[time], base_time)?
            .pop()
            .expect("one requested sample"))
    }
    /// Computes ordered shutter samples from one fixed base time, gathering
    /// motion arrays once. Duplicate and unsorted times are supported.
    ///
    /// In the interpolation fallback, a successful sample becomes the fallback
    /// for later samples whose topology differs, matching OpenUSD's batch method.
    /// No partial outputs are returned on error.
    pub fn compute_points_at_times(
        &self,
        times: &[Time],
        base_time: Time,
    ) -> Result<Vec<Vec<[f32; 3]>>, PointMotionError> {
        if times.iter().any(|&time| !compatible(time, base_time)) {
            return Err(PointMotionError::InvalidTime);
        }
        let mut inputs = self.motion_inputs(base_time)?;
        let count = inputs.points.len();
        let mut outputs = Vec::with_capacity(times.len());
        for &time in times {
            if inputs.velocities.is_empty()
                && count != 0
                && let Some(points) =
                    vectors(self, "points", time).filter(|data| data.len() == count)
            {
                inputs.points = points;
            }
            outputs.push(inputs.evaluate(time)?);
        }
        Ok(outputs)
    }
}
