// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! IEEE 754 binary16 (`half`) conversions, as OpenUSD's `GfHalf` does them.
//!
//! [`Value::Half`](crate::Value::Half) holds the bits of a half. OpenUSD
//! converts to half only from `float` (`half::half(float)`,
//! `pxr/base/gf/ilmbase_half.h`): a `double`, such as a USDA literal
//! (`Sdf_ParserHelpers::MakeScalarValueImpl`), is narrowed to `float` first.
//! [`from_f32`] reproduces that conversion bit for bit, and [`from_f64`]
//! narrows through `f32` the same way.
//!
//! Spec: AOUSD Core §6.3 (`half`, IEEE 754 binary16).

/// Widens half bits to `f32`, exactly.
#[must_use]
pub fn to_f32(bits: u16) -> f32 {
    let negative = bits & 0x8000 != 0;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let mantissa = u32::from(bits & 0x3ff);
    let magnitude = match exponent {
        // Subnormal halves are `mantissa * 2^-24`, normal in `f32`.
        0 => {
            #[allow(clippy::cast_precision_loss, reason = "mantissa has 10 bits")]
            let value = mantissa as f32 * (1.0 / 16_777_216.0);
            return if negative { -value } else { value };
        }
        0x1f => 0x7f80_0000 | (mantissa << 13),
        _ => ((exponent + 112) << 23) | (mantissa << 13),
    };
    f32::from_bits((u32::from(negative) << 31) | magnitude)
}

/// Narrows an `f32` to half bits as `GfHalf` does (`half::convert`,
/// `pxr/base/gf/ilmbase_half.cpp`).
///
/// Rounds to nearest, ties to even; keeps subnormal halves; values too large
/// for a half become infinity; a NaN keeps its sign and the top ten bits of
/// its significand, with the lowest bit set when those are all zero, so that
/// it stays a NaN.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bit manipulation of IEEE 754 fields"
)]
pub fn from_f32(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - (127 - 15);
    let mantissa = bits & 0x007f_ffff;
    if exponent <= 0 {
        // Below the smallest subnormal's half-way point, zero.
        if exponent < -10 {
            return sign;
        }
        // A subnormal half; rounding may carry into the smallest normal.
        let significand = mantissa | 0x0080_0000;
        let shift = (14 - exponent) as u32;
        let rounded =
            (significand + (1 << (shift - 1)) - 1 + ((significand >> shift) & 1)) >> shift;
        return sign | rounded as u16;
    }
    if exponent == 0xff - (127 - 15) {
        if mantissa == 0 {
            return sign | 0x7c00;
        }
        let top = (mantissa >> 13) as u16;
        return sign | 0x7c00 | top | u16::from(top == 0);
    }
    // A carry out of the significand increments the exponent.
    let rounded = mantissa + 0x0fff + ((mantissa >> 13) & 1);
    let (exponent, rounded) = if rounded & 0x0080_0000 != 0 {
        (exponent + 1, 0)
    } else {
        (exponent, rounded)
    };
    if exponent > 30 {
        return sign | 0x7c00;
    }
    sign | ((exponent as u16) << 10) | (rounded >> 13) as u16
}

/// Narrows an `f64` to half bits through `f32`, as OpenUSD narrows a
/// `double` to `GfHalf`.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    reason = "OpenUSD narrows to float before half"
)]
pub fn from_f64(value: f64) -> u16 {
    from_f32(value as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every half widens and narrows back to itself (NaNs stay NaNs).
    #[test]
    fn every_half_round_trips() {
        for bits in 0..=u16::MAX {
            let widened = to_f32(bits);
            if widened.is_nan() {
                assert!(to_f32(from_f32(widened)).is_nan(), "{bits:#06x}");
            } else {
                assert_eq!(from_f32(widened), bits, "{bits:#06x}");
            }
        }
    }

    /// Between every pair of adjacent finite halves of the same sign: values
    /// just below the midpoint round down, just above round up, and the
    /// midpoint itself rounds to the even one. Above the largest half, the
    /// midpoint towards the next power of two overflows to infinity.
    #[test]
    fn every_midpoint_rounds_to_nearest_even() {
        for sign in [0_u16, 0x8000] {
            for magnitude in 0..0x7bff_u16 {
                let (low, high) = (sign | magnitude, sign | (magnitude + 1));
                let (a, b) = (f64::from(to_f32(low)), f64::from(to_f32(high)));
                #[allow(clippy::cast_possible_truncation, reason = "exact in f32")]
                let mid = ((a + b) / 2.0) as f32;
                let even = if magnitude & 1 == 0 { low } else { high };
                assert_eq!(from_f32(mid), even, "midpoint of {low:#06x}");
                // One `f32` step towards zero, and away from it.
                let toward_low = f32::from_bits(mid.to_bits() - 1);
                let toward_high = f32::from_bits(mid.to_bits() + 1);
                assert_eq!(from_f32(toward_low), low, "below midpoint of {low:#06x}");
                assert_eq!(from_f32(toward_high), high, "above midpoint of {low:#06x}");
            }
        }
        // 65520 is the midpoint between the largest half and 2^16.
        assert_eq!(from_f32(65_519.996), 0x7bff);
        assert_eq!(from_f32(65_520.0), 0x7c00);
        assert_eq!(from_f32(-1e9), 0xfc00);
    }

    #[test]
    fn nans_and_infinities() {
        assert_eq!(from_f32(f32::INFINITY), 0x7c00);
        assert_eq!(from_f32(f32::NEG_INFINITY), 0xfc00);
        assert_eq!(from_f32(f32::NAN), 0x7e00);
        // A NaN whose top ten significand bits are zero keeps a set bit.
        assert_eq!(from_f32(f32::from_bits(0x7f80_0001)), 0x7c01);
        assert_eq!(from_f32(f32::from_bits(0xff80_2000)), 0xfc01);
        assert_eq!(from_f64(f64::NAN), 0x7e00);
    }

    /// `from_f64` narrows to `f32` first, as OpenUSD does: a double just
    /// above a half midpoint that is not representable in `f32` rounds to the
    /// midpoint, then to even.
    #[test]
    fn doubles_narrow_through_float() {
        let midpoint = 1.0 + 2_f64.powi(-11);
        assert_eq!(from_f64(midpoint + 2_f64.powi(-40)), 0x3c00);
        assert_eq!(from_f64(midpoint + 2_f64.powi(-20)), 0x3c01);
        assert_eq!(from_f64(0.1), 0x2e66);
        assert_eq!(from_f64(5.960_464_477_539_063e-8), 0x0001);
    }
}
