// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Conversions between layerstack [`Value`]s and the Rust types the views
//! read and write, one pair per USD value type.
//!
//! Each `read_*` returns `None` for a value of another type, so a view
//! getter reads an attribute authored with the wrong type as no value.
//! Quaternions are `[i, j, k, r]`, as layerstack stores them; matrices are
//! rows, `m[row][column]`, with translations in the last row (AOUSD Core
//! §6.3). Halves are read and written as `f32`.

#![allow(
    dead_code,
    reason = "each conversion serves a value type some domain declares, and a build \
              with fewer domains uses fewer of them"
)]

use alloc::{sync::Arc, vec::Vec};

use layerstack::{TokenInterner, Value, half};

/// Reads an array of `read`'s type.
pub(crate) fn read_array<'a, T>(
    value: &Value,
    tokens: &'a TokenInterner,
    read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Option<Vec<T>> {
    let array = value.array_ref()?;
    if let Some(typed) = array.typed() {
        // Empty native buffers retain a kind; an empty buffer of another
        // element kind must not silently pass a typed read.
        read(&typed.element_kind(), tokens)?;
    }
    array.iter().map(|item| read(&item, tokens)).collect()
}

/// Writes an array of `write`'s type.
pub(crate) fn write_array<T: Copy>(
    items: &[T],
    tokens: &mut TokenInterner,
    write: impl Fn(T, &mut TokenInterner) -> Value,
) -> Value {
    Value::array_from_iter(items.iter().map(|item| write(*item, tokens)), None)
}

macro_rules! plain {
    ($read:ident, $write:ident, $ty:ty, $variant:ident) => {
        pub(crate) fn $read(value: &Value, _: &TokenInterner) -> Option<$ty> {
            match value {
                Value::$variant(v) => Some(*v),
                _ => None,
            }
        }

        pub(crate) fn $write(value: $ty, _: &mut TokenInterner) -> Value {
            Value::$variant(value)
        }
    };
}

plain!(read_bool, write_bool, bool, Bool);
plain!(read_uchar, write_uchar, u8, UChar);
plain!(read_int, write_int, i32, Int);
plain!(read_uint, write_uint, u32, UInt);
plain!(read_int64, write_int64, i64, Int64);
plain!(read_uint64, write_uint64, u64, UInt64);
plain!(read_float, write_float, f32, Float);
plain!(read_double, write_double, f64, Double);
plain!(read_timecode, write_timecode, f64, TimeCode);
plain!(read_float2, write_float2, [f32; 2], Vec2f);
plain!(read_float3, write_float3, [f32; 3], Vec3f);
plain!(read_float4, write_float4, [f32; 4], Vec4f);
plain!(read_double2, write_double2, [f64; 2], Vec2d);
plain!(read_double3, write_double3, [f64; 3], Vec3d);
plain!(read_double4, write_double4, [f64; 4], Vec4d);
plain!(read_int2, write_int2, [i32; 2], Vec2i);
plain!(read_int3, write_int3, [i32; 3], Vec3i);
plain!(read_int4, write_int4, [i32; 4], Vec4i);
plain!(read_quatf, write_quatf, [f32; 4], Quatf);
plain!(read_quatd, write_quatd, [f64; 4], Quatd);

macro_rules! text {
    ($read:ident, $write:ident, $variant:ident) => {
        pub(crate) fn $read(value: &Value, _: &TokenInterner) -> Option<Arc<str>> {
            match value {
                Value::$variant(v) => Some(v.clone()),
                _ => None,
            }
        }

        pub(crate) fn $write(value: &str, _: &mut TokenInterner) -> Value {
            Value::$variant(Arc::from(value))
        }
    };
}

text!(read_string, write_string, String);
text!(read_asset, write_asset, Asset);
text!(read_path_expression, write_path_expression, PathExpression);

pub(crate) fn read_token<'a>(value: &Value, tokens: &'a TokenInterner) -> Option<&'a str> {
    match value {
        Value::Token(token) => Some(tokens.resolve(*token)),
        _ => None,
    }
}

pub(crate) fn write_token(value: &str, tokens: &mut TokenInterner) -> Value {
    Value::Token(tokens.intern(value))
}

pub(crate) fn read_half(value: &Value, _: &TokenInterner) -> Option<f32> {
    match value {
        Value::Half(bits) => Some(half::to_f32(*bits)),
        _ => None,
    }
}

pub(crate) fn write_half(value: f32, _: &mut TokenInterner) -> Value {
    Value::Half(half::from_f32(value))
}

macro_rules! halves {
    ($read:ident, $write:ident, $n:literal, $variant:ident) => {
        pub(crate) fn $read(value: &Value, _: &TokenInterner) -> Option<[f32; $n]> {
            match value {
                Value::$variant(bits) => Some(bits.map(half::to_f32)),
                _ => None,
            }
        }

        pub(crate) fn $write(value: [f32; $n], _: &mut TokenInterner) -> Value {
            Value::$variant(value.map(half::from_f32))
        }
    };
}

halves!(read_half2, write_half2, 2, Vec2h);
halves!(read_half3, write_half3, 3, Vec3h);
halves!(read_half4, write_half4, 4, Vec4h);
halves!(read_quath, write_quath, 4, Quath);

macro_rules! matrix {
    ($read:ident, $write:ident, $n:literal, $variant:ident) => {
        pub(crate) fn $read(value: &Value, _: &TokenInterner) -> Option<[[f64; $n]; $n]> {
            match value {
                Value::$variant(m) => {
                    let mut rows = [[0.0; $n]; $n];
                    for (i, row) in rows.iter_mut().enumerate() {
                        row.copy_from_slice(&m[i * $n..(i + 1) * $n]);
                    }
                    Some(rows)
                }
                _ => None,
            }
        }

        pub(crate) fn $write(value: [[f64; $n]; $n], _: &mut TokenInterner) -> Value {
            let mut flat = [0.0; $n * $n];
            for (i, row) in value.iter().enumerate() {
                flat[i * $n..(i + 1) * $n].copy_from_slice(row);
            }
            Value::$variant(alloc::boxed::Box::new(flat))
        }
    };
}

matrix!(read_matrix2d, write_matrix2d, 2, Matrix2d);
matrix!(read_matrix3d, write_matrix3d, 3, Matrix3d);
matrix!(read_matrix4d, write_matrix4d, 4, Matrix4d);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip() {
        let mut tokens = TokenInterner::default();
        let matrix = [
            [1.0, 2.0, 3.0, 4.0],
            [5.0, 6.0, 7.0, 8.0],
            [9.0, 10.0, 11.0, 12.0],
            [13.0, 14.0, 15.0, 16.0],
        ];
        let written = write_matrix4d(matrix, &mut tokens);
        assert_eq!(
            written,
            Value::Matrix4d(alloc::boxed::Box::new(core::array::from_fn(|i| {
                f64::from(u8::try_from(i + 1).expect("small"))
            })))
        );
        assert_eq!(read_matrix4d(&written, &tokens), Some(matrix));

        let token = write_token("catmullClark", &mut tokens);
        assert_eq!(read_token(&token, &tokens), Some("catmullClark"));
        let halves = write_half3([0.5, 1.0, 2.0], &mut tokens);
        assert_eq!(read_half3(&halves, &tokens), Some([0.5, 1.0, 2.0]));
        let array = write_array(&[[1.0_f32, 2.0, 3.0]], &mut tokens, write_float3);
        assert_eq!(
            read_array(&array, &tokens, read_float3),
            Some(alloc::vec![[1.0, 2.0, 3.0]])
        );
        // Another type reads as no value.
        assert_eq!(read_float(&Value::Double(1.0), &tokens), None);
    }
}
