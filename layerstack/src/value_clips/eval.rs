// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Owned sample evaluation for an already selected, manifest-eligible clip set.
//! This module owns time mapping and interpolation; catalog composition, asset
//! lookup, manifest eligibility and ordinary opinion strength remain elsewhere.
//! AOUSD Core §12.3.2, §12.5; OpenUSD 26.08 `usd/clip.cpp` and `clipSet.cpp`.

use crate::value_resolution::interpolate_samples;
use crate::{InterpolationType, LayerOffset, PropertySpec, Value};
use alloc::{sync::Arc, vec::Vec};

/// OpenUSD's default `UsdTimeCode::SafeStep()` for jump discontinuities.
pub(super) const CLIP_SAFE_STEP: f64 = f64::EPSILON * 1e6 * 10. * 2.;

/// A schedule or property which cannot be evaluated coherently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipEvalError {
    /// At least one activation and one asset slot are required.
    EmptySchedule,
    /// Activations must be finite, unique and index an existing asset slot.
    InvalidActivation,
    /// Mapping values must be finite, with at most two entries at a stage time.
    InvalidTimeMapping,
    /// Source sample times must be finite, unique and strictly increasing.
    InvalidSamples,
    /// Numeric queries and their mapped times must be finite.
    InvalidQuery,
    /// Spline loops and regressive tangents exceed the existing query core.
    UnsupportedSpline,
    /// The source spline has invalid knots, tangents or nonfinite data.
    InvalidSpline,
}
impl core::fmt::Display for ClipEvalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid clip evaluation: {self:?}")
    }
}
impl core::error::Error for ClipEvalError {}

/// A value and its sample endpoints in the stage's time coordinates.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Evaluation {
    /// Evaluated value; `None` means this eligible set supplied no value.
    /// It must not cause ordinary weaker defaults or schema fallbacks to win.
    pub(super) value: Option<Value>,
    /// Lower external sample time (clamped outside the prepared timeline).
    pub(super) lower: f64,
    /// Upper external sample time.
    pub(super) upper: f64,
    /// Asset slot supplying the lower endpoint.
    pub(super) lower_clip: usize,
    /// Asset slot supplying the upper endpoint.
    pub(super) upper_clip: usize,
    /// Lower endpoint mapped into its clip's local time.
    pub(super) lower_internal: f64,
    /// Upper endpoint mapped into its clip's local time.
    pub(super) upper_internal: f64,
    /// The lower endpoint came from the manifest default rather than clip data.
    pub(super) lower_from_manifest: bool,
    /// The upper endpoint came from the manifest default rather than clip data.
    pub(super) upper_from_manifest: bool,
}

#[derive(Clone, Copy, Debug)]
struct Mapping {
    external: f64,
    authored: f64,
    internal: f64,
    jump: bool,
}
/// Immutable property snapshot with external sample times prepared once.
///
/// Raw clip defaults are ignored. Missing clip samples use the manifest default
/// or no value; with `interpolate_missing`, missing clips without a manifest
/// default contribute no boundaries. All asset slots share one time mapping.
/// Queries use binary search over the prepared timeline and source sample maps.
#[derive(Clone, Debug)]
pub(super) struct PreparedClipProperty {
    active: Vec<(f64, usize)>,
    times: Vec<Mapping>,
    clips: Vec<Option<Arc<PropertySpec>>>,
    manifest_default: Option<Value>,
    sample_times: Vec<f64>,
    spline_mode: bool,
    interpolate_missing: bool,
    blocked_activations: Vec<f64>,
}
impl PreparedClipProperty {
    /// Prepares a manifest-selected sample property. `active` indices address
    /// `clips`; a missing asset/property is represented by `None`. Activations
    /// are sorted; time mappings are stably sorted to preserve jump pair order.
    /// First/last activations extend to negative/positive infinity. Mapping
    /// endpoints clamp, while an absent mapping means identity stage/clip time.
    /// Invalid inputs return an error before a usable snapshot is produced.
    pub(super) fn new(
        mut active: Vec<(f64, usize)>,
        mut times: Vec<(f64, f64)>,
        clips: Vec<Option<Arc<PropertySpec>>>,
        manifest_default: Option<Value>,
        interpolate_missing: bool,
    ) -> Result<Self, ClipEvalError> {
        if active.is_empty() || clips.is_empty() {
            return Err(ClipEvalError::EmptySchedule);
        }
        if active
            .iter()
            .any(|(t, i)| !t.is_finite() || *i >= clips.len())
        {
            return Err(ClipEvalError::InvalidActivation);
        }
        active.sort_by(|a, b| a.0.total_cmp(&b.0));
        if active.windows(2).any(|p| p[0].0 == p[1].0) {
            return Err(ClipEvalError::InvalidActivation);
        }
        if times.iter().any(|(a, b)| !a.is_finite() || !b.is_finite()) {
            return Err(ClipEvalError::InvalidTimeMapping);
        }
        times.sort_by(|a, b| a.0.total_cmp(&b.0));
        if times.windows(3).any(|p| p[0].0 == p[2].0) {
            return Err(ClipEvalError::InvalidTimeMapping);
        }
        let mut mappings: Vec<_> = times
            .into_iter()
            .map(|(external, internal)| Mapping {
                external,
                authored: external,
                internal,
                jump: false,
            })
            .collect();
        for i in 0..mappings.len().saturating_sub(1) {
            if mappings[i].external == mappings[i + 1].external {
                let before = mappings[i].external - CLIP_SAFE_STEP;
                if !before.is_finite()
                    || before >= mappings[i].external
                    || (i > 0 && before <= mappings[i - 1].external)
                {
                    return Err(ClipEvalError::InvalidTimeMapping);
                }
                mappings[i].external = before;
                mappings[i].jump = true;
            }
        }
        if mappings.windows(2).any(|p| {
            !(p[1].external - p[0].external).is_finite()
                || !(p[1].internal - p[0].internal).is_finite()
        }) {
            return Err(ClipEvalError::InvalidTimeMapping);
        }
        for prop in clips.iter().flatten() {
            if let Some(samples) = &prop.time_samples
                && (samples.iter().any(|(t, _)| !t.is_finite())
                    || samples
                        .windows(2)
                        .any(|p| p[0].0 >= p[1].0 || !(p[1].0 - p[0].0).is_finite()))
            {
                return Err(ClipEvalError::InvalidSamples);
            }
        }
        let mut result = Self {
            active,
            times: mappings,
            clips,
            manifest_default,
            sample_times: Vec::new(),
            spline_mode: false,
            interpolate_missing,
            blocked_activations: Vec::new(),
        };
        result.prepare_sample_times()?;
        Ok(result)
    }
    fn prepare_sample_times(&mut self) -> Result<(), ClipEvalError> {
        self.sample_times.clear();
        for index in 0..self.active.len() {
            let (start, asset) = self.active[index];
            let samples = self.clips[asset]
                .as_ref()
                .and_then(|p| p.time_samples.as_deref())
                .unwrap_or(&[]);
            if self.interpolate_missing
                && (samples.is_empty() || self.activation_blocked(index))
                && self.manifest_default.is_none()
            {
                continue;
            }
            let low = if index == 0 { f64::NEG_INFINITY } else { start };
            let high = self.active.get(index + 1).map_or(f64::INFINITY, |v| v.0);
            let inside = |t: f64| t >= low && t < high;
            self.sample_times.push(start);
            for mapping in &self.times {
                if inside(mapping.external) {
                    self.sample_times.push(mapping.external);
                }
            }
            if self.times.is_empty() {
                self.sample_times
                    .extend(samples.iter().map(|(t, _)| *t).filter(|t| inside(*t)));
            } else {
                // clip.cpp::_ListTimeSamplesForPathFromClipLayer: invert every
                // segment because looping/reversing maps can have many preimages.
                for (t, _) in samples {
                    for pair in self.times.windows(2) {
                        let (a, b) = (pair[0], pair[1]);
                        if a.jump
                            || *t < a.internal.min(b.internal)
                            || *t > a.internal.max(b.internal)
                        {
                            continue;
                        }
                        if a.internal == b.internal {
                            for external in [a.external, b.external] {
                                if inside(external) {
                                    self.sample_times.push(external);
                                }
                            }
                        } else {
                            let external = a.external
                                + (*t - a.internal) / (b.internal - a.internal)
                                    * (b.authored - a.external);
                            if !external.is_finite() {
                                return Err(ClipEvalError::InvalidTimeMapping);
                            }
                            if inside(external) {
                                self.sample_times.push(external);
                            }
                        }
                    }
                }
            }
        }
        if self.sample_times.is_empty() {
            self.sample_times.push(self.active[0].0);
        }
        self.sample_times.sort_by(f64::total_cmp);
        self.sample_times.dedup();
        Ok(())
    }
    /// Prepares manifest-selected spline evaluation with the same schedule.
    /// Authored splines retain their interpolation regardless of stage policy.
    /// Loops are explicitly unsupported; malformed and regressive sources error.
    /// Spline properties have no discrete sample timeline, as in OpenUSD 26.08.
    pub(super) fn new_spline(
        active: Vec<(f64, usize)>,
        times: Vec<(f64, f64)>,
        clips: Vec<Option<Arc<PropertySpec>>>,
        manifest_default: Option<Value>,
        interpolate_missing: bool,
    ) -> Result<Self, ClipEvalError> {
        for spline in clips.iter().flatten().filter_map(|p| p.spline.as_ref()) {
            let time = spline.knots.first().map_or(0., |k| k.time);
            spline
                .evaluate_pre_value(time)
                .map_err(|error| match error {
                    crate::spline::SplineQueryError::UnsupportedLoops => {
                        ClipEvalError::UnsupportedSpline
                    }
                    _ => ClipEvalError::InvalidSpline,
                })?;
            if spline
                .has_regressive_tangents()
                .map_err(|_| ClipEvalError::InvalidSpline)?
            {
                return Err(ClipEvalError::UnsupportedSpline);
            }
        }
        let mut result = Self::new(active, times, clips, manifest_default, interpolate_missing)?;
        result.spline_mode = true;
        result.sample_times.clear();
        Ok(result)
    }
    /// Applies manifest blocks by activation time, preserving repeated assets.
    /// Sample mode uses blocks only when interpolating missing clips; spline
    /// mode always excludes those activations, per `clipSet.cpp::QuerySpline`.
    pub(super) fn with_manifest_blocks(
        mut self,
        mut blocked: Vec<f64>,
    ) -> Result<Self, ClipEvalError> {
        if blocked.iter().any(|t| !t.is_finite()) {
            return Err(ClipEvalError::InvalidSamples);
        }
        blocked.sort_by(f64::total_cmp);
        blocked.dedup();
        self.blocked_activations = blocked;
        if !self.spline_mode {
            self.prepare_sample_times()?;
        }
        Ok(self)
    }
    /// Whether this property uses manifest-selected spline evaluation.
    pub(super) fn is_spline(&self) -> bool {
        self.spline_mode
    }
    fn activation_blocked(&self, index: usize) -> bool {
        self.blocked_activations
            .binary_search_by(|t| t.total_cmp(&self.active[index].0))
            .is_ok()
    }
    fn spline_value(
        &self,
        asset: usize,
        time: f64,
        pre: bool,
    ) -> Result<(Option<Value>, f64), ClipEvalError> {
        let Some(property) = self.clips[asset].as_ref() else {
            return Ok((None, self.internal_time(time)?));
        };
        let Some(spline) = property.spline.as_ref() else {
            return Ok((None, self.internal_time(time)?));
        };
        let exterior = self.times.first().is_some_and(|m| time < m.external)
            || self.times.last().is_some_and(|m| time > m.external);
        let section = if pre {
            // A spline gap's left endpoint is a pre-time query. At a mapping
            // knot it uses the preceding section; at a jump it skips the
            // synthetic discontinuity section (clip.cpp::_GetBracketingTimeSegment).
            self.times
                .iter()
                .position(|m| m.jump && m.authored == time)
                .unwrap_or_else(|| self.times.partition_point(|m| m.external < time))
                .saturating_sub(1)
        } else {
            self.times
                .partition_point(|m| m.external <= time)
                .saturating_sub(1)
        };
        let reversed = self
            .times
            .get(section + 1)
            .is_some_and(|b| self.times[section].internal > b.internal);
        let mut internal = self.internal_time(time)?;
        if pre && let Some(mapping) = self.times.iter().find(|m| m.jump && m.authored == time) {
            internal = mapping.internal;
        }
        let value = if !exterior && (pre != reversed) {
            spline
                .evaluate_pre_value(internal)
                .map_err(|_| ClipEvalError::InvalidSpline)?
        } else {
            spline.evaluate(internal)
        };
        let value = value.map(|v| {
            if property
                .type_name
                .as_ref()
                .is_some_and(|t| &*t.type_name == "timecode")
            {
                let external = self.times.first().map_or(time, |a| time.max(a.external));
                let external = self
                    .times
                    .last()
                    .map_or(external, |b| external.min(b.external));
                Value::TimeCode(v + external - internal)
            } else {
                spline_value_type(spline.data_type, v)
            }
        });
        Ok((value, internal))
    }
    fn evaluate_spline(&self, time: f64) -> Result<Evaluation, ClipEvalError> {
        let index = self
            .active
            .partition_point(|(t, _)| *t <= time)
            .saturating_sub(1);
        let asset = self.active[index].1;
        let internal = self.internal_time(time)?;
        let result =
            |value, lower, upper, lower_clip, upper_clip, lower_internal, upper_internal| {
                Evaluation {
                    value,
                    lower,
                    upper,
                    lower_clip,
                    upper_clip,
                    lower_internal,
                    upper_internal,
                    lower_from_manifest: false,
                    upper_from_manifest: false,
                }
            };
        if !self.activation_blocked(index)
            && self.clips[asset]
                .as_ref()
                .is_some_and(|p| p.spline.is_some())
        {
            let (value, internal) = self.spline_value(asset, time, false)?;
            return Ok(result(value, time, time, asset, asset, internal, internal));
        }
        if self
            .manifest_default
            .as_ref()
            .is_some_and(|v| *v != Value::Blocked)
            || !self.interpolate_missing
        {
            let mut evaluation = result(
                self.manifest_default
                    .clone()
                    .filter(|v| *v != Value::Blocked),
                time,
                time,
                asset,
                asset,
                internal,
                internal,
            );
            evaluation.lower_from_manifest = self.manifest_default.is_some();
            evaluation.upper_from_manifest = evaluation.lower_from_manifest;
            return Ok(evaluation);
        }
        let contributes = |i: usize| {
            !self.activation_blocked(i)
                && self.clips[self.active[i].1]
                    .as_ref()
                    .is_some_and(|p| p.spline.is_some())
        };
        let previous = (0..index).rev().find(|i| contributes(*i));
        let next = (index + 1..self.active.len()).find(|i| contributes(*i));
        let left = previous
            .map(|i| {
                let t = self.active[i + 1].0;
                self.spline_value(self.active[i].1, t, true)
                    .map(|(v, internal)| (t, self.active[i].1, internal, v))
            })
            .transpose()?;
        let right = next
            .map(|i| {
                let t = self.active[i].0;
                self.spline_value(self.active[i].1, t, false)
                    .map(|(v, internal)| (t, self.active[i].1, internal, v))
            })
            .transpose()?;
        match (left, right) {
            (Some((a, ac, ai, av)), Some((b, bc, bi, bv))) => {
                let value = av
                    .and_then(|v| {
                        interpolate_samples(
                            &[(a, v), (b, bv.unwrap_or(Value::Blocked))],
                            time,
                            InterpolationType::Linear,
                        )
                    })
                    .filter(|v| *v != Value::Blocked);
                Ok(result(value, a, b, ac, bc, ai, bi))
            }
            (Some((t, c, i, v)), None) | (None, Some((t, c, i, v))) => {
                Ok(result(v, t, t, c, c, i, i))
            }
            (None, None) => Ok(result(None, time, time, asset, asset, internal, internal)),
        }
    }
    /// Sorted distinct external sample times, including activation/mapping knots.
    #[must_use]
    pub(super) fn sample_times(&self) -> &[f64] {
        &self.sample_times
    }
    /// Exact raw sample composability, falling back to the manifest default kind.
    /// No payloads are cloned or interpolated. Synthetic activation/mapping
    /// positions can have an interpolated sparse payload but a dense type query.
    /// AOUSD Core §12.5; OpenUSD `clip.cpp::QueryTimeSampleTypeid`,
    /// `clipSet.cpp::QueryTimeSampleTypeid`, `sdf/abstractData.cpp::QueryTimeSampleTypeid`.
    pub(super) fn sample_is_sparse(&self, time: f64) -> bool {
        if self.spline_mode || !time.is_finite() {
            return false;
        }
        let Ok(internal) = self.internal_time(time) else {
            return false;
        };
        let exact = self.clips[self.active_clip(time)]
            .as_ref()
            .and_then(|p| p.time_samples.as_deref())
            .and_then(|samples| {
                samples
                    .binary_search_by(|(t, _)| {
                        t.partial_cmp(&internal).expect("validated finite samples")
                    })
                    .ok()
                    .map(|at| &samples[at].1)
            });
        exact
            .or(self.manifest_default.as_ref())
            .is_some_and(|v| v.array_edit_ref().is_some())
    }
    fn active_clip(&self, time: f64) -> usize {
        let at = self
            .active
            .partition_point(|(t, _)| *t <= time)
            .saturating_sub(1);
        self.active[at].1
    }
    fn internal_time(&self, time: f64) -> Result<f64, ClipEvalError> {
        if self.times.is_empty() {
            return Ok(time);
        }
        let upper = self.times.partition_point(|v| v.external < time);
        let mapped = if upper == 0 {
            self.times[0].internal
        } else if upper == self.times.len() {
            self.times[upper - 1].internal
        } else {
            let (a, b) = (self.times[upper - 1], self.times[upper]);
            // Preserve the authored external endpoint on the left side of a
            // jump; the synthetic SafeStep coordinate is only a sample marker.
            // clip.cpp::_TranslateTimeToInternal/_TranslateTimeToExternal.
            a.internal + (time - a.external) / (b.authored - a.external) * (b.internal - a.internal)
        };
        if mapped.is_finite() {
            Ok(mapped)
        } else {
            Err(ClipEvalError::InvalidQuery)
        }
    }
    fn endpoint(
        &self,
        time: f64,
        interp: InterpolationType,
    ) -> Result<(Option<Value>, usize, f64, bool), ClipEvalError> {
        let asset = self.active_clip(time);
        let internal = self.internal_time(time)?;
        let raw = self.clips[asset]
            .as_ref()
            .and_then(|p| p.time_samples.as_deref())
            .filter(|s| !s.is_empty());
        let value = if let Some(samples) = raw {
            interpolate_samples(samples, internal, interp).map(|value| {
                // Clip timecode payloads SHIFT by external-internal; they do not
                // use the inverse affine map (clip.cpp::_ConvertValueForTime).
                crate::stage::stage_time::retime_value(
                    &value,
                    LayerOffset {
                        offset: time - internal,
                        scale: 1.,
                    },
                )
                .unwrap_or(value)
            })
        } else {
            self.manifest_default.clone()
        };
        Ok((
            value,
            asset,
            internal,
            raw.is_none() && self.manifest_default.is_some(),
        ))
    }
    /// Evaluates one finite numeric query. Exact endpoints and exterior queries
    /// clamp to one sample. A blocked/missing lower endpoint supplies no value;
    /// a blocked/missing upper endpoint holds the lower value, matching USD.
    /// Returned endpoint times remain inspectable under held interpolation.
    pub(super) fn evaluate(
        &self,
        time: f64,
        interp: InterpolationType,
    ) -> Result<Evaluation, ClipEvalError> {
        if !time.is_finite() {
            return Err(ClipEvalError::InvalidQuery);
        }
        if self.spline_mode {
            return self.evaluate_spline(time);
        }
        let next = self.sample_times.partition_point(|t| *t < time);
        let (lower, upper) = if next == 0 {
            (self.sample_times[0], self.sample_times[0])
        } else if next == self.sample_times.len() {
            let t = self.sample_times[next - 1];
            (t, t)
        } else if self.sample_times[next] == time {
            let t = self.sample_times[next];
            (t, t)
        } else {
            (self.sample_times[next - 1], self.sample_times[next])
        };
        let (a, lower_clip, lower_internal, lower_from_manifest) = self.endpoint(lower, interp)?;
        let (b, upper_clip, upper_internal, upper_from_manifest) = self.endpoint(upper, interp)?;
        let value = a.and_then(|a| {
            if a == Value::Blocked {
                return None;
            }
            if upper == lower {
                return Some(a);
            }
            let b = b.unwrap_or(Value::Blocked);
            interpolate_samples(&[(lower, a), (upper, b)], time, interp)
                .filter(|v| *v != Value::Blocked)
        });
        Ok(Evaluation {
            value,
            lower,
            upper,
            lower_clip,
            upper_clip,
            lower_internal,
            upper_internal,
            lower_from_manifest,
            upper_from_manifest,
        })
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "USD float spline values narrow to their declared precision"
)]
fn spline_value_type(kind: crate::spline::SplineDataType, value: f64) -> Value {
    match kind {
        crate::spline::SplineDataType::Float => Value::Float(value as f32),
        crate::spline::SplineDataType::Half => Value::Half(crate::half::from_f64(value)),
        crate::spline::SplineDataType::Double | crate::spline::SplineDataType::Unspecified => {
            Value::Double(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    fn clip(samples: &[(f64, f64)]) -> Option<Arc<PropertySpec>> {
        Some(Arc::new(
            PropertySpec::attribute().with_time_samples(
                samples
                    .iter()
                    .map(|(t, v)| (*t, Value::Double(*v)))
                    .collect(),
            ),
        ))
    }
    fn value(p: &PreparedClipProperty, t: f64) -> Option<Value> {
        p.evaluate(t, InterpolationType::Linear).unwrap().value
    }
    #[test]
    fn switch_gap_defaults_blocks_and_held_match_cpp() {
        let active = vec![(0., 0), (2., 1), (4., 2)];
        let times = vec![(0., 0.), (6., 6.)];
        let clips = vec![
            clip(&[(0., 0.), (1., 10.)]),
            None,
            clip(&[(4., 40.), (5., 50.)]),
        ];
        let p =
            PreparedClipProperty::new(active.clone(), times.clone(), clips.clone(), None, false)
                .unwrap();
        assert_eq!(value(&p, 1.5), Some(Value::Double(10.)));
        assert_eq!(value(&p, 2.5), None);
        assert_eq!(value(&p, 4.5), Some(Value::Double(45.)));
        assert_eq!(p.sample_times(), &[0., 1., 2., 4., 5., 6.]);
        let p = PreparedClipProperty::new(active.clone(), times.clone(), clips.clone(), None, true)
            .unwrap();
        assert_eq!(value(&p, 2.5), Some(Value::Double(25.)));
        assert_eq!(p.sample_times(), &[0., 1., 4., 5., 6.]);
        assert_eq!(
            p.evaluate(2.5, InterpolationType::Held).unwrap().value,
            Some(Value::Double(10.))
        );
        let p = PreparedClipProperty::new(active, times, clips, Some(Value::Double(42.)), true)
            .unwrap();
        assert_eq!(value(&p, 1.5), Some(Value::Double(26.)));
        assert_eq!(value(&p, 2.5), Some(Value::Double(41.5)));
    }
    #[test]
    fn inverse_offset_gap_and_repeated_assets_match_emitted_bundle() {
        let p = PreparedClipProperty::new(
            vec![(0., 1), (20., 0)],
            vec![(0., 100.), (10., 110.), (20., 0.), (40., 10.)],
            vec![
                clip(&[(0., 0.), (10., 10.)]),
                clip(&[(100., 1000.), (110., 1010.)]),
            ],
            Some(Value::Double(7.)),
            false,
        )
        .unwrap();
        assert_eq!(
            p.sample_times(),
            &[0., 10., 10.909_090_909_090_91, 20., 40.]
        );
        for (t, v) in [(3., 1003.), (15., 550.), (24., 2.), (39., 9.5)] {
            assert!(
                (match value(&p, t).unwrap() {
                    Value::Double(v) => v,
                    _ => panic!(),
                } - v)
                    .abs()
                    < 1e-10
            );
        }
        let e = p.evaluate(15., InterpolationType::Linear).unwrap();
        assert_eq!((e.lower_clip, e.upper_clip), (1, 0));
    }
    #[test]
    fn jumps_reverse_constant_identity_and_timecode_shift() {
        let clips = vec![clip(&[(0., 0.), (10., 10.)])];
        let p = PreparedClipProperty::new(
            vec![(0., 0)],
            vec![(0., 0.), (10., 10.), (10., 0.), (20., 10.)],
            clips.clone(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(p.sample_times(), &[0., 10. - CLIP_SAFE_STEP, 10., 20.]);
        assert_eq!(value(&p, 9.5), Some(Value::Double(9.5)));
        assert_eq!(value(&p, 10.), Some(Value::Double(0.)));
        let p = PreparedClipProperty::new(
            vec![(0., 0)],
            vec![(20., 0.), (10., 10.), (0., 0.)],
            clips.clone(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(value(&p, 15.), Some(Value::Double(5.)));
        let p = PreparedClipProperty::new(
            vec![(0., 0)],
            vec![(0., 5.), (10., 5.)],
            clips.clone(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(value(&p, -1.), Some(Value::Double(5.)));
        let p = PreparedClipProperty::new(vec![(20., 0)], vec![], clips, None, false).unwrap();
        assert_eq!(p.sample_times(), &[0., 10., 20.]);
        assert_eq!(value(&p, 5.), Some(Value::Double(5.)));
        let tc = Some(Arc::new(PropertySpec::attribute().with_time_samples(vec![
            (0., Value::TimeCode(10.)),
            (20., Value::TimeCode(30.)),
        ])));
        let p = PreparedClipProperty::new(
            vec![(0., 0)],
            vec![(0., 0.), (10., 20.)],
            vec![tc],
            None,
            false,
        )
        .unwrap();
        assert_eq!(value(&p, 5.), Some(Value::TimeCode(15.)));
    }
    #[test]
    fn raw_defaults_ignored_and_invalid_inputs_reported() {
        let property = Some(Arc::new(
            PropertySpec::attribute().with_default(Value::Double(999.)),
        ));
        let p =
            PreparedClipProperty::new(vec![(0., 0)], vec![], vec![property], None, false).unwrap();
        assert_eq!(value(&p, 0.), None);
        assert_eq!(
            PreparedClipProperty::new(vec![], vec![], vec![], None, false).unwrap_err(),
            ClipEvalError::EmptySchedule
        );
        assert_eq!(
            PreparedClipProperty::new(vec![(0., 1)], vec![], vec![None], None, false).unwrap_err(),
            ClipEvalError::InvalidActivation
        );
        assert_eq!(
            PreparedClipProperty::new(vec![(0., 0), (0., 0)], vec![], vec![None], None, false)
                .unwrap_err(),
            ClipEvalError::InvalidActivation
        );
        assert_eq!(
            PreparedClipProperty::new(
                vec![(0., 0)],
                vec![(1., 0.), (1., 1.), (1., 2.)],
                vec![None],
                None,
                false
            )
            .unwrap_err(),
            ClipEvalError::InvalidTimeMapping
        );
        assert_eq!(
            PreparedClipProperty::new(
                vec![(0., 0)],
                vec![],
                vec![Some(Arc::new(PropertySpec {
                    time_samples: Some(
                        vec![(1., Value::Double(1.)), (0., Value::Double(0.))].into()
                    ),
                    ..PropertySpec::attribute()
                }))],
                None,
                false
            )
            .unwrap_err(),
            ClipEvalError::InvalidSamples
        );
        assert_eq!(
            p.evaluate(f64::NAN, InterpolationType::Linear).unwrap_err(),
            ClipEvalError::InvalidQuery
        );
    }
    fn spline_clip(start: f64, end: f64) -> Option<Arc<PropertySpec>> {
        use crate::spline::{
            CurveType, Extrapolation, Knot, KnotInterp, SplineData, SplineDataType,
        };
        let knot = |time| Knot {
            time,
            value: time,
            pre_value: None,
            next_interp: KnotInterp::Linear,
            curve_type: CurveType::Bezier,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 0.,
            post_tan_width: 0.,
            pre_tan_slope: 0.,
            post_tan_slope: 0.,
        };
        Some(Arc::new(PropertySpec::attribute().with_spline(
            SplineData {
                data_type: SplineDataType::Double,
                default_curve_type: CurveType::Bezier,
                pre_extrapolation: Extrapolation::Held,
                post_extrapolation: Extrapolation::Held,
                loop_params: None,
                knots: vec![knot(start), knot(end)],
            },
        )))
    }
    #[test]
    fn spline_gap_manifest_defaults_and_stage_held_match_cpp() {
        let active = vec![(0., 0), (10., 1), (20., 2)];
        let clips = vec![spline_clip(0., 10.), None, spline_clip(20., 30.)];
        for (default, interpolate, expected) in [
            (None, false, None),
            (None, true, Some(Value::Double(15.))),
            (Some(Value::Double(100.)), true, Some(Value::Double(100.))),
            (Some(Value::Blocked), true, Some(Value::Double(15.))),
            (Some(Value::Blocked), false, None),
        ] {
            let p = PreparedClipProperty::new_spline(
                active.clone(),
                vec![],
                clips.clone(),
                default,
                interpolate,
            )
            .unwrap();
            assert!(p.is_spline());
            assert!(p.sample_times().is_empty());
            assert_eq!(
                p.evaluate(5., InterpolationType::Held).unwrap().value,
                Some(Value::Double(5.))
            );
            assert_eq!(value(&p, 15.), expected);
        }
    }
    #[test]
    fn manifest_activation_blocks_preserve_repeated_assets_and_spline_mode() {
        // The same slot is excluded at time 10 but remains usable at time 30.
        let p = PreparedClipProperty::new(
            vec![(0., 0), (10., 1), (20., 2), (30., 1)],
            vec![],
            vec![
                clip(&[(0., 0.)]),
                clip(&[(10., 100.), (30., 300.)]),
                clip(&[(20., 20.)]),
            ],
            None,
            true,
        )
        .unwrap()
        .with_manifest_blocks(vec![10.])
        .unwrap();
        assert_eq!(p.sample_times(), &[0., 20., 30.]);
        assert_eq!(value(&p, 5.), Some(Value::Double(5.)));
        assert_eq!(value(&p, 15.), Some(Value::Double(15.)));
        assert_eq!(value(&p, 30.), Some(Value::Double(300.)));
        let p = PreparedClipProperty::new_spline(
            vec![(0., 0), (10., 1), (20., 2)],
            vec![],
            vec![
                spline_clip(0., 10.),
                spline_clip(10., 20.),
                spline_clip(20., 30.),
            ],
            None,
            false,
        )
        .unwrap()
        .with_manifest_blocks(vec![10.])
        .unwrap();
        assert_eq!(value(&p, 15.), None);
    }
    #[test]
    fn mapped_timecode_held_and_exterior_queries_shift_sample_endpoints() {
        let p = PreparedClipProperty::new(
            vec![(0., 0)],
            vec![(0., 100.), (10., 120.)],
            vec![Some(Arc::new(PropertySpec::attribute().with_time_samples(
                vec![
                    (100., Value::TimeCode(1000.)),
                    (120., Value::TimeCode(1020.)),
                ],
            )))],
            None,
            false,
        )
        .unwrap();
        for (t, linear, held) in [
            (-5., 900., 900.),
            (2., 902., 900.),
            (5., 905., 900.),
            (15., 910., 910.),
        ] {
            assert_eq!(value(&p, t), Some(Value::TimeCode(linear)));
            assert_eq!(
                p.evaluate(t, InterpolationType::Held).unwrap().value,
                Some(Value::TimeCode(held))
            );
        }
    }
    #[test]
    fn spline_gap_left_limit_uses_preceding_reversed_mapping_section() {
        // Pinned OpenUSD26.8: a gap beginning at a reversed-to-forward mapping
        // knot approaches the source knot from the right, so its pre-value -10
        // is not used. Native values at external10/15/19 are 0/5/9.
        let mut a = (*spline_clip(0., 10.).unwrap()).clone();
        a.spline.as_mut().unwrap().knots[0].pre_value = Some(-10.);
        let p = PreparedClipProperty::new_spline(
            vec![(0., 0), (10., 1), (20., 2)],
            vec![(0., 10.), (10., 0.), (20., 10.)],
            vec![Some(Arc::new(a)), None, spline_clip(0., 10.)],
            None,
            true,
        )
        .unwrap();
        for (t, v) in [(10., 0.), (15., 5.), (19., 9.)] {
            assert_eq!(value(&p, t), Some(Value::Double(v)));
        }
    }
    #[test]
    fn spline_invalid_and_unsupported_sources_are_explicit() {
        use crate::spline::{Extrapolation, KnotInterp};
        let prepare = |p| {
            PreparedClipProperty::new_spline(
                vec![(0., 0)],
                vec![],
                vec![Some(Arc::new(p))],
                None,
                false,
            )
        };
        let mut p = (*spline_clip(0., 10.).unwrap()).clone();
        p.spline.as_mut().unwrap().knots[0].value = f64::NAN;
        assert_eq!(prepare(p).unwrap_err(), ClipEvalError::InvalidSpline);
        let mut p = (*spline_clip(0., 10.).unwrap()).clone();
        p.spline.as_mut().unwrap().post_extrapolation = Extrapolation::LoopRepeat;
        assert_eq!(prepare(p).unwrap_err(), ClipEvalError::UnsupportedSpline);
        let mut p = (*spline_clip(0., 10.).unwrap()).clone();
        let spline = p.spline.as_mut().unwrap();
        spline.knots[0].next_interp = KnotInterp::Curve;
        spline.knots[0].post_tan_width = 15.;
        assert_eq!(prepare(p).unwrap_err(), ClipEvalError::UnsupportedSpline);
    }
    #[test]
    fn endpoint_origins_track_manifest_defaults_and_blocked_spline_activations() {
        let p = PreparedClipProperty::new(
            vec![(0., 0), (10., 1)],
            vec![],
            vec![clip(&[(0., 0.)]), None],
            Some(Value::Double(42.)),
            false,
        )
        .unwrap();
        let e = p.evaluate(5., InterpolationType::Linear).unwrap();
        assert!(!e.lower_from_manifest);
        assert!(e.upper_from_manifest);
        let p = PreparedClipProperty::new_spline(
            vec![(0., 0), (10., 0)],
            vec![],
            vec![spline_clip(0., 20.)],
            Some(Value::Double(42.)),
            true,
        )
        .unwrap()
        .with_manifest_blocks(vec![0.])
        .unwrap();
        let e = p.evaluate(5., InterpolationType::Linear).unwrap();
        assert_eq!(e.value, Some(Value::Double(42.)));
        assert!(e.lower_from_manifest && e.upper_from_manifest);
        let e = p.evaluate(15., InterpolationType::Linear).unwrap();
        assert_eq!(e.value, Some(Value::Double(15.)));
        assert!(!e.lower_from_manifest && !e.upper_from_manifest);
    }
    #[test]
    fn sparse_sample_metadata_uses_exact_reversed_samples_and_manifest_fallback() {
        let edit = Value::ArrayEdit(crate::ArrayEdit::default());
        let source = Some(Arc::new(PropertySpec::attribute().with_time_samples(vec![
            (0., edit.clone()),
            (10., Value::Array(vec![Value::Double(10.)])),
        ])));
        let prepare = |default, interpolate| {
            PreparedClipProperty::new(
                vec![(0., 0), (5., 0), (10., 0)],
                vec![(0., 10.), (5., 5.), (10., 0.)],
                vec![source.clone()],
                default,
                interpolate,
            )
            .unwrap()
        };
        let p = prepare(None, false);
        assert!(!p.sample_is_sparse(0.));
        assert!(!p.sample_is_sparse(5.)); // Internal 5 has no exact raw sample.
        assert!(p.sample_is_sparse(10.));
        assert!(
            p.evaluate(5., InterpolationType::Held)
                .unwrap()
                .value
                .unwrap()
                .array_edit_ref()
                .is_some()
        );
        let p = prepare(None, true).with_manifest_blocks(vec![5.]).unwrap();
        assert!(!p.sample_times().contains(&5.));
        assert!(!p.sample_is_sparse(5.));
        assert!(p.sample_is_sparse(10.));
        let p = prepare(Some(edit.clone()), false);
        assert!(p.sample_is_sparse(5.)); // Missing exact raw type uses manifest kind.
        assert!(!p.sample_is_sparse(0.)); // Exact dense type takes precedence.
        let p = PreparedClipProperty::new(vec![(0., 0)], vec![], vec![None], Some(edit), false)
            .unwrap();
        assert!(p.sample_is_sparse(-0.));
        assert!(!p.sample_is_sparse(f64::NAN));
    }
}
