// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Values spelled as OpenUSD's `TfStringify` spells a `VtValue` holding
//! them: the stream operator of each held type.
//!
//! - strings, tokens, path expressions, unregistered values and paths as
//!   their text; assets as `@path@`; booleans as `1` or `0`; integers in
//!   decimal (a `uchar` too, which `Vt` streams as a number);
//! - `double` and `float` values, alone or in vectors, quaternions and
//!   matrices, in shortest round-trip form (`TfStreamDouble`,
//!   `TfStreamFloat`: double-conversion's `ToShortest` with positional
//!   digits for a decimal exponent from -6 to 14, `inf`, `-inf`, `nan`);
//! - `half`, `timecode` and `duration` values with the stream's default
//!   `%g` formatting (`operator<<` for `GfHalf`, `SdfTimeCode`,
//!   `GfDuration`): six significant digits, trailing zeros dropped, and a
//!   signed exponent of at least two digits below `1e-4` or from `1e6`;
//! - vectors as `(x, y)`; quaternions as `(real, i, j, k)`; matrices as
//!   `( (a, b), (c, d) )` (`Gf_OstreamHelperP`);
//! - arrays as `[a, b]`; dictionaries as `{'key': value}` in key order
//!   (`operator<<(VtDictionary)`); list ops as
//!   `SdfTokenListOp(Prepended Items: [a, b])` (`SdfListOp`'s `_StreamOut`);
//! - `SdfValueBlock` as `None`, `SdfAnimationBlock` as `AnimationBlock`,
//!   `SdfOpaqueValue` as `OpaqueValue`, and an empty value, or an array
//!   edit (which no dictionary holds), as the empty text.

use alloc::{format, string::String, vec::Vec};
use layerstack::{FieldValue, LayerStore, ListOp, Value, half};

/// A dictionary entry or metadata field value as `TfStringify` spells it.
pub(super) fn field(value: &FieldValue, store: &dyn LayerStore) -> String {
    let tokens = store.tokens();
    match value {
        FieldValue::Value(value) => self::value(value, store),
        FieldValue::TokenListOp(op) => list_op("SdfTokenListOp", op, |token| {
            String::from(tokens.resolve(*token))
        }),
        FieldValue::PathListOp(op) => list_op("SdfPathListOp", op, |path| {
            path.display(store.paths(), tokens)
        }),
        FieldValue::StringListOp(op) => {
            list_op("SdfStringListOp", op, |text| String::from(&**text))
        }
        FieldValue::IntListOp(op) => list_op("SdfIntListOp", op, |v| format!("{v}")),
        FieldValue::UIntListOp(op) => list_op("SdfUIntListOp", op, |v| format!("{v}")),
        FieldValue::Int64ListOp(op) => list_op("SdfInt64ListOp", op, |v| format!("{v}")),
        FieldValue::UInt64ListOp(op) => list_op("SdfUInt64ListOp", op, |v| format!("{v}")),
        FieldValue::UnregisteredListOp(op) => list_op("SdfUnregisteredValueListOp", op, |text| {
            String::from(&**text)
        }),
        // `FieldValue` is non-exhaustive; no other kind is a dictionary
        // entry.
        _ => String::new(),
    }
}

/// A value as `TfStringify` spells it.
pub(super) fn value(value: &Value, store: &dyn LayerStore) -> String {
    let tokens = store.tokens();
    match value {
        Value::Null | Value::ArrayEdit(_) | Value::TypedArrayEdit(_) => String::new(),
        Value::Blocked => String::from("None"),
        Value::AnimationBlock => String::from("AnimationBlock"),
        Value::Opaque { .. } => String::from("OpaqueValue"),
        Value::String(text)
        | Value::PathExpression(text)
        | Value::Unregistered(text)
        | Value::PathText(text) => String::from(&**text),
        Value::Token(token) => String::from(tokens.resolve(*token)),
        Value::Asset(path) => format!("@{path}@"),
        Value::Bool(value) => String::from(if *value { "1" } else { "0" }),
        Value::UChar(value) => format!("{value}"),
        Value::Int(value) => format!("{value}"),
        Value::UInt(value) => format!("{value}"),
        Value::Int64(value) => format!("{value}"),
        Value::UInt64(value) => format!("{value}"),
        Value::Double(value) => double(*value),
        Value::Float(value) => float(*value),
        Value::Half(bits) => general(f64::from(half::to_f32(*bits))),
        Value::TimeCode(value) | Value::Duration(value) => general(*value),
        Value::Vec2d(v) => tuple(v.iter().map(|&x| double(x))),
        Value::Vec3d(v) => tuple(v.iter().map(|&x| double(x))),
        Value::Vec4d(v) => tuple(v.iter().map(|&x| double(x))),
        Value::Vec2f(v) => tuple(v.iter().map(|&x| float(x))),
        Value::Vec3f(v) => tuple(v.iter().map(|&x| float(x))),
        Value::Vec4f(v) => tuple(v.iter().map(|&x| float(x))),
        Value::Vec2h(v) => tuple(v.iter().map(|&x| general(f64::from(half::to_f32(x))))),
        Value::Vec3h(v) => tuple(v.iter().map(|&x| general(f64::from(half::to_f32(x))))),
        Value::Vec4h(v) => tuple(v.iter().map(|&x| general(f64::from(half::to_f32(x))))),
        Value::Vec2i(v) => tuple(v.iter().map(|x| format!("{x}"))),
        Value::Vec3i(v) => tuple(v.iter().map(|x| format!("{x}"))),
        Value::Vec4i(v) => tuple(v.iter().map(|x| format!("{x}"))),
        // Stored `[i, j, k, r]`; streamed real first.
        Value::Quatd([i, j, k, r]) => tuple([r, i, j, k].into_iter().map(|&x| double(x))),
        Value::Quatf([i, j, k, r]) => tuple([r, i, j, k].into_iter().map(|&x| float(x))),
        Value::Quath([i, j, k, r]) => tuple(
            [r, i, j, k]
                .into_iter()
                .map(|&x| general(f64::from(half::to_f32(x)))),
        ),
        Value::Matrix2d(m) => matrix(&m[..], 2),
        Value::Matrix3d(m) => matrix(&m[..], 3),
        Value::Matrix4d(m) => matrix(&m[..], 4),
        Value::Array(items) => list(items.iter().map(|item| self::value(item, store))),
        Value::TypedArray(items) => list(items.values().map(|item| self::value(&item, store))),
        Value::Dictionary(entries) => {
            let mut sorted: Vec<_> = entries.iter().collect();
            sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            let body: Vec<String> = sorted
                .into_iter()
                .map(|(key, value)| format!("'{key}': {}", field(value, store)))
                .collect();
            format!("{{{}}}", body.join(", "))
        }
        // `Value` is non-exhaustive; a kind added later spells as empty
        // until it is given its stream operator here.
        _ => String::new(),
    }
}

/// `SdfListOp`'s `_StreamOut`: the type's alias, then the explicit items,
/// or each nonempty list of the others.
fn list_op<T: Clone>(alias: &str, op: &ListOp<T>, item: impl Fn(&T) -> String) -> String {
    let items = |name: &str, items: &[T]| {
        let items: Vec<String> = items.iter().map(&item).collect();
        format!("{name} Items: [{}]", items.join(", "))
    };
    let parts: Vec<String> = match &op.explicit {
        Some(explicit) => alloc::vec![items("Explicit", explicit)],
        None => [
            ("Deleted", &op.delete),
            ("Added", &op.add),
            ("Prepended", &op.prepend),
            ("Appended", &op.append),
            ("Ordered", &op.reorder),
        ]
        .into_iter()
        .filter(|(_, list)| !list.is_empty())
        .map(|(name, list)| items(name, list))
        .collect(),
    };
    format!("{alias}({})", parts.join(", "))
}

fn tuple(parts: impl Iterator<Item = String>) -> String {
    format!("({})", parts.collect::<Vec<_>>().join(", "))
}

fn list(parts: impl Iterator<Item = String>) -> String {
    format!("[{}]", parts.collect::<Vec<_>>().join(", "))
}

/// `Gf_OstreamHelperP` for a matrix: `( (a, b), (c, d) )`.
fn matrix(flat: &[f64], size: usize) -> String {
    let rows: Vec<String> = flat
        .chunks(size)
        .map(|row| tuple(row.iter().map(|&x| double(x))))
        .collect();
    format!("( {} )", rows.join(", "))
}

fn double(value: f64) -> String {
    shortest(value, &format!("{value:e}"))
}

fn float(value: f32) -> String {
    shortest(f64::from(value), &format!("{value:e}"))
}

/// The specials as OpenUSD spells them, or `None` for a finite value.
fn special(value: f64) -> Option<&'static str> {
    if value.is_nan() {
        Some("nan")
    } else if value.is_infinite() {
        Some(if value > 0.0 { "inf" } else { "-inf" })
    } else if value == 0.0 {
        Some(if value.is_sign_negative() { "-0" } else { "0" })
    } else {
        None
    }
}

/// A float in shortest round-trip form, from its `{:e}` spelling
/// (`scientific`, whose digits are the shortest that read back as the same
/// value): positional for a decimal exponent from -6 to 14, else
/// `d.ddde-7`, as double-conversion's `ToShortest` with OpenUSD's settings
/// (`Tf_ApplyDoubleToStringConverter`) writes it.
fn shortest(value: f64, scientific: &str) -> String {
    if let Some(text) = special(value) {
        return String::from(text);
    }
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    if (-6..15).contains(&exponent) {
        positional(&mut out, &digits, exponent);
    } else {
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        out.push('e');
        out.push_str(&format!("{exponent}"));
    }
    out
}

/// `digits` (`d.ddd` without the point) times `10^exponent`, positionally.
fn positional(out: &mut String, digits: &str, exponent: i32) {
    let point = exponent + 1;
    let len = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    if point <= 0 {
        out.push_str("0.");
        for _ in 0..-point {
            out.push('0');
        }
        out.push_str(digits);
    } else if point >= len {
        out.push_str(digits);
        for _ in 0..point - len {
            out.push('0');
        }
    } else {
        let (whole, fraction) = digits.split_at(point.unsigned_abs() as usize);
        out.push_str(whole);
        out.push('.');
        out.push_str(fraction);
    }
}

/// `value` with C's `%g`: six significant digits, trailing zeros dropped,
/// in exponent form (`1.23457e+06`, `1e-05`) when the decimal exponent of
/// the rounded value is below -4 or at least 6.
fn general(value: f64) -> String {
    if let Some(text) = special(value) {
        return String::from(text);
    }
    // Six significant digits, rounded as `printf` rounds them.
    let scientific = format!("{:.5e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    if (-4..6).contains(&exponent) {
        positional(&mut out, digits, exponent);
    } else {
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        let sign = if exponent < 0 { '-' } else { '+' };
        out.push_str(&format!("e{sign}{:02}", exponent.unsigned_abs()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{double, float, general};

    #[test]
    fn floats_print_as_openusd_prints_them() {
        for (value, text) in [
            (1.5, "1.5"),
            (0.1, "0.1"),
            (1e20, "1e20"),
            (1e14, "100000000000000"),
            (1e15, "1e15"),
            (1e-6, "0.000001"),
            (1.25e-7, "1.25e-7"),
            (-2.0, "-2"),
            (-0.0, "-0"),
        ] {
            assert_eq!(double(value), text, "{value}");
        }
        assert_eq!(float(0.1), "0.1");
    }

    #[test]
    fn general_format_matches_printf() {
        for (value, text) in [
            (1.234_567_8, "1.23457"),
            (1_234_567.0, "1.23457e+06"),
            (1e-5, "1e-05"),
            (0.000_123_4, "0.0001234"),
            (999_999.5, "1e+06"),
            (100_000.0, "100000"),
            (1.5, "1.5"),
            (-2.5, "-2.5"),
            (65504.0, "65504"),
            (f64::from(0.1_f32), "0.1"),
        ] {
            assert_eq!(general(value), text, "{value}");
        }
    }
}
