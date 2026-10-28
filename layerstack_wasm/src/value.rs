// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composed values as the wrapper receives them.
//!
//! A value is the record `[type, tag, ...]`, `type` its value type name
//! (`float`, `point3f[]`) and `tag` its shape:
//!
//! - `"s"`, `[type, "s", value]`: a scalar. Numbers are numbers, `int64`
//!   and `uint64` are `bigint`s, text types (`string`, `token`, `asset`,
//!   `pathExpression`, `path`, and `unregistered` for metadata text kept
//!   unparsed) are strings, and a `half` is its `float` value.
//! - `"t"`, `[type, "t", numbers]`: a vector, matrix (row-major) or
//!   quaternion (stored imaginary first, `[i, j, k, r]`).
//! - `"a"`, `[type, "a", scalar, components, length, data]`: an array,
//!   `data` one typed array of `length * components` scalars (half floats
//!   as raw bits, `bool` as bytes) or, for text, a list of strings.
//! - `"d"`, `[type, "d", [[key, value], ...]]`: a dictionary.
//! - `"l"`, `[type, "l", [value, ...]]`: a list no buffer holds.
//! - `"x"`, `[type, "x"]`: a value the bindings do not represent.
//!
//! Spec: AOUSD Core §6.2–§6.3 (scene description data types).

use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};

use layerstack::{TokenInterner, TypedArray, Value, half};

use crate::{
    error::Error,
    wire::{Elements, Wire},
};

/// `value` as a value record, its type named `type_name` when the attribute
/// declares one; `None` for no value (a block or `Null`).
pub(crate) fn encode(
    value: &Value,
    type_name: Option<&str>,
    tokens: &TokenInterner,
) -> Result<Option<Wire>, Error> {
    let named = |default: &str| String::from(type_name.unwrap_or(default));
    let scalar = |default: &str, value: Wire| {
        Wire::List(vec![Wire::Text(named(default)), Wire::text("s"), value])
    };
    let tuple = |default: &str, items: Vec<f64>| {
        Wire::List(vec![
            Wire::Text(named(default)),
            Wire::text("t"),
            Wire::List(items.into_iter().map(Wire::Number).collect()),
        ])
    };
    let halves = |bits: &[u16]| bits.iter().map(|b| f64::from(half::to_f32(*b))).collect();
    let floats = |items: &[f32]| items.iter().map(|v| f64::from(*v)).collect();
    let ints = |items: &[i32]| items.iter().map(|v| f64::from(*v)).collect();
    Ok(Some(match value {
        Value::AnimationBlock | Value::Null | Value::Blocked => return Ok(None),
        Value::Bool(v) => scalar("bool", Wire::Bool(*v)),
        Value::UChar(v) => scalar("uchar", Wire::Number(f64::from(*v))),
        Value::Int(v) => scalar("int", Wire::Number(f64::from(*v))),
        Value::UInt(v) => scalar("uint", Wire::Number(f64::from(*v))),
        Value::Int64(v) => scalar("int64", Wire::Int64(*v)),
        Value::UInt64(v) => scalar("uint64", Wire::UInt64(*v)),
        Value::Half(v) => scalar("half", Wire::Number(f64::from(half::to_f32(*v)))),
        Value::Float(v) => scalar("float", Wire::Number(f64::from(*v))),
        Value::Double(v) => scalar("double", Wire::Number(*v)),
        Value::TimeCode(v) => scalar("timecode", Wire::Number(*v)),
        Value::Duration(v) => scalar("duration", Wire::Number(*v)),
        Value::String(v) => scalar("string", Wire::text(&**v)),
        Value::Token(v) => scalar("token", Wire::text(tokens.resolve(*v))),
        Value::Asset(v) => scalar("asset", Wire::text(&**v)),
        Value::PathExpression(v) => scalar("pathExpression", Wire::text(&**v)),
        // The text OpenUSD keeps for metadata it has no type for, and a
        // relationship's legacy path default.
        Value::Unregistered(v) => scalar("unregistered", Wire::text(&**v)),
        Value::Path(v) => scalar("path", Wire::text(&**v)),
        Value::Vec2d(v) => tuple("double2", v.to_vec()),
        Value::Vec3d(v) => tuple("double3", v.to_vec()),
        Value::Vec4d(v) => tuple("double4", v.to_vec()),
        Value::Vec2f(v) => tuple("float2", floats(v)),
        Value::Vec3f(v) => tuple("float3", floats(v)),
        Value::Vec4f(v) => tuple("float4", floats(v)),
        Value::Vec2h(v) => tuple("half2", halves(v)),
        Value::Vec3h(v) => tuple("half3", halves(v)),
        Value::Vec4h(v) => tuple("half4", halves(v)),
        Value::Vec2i(v) => tuple("int2", ints(v)),
        Value::Vec3i(v) => tuple("int3", ints(v)),
        Value::Vec4i(v) => tuple("int4", ints(v)),
        Value::Matrix2d(v) => tuple("matrix2d", v.to_vec()),
        Value::Matrix3d(v) => tuple("matrix3d", v.to_vec()),
        Value::Matrix4d(v) => tuple("matrix4d", v.to_vec()),
        Value::Quatd(v) => tuple("quatd", v.to_vec()),
        Value::Quatf(v) => tuple("quatf", floats(v)),
        Value::Quath(v) => tuple("quath", halves(v)),
        Value::TypedArray(array) => encode_array(array, type_name, tokens)?,
        Value::Array(items) => encode_list(items, type_name, tokens)?,
        Value::Dictionary(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                let value = encode(value, None, tokens)?.unwrap_or(Wire::Null);
                out.push(Wire::List(vec![Wire::text(&**key), value]));
            }
            Wire::List(vec![
                Wire::Text(named("dictionary")),
                Wire::text("d"),
                Wire::List(out),
            ])
        }
        Value::Deferred(deferred) => {
            let value = deferred
                .try_materialize()
                .map_err(|error| Error::new("value.decode", error.to_string()))?;
            return encode(value, type_name, tokens);
        }
        Value::Opaque {
            type_name: opaque, ..
        } => Wire::List(vec![
            Wire::Text(String::from(type_name.unwrap_or(tokens.resolve(*opaque)))),
            Wire::text("x"),
        ]),
        Value::ArrayEdit(_) | Value::TypedArrayEdit(_) => {
            Wire::List(vec![Wire::Text(named("arrayEdit")), Wire::text("x")])
        }
    }))
}

/// The array record of `array`: its scalars in one buffer, copied once.
fn encode_array(
    array: &TypedArray,
    type_name: Option<&str>,
    tokens: &TokenInterner,
) -> Result<Wire, Error> {
    let array = array
        .try_materialize()
        .map_err(|error| Error::new("value.decode", error.to_string()))?;
    let (default, scalar, components, data) = match array {
        TypedArray::Bool(items) => (
            "bool[]",
            "bool",
            1,
            Wire::Elements(Elements::U8(items.iter().map(|b| u8::from(*b)).collect())),
        ),
        TypedArray::UChar(items) => (
            "uchar[]",
            "u8",
            1,
            Wire::Elements(Elements::U8(items.to_vec())),
        ),
        TypedArray::Int(items) => (
            "int[]",
            "i32",
            1,
            Wire::Elements(Elements::I32(items.to_vec())),
        ),
        TypedArray::UInt(items) => (
            "uint[]",
            "u32",
            1,
            Wire::Elements(Elements::U32(items.to_vec())),
        ),
        TypedArray::Int64(items) => (
            "int64[]",
            "i64",
            1,
            Wire::Elements(Elements::I64(items.to_vec())),
        ),
        TypedArray::UInt64(items) => (
            "uint64[]",
            "u64",
            1,
            Wire::Elements(Elements::U64(items.to_vec())),
        ),
        TypedArray::Half(items) => (
            "half[]",
            "f16",
            1,
            Wire::Elements(Elements::U16(items.to_vec())),
        ),
        TypedArray::Float(items) => (
            "float[]",
            "f32",
            1,
            Wire::Elements(Elements::F32(items.to_vec())),
        ),
        TypedArray::Double(items) => (
            "double[]",
            "f64",
            1,
            Wire::Elements(Elements::F64(items.to_vec())),
        ),
        TypedArray::TimeCode(items) => (
            "timecode[]",
            "f64",
            1,
            Wire::Elements(Elements::F64(items.to_vec())),
        ),
        TypedArray::Duration(items) => (
            "duration[]",
            "f64",
            1,
            Wire::Elements(Elements::F64(items.to_vec())),
        ),
        TypedArray::Vec2f(items) => ("float2[]", "f32", 2, f32s(items.as_flattened())),
        TypedArray::Vec3f(items) => ("float3[]", "f32", 3, f32s(items.as_flattened())),
        TypedArray::Vec4f(items) => ("float4[]", "f32", 4, f32s(items.as_flattened())),
        TypedArray::Quatf(items) => ("quatf[]", "f32", 4, f32s(items.as_flattened())),
        TypedArray::Vec2d(items) => ("double2[]", "f64", 2, f64s(items.as_flattened())),
        TypedArray::Vec3d(items) => ("double3[]", "f64", 3, f64s(items.as_flattened())),
        TypedArray::Vec4d(items) => ("double4[]", "f64", 4, f64s(items.as_flattened())),
        TypedArray::Quatd(items) => ("quatd[]", "f64", 4, f64s(items.as_flattened())),
        TypedArray::Matrix2d(items) => ("matrix2d[]", "f64", 4, f64s(items.as_flattened())),
        TypedArray::Matrix3d(items) => ("matrix3d[]", "f64", 9, f64s(items.as_flattened())),
        TypedArray::Matrix4d(items) => ("matrix4d[]", "f64", 16, f64s(items.as_flattened())),
        TypedArray::Vec2h(items) => ("half2[]", "f16", 2, u16s(items.as_flattened())),
        TypedArray::Vec3h(items) => ("half3[]", "f16", 3, u16s(items.as_flattened())),
        TypedArray::Vec4h(items) => ("half4[]", "f16", 4, u16s(items.as_flattened())),
        TypedArray::Quath(items) => ("quath[]", "f16", 4, u16s(items.as_flattened())),
        TypedArray::Vec2i(items) => ("int2[]", "i32", 2, i32s(items.as_flattened())),
        TypedArray::Vec3i(items) => ("int3[]", "i32", 3, i32s(items.as_flattened())),
        TypedArray::Vec4i(items) => ("int4[]", "i32", 4, i32s(items.as_flattened())),
        TypedArray::Token(items) => (
            "token[]",
            "string",
            1,
            Wire::List(
                items
                    .iter()
                    .map(|t| Wire::text(tokens.resolve(*t)))
                    .collect(),
            ),
        ),
        TypedArray::String(items) => (
            "string[]",
            "string",
            1,
            Wire::List(items.iter().map(Wire::text).collect()),
        ),
        TypedArray::Deferred(_) => {
            return Err(Error::new(
                "value.decode",
                "a retained array materialized to another retained array",
            ));
        }
    };
    let length = array.len();
    Ok(Wire::List(vec![
        Wire::Text(String::from(type_name.unwrap_or(default))),
        Wire::text("a"),
        Wire::text(scalar),
        Wire::Number(f64::from(components)),
        Wire::Number(length as f64),
        data,
    ]))
}

/// A list no buffer holds: text elements (an `asset[]`) as a text array,
/// anything else element by element.
fn encode_list(
    items: &[Value],
    type_name: Option<&str>,
    tokens: &TokenInterner,
) -> Result<Wire, Error> {
    let text: Option<Vec<Wire>> = items
        .iter()
        .map(|item| match item {
            Value::Asset(text) | Value::String(text) | Value::PathExpression(text) => {
                Some(Wire::text(&**text))
            }
            Value::Token(token) => Some(Wire::text(tokens.resolve(*token))),
            _ => None,
        })
        .collect();
    let name = String::from(type_name.unwrap_or("list"));
    if let Some(text) = text {
        return Ok(Wire::List(vec![
            Wire::Text(name),
            Wire::text("a"),
            Wire::text("string"),
            Wire::Number(1.0),
            Wire::Number(text.len() as f64),
            Wire::List(text),
        ]));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(encode(item, None, tokens)?.unwrap_or(Wire::Null));
    }
    Ok(Wire::List(vec![
        Wire::Text(name),
        Wire::text("l"),
        Wire::List(out),
    ]))
}

fn f32s(items: &[f32]) -> Wire {
    Wire::Elements(Elements::F32(items.to_vec()))
}

fn f64s(items: &[f64]) -> Wire {
    Wire::Elements(Elements::F64(items.to_vec()))
}

fn u16s(items: &[u16]) -> Wire {
    Wire::Elements(Elements::U16(items.to_vec()))
}

fn i32s(items: &[i32]) -> Wire {
    Wire::Elements(Elements::I32(items.to_vec()))
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::*;

    #[test]
    fn arrays_are_one_buffer_with_their_layout() {
        let tokens = TokenInterner::default();
        let points = Value::TypedArray(TypedArray::Vec3f(Arc::new(vec![
            [1.0, 2.0, 3.0],
            [4.0, 5.0, 6.0],
        ])));
        let wire = encode(&points, Some("point3f[]"), &tokens)
            .unwrap()
            .unwrap();
        assert_eq!(
            wire,
            Wire::List(vec![
                Wire::text("point3f[]"),
                Wire::text("a"),
                Wire::text("f32"),
                Wire::Number(3.0),
                Wire::Number(2.0),
                Wire::Elements(Elements::F32(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0])),
            ])
        );
    }

    #[test]
    fn scalars_keep_64_bit_integers_exact() {
        let tokens = TokenInterner::default();
        let big = Value::Int64(i64::MAX);
        assert_eq!(
            encode(&big, None, &tokens).unwrap().unwrap(),
            Wire::List(vec![
                Wire::text("int64"),
                Wire::text("s"),
                Wire::Int64(i64::MAX)
            ])
        );
        assert_eq!(encode(&Value::Blocked, None, &tokens).unwrap(), None);
        let quat = encode(&Value::Quatf([0.0, 0.0, 0.0, 1.0]), None, &tokens)
            .unwrap()
            .unwrap();
        assert_eq!(quat.as_list().unwrap()[1].as_text(), Some("t"));
    }
}
