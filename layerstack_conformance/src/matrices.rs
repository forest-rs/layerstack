// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Comparing layerstack's matrices with OpenUSD's.
//!
//! Both compute each transform op and product as OpenUSD's `Gf` does, step
//! for step; they differ only where the platform's trigonometry rounds
//! differently and where the C++ compiler fuses multiply-adds, a few units
//! in the last place per step. [`TOLERANCE`], relative to the matrix's
//! scale, allows that and nothing a wrong op, order or sign would give.

/// The largest difference allowed, relative to the matrix's scale.
pub const TOLERANCE: f64 = 1e-12;

/// How far `got` is from `expected`, relative to the scale of `expected`
/// (its largest finite entry, at least 1): infinite when either has a
/// non-finite entry the other does not have in the same place (folding with
/// `f64::max` would drop a NaN).
#[must_use]
pub fn relative_error(got: &[[f64; 4]; 4], expected: &[[f64; 4]; 4]) -> f64 {
    let scale = expected
        .iter()
        .flatten()
        .filter(|v| v.is_finite())
        .fold(1.0_f64, |m, v| m.max(v.abs()));
    let mut worst = 0.0_f64;
    for (a, b) in got.iter().flatten().zip(expected.iter().flatten()) {
        let e = if a.is_finite() && b.is_finite() {
            (a - b).abs() / scale
        } else if (a.is_nan() && b.is_nan()) || a == b {
            0.0
        } else {
            f64::INFINITY
        };
        if e > worst {
            worst = e;
        }
    }
    worst
}

/// Whether `got` agrees with `expected` to within [`TOLERANCE`].
#[must_use]
pub fn agree(got: &[[f64; 4]; 4], expected: &[[f64; 4]; 4]) -> bool {
    relative_error(got, expected) <= TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [[f64; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    /// The comparator fails on a NaN or infinity the other side lacks.
    #[test]
    fn non_finite_entries_fail_unless_matched() {
        let nan = [[f64::NAN; 4]; 4];
        assert_eq!(relative_error(&IDENTITY, &IDENTITY), 0.0, "equal");
        assert!(!agree(&nan, &IDENTITY), "NaN got");
        assert!(!agree(&IDENTITY, &nan), "NaN expected");
        assert_eq!(relative_error(&nan, &nan), 0.0, "NaN in the same places");
        let mut infinite = IDENTITY;
        infinite[3][0] = f64::INFINITY;
        assert!(!agree(&infinite, &IDENTITY), "infinity got");
        assert!(!agree(&IDENTITY, &infinite), "infinity expected");
        assert_eq!(
            relative_error(&infinite, &infinite),
            0.0,
            "the same infinity"
        );
        let mut negative = IDENTITY;
        negative[3][0] = f64::NEG_INFINITY;
        assert!(!agree(&infinite, &negative), "opposite infinities");
    }

    /// Differences are relative to the expected matrix's scale.
    #[test]
    fn differences_are_relative_to_the_scale() {
        let mut off = IDENTITY;
        off[3][0] = 1e-9;
        assert!(!agree(&off, &IDENTITY), "a small difference");
        let mut big = IDENTITY;
        big[3][0] = 1e6;
        let mut near = big;
        near[3][0] += 1e-7;
        assert!(agree(&near, &big), "within 1e-12 of 1e6");
    }
}
