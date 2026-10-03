// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USD numeric conversion shared by AST emission and the import event sink.
//! Owns scalar ranges, component layouts and precision; not grammar or layers.
#![allow(
    clippy::cast_possible_truncation,
    reason = "USD conversions intentionally narrow numeric types"
)]
use crate::ast;
use alloc::{boxed::Box, string::String};
use layerstack::Value;

pub(crate) fn dimensioned_value(
    items: &[ast::Value<'_>],
    type_hint: &str,
    elem_hint: &str,
) -> Result<Option<Value>, String> {
    // Strip array suffix for matching: "float3[]" → "float3".
    let base = type_hint.strip_suffix("[]").unwrap_or(type_hint);
    if let Some(component) = component_type(base)
        && let Err(message) = check_components(items, component)
    {
        return Err(alloc::format!("`{base}`: {message}"));
    }
    Ok(match base {
        // Vectors — f64
        "double2" => Some(Value::Vec2d(extract_f64s::<2>(items))),
        "double3" => Some(Value::Vec3d(extract_f64s::<3>(items))),
        "double4" => Some(Value::Vec4d(extract_f64s::<4>(items))),
        // Vectors — f32
        "float2" => Some(Value::Vec2f(extract_f32s::<2>(items))),
        "float3" => Some(Value::Vec3f(extract_f32s::<3>(items))),
        "float4" => Some(Value::Vec4f(extract_f32s::<4>(items))),
        // Vectors — half
        "half2" => Some(Value::Vec2h(extract_halves::<2>(items))),
        "half3" => Some(Value::Vec3h(extract_halves::<3>(items))),
        "half4" => Some(Value::Vec4h(extract_halves::<4>(items))),
        // Vectors — i32
        "int2" => Some(Value::Vec2i(extract_i32s::<2>(items))),
        "int3" => Some(Value::Vec3i(extract_i32s::<3>(items))),
        "int4" => Some(Value::Vec4i(extract_i32s::<4>(items))),
        // Matrices — f64
        "matrix2d" => Some(Value::Matrix2d(Box::new(extract_matrix_f64::<4>(items)))),
        "matrix3d" => Some(Value::Matrix3d(Box::new(extract_matrix_f64::<9>(items)))),
        // `frame4d` is the frame role of `matrix4d` (AOUSD Core §6.5).
        "matrix4d" | "frame4d" => Some(Value::Matrix4d(Box::new(extract_matrix_f64::<16>(items)))),
        // Quaternions — stored as (i, j, k, r) but authored as (r, i, j, k)
        // in USDA text per §16.3.10.22.
        "quatd" => {
            let v = extract_f64s::<4>(items);
            Some(Value::Quatd([v[1], v[2], v[3], v[0]]))
        }
        "quatf" => {
            let v = extract_f32s::<4>(items);
            Some(Value::Quatf([v[1], v[2], v[3], v[0]]))
        }
        "quath" => {
            let v = extract_halves::<4>(items);
            Some(Value::Quath([v[1], v[2], v[3], v[0]]))
        }
        // Semantic aliases (§6.5) — same element layout, different type name.
        _ if is_semantic_vec_alias(base, 'f') => {
            let n = semantic_component_count(base);
            match n {
                2 => Some(Value::Vec2f(extract_f32s::<2>(items))),
                3 => Some(Value::Vec3f(extract_f32s::<3>(items))),
                4 => Some(Value::Vec4f(extract_f32s::<4>(items))),
                _ => None,
            }
        }
        _ if is_semantic_vec_alias(base, 'd') => {
            let n = semantic_component_count(base);
            match n {
                2 => Some(Value::Vec2d(extract_f64s::<2>(items))),
                3 => Some(Value::Vec3d(extract_f64s::<3>(items))),
                4 => Some(Value::Vec4d(extract_f64s::<4>(items))),
                _ => None,
            }
        }
        _ if is_semantic_vec_alias(base, 'h') => {
            let n = semantic_component_count(base);
            match n {
                2 => Some(Value::Vec2h(extract_halves::<2>(items))),
                3 => Some(Value::Vec3h(extract_halves::<3>(items))),
                4 => Some(Value::Vec4h(extract_halves::<4>(items))),
                _ => None,
            }
        }
        _ => {
            // Not a recognized dimensioned type — fall through to
            // generic array handling. This also covers nested arrays of
            // tuples (e.g. `float3[]`), where the inner tuples will be
            // converted individually via recursive `convert_value` calls.
            let _ = elem_hint;
            None
        }
    })
}
// ── Type hint decomposition ─────────────────────────────────────────────

/// Extract the scalar element type from a compound USD type name.
///
/// Handles vector types (`float3` → `float`), array types (`int[]` → `int`),
/// combined forms (`float3[]` → `float`), and named compound types
/// (`color3f` → `float`, `matrix4d` → `double`, `quatf` → `float`).
///
/// Returns the original hint unchanged for already-scalar types.
///
/// Spec: AOUSD Core §6.2 (scene description data types).
pub(crate) fn element_type_hint(hint: &str) -> &str {
    // Strip array suffix first: "float3[]" → "float3", "int[]" → "int"
    let base = hint.strip_suffix("[]").unwrap_or(hint);

    // Named compound types with element-type suffixes.
    // color3f, color4f, normal3f, point3f, vector3f, texCoord2f, texCoord3f → float
    // color3d, color4d, normal3d, point3d, vector3d, texCoord2d, texCoord3d → double
    // color3h, normal3h, point3h, vector3h, texCoord2h, texCoord3h → half
    if base.ends_with('f')
        && (base.starts_with("color")
            || base.starts_with("normal")
            || base.starts_with("point")
            || base.starts_with("vector")
            || base.starts_with("texCoord"))
    {
        return "float";
    }
    if base.ends_with('d')
        && (base.starts_with("color")
            || base.starts_with("normal")
            || base.starts_with("point")
            || base.starts_with("vector")
            || base.starts_with("texCoord"))
    {
        return "double";
    }
    if base.ends_with('h')
        && (base.starts_with("color")
            || base.starts_with("normal")
            || base.starts_with("point")
            || base.starts_with("vector")
            || base.starts_with("texCoord"))
    {
        return "half";
    }

    // matrix2d, matrix3d, matrix4d → double
    if base.starts_with("matrix") && base.ends_with('d') {
        return "double";
    }

    // quatf → float, quatd → double, quath → half
    match base {
        "quatf" => return "float",
        "quatd" => return "double",
        "quath" => return "half",
        _ => {}
    }

    // Simple vector types: float2, float3, float4, double2, double3, double4,
    // int2, int3, int4, half2, half3, half4, etc.
    // Strip trailing digits to get the scalar type.
    let trimmed = base.trim_end_matches(|c: char| c.is_ascii_digit());
    if !trimmed.is_empty() && trimmed.len() < base.len() {
        return trimmed;
    }

    // Already scalar or unrecognised — return as-is.
    base
}

// ── Numeric conversion with type hints ──────────────────────────────────

/// A number literal's value, as OpenUSD's text parser holds it.
#[derive(Clone, Copy)]
enum Number {
    Int(i64),
    UInt(u64),
    Float(f64),
}

/// Converts a number, boolean or string literal to the scalar type `ty` as
/// OpenUSD's text parser does (`Sdf_ParserHelpers::_GetImpl`,
/// `pxr/usd/sdf/parserHelpers.h`, through `GfNumericCast`,
/// `pxr/base/gf/numericCast.h`), so that the value always has the declared
/// type or is rejected:
///
/// - `bool` takes `true`/`false`, a number (true when nonzero, NaN
///   included), or a string `Sdf_BoolFromString` accepts;
/// - an integer type takes an integer in its range, or a finite number whose
///   value truncated toward zero is;
/// - `half`, `float`, `double` and `timecode` take any number (a `half`
///   narrows through `float`), or the strings `"inf"`, `"-inf"` and `"nan"`.
///
/// Returns `None` when `ty` is not one of these types, and for a value
/// block, an array or an array edit, whose elements convert one by one.
///
/// Spec: AOUSD Core §6.3 (scalar value types), §16.2.11 (values).
pub(crate) fn convert_scalar(value: &ast::Value<'_>, ty: &str) -> Option<Result<Value, String>> {
    if !matches!(
        ty,
        "bool"
            | "uchar"
            | "int"
            | "uint"
            | "int64"
            | "uint64"
            | "half"
            | "float"
            | "double"
            | "timecode"
    ) || matches!(
        value,
        ast::Value::Blocked
            | ast::Value::AnimationBlock
            | ast::Value::Array(_)
            | ast::Value::ArrayEdit(_)
    ) {
        return None;
    }
    let floating = matches!(ty, "half" | "float" | "double" | "timecode");
    let number = match value {
        ast::Value::Int(n) => Number::Int(*n),
        ast::Value::UInt(n) => Number::UInt(*n),
        ast::Value::Number(n) => Number::Float(*n),
        ast::Value::Bool(b) if ty == "bool" => return Some(Ok(Value::Bool(*b))),
        ast::Value::String(s) if ty == "bool" => {
            return Some(
                bool_from_string(s)
                    .map(Value::Bool)
                    .ok_or_else(|| alloc::format!("`{s}` is not a `bool`")),
            );
        }
        ast::Value::String(s) if floating && matches!(&**s, "inf" | "-inf" | "nan") => {
            Number::Float(match &**s {
                "inf" => f64::INFINITY,
                "-inf" => f64::NEG_INFINITY,
                _ => f64::NAN,
            })
        }
        _ => return Some(Err(alloc::format!("a `{ty}` value must be a number"))),
    };
    Some(convert_number(number, ty))
}

/// Converts `number` to the numeric or boolean type `ty` (see
/// [`convert_scalar`]).
fn convert_number(number: Number, ty: &str) -> Result<Value, String> {
    let as_f64 = match number {
        Number::Int(n) => n as f64,
        Number::UInt(n) => n as f64,
        Number::Float(f) => f,
    };
    // An integer target takes the value truncated toward zero, if finite.
    let integral = |min: i128, max: i128| -> Result<i128, String> {
        let n = match number {
            Number::Int(n) => i128::from(n),
            Number::UInt(n) => i128::from(n),
            // `as` truncates toward zero.
            Number::Float(f) if f.is_finite() => f as i128,
            Number::Float(f) => return Err(alloc::format!("{f} is not a `{ty}`")),
        };
        if (min..=max).contains(&n) {
            Ok(n)
        } else {
            Err(alloc::format!("{as_f64} is out of range for `{ty}`"))
        }
    };
    Ok(match ty {
        "bool" => Value::Bool(match number {
            Number::Int(n) => n != 0,
            Number::UInt(n) => n != 0,
            Number::Float(f) => f != 0.0,
        }),
        "uchar" => Value::UChar(integral(0, u8::MAX.into())? as u8),
        "int" => Value::Int(integral(i32::MIN.into(), i32::MAX.into())? as i32),
        "uint" => Value::UInt(integral(0, u32::MAX.into())? as u32),
        "int64" => Value::Int64(integral(i64::MIN.into(), i64::MAX.into())? as i64),
        "uint64" => Value::UInt64(integral(0, u64::MAX.into())? as u64),
        "half" => Value::Half(match number {
            Number::Int(n) => layerstack::half::from_f32(n as f32),
            Number::UInt(n) => layerstack::half::from_f32(n as f32),
            Number::Float(f) => half_from_f64(f),
        }),
        "float" => Value::Float(match number {
            Number::Int(n) => n as f32,
            Number::UInt(n) => n as f32,
            Number::Float(f) => f as f32,
        }),
        "double" => Value::Double(as_f64),
        "timecode" => Value::TimeCode(as_f64),
        _ => unreachable!("`convert_scalar` passes numeric types only"),
    })
}

/// `Sdf_BoolFromString` (`pxr/usd/sdf/parserHelpers.cpp`): `true`, `yes`,
/// `1`, `false`, `no` or `0`, in any case.
fn bool_from_string(s: &str) -> Option<bool> {
    let s = s.to_ascii_lowercase();
    match s.as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// The scalar type of each component of the vector, matrix or quaternion
/// type `base`, or `None` for other types.
fn component_type(base: &str) -> Option<&'static str> {
    match base {
        "int2" | "int3" | "int4" => Some("int"),
        "double2" | "double3" | "double4" | "matrix2d" | "matrix3d" | "matrix4d" | "frame4d"
        | "quatd" => Some("double"),
        "float2" | "float3" | "float4" | "quatf" => Some("float"),
        "half2" | "half3" | "half4" | "quath" => Some("half"),
        _ if is_semantic_vec_alias(base, 'd') => Some("double"),
        _ if is_semantic_vec_alias(base, 'f') => Some("float"),
        _ if is_semantic_vec_alias(base, 'h') => Some("half"),
        _ => None,
    }
}

/// Checks that every component of a vector, matrix or quaternion (rows
/// included) converts to `component`.
fn check_components(items: &[ast::Value<'_>], component: &str) -> Result<(), String> {
    for item in items {
        match item {
            ast::Value::Tuple(row) => check_components(row, component)?,
            other => {
                convert_scalar(other, component).unwrap_or_else(|| {
                    Err(alloc::format!("a `{component}` component must be a number"))
                })?;
            }
        }
    }
    Ok(())
}

/// A `half` literal's bits: the parsed `double`, narrowed through `float`
/// and rounded to nearest even, as OpenUSD's text parser does
/// (`GfHalf(float)`; see [`layerstack::half`]).
fn half_from_f64(v: f64) -> u16 {
    layerstack::half::from_f64(v)
}

// ── Dimensioned type helpers (§6.3) ─────────────────────────────────────

/// Extracts `N` f64 values from AST value nodes.
fn extract_f64s<const N: usize>(items: &[ast::Value<'_>]) -> [f64; N] {
    let mut out = [0.0_f64; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = items.get(i).map_or(0.0, ast_to_f64);
    }
    out
}

/// Extracts `N` f32 values from AST value nodes.
fn extract_f32s<const N: usize>(items: &[ast::Value<'_>]) -> [f32; N] {
    let mut out = [0.0_f32; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = items.get(i).map_or(0.0, |v| ast_to_f64(v) as f32);
    }
    out
}

/// Extracts `N` half values (as raw u16 bits) from AST value nodes.
fn extract_halves<const N: usize>(items: &[ast::Value<'_>]) -> [u16; N] {
    let mut out = [0_u16; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = items.get(i).map_or(0, |v| half_from_f64(ast_to_f64(v)));
    }
    out
}

/// Extracts `N` i32 values from AST value nodes.
fn extract_i32s<const N: usize>(items: &[ast::Value<'_>]) -> [i32; N] {
    let mut out = [0_i32; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = items.get(i).map_or(0, ast_to_i32);
    }
    out
}

/// Extracts `N` f64 values from a matrix tuple-of-tuples or flat tuple.
///
/// USDA matrices are authored as nested tuples:
///   `((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))`
/// Each element is either a `Tuple` (nested row) or a scalar (flat).
fn extract_matrix_f64<const N: usize>(items: &[ast::Value<'_>]) -> [f64; N] {
    let mut out = [0.0_f64; N];
    let mut idx = 0;
    for item in items {
        match item {
            ast::Value::Tuple(row) => {
                for elem in row {
                    if idx < N {
                        out[idx] = ast_to_f64(elem);
                        idx += 1;
                    }
                }
            }
            _ => {
                if idx < N {
                    out[idx] = ast_to_f64(item);
                    idx += 1;
                }
            }
        }
    }
    out
}

/// Converts an AST value node to f64 (best-effort).
fn ast_to_f64(v: &ast::Value<'_>) -> f64 {
    match v {
        ast::Value::Number(n) => *n,
        ast::Value::Int(n) => *n as f64,
        ast::Value::UInt(n) => *n as f64,
        _ => 0.0,
    }
}

/// Converts an AST value node to i32 (best-effort).
fn ast_to_i32(v: &ast::Value<'_>) -> i32 {
    match v {
        ast::Value::Int(n) => *n as i32,
        ast::Value::UInt(n) => *n as i32,
        ast::Value::Number(n) => *n as i32,
        _ => 0,
    }
}

/// Returns `true` if `name` is a semantic type alias (§6.5) ending with
/// precision suffix `p` ('f', 'd', or 'h').
///
/// Semantic aliases: `color3f`, `color4f`, `normal3f`, `point3f`,
/// `vector3f`, `texCoord2f`, `texCoord3f`, etc. (`frame4d` is a matrix.)
pub(crate) fn is_semantic_vec_alias(name: &str, precision: char) -> bool {
    if !name.ends_with(precision) {
        return false;
    }
    name.starts_with("color")
        || name.starts_with("normal")
        || name.starts_with("point")
        || name.starts_with("vector")
        || name.starts_with("texCoord")
}

/// Extracts the component count from a semantic alias name.
///
/// E.g., `"color3f"` → 3, `"texCoord2f"` → 2.
pub(crate) fn semantic_component_count(name: &str) -> usize {
    // The digit is always the second-to-last character.
    name.chars()
        .rev()
        .nth(1)
        .and_then(|c| c.to_digit(10))
        .unwrap_or(0) as usize
}

pub(crate) fn element(
    items: &[ast::Value<'_>],
    type_hint: &str,
    width: usize,
) -> Result<Value, String> {
    if width == 0 {
        convert_scalar(&items[0], type_hint).expect("numeric scalar type")
    } else {
        dimensioned_value(items, type_hint, element_type_hint(type_hint))?
            .ok_or_else(|| alloc::format!("unsupported numeric type `{type_hint}`"))
    }
}
