// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Values across the boundary: composed values as the wrapper receives
//! them ([`encode`]), and values the wrapper sends for edits ([`decode`]).
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
//! A value the wrapper sends is a [`ValueInput`]: a value type name and
//! its scalars in one buffer, in the layout `encode` produces (tuples
//! flattened, matrices row-major, quaternions `[i, j, k, r]`, half floats
//! as numbers or as raw bits), so a value read can be sent back as it is.
//!
//! Spec: AOUSD Core §6.2–§6.3 (scene description data types).

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};

use layerstack::{StringArray, TokenInterner, TypedArray, Value, ValueType, half};

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

/// A value the wrapper sends: its value type name (`double`, `color3f`,
/// `point3f[]`) and its scalars.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ValueInput {
    /// The value type name, roles and `[]` included.
    pub(crate) type_name: String,
    /// The scalars, flattened.
    pub(crate) data: InputData,
}

/// The scalars of a [`ValueInput`], in the buffer the wrapper filled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum InputData {
    /// Numbers: any numeric type, integers checked to be exact and in
    /// range, `bool` as 0 or 1.
    Numbers(Vec<f64>),
    /// A `Float32Array`: any floating-point type, kept exact for `float`.
    Floats(Vec<f32>),
    /// `bigint`s: any integer type, checked to be in range.
    Int64(Vec<i64>),
    /// `bigint`s from a `BigUint64Array`: any integer type, checked to be
    /// in range.
    UInt64(Vec<u64>),
    /// Half floats as raw bits (a `Uint16Array`), for the half types.
    HalfBits(Vec<u16>),
    /// Text, for `string`, `token`, `asset` and `pathExpression`.
    Text(Vec<String>),
}

impl InputData {
    fn len(&self) -> usize {
        match self {
            Self::Numbers(items) => items.len(),
            Self::Floats(items) => items.len(),
            Self::Int64(items) => items.len(),
            Self::UInt64(items) => items.len(),
            Self::HalfBits(items) => items.len(),
            Self::Text(items) => items.len(),
        }
    }

    /// What the buffer holds, in words, for errors.
    fn describe(&self) -> &'static str {
        match self {
            Self::Numbers(_) => "numbers",
            Self::Floats(_) => "a Float32Array",
            Self::Int64(_) | Self::UInt64(_) => "bigints",
            Self::HalfBits(_) => "half float bits",
            Self::Text(_) => "strings",
        }
    }
}

/// The largest integer a JavaScript number holds exactly (2^53 - 1).
const SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The layerstack value `input` names: a scalar, a tuple of its type's
/// component count, or an array of any length of those, tokens interned
/// in `tokens`.
///
/// Spec: AOUSD Core §6.2 (scalar types), §6.3 (dimensioned types), §6.5
/// (roles change meaning, not storage).
///
/// # Errors
///
/// - `value.unknown_type`: no registered value type has the name
///   (OpenUSD's `Sdf_ValueTypeRegistry`); `value.unsupported` for
///   `opaque` and `group`, which hold no value.
/// - `value.kind`: the buffer holds the wrong kind of scalar, such as
///   strings for a `float` or numbers for a `token`.
/// - `value.shape`: a scalar or tuple with the wrong number of components,
///   or an array whose length is not a multiple of them.
/// - `value.range`: a number that is not exactly an integer of the type
///   (including one past 2^53, which a number does not hold exactly: send
///   a `bigint`), a `bool` other than 0 or 1, or a finite number a `float`
///   or `half` overflows.
pub(crate) fn decode(input: &ValueInput, tokens: &TokenInterner) -> Result<Value, Error> {
    let name = input.type_name.as_str();
    let Some((value_type, is_array)) = ValueType::parse(name) else {
        return Err(Error::new(
            "value.unknown_type",
            format!(
                "`{name}` is not a registered value type name, such as `double3` or `point3f[]`"
            ),
        ));
    };
    let storage = value_type.storage();
    let components = components(storage);
    if components == 0 {
        return Err(Error::new(
            "value.unsupported",
            format!("`{name}` holds no value"),
        ));
    }
    let length = input.data.len();
    let fits = if is_array {
        length.is_multiple_of(components)
    } else {
        length == components
    };
    if !fits {
        let expected = if is_array {
            format!("a multiple of {components}")
        } else {
            format!("{components}")
        };
        return Err(Error::new(
            "value.shape",
            format!("`{name}` takes {expected} scalars, but {length} were sent"),
        ));
    }
    let data = &input.data;
    use ValueType as T;
    let value = match (storage, is_array) {
        (T::Bool, false) => Value::Bool(bools(data, name)?[0]),
        (T::Bool, true) => Value::TypedArray(TypedArray::Bool(Arc::new(bools(data, name)?))),
        (T::UChar, false) => Value::UChar(ints(data, name)?[0]),
        (T::UChar, true) => Value::TypedArray(TypedArray::UChar(Arc::new(ints(data, name)?))),
        (T::Int, false) => Value::Int(ints(data, name)?[0]),
        (T::Int, true) => Value::TypedArray(TypedArray::Int(Arc::new(ints(data, name)?))),
        (T::UInt, false) => Value::UInt(ints(data, name)?[0]),
        (T::UInt, true) => Value::TypedArray(TypedArray::UInt(Arc::new(ints(data, name)?))),
        (T::Int64, false) => Value::Int64(ints(data, name)?[0]),
        (T::Int64, true) => Value::TypedArray(TypedArray::Int64(Arc::new(ints(data, name)?))),
        (T::UInt64, false) => Value::UInt64(ints(data, name)?[0]),
        (T::UInt64, true) => Value::TypedArray(TypedArray::UInt64(Arc::new(ints(data, name)?))),
        (T::Half, false) => Value::Half(halves(data, name)?[0]),
        (T::Half, true) => Value::TypedArray(TypedArray::Half(Arc::new(halves(data, name)?))),
        (T::Float, false) => Value::Float(floats(data, name)?[0]),
        (T::Float, true) => Value::TypedArray(TypedArray::Float(Arc::new(floats(data, name)?))),
        (T::Double, false) => Value::Double(doubles(data, name)?[0]),
        (T::Double, true) => Value::TypedArray(TypedArray::Double(Arc::new(doubles(data, name)?))),
        (T::TimeCode, false) => Value::TimeCode(doubles(data, name)?[0]),
        (T::TimeCode, true) => {
            Value::TypedArray(TypedArray::TimeCode(Arc::new(doubles(data, name)?)))
        }
        (T::Duration, false) => Value::Duration(doubles(data, name)?[0]),
        (T::Duration, true) => {
            Value::TypedArray(TypedArray::Duration(Arc::new(doubles(data, name)?)))
        }
        (T::Int2, false) => Value::Vec2i(tuple(&ints(data, name)?)),
        (T::Int3, false) => Value::Vec3i(tuple(&ints(data, name)?)),
        (T::Int4, false) => Value::Vec4i(tuple(&ints(data, name)?)),
        (T::Half2, false) => Value::Vec2h(tuple(&halves(data, name)?)),
        (T::Half3, false) => Value::Vec3h(tuple(&halves(data, name)?)),
        (T::Half4, false) => Value::Vec4h(tuple(&halves(data, name)?)),
        (T::Quath, false) => Value::Quath(tuple(&halves(data, name)?)),
        (T::Float2, false) => Value::Vec2f(tuple(&floats(data, name)?)),
        (T::Float3, false) => Value::Vec3f(tuple(&floats(data, name)?)),
        (T::Float4, false) => Value::Vec4f(tuple(&floats(data, name)?)),
        (T::Quatf, false) => Value::Quatf(tuple(&floats(data, name)?)),
        (T::Double2, false) => Value::Vec2d(tuple(&doubles(data, name)?)),
        (T::Double3, false) => Value::Vec3d(tuple(&doubles(data, name)?)),
        (T::Double4, false) => Value::Vec4d(tuple(&doubles(data, name)?)),
        (T::Quatd, false) => Value::Quatd(tuple(&doubles(data, name)?)),
        (T::Matrix2d, false) => Value::Matrix2d(Box::new(tuple(&doubles(data, name)?))),
        (T::Matrix3d, false) => Value::Matrix3d(Box::new(tuple(&doubles(data, name)?))),
        (T::Matrix4d, false) => Value::Matrix4d(Box::new(tuple(&doubles(data, name)?))),
        (T::Int2, true) => {
            Value::TypedArray(TypedArray::Vec2i(Arc::new(tuples(&ints(data, name)?))))
        }
        (T::Int3, true) => {
            Value::TypedArray(TypedArray::Vec3i(Arc::new(tuples(&ints(data, name)?))))
        }
        (T::Int4, true) => {
            Value::TypedArray(TypedArray::Vec4i(Arc::new(tuples(&ints(data, name)?))))
        }
        (T::Half2, true) => {
            Value::TypedArray(TypedArray::Vec2h(Arc::new(tuples(&halves(data, name)?))))
        }
        (T::Half3, true) => {
            Value::TypedArray(TypedArray::Vec3h(Arc::new(tuples(&halves(data, name)?))))
        }
        (T::Half4, true) => {
            Value::TypedArray(TypedArray::Vec4h(Arc::new(tuples(&halves(data, name)?))))
        }
        (T::Quath, true) => {
            Value::TypedArray(TypedArray::Quath(Arc::new(tuples(&halves(data, name)?))))
        }
        (T::Float2, true) => {
            Value::TypedArray(TypedArray::Vec2f(Arc::new(tuples(&floats(data, name)?))))
        }
        (T::Float3, true) => {
            Value::TypedArray(TypedArray::Vec3f(Arc::new(tuples(&floats(data, name)?))))
        }
        (T::Float4, true) => {
            Value::TypedArray(TypedArray::Vec4f(Arc::new(tuples(&floats(data, name)?))))
        }
        (T::Quatf, true) => {
            Value::TypedArray(TypedArray::Quatf(Arc::new(tuples(&floats(data, name)?))))
        }
        (T::Double2, true) => {
            Value::TypedArray(TypedArray::Vec2d(Arc::new(tuples(&doubles(data, name)?))))
        }
        (T::Double3, true) => {
            Value::TypedArray(TypedArray::Vec3d(Arc::new(tuples(&doubles(data, name)?))))
        }
        (T::Double4, true) => {
            Value::TypedArray(TypedArray::Vec4d(Arc::new(tuples(&doubles(data, name)?))))
        }
        (T::Quatd, true) => {
            Value::TypedArray(TypedArray::Quatd(Arc::new(tuples(&doubles(data, name)?))))
        }
        (T::Matrix2d, true) => Value::TypedArray(TypedArray::Matrix2d(Arc::new(tuples(&doubles(
            data, name,
        )?)))),
        (T::Matrix3d, true) => Value::TypedArray(TypedArray::Matrix3d(Arc::new(tuples(&doubles(
            data, name,
        )?)))),
        (T::Matrix4d, true) => Value::TypedArray(TypedArray::Matrix4d(Arc::new(tuples(&doubles(
            data, name,
        )?)))),
        (T::String, false) => Value::String(Arc::from(texts(data, name)?[0].as_str())),
        (T::String, true) => Value::TypedArray(TypedArray::String(Arc::new(
            texts(data, name)?.iter().collect::<StringArray>(),
        ))),
        (T::Token, false) => Value::Token(tokens.intern(&texts(data, name)?[0])),
        (T::Token, true) => Value::TypedArray(TypedArray::Token(
            texts(data, name)?
                .iter()
                .map(|text| tokens.intern(text))
                .collect(),
        )),
        (T::Asset, false) => Value::Asset(Arc::from(texts(data, name)?[0].as_str())),
        (T::PathExpression, false) => {
            Value::PathExpression(Arc::from(texts(data, name)?[0].as_str()))
        }
        // OpenUSD's `SdfAssetPath` and `SdfPathExpression` arrays have no
        // buffer type here: they are lists of their scalars.
        (T::Asset, true) => Value::Array(
            texts(data, name)?
                .iter()
                .map(|text| Value::Asset(Arc::from(text.as_str())))
                .collect(),
        ),
        (T::PathExpression, true) => Value::Array(
            texts(data, name)?
                .iter()
                .map(|text| Value::PathExpression(Arc::from(text.as_str())))
                .collect(),
        ),
        _ => {
            return Err(Error::new(
                "value.unsupported",
                format!("`{name}` holds no value"),
            ));
        }
    };
    Ok(value)
}

/// The scalars per element of a role-less value type; 0 for one that
/// holds no value.
fn components(storage: ValueType) -> usize {
    use ValueType as T;
    match storage {
        T::Int2 | T::Half2 | T::Float2 | T::Double2 => 2,
        T::Int3 | T::Half3 | T::Float3 | T::Double3 => 3,
        T::Int4 | T::Half4 | T::Float4 | T::Double4 => 4,
        T::Quath | T::Quatf | T::Quatd | T::Matrix2d => 4,
        T::Matrix3d => 9,
        T::Matrix4d => 16,
        T::Opaque => 0,
        _ => 1,
    }
}

fn kind_error(data: &InputData, name: &str) -> Error {
    Error::new(
        "value.kind",
        format!("`{name}` does not take {}", data.describe()),
    )
}

fn range_error(name: &str, what: impl core::fmt::Display) -> Error {
    Error::new("value.range", format!("{what} is not a `{name}` scalar"))
}

/// The scalars as integers of `I`, each exactly representable.
fn ints<I: TryFrom<i64> + TryFrom<u64>>(data: &InputData, name: &str) -> Result<Vec<I>, Error> {
    match data {
        InputData::Numbers(items) => items
            .iter()
            .map(|&number| {
                exact_integer(number)
                    .and_then(|integer| I::try_from(integer).ok())
                    .ok_or_else(|| range_error(name, number))
            })
            .collect(),
        InputData::Int64(items) => items
            .iter()
            .map(|&integer| I::try_from(integer).map_err(|_| range_error(name, integer)))
            .collect(),
        InputData::UInt64(items) => items
            .iter()
            .map(|&integer| I::try_from(integer).map_err(|_| range_error(name, integer)))
            .collect(),
        _ => Err(kind_error(data, name)),
    }
}

/// `number` as an integer, when it is one a number holds exactly.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the number is integral and within 2^53, so the cast is exact"
)]
fn exact_integer(number: f64) -> Option<i64> {
    let integer = number as i64;
    // `f64::fract` needs `std`; a round trip through `i64` checks the same.
    let exact = integer as f64 == number;
    ((-SAFE_INTEGER..=SAFE_INTEGER).contains(&number) && exact).then_some(integer)
}

fn bools(data: &InputData, name: &str) -> Result<Vec<bool>, Error> {
    let numbers: Vec<u8> = ints(data, name)?;
    numbers
        .into_iter()
        .map(|number| match number {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(range_error(name, other)),
        })
        .collect()
}

fn doubles(data: &InputData, name: &str) -> Result<Vec<f64>, Error> {
    match data {
        InputData::Numbers(items) => Ok(items.clone()),
        InputData::Floats(items) => Ok(items.iter().map(|&v| f64::from(v)).collect()),
        _ => Err(kind_error(data, name)),
    }
}

/// The scalars as `float`s: a `Float32Array` as it is, numbers rounded to
/// the nearest `float`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "rounding a number to the nearest float is the conversion asked for"
)]
fn floats(data: &InputData, name: &str) -> Result<Vec<f32>, Error> {
    match data {
        InputData::Floats(items) => Ok(items.clone()),
        InputData::Numbers(items) => items
            .iter()
            .map(|&number| {
                let float = number as f32;
                if float.is_infinite() && number.is_finite() {
                    Err(range_error(name, number))
                } else {
                    Ok(float)
                }
            })
            .collect(),
        _ => Err(kind_error(data, name)),
    }
}

/// The scalars as half float bits: raw bits as they are, numbers rounded
/// to the nearest half.
fn halves(data: &InputData, name: &str) -> Result<Vec<u16>, Error> {
    if let InputData::HalfBits(items) = data {
        return Ok(items.clone());
    }
    let floats = floats(data, name)?;
    let mut out = Vec::with_capacity(floats.len());
    for float in floats {
        let bits = half::from_f32(float);
        if half::to_f32(bits).is_infinite() && float.is_finite() {
            return Err(range_error(name, float));
        }
        out.push(bits);
    }
    Ok(out)
}

fn texts<'a>(data: &'a InputData, name: &str) -> Result<&'a [String], Error> {
    match data {
        InputData::Text(items) => Ok(items),
        _ => Err(kind_error(data, name)),
    }
}

/// The first `N` scalars as one tuple; the caller checked the length.
fn tuple<T: Copy + Default, const N: usize>(items: &[T]) -> [T; N] {
    let mut out = [T::default(); N];
    out.copy_from_slice(&items[..N]);
    out
}

/// The scalars as tuples of `N`; the caller checked the length.
fn tuples<T: Copy + Default, const N: usize>(items: &[T]) -> Vec<[T; N]> {
    items.as_chunks::<N>().0.to_vec()
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

    fn input(type_name: &str, data: InputData) -> ValueInput {
        ValueInput {
            type_name: type_name.into(),
            data,
        }
    }

    fn code(type_name: &str, data: InputData) -> &'static str {
        let tokens = TokenInterner::default();
        decode(&input(type_name, data), &tokens).unwrap_err().code
    }

    #[test]
    fn values_read_decode_back_to_themselves() {
        let tokens = TokenInterner::default();
        let round = |type_name: &str, value: Value| {
            let wire = encode(&value, Some(type_name), &tokens).unwrap().unwrap();
            let items = wire.as_list().unwrap();
            let data = match (&items[1], items.get(2), items.get(5)) {
                (Wire::Text(tag), Some(Wire::List(numbers)), _) if tag == "t" => {
                    InputData::Numbers(
                        numbers
                            .iter()
                            .map(|n| match n {
                                Wire::Number(n) => *n,
                                other => panic!("{other:?}"),
                            })
                            .collect(),
                    )
                }
                (_, _, Some(Wire::Elements(Elements::F32(items)))) => {
                    InputData::Floats(items.clone())
                }
                (_, _, Some(Wire::Elements(Elements::U16(items)))) => {
                    InputData::HalfBits(items.clone())
                }
                (_, _, Some(Wire::Elements(Elements::I64(items)))) => {
                    InputData::Int64(items.clone())
                }
                (_, _, Some(Wire::List(items))) => {
                    InputData::Text(items.iter().map(|t| t.as_text().unwrap().into()).collect())
                }
                (_, Some(Wire::Number(n)), _) => InputData::Numbers(vec![*n]),
                (_, Some(Wire::Int64(n)), _) => InputData::Int64(vec![*n]),
                (_, Some(Wire::UInt64(n)), _) => InputData::UInt64(vec![*n]),
                (_, Some(Wire::Bool(b)), _) => InputData::Numbers(vec![f64::from(u8::from(*b))]),
                (_, Some(Wire::Text(t)), _) => InputData::Text(vec![t.clone()]),
                other => panic!("{other:?}"),
            };
            assert_eq!(
                decode(&input(type_name, data), &tokens).unwrap(),
                value,
                "{type_name}"
            );
        };
        round("bool", Value::Bool(true));
        round("int", Value::Int(-7));
        round("uint64", Value::UInt64(u64::MAX));
        round("int64", Value::Int64(i64::MIN));
        round("half", Value::Half(half::from_f32(0.5)));
        round("float", Value::Float(0.1));
        round("token", Value::Token(tokens.intern("render")));
        round("asset", Value::Asset(Arc::from("./a.usda")));
        round("color3f", Value::Vec3f([0.25, 0.5, 1.0]));
        round(
            "half2",
            Value::Vec2h([half::from_f32(1.0), half::from_f32(2.0)]),
        );
        round("matrix2d", Value::Matrix2d(Box::new([1.0, 2.0, 3.0, 4.0])));
        round("quatf", Value::Quatf([0.0, 0.0, 0.0, 1.0]));
        round(
            "point3f[]",
            Value::TypedArray(TypedArray::Vec3f(Arc::new(vec![
                [0.1, 0.2, 0.3],
                [1.0, 2.0, 3.0],
            ]))),
        );
        round(
            "half[]",
            Value::TypedArray(TypedArray::Half(Arc::new(vec![half::from_f32(0.1)]))),
        );
        round(
            "int64[]",
            Value::TypedArray(TypedArray::Int64(Arc::new(vec![i64::MAX, -1]))),
        );
        round(
            "token[]",
            Value::TypedArray(TypedArray::Token(Arc::from(vec![
                tokens.intern("a"),
                tokens.intern("b"),
            ]))),
        );
        round(
            "string[]",
            Value::TypedArray(TypedArray::String(Arc::new(
                ["x", "y"].into_iter().collect::<StringArray>(),
            ))),
        );
    }

    #[test]
    fn numbers_convert_to_any_numeric_type_exactly() {
        let tokens = TokenInterner::default();
        let decoded =
            |type_name: &str, data: InputData| decode(&input(type_name, data), &tokens).unwrap();
        assert_eq!(
            decoded("int64", InputData::Numbers(vec![9_007_199_254_740_991.0])),
            Value::Int64(9_007_199_254_740_991)
        );
        assert_eq!(
            decoded("uchar", InputData::Int64(vec![255])),
            Value::UChar(255)
        );
        assert_eq!(
            decoded("double[]", InputData::Floats(vec![0.5, 1.5])),
            Value::TypedArray(TypedArray::Double(Arc::new(vec![0.5, 1.5])))
        );
        assert_eq!(
            decoded("asset[]", InputData::Text(vec!["./a.usda".into()])),
            Value::Array(vec![Value::Asset(Arc::from("./a.usda"))])
        );
        assert_eq!(
            decoded("timecode", InputData::Numbers(vec![24.0])),
            Value::TimeCode(24.0)
        );
    }

    #[test]
    fn bad_values_fail_with_precise_codes() {
        assert_eq!(
            code("float9", InputData::Numbers(vec![1.0])),
            "value.unknown_type"
        );
        assert_eq!(
            code("opaque", InputData::Numbers(vec![])),
            "value.unsupported"
        );
        assert_eq!(
            code("double3", InputData::Numbers(vec![1.0, 2.0])),
            "value.shape"
        );
        assert_eq!(
            code("double3[]", InputData::Numbers(vec![1.0; 4])),
            "value.shape"
        );
        assert_eq!(code("int", InputData::Numbers(vec![1.5])), "value.range");
        assert_eq!(code("uint", InputData::Numbers(vec![-1.0])), "value.range");
        assert_eq!(code("int", InputData::Int64(vec![i64::MAX])), "value.range");
        assert_eq!(
            code("int64", InputData::Numbers(vec![9_007_199_254_740_993.0])),
            "value.range",
            "past 2^53 a number is not exact: send a bigint"
        );
        assert_eq!(code("bool", InputData::Numbers(vec![2.0])), "value.range");
        assert_eq!(
            code("float", InputData::Numbers(vec![1e300])),
            "value.range"
        );
        assert_eq!(code("half", InputData::Numbers(vec![1e6])), "value.range");
        assert_eq!(
            code("float", InputData::Text(vec!["1".into()])),
            "value.kind"
        );
        assert_eq!(code("token", InputData::Numbers(vec![1.0])), "value.kind");
        assert_eq!(code("double", InputData::Int64(vec![1])), "value.kind");
        assert_eq!(code("float", InputData::HalfBits(vec![1])), "value.kind");
    }
}
