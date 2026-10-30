// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Floating-point numbers spelled as OpenUSD spells them in text: the one
//! implementation behind `layerstack_usda`'s writer, `layerstack_schemas`'
//! sdr metadata strings and path-expression predicate arguments. Public
//! only with the `unstable-internals` feature.
//!
//! OpenUSD writes a value with `TfStringify` or its stream operator
//! (`Sdf_FileIOUtility::StringFromVtValue`, `pxr/usd/sdf/fileIO_Common.cpp`),
//! and the two kinds of floating-point number take different routes:
//!
//! - `double` and `float` values, alone or in vectors, quaternions,
//!   matrices, arrays and dictionaries, and time-sample times, layer
//!   offsets and spline times, go through
//!   `Tf_ApplyDoubleToStringConverter` (`pxr/base/tf/stringUtils.cpp`,
//!   `TfStreamDouble` and `TfStreamFloat` in `Gf_OstreamHelperP` and
//!   `VtStreamOut`): double-conversion's `ToShortest` or
//!   `ToShortestSingle` with no flags and `decimal_in_shortest_low = -6`,
//!   `decimal_in_shortest_high = 15`. The digits are the fewest that read
//!   back as the same number, the even candidate where two are equally
//!   close; with a decimal exponent in `[-6, 15)` they
//!   are written positionally (`0.000001`, `100000000000000`), otherwise
//!   as `d.ddde-7` or `1e15`, with no `+` and no padding. An integral
//!   value has no point (`1`), negative zero is `-0`, and the specials are
//!   `inf`, `-inf` and `nan`, whatever the sign of the NaN.
//! - `half` values stream as the `float` they widen to
//!   (`operator<<(ostream&, half)`, `pxr/base/gf/ilmbase_half.cpp`), and
//!   `timecode` values as their `double` (`operator<<(ostream&,
//!   GfTimeCode)`, `pxr/base/gf/timeCode.cpp`), both with the stream's
//!   default formatting: `%g` with six significant digits, so trailing
//!   zeros dropped and an exponent of at least two digits, signed, when
//!   the decimal exponent is below -4 or six or more (`1.00136e-05`,
//!   `1.23457e+06`). The C library spells the specials: `inf`, `-inf`
//!   and `nan`, but glibc's `-nan` for a NaN with its sign bit set.
//!
//! Six digits are always enough for a `half`, so its spelling reads back
//! as the same half. They are not enough for every `timecode`: OpenUSD's
//! text loses the rest (`1.2345678` is written `1.23457`). A time code is
//! written as OpenUSD writes it whenever that text reads back as the same
//! number, and otherwise with the `double` spelling, which does.
//!
//! Spec: AOUSD Core §16.2.5 (numeric literals).

use alloc::string::String;
use core::fmt::{self, Write as _};

/// Appends `v` as OpenUSD writes a `double`.
#[inline]
pub fn push_double(out: &mut String, v: f64) {
    // Most doubles: of a positional magnitude, and `k * 2^q` for an odd `k`
    // with `q` below -25, so no tie is possible ([`may_tie`]). The range
    // holds no subnormal, so `q` is at most the exponent less 1075 plus
    // the trailing zeros.
    let bits = v.to_bits() & !(1 << 63);
    if POSITIONAL_F64.contains(&bits) && (bits >> 52) + u64::from(bits.trailing_zeros()) < 1050 {
        push_display(out, v);
    } else {
        push_shortest(out, v, v, 1e15, 17);
    }
}

/// Appends `v` as OpenUSD writes a `float`: the fewest digits that read
/// back as the same `float`.
#[inline]
pub fn push_float(out: &mut String, v: f32) {
    // Most floats: of a positional magnitude, and `k * 2^q` for an odd `k`
    // with `q` below -14, so no tie is possible ([`may_tie`]). The range
    // holds no subnormal, so `q` is at most the exponent less 150 plus the
    // trailing zeros.
    let bits = v.to_bits() & !(1 << 31);
    if POSITIONAL_F32.contains(&bits) && (bits >> 23) + bits.trailing_zeros() < 136 {
        push_display(out, v);
    } else {
        push_shortest(out, f64::from(v), v, 16_777_216.0, 9);
    }
}

/// The bits of the magnitudes [`is_positional`] accepts, which order as
/// the magnitudes do.
const POSITIONAL_F64: core::ops::Range<u64> =
    1.000_001e-6_f64.to_bits() + 1..0.999_999e15_f64.to_bits();
const POSITIONAL_F32: core::ops::Range<u32> =
    1.000_001e-6_f32.to_bits() + 1..0.999_999e15_f32.to_bits();

/// The fewest digits that read back as `v`, `wide` as a `f64`, of at most
/// `digits` digits, as double-conversion's `ToShortest` writes them.
///
/// An integer below `integers` is its digits: below 10^15 for a `double`,
/// written positionally, whose steps there are at most 1/8, so that no
/// shorter decimal reads back as it; below 2^24 for a `float`, whose steps
/// there are at most 1. Otherwise Rust's `Display` has the digits and the
/// layout wherever they are surely positional and no tie can break them
/// apart ([`may_tie`]); the rest is laid out here from Rust's exponent
/// form, a tie broken to the even candidate.
#[cold]
#[inline(never)]
fn push_shortest<T>(out: &mut String, wide: f64, v: T, integers: f64, digits: u32)
where
    T: fmt::Display + fmt::LowerExp,
{
    if !wide.is_finite() {
        return push_non_finite(out, wide);
    }
    let Some((k, q)) = odd_significand(wide) else {
        out.push_str(if wide.is_sign_negative() { "-0" } else { "0" });
        return;
    };
    let magnitude = wide.abs();
    if q >= 0 && magnitude < integers {
        // `k << q` is below 10^15.
        let integer = i64::try_from(k << q).unwrap_or(i64::MAX);
        return push_integer(out, if wide < 0.0 { -integer } else { integer });
    }
    if is_positional(magnitude) && !may_tie(k, q, digits) {
        return push_display(out, v);
    }
    let mut decimal = Digits::read(format_args!("{v:e}"));
    decimal.break_tie_to_even(wide);
    decimal.push_shortest(out);
}

/// Rust's `Display` of a float: the fewest digits that read back, as
/// double-conversion's, and positional, as double-conversion writes them
/// for a decimal exponent in `[-6, 15)`. The two differ only on a tie,
/// which the caller has ruled out ([`may_tie`]).
fn push_display(out: &mut String, v: impl fmt::Display) {
    let _ = write!(out, "{v}");
}

/// Whether the fewest digits of a number of magnitude `magnitude` surely
/// have a decimal exponent in `[-6, 15)`: they are within a part in 2^24
/// of it, so a margin of a part in 10^6 from either end is enough.
fn is_positional(magnitude: f64) -> bool {
    1.000_001e-6 < magnitude && magnitude < 0.999_999e15
}

/// Whether two candidates of the fewest digits can be equally close to
/// `v = k * 2^q`, `k` odd, whose fewest digits number at most `digits` (9
/// for a `float`, 17 for a `double`): only when `v` is exactly a decimal of at most
/// `digits + 1` digits ending in `5` ([`equals_decimal`]): `k * 5^-q` at
/// the place `q` for a negative `q`, and `k / 5^q` for another, which must
/// end in `5`.
fn may_tie(k: u64, q: i32, digits: u32) -> bool {
    let Some(&limit) = POWERS_OF_TEN.get(digits as usize + 1) else {
        return true;
    };
    match POWERS_OF_FIVE.get(q.unsigned_abs() as usize + usize::from(q >= 0)) {
        // Past the table, `k * 5^-q` has too many digits, and no `k` below
        // 2^53 is a multiple of `5^(q + 1)`.
        None => false,
        Some(&power) if q >= 0 => k.is_multiple_of(power),
        Some(&power) => k.checked_mul(power).is_some_and(|decimal| decimal < limit),
    }
}

/// `5^i` for every `i` a `u64` holds.
const POWERS_OF_FIVE: [u64; 28] = {
    let mut powers = [1; 28];
    let mut i = 1;
    while i < powers.len() {
        powers[i] = powers[i - 1] * 5;
        i += 1;
    }
    powers
};

/// `10^i` for every `i` a `u64` holds.
const POWERS_OF_TEN: [u64; 20] = {
    let mut powers = [1; 20];
    let mut i = 1;
    while i < powers.len() {
        powers[i] = powers[i - 1] * 10;
        i += 1;
    }
    powers
};

/// `v`'s magnitude as `k * 2^q` for an odd `k`; `None` for zero.
fn odd_significand(v: f64) -> Option<(u64, i32)> {
    let bits = v.to_bits();
    let exponent = i32::try_from((bits >> 52) & 0x7FF).unwrap_or(0);
    let fraction = bits & ((1 << 52) - 1);
    let (k, q) = if exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | 1 << 52, exponent - 1075)
    };
    if k == 0 {
        return None;
    }
    let zeros = k.trailing_zeros();
    Some((k >> zeros, q + i32::try_from(zeros).unwrap_or(0)))
}

/// Appends the half `bits` as OpenUSD writes a `half`: `%g`, six
/// significant digits, rounded half to even from the half's exact value.
pub fn push_half(out: &mut String, bits: u16) {
    let v = f64::from(crate::half::to_f32(bits));
    if !v.is_finite() {
        push_non_finite(out, v);
    } else if let Some(integer) = small_integer(v, 1e6) {
        push_integer(out, integer);
    } else if let Some((k, q)) = odd_significand(v) {
        // Not an integer, so `q` is negative: `v = k * 5^-q * 10^q`
        // exactly, `k` below 2^11 and `-q` at most 24.
        let exact = u128::from(k) * u128::from(POWERS_OF_FIVE[q.unsigned_abs() as usize]);
        Digits::rounded(v < 0.0, exact, q, 6).push_six_digit_layout(out);
    } else {
        // Negative zero, which `small_integer` leaves to keep its sign.
        out.push_str("-0");
    }
}

/// Appends `v` as OpenUSD writes a `timecode` (six significant digits,
/// `%g`) when that reads back as `v`, and as a `double` otherwise.
pub fn push_time_code(out: &mut String, v: f64) {
    if !v.is_finite() {
        push_non_finite(out, v);
    } else if let Some(integer) = small_integer(v, 1e6) {
        push_integer(out, integer);
    } else {
        let digits = Digits::read(format_args!("{v:e}"));
        // When the fewest digits that read back as `v` number six or
        // fewer, they are the six `%g` rounds to: no other six-digit
        // decimal is that close to `v`.
        if digits.len <= 6 {
            digits.push_six_digit_layout(out);
        } else {
            digits.push_shortest(out);
        }
    }
}

/// `v` as an integer when it is one of magnitude below `limit` (10^6,
/// where six significant digits are its digits); never negative zero,
/// which keeps its sign.
fn small_integer(v: f64, limit: f64) -> Option<i64> {
    if !(-limit < v && v < limit) || (v == 0.0 && v.is_sign_negative()) {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "`v` is below `limit` in magnitude"
    )]
    let integer = v as i64;
    #[allow(clippy::cast_precision_loss, reason = "`integer` is below `limit`")]
    let exact = integer as f64 == v;
    exact.then_some(integer)
}

fn push_integer(out: &mut String, v: i64) {
    let _ = write!(out, "{v}");
}

/// `inf`, `-inf` or `nan`: double-conversion writes a NaN without its
/// sign, and so does Apple's C library for a `half` or `timecode`, where
/// glibc writes `-nan` for a negative one. Either reads back as a NaN.
fn push_non_finite(out: &mut String, v: f64) {
    out.push_str(if v.is_nan() {
        "nan"
    } else if v < 0.0 {
        "-inf"
    } else {
        "inf"
    });
}

/// The decimal digits of a finite number and the power of ten of the
/// first: `-0.00123` is `-`, `123`, `-3`.
struct Digits {
    negative: bool,
    digits: [u8; DIGITS],
    len: usize,
    exponent: i32,
}

/// More digits than a `f64` written in full needs (17).
const DIGITS: usize = 24;

impl Digits {
    /// Reads Rust's exponent form (`-1.25e-7`, `0e0`), shortest or of a
    /// given precision, formatted on the stack.
    fn read(args: fmt::Arguments<'_>) -> Self {
        let mut text = StackText::default();
        let _ = text.write_fmt(args);
        let text = &text.bytes[..text.len];
        let (negative, text) = match text {
            [b'-', rest @ ..] => (true, rest),
            _ => (false, text),
        };
        let mut digits = Self {
            negative,
            digits: [b'0'; DIGITS],
            len: 0,
            exponent: 0,
        };
        let mut rest = text.iter();
        for &byte in rest.by_ref() {
            match byte {
                b'e' => break,
                b'.' => {}
                digit => {
                    digits.digits[digits.len] = digit;
                    digits.len += 1;
                }
            }
        }
        let mut exponent_negative = false;
        for &byte in rest {
            match byte {
                b'-' => exponent_negative = true,
                digit => digits.exponent = digits.exponent * 10 + i32::from(digit - b'0'),
            }
        }
        if exponent_negative {
            digits.exponent = -digits.exponent;
        }
        digits
    }

    /// The number `exact * 10^place` rounded to `significant` digits,
    /// half to even, trailing zeros dropped.
    fn rounded(negative: bool, exact: u128, place: i32, significant: u32) -> Self {
        // Below 2^64 in practice: a half's exact digits fit but for the
        // smallest subnormals.
        let (mut kept, length, place) = match u64::try_from(exact) {
            Ok(exact) => (
                exact,
                exact.checked_ilog10().map_or(1, |log| log + 1),
                place,
            ),
            Err(_) => {
                // Its first 19 digits, the last made odd when any digit
                // dropped is not zero, so the rounding below still tells
                // a value above halfway from one at it.
                let dropped = exact.ilog10() + 1 - 19;
                let divisor = 10_u128.pow(dropped);
                let (quotient, remainder) = (exact / divisor, exact % divisor);
                let kept = u64::try_from(quotient).unwrap_or(u64::MAX) | u64::from(remainder != 0);
                (kept, 19, place + i32::try_from(dropped).unwrap_or(0))
            }
        };
        let mut exponent = i32::try_from(length).unwrap_or(0) - 1 + place;
        if length > significant {
            let divisor = POWERS_OF_TEN[(length - significant) as usize];
            let (quotient, remainder) = (kept / divisor, kept % divisor);
            let half = divisor / 2;
            kept = quotient;
            if remainder > half || (remainder == half && quotient % 2 == 1) {
                kept += 1;
            }
            if kept == POWERS_OF_TEN[significant as usize] {
                kept /= 10;
                exponent += 1;
            }
        }
        let mut digits = Self {
            negative,
            digits: [b'0'; DIGITS],
            len: kept.checked_ilog10().map_or(1, |log| log as usize + 1),
            exponent,
        };
        for slot in digits.digits[..digits.len].iter_mut().rev() {
            // A single decimal digit.
            *slot = b'0' + u8::try_from(kept % 10).unwrap_or(0);
            kept /= 10;
        }
        digits.trim_trailing_zeros();
        digits
    }

    /// Where two candidates of the fewest digits are equally close to `v`
    /// (`3571017.25` as a `float`: `3571017.2` and `3571017.3`),
    /// double-conversion takes the even one (`GenerateShortestDigits` in
    /// `pxr/base/tf/pxrDoubleConversion/bignum-dtoa.cc`), where Rust's
    /// formatting takes the one above. Being halfway, `v` is exactly the
    /// decimal between them, `5` after the digits less one.
    fn break_tie_to_even(&mut self, v: f64) {
        // ASCII `'0'` is even, so a digit's byte has the digit's parity.
        if self.digits[self.len - 1].is_multiple_of(2) {
            return;
        }
        let digits = self.digits[..self.len]
            .iter()
            .fold(0_u64, |n, digit| n * 10 + u64::from(digit - b'0'));
        // Shortest digits number at most 17.
        let place = self.exponent - i32::try_from(self.len).unwrap_or(i32::MAX);
        if equals_decimal(v.abs(), digits * 10 - 5, place) {
            self.digits[self.len - 1] -= 1;
            self.trim_trailing_zeros();
        }
    }

    fn trim_trailing_zeros(&mut self) {
        while self.len > 1 && self.digits[self.len - 1] == b'0' {
            self.len -= 1;
        }
    }

    fn text(&self) -> &str {
        // Only ASCII digits are stored.
        core::str::from_utf8(&self.digits[..self.len]).unwrap_or_default()
    }

    /// double-conversion's `ToShortest` layout: positional for a decimal
    /// exponent in `[-6, 15)`, otherwise `d.ddde-7`.
    fn push_shortest(&self, out: &mut String) {
        if (-6..15).contains(&self.exponent) {
            self.push_positional(out);
        } else {
            self.push_exponential(out, false);
        }
    }

    /// `%g`'s layout: positional for a decimal exponent in `[-4, 6)`,
    /// otherwise `d.ddde-05`.
    fn push_six_digit_layout(&self, out: &mut String) {
        if (-4..6).contains(&self.exponent) {
            self.push_positional(out);
        } else {
            self.push_exponential(out, true);
        }
    }

    /// `123.45`, `0.00012` or `1200`: no point after the last digit.
    fn push_positional(&self, out: &mut String) {
        if self.negative {
            out.push('-');
        }
        let digits = self.text();
        let point = self.exponent + 1;
        match usize::try_from(point) {
            Err(_) | Ok(0) => {
                out.push_str("0.");
                for _ in 0..point.unsigned_abs() {
                    out.push('0');
                }
                out.push_str(digits);
            }
            Ok(point) if point >= digits.len() => {
                out.push_str(digits);
                for _ in digits.len()..point {
                    out.push('0');
                }
            }
            Ok(point) => {
                out.push_str(&digits[..point]);
                out.push('.');
                out.push_str(&digits[point..]);
            }
        }
    }

    /// `1.5e-7` (double-conversion) or, with `c_style`, `1.5e-07` and
    /// `1.5e+07` (the C library's `%g`).
    fn push_exponential(&self, out: &mut String, c_style: bool) {
        if self.negative {
            out.push('-');
        }
        let digits = self.text();
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        if self.exponent < 0 {
            out.push('-');
        } else if c_style {
            out.push('+');
        }
        let magnitude = self.exponent.unsigned_abs();
        if c_style && magnitude < 10 {
            out.push('0');
        }
        let _ = write!(out, "{magnitude}");
    }
}

/// Whether `v` is exactly `m * 10^place`, for an `m` ending in `5`.
///
/// With `v = k * 2^q` for an odd `k`, and `m` odd, the powers of two agree
/// only when `q == place`; then `m * 5^place == k`, or, for a negative
/// place, `m == k * 5^-place`.
fn equals_decimal(v: f64, m: u64, place: i32) -> bool {
    let Some((k, q)) = odd_significand(v) else {
        return false;
    };
    if q != place {
        return false;
    }
    let power = 5_u64.checked_pow(place.unsigned_abs());
    if place >= 0 {
        power.and_then(|power| m.checked_mul(power)) == Some(k)
    } else {
        power.and_then(|power| k.checked_mul(power)) == Some(m)
    }
}

/// A formatted number held on the stack.
struct StackText {
    bytes: [u8; 40],
    len: usize,
}

impl Default for StackText {
    fn default() -> Self {
        Self {
            bytes: [0; 40],
            len: 0,
        }
    }
}

impl fmt::Write for StackText {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn double(v: f64) -> String {
        let mut out = String::new();
        push_double(&mut out, v);
        out
    }

    fn float(v: f32) -> String {
        let mut out = String::new();
        push_float(&mut out, v);
        out
    }

    fn half(v: f32) -> String {
        let mut out = String::new();
        push_half(&mut out, crate::half::from_f32(v));
        out
    }

    fn time_code(v: f64) -> String {
        let mut out = String::new();
        push_time_code(&mut out, v);
        out
    }

    /// OpenUSD 26.08's text for these values (`ExportToString` of
    /// `double[]`, `float[]`, `half[]` and `timecode[]` attributes).
    #[test]
    #[allow(
        clippy::excessive_precision,
        reason = "halfway values, exact in binary"
    )]
    fn numbers_are_spelled_as_openusd_writes_them() {
        for (v, text) in [
            (0.1, "0.1"),
            (3.42e-8, "3.42e-8"),
            (1e-6, "0.000001"),
            (1.5e-6, "0.0000015"),
            (1e-7, "1e-7"),
            (1e15, "1e15"),
            (999_999_999_999_999.0, "999999999999999"),
            (1e16, "1e16"),
            (1e21, "1e21"),
            (1e300, "1e300"),
            (5e-324, "5e-324"),
            (-0.0, "-0"),
            (0.0, "0"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (123_456_789_012_345_680.0, "1.2345678901234568e17"),
            // Halfway between the two shortest candidates: the even one.
            (-886_093_775_796_624.25, "-886093775796624.2"),
            (1_152_765_711_724_540.25, "1.1527657117245402e15"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            (-f64::NAN, "nan"),
        ] {
            assert_eq!(double(v), text, "{v:e}");
        }
        for (v, text) in [
            (0.1, "0.1"),
            (1e-7, "1e-7"),
            (1e-6, "0.000001"),
            (1e15, "1e15"),
            (1e14, "100000000000000"),
            (123_456_789.0, "123456790"),
            (-3_571_017.25, "-3571017.2"),
            (392_424.625, "392424.62"),
            (3_571_017.75, "3571017.8"),
            (-0.0, "-0"),
            (3.4e38, "3.4e38"),
            (1e-45, "1e-45"),
            (f32::NAN, "nan"),
            (f32::NEG_INFINITY, "-inf"),
        ] {
            assert_eq!(float(v), text, "{v:e}");
        }
        for (v, text) in [
            (0.1, "0.0999756"),
            (1e-5, "1.00136e-05"),
            (6e-8, "5.96046e-08"),
            (65504.0, "65504"),
            (-0.0, "-0"),
            (1.0, "1"),
            (0.0001, "0.000100017"),
            (0.00009, "9.0003e-05"),
            (f32::INFINITY, "inf"),
            (f32::NEG_INFINITY, "-inf"),
            (f32::NAN, "nan"),
        ] {
            assert_eq!(half(v), text, "{v:e}");
        }
        for (v, text) in [
            (1e-7, "1e-07"),
            (1_234_560.0, "1.23456e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (2.5, "2.5"),
            (-0.0, "-0"),
            (1e-10, "1e-10"),
            (f64::NEG_INFINITY, "-inf"),
        ] {
            assert_eq!(time_code(v), text, "{v:e}");
        }
    }

    /// OpenUSD writes `1.2345678` as `1.23457`, which reads back as
    /// another time; the `double` spelling keeps it.
    #[test]
    fn time_codes_six_digits_cannot_hold_keep_their_digits() {
        assert_eq!(time_code(1.234_567_8), "1.2345678");
        assert_eq!(time_code(1_234_567.0), "1234567");
        assert_eq!(time_code(123_456.5), "123456.5");
        assert_eq!(time_code(0.1 + 0.2), "0.30000000000000004");
    }

    /// Every half's text reads back as that half, and has at most six
    /// significant digits.
    #[test]
    fn every_half_reads_back_as_itself() {
        for bits in 0..=u16::MAX {
            let value = crate::half::to_f32(bits);
            if value.is_nan() {
                continue;
            }
            let mut text = String::new();
            push_half(&mut text, bits);
            let read: f64 = text.parse().unwrap();
            assert_eq!(crate::half::from_f64(read), bits, "{text}");
        }
    }

    /// Doubles and floats read back as themselves, across magnitudes.
    #[test]
    fn doubles_and_floats_read_back_as_themselves() {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for _ in 0..200_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let v = f64::from_bits(state);
            if v.is_finite() {
                assert_eq!(double(v).parse::<f64>().unwrap().to_bits(), v.to_bits());
            }
            #[allow(clippy::cast_possible_truncation, reason = "the high bits")]
            let f = f32::from_bits((state >> 32) as u32);
            if f.is_finite() {
                assert_eq!(float(f).parse::<f32>().unwrap().to_bits(), f.to_bits());
            }
            if v.is_finite() {
                assert_eq!(time_code(v).parse::<f64>().unwrap().to_bits(), v.to_bits());
            }
        }
    }
}
