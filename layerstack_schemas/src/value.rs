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
    ($read:ident, $write:ident, $ty:ty, $variant:ident, $read_array:ident, $write_array:ident) => {
        pub(crate) fn $read(value: &Value, _: &TokenInterner) -> Option<$ty> {
            match value {
                Value::$variant(v) => Some(*v),
                _ => None,
            }
        }

        pub(crate) fn $write(value: $ty, _: &mut TokenInterner) -> Value {
            Value::$variant(value)
        }

        pub(crate) fn $read_array(value: &Value, tokens: &TokenInterner) -> Option<Vec<$ty>> {
            match value {
                Value::TypedArray(layerstack::TypedArray::$variant(items)) => {
                    Some(items.as_ref().clone())
                }
                Value::TypedArray(_) => None,
                _ => read_array(value, tokens, $read),
            }
        }

        pub(crate) fn $write_array(items: &[$ty], _: &mut TokenInterner) -> Value {
            Value::TypedArray(layerstack::TypedArray::$variant(Arc::new(items.to_vec())))
        }
    };
}

plain!(
    read_bool,
    write_bool,
    bool,
    Bool,
    read_bool_array,
    write_bool_array
);
plain!(
    read_uchar,
    write_uchar,
    u8,
    UChar,
    read_uchar_array,
    write_uchar_array
);
plain!(
    read_int,
    write_int,
    i32,
    Int,
    read_int_array,
    write_int_array
);
plain!(
    read_uint,
    write_uint,
    u32,
    UInt,
    read_uint_array,
    write_uint_array
);
plain!(
    read_int64,
    write_int64,
    i64,
    Int64,
    read_int64_array,
    write_int64_array
);
plain!(
    read_uint64,
    write_uint64,
    u64,
    UInt64,
    read_uint64_array,
    write_uint64_array
);
plain!(
    read_float,
    write_float,
    f32,
    Float,
    read_float_array,
    write_float_array
);
plain!(
    read_double,
    write_double,
    f64,
    Double,
    read_double_array,
    write_double_array
);
plain!(
    read_timecode,
    write_timecode,
    f64,
    TimeCode,
    read_timecode_array,
    write_timecode_array
);
plain!(
    read_float2,
    write_float2,
    [f32; 2],
    Vec2f,
    read_float2_array,
    write_float2_array
);
plain!(
    read_float3,
    write_float3,
    [f32; 3],
    Vec3f,
    read_float3_array,
    write_float3_array
);
plain!(
    read_float4,
    write_float4,
    [f32; 4],
    Vec4f,
    read_float4_array,
    write_float4_array
);
plain!(
    read_double2,
    write_double2,
    [f64; 2],
    Vec2d,
    read_double2_array,
    write_double2_array
);
plain!(
    read_double3,
    write_double3,
    [f64; 3],
    Vec3d,
    read_double3_array,
    write_double3_array
);
plain!(
    read_double4,
    write_double4,
    [f64; 4],
    Vec4d,
    read_double4_array,
    write_double4_array
);
plain!(
    read_int2,
    write_int2,
    [i32; 2],
    Vec2i,
    read_int2_array,
    write_int2_array
);
plain!(
    read_int3,
    write_int3,
    [i32; 3],
    Vec3i,
    read_int3_array,
    write_int3_array
);
plain!(
    read_int4,
    write_int4,
    [i32; 4],
    Vec4i,
    read_int4_array,
    write_int4_array
);
plain!(
    read_quatf,
    write_quatf,
    [f32; 4],
    Quatf,
    read_quatf_array,
    write_quatf_array
);
plain!(
    read_quatd,
    write_quatd,
    [f64; 4],
    Quatd,
    read_quatd_array,
    write_quatd_array
);

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

pub(crate) fn read_half_array(value: &Value, tokens: &TokenInterner) -> Option<Vec<f32>> {
    match value {
        Value::TypedArray(layerstack::TypedArray::Half(items)) => {
            Some(items.iter().copied().map(half::to_f32).collect())
        }
        Value::TypedArray(_) => None,
        _ => read_array(value, tokens, read_half),
    }
}

pub(crate) fn write_half_array(items: &[f32], _: &mut TokenInterner) -> Value {
    Value::TypedArray(layerstack::TypedArray::Half(Arc::new(
        items.iter().copied().map(half::from_f32).collect(),
    )))
}

macro_rules! halves {
    ($read:ident, $write:ident, $n:literal, $variant:ident, $read_array:ident, $write_array:ident) => {
        pub(crate) fn $read(value: &Value, _: &TokenInterner) -> Option<[f32; $n]> {
            match value {
                Value::$variant(bits) => Some(bits.map(half::to_f32)),
                _ => None,
            }
        }

        pub(crate) fn $write(value: [f32; $n], _: &mut TokenInterner) -> Value {
            Value::$variant(value.map(half::from_f32))
        }

        pub(crate) fn $read_array(value: &Value, tokens: &TokenInterner) -> Option<Vec<[f32; $n]>> {
            match value {
                Value::TypedArray(layerstack::TypedArray::$variant(items)) => {
                    Some(items.iter().map(|bits| bits.map(half::to_f32)).collect())
                }
                Value::TypedArray(_) => None,
                _ => read_array(value, tokens, $read),
            }
        }

        pub(crate) fn $write_array(items: &[[f32; $n]], _: &mut TokenInterner) -> Value {
            Value::TypedArray(layerstack::TypedArray::$variant(Arc::new(
                items
                    .iter()
                    .map(|value| value.map(half::from_f32))
                    .collect(),
            )))
        }
    };
}

halves!(
    read_half2,
    write_half2,
    2,
    Vec2h,
    read_half2_array,
    write_half2_array
);
halves!(
    read_half3,
    write_half3,
    3,
    Vec3h,
    read_half3_array,
    write_half3_array
);
halves!(
    read_half4,
    write_half4,
    4,
    Vec4h,
    read_half4_array,
    write_half4_array
);
halves!(
    read_quath,
    write_quath,
    4,
    Quath,
    read_quath_array,
    write_quath_array
);

macro_rules! matrix {
    ($read:ident, $write:ident, $n:literal, $variant:ident, $read_array:ident, $write_array:ident) => {
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

        pub(crate) fn $read_array(
            value: &Value,
            tokens: &TokenInterner,
        ) -> Option<Vec<[[f64; $n]; $n]>> {
            match value {
                Value::TypedArray(layerstack::TypedArray::$variant(items)) => Some(
                    items
                        .iter()
                        .map(|flat| {
                            core::array::from_fn(|row| {
                                core::array::from_fn(|column| flat[row * $n + column])
                            })
                        })
                        .collect(),
                ),
                Value::TypedArray(_) => None,
                _ => read_array(value, tokens, $read),
            }
        }

        pub(crate) fn $write_array(items: &[[[f64; $n]; $n]], _: &mut TokenInterner) -> Value {
            Value::TypedArray(layerstack::TypedArray::$variant(Arc::new(
                items
                    .iter()
                    .map(|rows| {
                        let mut flat = [0.0; $n * $n];
                        for (i, row) in rows.iter().enumerate() {
                            flat[i * $n..(i + 1) * $n].copy_from_slice(row);
                        }
                        flat
                    })
                    .collect(),
            )))
        }
    };
}

matrix!(
    read_matrix2d,
    write_matrix2d,
    2,
    Matrix2d,
    read_matrix2d_array,
    write_matrix2d_array
);
matrix!(
    read_matrix3d,
    write_matrix3d,
    3,
    Matrix3d,
    read_matrix3d_array,
    write_matrix3d_array
);
matrix!(
    read_matrix4d,
    write_matrix4d,
    4,
    Matrix4d,
    read_matrix4d_array,
    write_matrix4d_array
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_array_readers_keep_kind_and_bits() {
        let mut tokens = TokenInterner::default();
        let values = [[-0.0_f32, f32::from_bits(0x7fc0_1234), 2.0]];
        let native = write_float3_array(&values, &mut tokens);
        let read = read_float3_array(&native, &tokens).unwrap();
        assert_eq!(read[0].map(f32::to_bits), values[0].map(f32::to_bits));
        let empty = write_float3_array(&[], &mut tokens);
        assert!(matches!(
            empty,
            Value::TypedArray(layerstack::TypedArray::Vec3f(_))
        ));
        assert_eq!(read_float3_array(&empty, &tokens), Some(Vec::new()));
        assert_eq!(read_double3_array(&empty, &tokens), None);
        assert_eq!(read_quatf_array(&empty, &tokens), None);
        assert_eq!(
            read_float3_array(&Value::Array(Vec::new()), &tokens),
            Some(Vec::new())
        );

        let times = write_timecode_array(&[1.0, -0.0], &mut tokens);
        assert_eq!(
            read_timecode_array(&times, &tokens),
            Some(alloc::vec![1.0, -0.0])
        );
        assert_eq!(read_double_array(&times, &tokens), None);
        let halves = [[0.5, 1.0, 2.0]];
        assert_eq!(
            read_half3_array(&write_half3_array(&halves, &mut tokens), &tokens),
            Some(halves.to_vec())
        );
    }

    #[test]
    fn native_matrix_arrays_convert_rows_without_scalar_boxes() {
        let mut tokens = TokenInterner::default();
        let rows = core::array::from_fn::<_, 4, _>(|row| {
            core::array::from_fn::<_, 4, _>(|column| {
                f64::from(u8::try_from(row * 4 + column).unwrap())
            })
        });
        let native = write_matrix4d_array(&[rows], &mut tokens);
        let Value::TypedArray(layerstack::TypedArray::Matrix4d(flat)) = &native else {
            panic!("native matrix storage");
        };
        assert_eq!(flat[0][6], rows[1][2]);
        assert_eq!(
            read_matrix4d_array(&native, &tokens),
            Some(alloc::vec![rows])
        );
        let legacy = Value::Array(alloc::vec![write_matrix4d(rows, &mut tokens)]);
        assert_eq!(
            read_matrix4d_array(&legacy, &tokens),
            read_matrix4d_array(&native, &tokens)
        );
        assert_eq!(read_matrix3d_array(&native, &tokens), None);
    }

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
