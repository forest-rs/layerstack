// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reads over an attribute's composed sample times: bracketing, counts,
//! intervals and unions.
//!
//! Every read here is a pure function of the sorted, unique stage-time
//! grid [`Stage::property_sample_times`] composes (layer offsets, blocks,
//! sparse edits and value clips already applied), so an attribute and an
//! [`AttributeQuery`](super::AttributeQuery) holding that grid answer the
//! same.
//!
//! OpenUSD 26.11: `UsdAttribute::GetBracketingTimeSamples`,
//! `GetNumTimeSamples`, `GetUnionedTimeSamples(InInterval)` and their
//! `UsdAttributeQuery` twins (`pxr/usd/usd/attribute.cpp`,
//! `attributeQuery.cpp`), over `UsdStage::_GetBracketingTimeSamples` and
//! `_GetTimeSamplesInInterval` (`pxr/usd/usd/stage.cpp`).
//!
//! Spec: AOUSD Core §12.3.2 (time samples, their stage-time mapping and
//! interpolation brackets), §12.3.4 (value clips), §12.3.6 (blocks).

use alloc::vec::Vec;
use core::ops::{Bound, RangeBounds};

use super::{SampleBracket, Stage, TimeInfo};
use crate::{interner::TokenId, path::PathId};

/// The samples bracketing a stage time, or why there are none.
///
/// OpenUSD answers `GetBracketingTimeSamples` with a success flag and
/// `hasTimeSamples`; the three variants are its three outcomes.
///
/// Spec: AOUSD Core §12.3.2 (interpolation between bracketing samples).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(alias = "UsdAttribute::GetBracketingTimeSamples")]
pub enum BracketingSamples {
    /// The attribute has time samples: the nearest at or below the time and
    /// the nearest at or above it, both the time itself on a sample, and
    /// both the first (last) sample before the first (after the last).
    /// OpenUSD: `true`, `hasTimeSamples` set.
    Samples(SampleBracket),
    /// A value exists without time samples: a default, a schema fallback,
    /// or nothing authored. OpenUSD: `true`, `hasTimeSamples` unset.
    NoSamples,
    /// No sample or default supplies a value: a block or an animation
    /// block ends resolution first, or a spline does (its knots are not
    /// sample times, and OpenUSD's bracketing stops at it), or the time is
    /// NaN, which is OpenUSD's default time (a coding error there).
    /// OpenUSD: `false`.
    NoValue,
}

impl BracketingSamples {
    /// The bracketing samples, when there are any.
    #[must_use]
    pub fn bracket(self) -> Option<SampleBracket> {
        match self {
            Self::Samples(bracket) => Some(bracket),
            Self::NoSamples | Self::NoValue => None,
        }
    }
}

impl TimeInfo {
    /// The bracketing samples of `time` in this grid.
    pub(crate) fn bracketing(&self, time: f64) -> BracketingSamples {
        if time.is_nan() {
            return BracketingSamples::NoValue;
        }
        match bracket_in(&self.times, time) {
            Some(bracket) => BracketingSamples::Samples(bracket),
            None if self.has_value => BracketingSamples::NoSamples,
            None => BracketingSamples::NoValue,
        }
    }
}

/// The samples of the sorted, unique `times` bracketing `time`, `None` when
/// `times` is empty. `time` is not NaN.
///
/// OpenUSD `SdfLayer::GetBracketingTimeSamplesForPath` clamps to the end
/// samples outside the grid; `_BracketingSamplesResolver` keeps, over every
/// contributing series, the nearest bounds, which are the composed grid's.
fn bracket_in(times: &[f64], time: f64) -> Option<SampleBracket> {
    let (&first, &last) = (times.first()?, times.last()?);
    let (lower, upper) = if time <= first {
        (first, first)
    } else if time >= last {
        (last, last)
    } else {
        // `first < time < last`, so `0 < at < len`.
        let at = times.partition_point(|&t| t < time);
        if times[at] == time {
            (time, time)
        } else {
            (times[at - 1], times[at])
        }
    };
    Some(SampleBracket { lower, upper })
}

/// The contiguous run of the sorted `times` inside `interval`: open,
/// closed or unbounded ends; a NaN bound or a reversed interval selects
/// none. OpenUSD: `GfInterval` and `Usd_CopyTimeSamplesInInterval`.
pub(crate) fn times_in_range<'t>(times: &'t [f64], interval: &impl RangeBounds<f64>) -> &'t [f64] {
    let nan =
        |bound: Bound<&f64>| matches!(bound, Bound::Included(v) | Bound::Excluded(v) if v.is_nan());
    if nan(interval.start_bound()) || nan(interval.end_bound()) {
        return &[];
    }
    let start = match interval.start_bound() {
        Bound::Included(&lo) => times.partition_point(|&t| t < lo),
        Bound::Excluded(&lo) => times.partition_point(|&t| t <= lo),
        Bound::Unbounded => 0,
    };
    let end = match interval.end_bound() {
        Bound::Included(&hi) => times.partition_point(|&t| t <= hi),
        Bound::Excluded(&hi) => times.partition_point(|&t| t < hi),
        Bound::Unbounded => times.len(),
    };
    times.get(start..end).unwrap_or_default()
}

/// Merges the sorted, unique `more` into the sorted, unique `times`.
///
/// OpenUSD `Usd_MergeTimeSamples` (`pxr/usd/usd/attribute.cpp`), a sorted
/// set union.
pub(crate) fn merge_times(times: &mut Vec<f64>, more: &[f64]) {
    if more.is_empty() {
        return;
    }
    if times.is_empty() {
        times.extend_from_slice(more);
        return;
    }
    let mut merged = Vec::with_capacity(times.len() + more.len());
    let (mut a, mut b) = (times.as_slice(), more);
    while let (Some(&x), Some(&y)) = (a.first(), b.first()) {
        if x < y {
            merged.push(x);
            a = &a[1..];
        } else if y < x {
            merged.push(y);
            b = &b[1..];
        } else {
            merged.push(x);
            a = &a[1..];
            b = &b[1..];
        }
    }
    merged.extend_from_slice(a);
    merged.extend_from_slice(b);
    *times = merged;
}

impl Stage {
    /// The samples bracketing stage time `time`, as
    /// [`Attribute::bracketing_samples`](super::Attribute::bracketing_samples)
    /// documents.
    pub(crate) fn property_bracketing_samples(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
    ) -> BracketingSamples {
        if time.is_nan() {
            return BracketingSamples::NoValue;
        }
        self.property_time_info(prim, field).bracketing(time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn pair(times: &[f64], time: f64) -> Option<(f64, f64)> {
        bracket_in(times, time).map(|b| (b.lower, b.upper))
    }

    #[test]
    fn brackets_clamp_to_the_ends_and_collapse_on_a_sample() {
        let times = [1.0, 3.0, 5.0];
        assert_eq!(pair(&times, -1.0), Some((1.0, 1.0)));
        assert_eq!(pair(&times, 1.0), Some((1.0, 1.0)));
        assert_eq!(pair(&times, 2.0), Some((1.0, 3.0)));
        assert_eq!(pair(&times, 3.0), Some((3.0, 3.0)));
        assert_eq!(pair(&times, 4.5), Some((3.0, 5.0)));
        assert_eq!(pair(&times, 9.0), Some((5.0, 5.0)));
        assert_eq!(pair(&times, f64::INFINITY), Some((5.0, 5.0)));
        assert_eq!(pair(&times, f64::NEG_INFINITY), Some((1.0, 1.0)));
        assert_eq!(pair(&[4.0], 0.0), Some((4.0, 4.0)));
        assert_eq!(pair(&[], 0.0), None);
    }

    #[test]
    fn ranges_select_a_contiguous_run() {
        let times = [1.0, 2.0, 3.0, 5.0];
        assert_eq!(times_in_range(&times, &(2.0..=5.0)), [2.0, 3.0, 5.0]);
        assert_eq!(times_in_range(&times, &(2.0..5.0)), [2.0, 3.0]);
        let open = (Bound::Excluded(2.0), Bound::Excluded(5.0));
        assert_eq!(times_in_range(&times, &open), [3.0]);
        assert_eq!(times_in_range(&times, &(..=2.0)), [1.0, 2.0]);
        assert_eq!(times_in_range(&times, &(..)), times);
        assert!(times_in_range(&times, &(5.0..=2.0)).is_empty());
        assert!(times_in_range(&times, &(f64::NAN..=5.0)).is_empty());
        assert!(times_in_range(&times, &(1.0..=f64::NAN)).is_empty());
    }

    #[test]
    fn merges_are_sorted_unions() {
        let mut times = vec![1.0, 3.0, 5.0];
        merge_times(&mut times, &[2.0, 3.0, 7.0]);
        assert_eq!(times, [1.0, 2.0, 3.0, 5.0, 7.0]);
        merge_times(&mut times, &[]);
        assert_eq!(times, [1.0, 2.0, 3.0, 5.0, 7.0]);
        let mut empty = Vec::new();
        merge_times(&mut empty, &[4.0]);
        assert_eq!(empty, [4.0]);
    }
}
