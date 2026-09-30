// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Native numeric array conversion shared by bulk and nested field decoding.

use crate::value_rep::{FloatArray, IntegerArray, MathArray};
use crate::value_type::ValueType;
use alloc::sync::Arc;
use layerstack::Value;

pub(crate) fn float_array(array: FloatArray) -> layerstack::TypedArray {
    use layerstack::TypedArray as A;
    match array {
        FloatArray::Half(values) => A::Half(Arc::new(values)),
        FloatArray::Float(values) => A::Float(Arc::new(values)),
        FloatArray::Double(values) => A::Double(Arc::new(values)),
        FloatArray::TimeCode(values) => A::TimeCode(Arc::new(values)),
    }
}

pub(crate) fn integer_array(array: IntegerArray) -> layerstack::TypedArray {
    if array.value_type == ValueType::Int64 {
        return layerstack::TypedArray::Int64(Arc::new(array.values));
    }
    let Value::TypedArray(value) = integer_values(array.value_type, array.values.into_iter())
    else {
        unreachable!("numeric array")
    };
    value
}

pub(crate) fn math_array(array: &MathArray<'_>) -> layerstack::TypedArray {
    let Value::TypedArray(value) = convert_math_array(array) else {
        unreachable!("numeric array")
    };
    value
}

/// Converts integer components without materializing a `CrateValue` per item.
/// The narrowing casts preserve the public decoder's bitwise signed/unsigned
/// interpretation (AOUSD Core §16.3.10).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "integer element bit patterns"
)]
pub(crate) fn convert_integer_array(vtype: ValueType, values: &[i64]) -> Value {
    integer_values(vtype, values.iter().copied())
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "integer element bit patterns"
)]
fn integer_values(vtype: ValueType, values: impl Iterator<Item = i64>) -> Value {
    use layerstack::TypedArray as A;
    Value::TypedArray(match vtype {
        ValueType::Bool => A::Bool(Arc::new(values.map(|v| v != 0).collect())),
        ValueType::UChar => A::UChar(Arc::new(values.map(|v| v as u8).collect())),
        ValueType::Int => A::Int(Arc::new(values.map(|v| v as i32).collect())),
        ValueType::UInt => A::UInt(Arc::new(values.map(|v| v as u32).collect())),
        ValueType::Int64 => A::Int64(Arc::new(values.collect())),
        ValueType::UInt64 => A::UInt64(Arc::new(values.map(|v| v as u64).collect())),
        _ => unreachable!("integer array types are selected by decode_field_within"),
    })
}

/// Dispatches once per array so each element is constructed directly in its
/// destination rather than passing through the scalar type switch.
pub(crate) fn convert_math_array(array: &MathArray<'_>) -> Value {
    macro_rules! convert {
        ($($variant:ident => $read:expr),* $(,)?) => {
            Value::TypedArray(match array.value_type {
                $(ValueType::$variant => layerstack::TypedArray::$variant(Arc::new(array.elements()
                    .map($read)
                    .collect())),)*
                _ => unreachable!("math array types are selected by decode_field_within"),
            })
        };
    }
    convert! {
        Vec2d => read_f64x2,
        Vec3d => read_f64x3,
        Vec4d => read_f64x4,
        Vec2f => read_f32x2,
        Vec3f => read_f32x3,
        Vec4f => read_f32x4,
        Vec2h => read_u16x2,
        Vec3h => read_u16x3,
        Vec4h => read_u16x4,
        Vec2i => read_i32x2,
        Vec3i => read_i32x3,
        Vec4i => read_i32x4,
        Quatd => read_f64x4,
        Quatf => read_f32x4,
        Quath => read_u16x4,
        Matrix2d => read_f64_array::<4>,
        Matrix3d => read_f64_array::<9>,
        Matrix4d => read_f64_array::<16>,
    }
}

pub(crate) fn read_f64x2(d: &[u8]) -> [f64; 2] {
    [f64_le(d, 0), f64_le(d, 1)]
}

pub(crate) fn read_f64x3(d: &[u8]) -> [f64; 3] {
    [f64_le(d, 0), f64_le(d, 1), f64_le(d, 2)]
}

pub(crate) fn read_f64x4(d: &[u8]) -> [f64; 4] {
    [f64_le(d, 0), f64_le(d, 1), f64_le(d, 2), f64_le(d, 3)]
}

pub(crate) fn read_f32x2(d: &[u8]) -> [f32; 2] {
    [f32_le(d, 0), f32_le(d, 1)]
}

pub(crate) fn read_f32x3(d: &[u8]) -> [f32; 3] {
    [f32_le(d, 0), f32_le(d, 1), f32_le(d, 2)]
}

pub(crate) fn read_f32x4(d: &[u8]) -> [f32; 4] {
    [f32_le(d, 0), f32_le(d, 1), f32_le(d, 2), f32_le(d, 3)]
}

pub(crate) fn read_u16x2(d: &[u8]) -> [u16; 2] {
    [u16_le(d, 0), u16_le(d, 1)]
}

pub(crate) fn read_u16x3(d: &[u8]) -> [u16; 3] {
    [u16_le(d, 0), u16_le(d, 1), u16_le(d, 2)]
}

pub(crate) fn read_u16x4(d: &[u8]) -> [u16; 4] {
    [u16_le(d, 0), u16_le(d, 1), u16_le(d, 2), u16_le(d, 3)]
}

pub(crate) fn read_i32x2(d: &[u8]) -> [i32; 2] {
    [i32_le(d, 0), i32_le(d, 1)]
}

pub(crate) fn read_i32x3(d: &[u8]) -> [i32; 3] {
    [i32_le(d, 0), i32_le(d, 1), i32_le(d, 2)]
}

pub(crate) fn read_i32x4(d: &[u8]) -> [i32; 4] {
    [i32_le(d, 0), i32_le(d, 1), i32_le(d, 2), i32_le(d, 3)]
}

pub(crate) fn read_f64_array<const N: usize>(d: &[u8]) -> [f64; N] {
    let mut out = [0.0_f64; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = f64_le(d, i);
    }
    out
}

// In the readers below, `idx` is a component index of a vector, quaternion
// or matrix (at most 15), so the offsets cannot overflow.

fn f64_le(d: &[u8], idx: usize) -> f64 {
    let off = idx * 8;
    f64::from_le_bytes(
        d.get(off..off + 8)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 8]),
    )
}

fn f32_le(d: &[u8], idx: usize) -> f32 {
    let off = idx * 4;
    f32::from_le_bytes(
        d.get(off..off + 4)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 4]),
    )
}

fn u16_le(d: &[u8], idx: usize) -> u16 {
    let off = idx * 2;
    u16::from_le_bytes(
        d.get(off..off + 2)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 2]),
    )
}

fn i32_le(d: &[u8], idx: usize) -> i32 {
    let off = idx * 4;
    i32::from_le_bytes(
        d.get(off..off + 4)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 4]),
    )
}

#[cfg(test)]
mod tests {
    use super::integer_array;
    use crate::value_rep::IntegerArray;
    use crate::value_rep::{
        CrateValue, DecodeBudget, DecodedField, RawValueRep, decode_field_within, decode_value,
    };
    use crate::value_type::SpecForm;
    use crate::value_type::ValueType;
    use crate::writer::{Spec, Specifier, Value as W, write_crate};
    use crate::{header::parse_header, section::parse_sections, toc::parse_toc};
    use alloc::vec;

    #[test]
    fn owned_int64_decode_retains_its_buffer() {
        let values = vec![i64::MIN, -1, 0, i64::MAX];
        let pointer = values.as_ptr();
        let layerstack::TypedArray::Int64(retained) = integer_array(IntegerArray {
            value_type: ValueType::Int64,
            values,
        }) else {
            panic!("int64 buffer");
        };
        assert_eq!(retained.as_ptr(), pointer);
        assert_eq!(retained.as_slice(), [i64::MIN, -1, 0, i64::MAX]);
        let layerstack::TypedArray::UInt64(unsigned) = integer_array(IntegerArray {
            value_type: ValueType::UInt64,
            values: vec![-1, i64::MIN],
        }) else {
            panic!("uint64 buffer");
        };
        assert_eq!(unsigned.as_slice(), [u64::MAX, 1_u64 << 63]);
    }

    #[test]
    fn sampled_and_dictionary_arrays_decode_natively_without_changing_generic_reads() {
        let specs = vec![
            Spec::new("/", SpecForm::PseudoRoot)
                .with_field("primChildren", W::TokenVector(vec!["Root".into()])),
            Spec::new("/Root", SpecForm::Prim)
                .with_field("specifier", W::Specifier(Specifier::Def))
                .with_field(
                    "customData",
                    W::Dictionary(vec![
                        ("points".into(), W::Vec3fArray(vec![[1.0, 2.0, 3.0]])),
                        ("matrices".into(), W::Matrix4dArray(vec![[[2.0; 4]; 4]])),
                    ]),
                ),
            Spec::new("/Root.points", SpecForm::Attribute)
                .with_field("typeName", W::Token("point3f[]".into()))
                .with_field(
                    "timeSamples",
                    W::TimeSamples(vec![
                        (1.0, W::Vec3fArray(vec![[1.0, 2.0, 3.0]])),
                        (2.0, W::Vec3fArray(vec![])),
                    ]),
                ),
        ];
        let bytes = write_crate(&specs).unwrap();
        let header = parse_header(&bytes).unwrap();
        let toc = parse_toc(&bytes, header.toc_offset).unwrap();
        let sections = parse_sections(
            &bytes,
            &toc,
            header.crate_version(),
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();
        let rep = |name| {
            RawValueRep::new(
                sections
                    .fields
                    .iter()
                    .find(|field| sections.tokens[field.token_index as usize] == name)
                    .unwrap()
                    .value_rep,
            )
        };
        let samples = decode_field_within(
            &rep("timeSamples"),
            &bytes,
            &sections,
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();
        let DecodedField::Value(CrateValue::TimeSamples(samples)) = samples else {
            panic!("samples");
        };
        assert!(
            matches!(&samples[0].1, CrateValue::TypedArray(layerstack::TypedArray::Vec3f(v)) if v.as_slice() == [[1.0,2.0,3.0]])
        );
        assert!(
            matches!(&samples[1].1, CrateValue::TypedArray(layerstack::TypedArray::Vec3f(v)) if v.is_empty())
        );
        let dictionary = decode_field_within(
            &rep("customData"),
            &bytes,
            &sections,
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();
        let DecodedField::Value(CrateValue::Dictionary(dictionary)) = dictionary else {
            panic!("dictionary");
        };
        assert!(dictionary.iter().any(|(_, value)| matches!(value, CrateValue::TypedArray(layerstack::TypedArray::Matrix4d(v)) if v.as_slice() == [[2.0;16]])));
        let CrateValue::TimeSamples(generic) =
            decode_value(&rep("timeSamples"), &bytes, &sections).unwrap()
        else {
            panic!("generic samples");
        };
        assert!(
            matches!(&generic[0].1, CrateValue::Array(_)),
            "generic decoding keeps its documented representation"
        );
        // Resource charging remains representation-independent: both paths
        // reject insufficient budgets, including nested values.
        assert!(
            decode_field_within(
                &rep("timeSamples"),
                &bytes,
                &sections,
                &mut DecodeBudget::with_limit(1)
            )
            .is_err()
        );
    }
}
