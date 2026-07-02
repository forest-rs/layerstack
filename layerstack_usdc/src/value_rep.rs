// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Value representation decoder for USDC fields.
//!
//! Each field in the FIELDS section carries an 8-byte `RawValueRep` that
//! encodes the value type, flags, and either an inlined scalar or an offset
//! to the value data elsewhere in the file.
//!
//! Spec: AOUSD Core §16.3.9–§16.3.10.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use layerstack::spline::{
    CurveType, Extrapolation, Knot, KnotInterp, LoopParams, SplineData, SplineDataType,
};

use crate::compression::read_compressed_ints;
use crate::error::UsdcError;
use crate::section::CrateSections;
use crate::value_type::ValueType;

// ---------------------------------------------------------------------------
// Raw representation
// ---------------------------------------------------------------------------

/// A raw 8-byte value representation from the FIELDS section.
///
/// Layout: bytes 0–5 = payload, byte 6 = `ValueType`, byte 7 = flags.
///
/// Flag bits:
/// - bit 7 (0x80): `is_array`
/// - bit 6 (0x40): `is_inlined`
/// - bit 5 (0x20): `is_compressed`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawValueRep {
    /// The raw 8 bytes.
    pub bytes: [u8; 8],
}

impl RawValueRep {
    /// Creates a new `RawValueRep` from raw bytes.
    #[must_use]
    pub fn new(bytes: [u8; 8]) -> Self {
        Self { bytes }
    }

    /// The 6-byte payload.
    #[must_use]
    pub fn payload(&self) -> [u8; 6] {
        let mut p = [0_u8; 6];
        p.copy_from_slice(&self.bytes[..6]);
        p
    }

    /// The value type.
    pub fn value_type(&self) -> Result<ValueType, UsdcError> {
        ValueType::try_from(self.bytes[6])
    }

    /// Flag byte.
    #[must_use]
    pub fn flags(&self) -> u8 {
        self.bytes[7]
    }

    /// Whether this is an array value.
    #[must_use]
    pub fn is_array(&self) -> bool {
        self.flags() & 0x80 != 0
    }

    /// Whether the value is inlined in the payload.
    #[must_use]
    pub fn is_inlined(&self) -> bool {
        self.flags() & 0x40 != 0
    }

    /// Whether the value data is compressed.
    #[must_use]
    pub fn is_compressed(&self) -> bool {
        self.flags() & 0x20 != 0
    }

    /// The payload interpreted as a little-endian u48 offset (for non-inlined
    /// values).
    #[must_use]
    pub fn payload_offset(&self) -> u64 {
        let p = self.payload();
        u64::from(p[0])
            | (u64::from(p[1]) << 8)
            | (u64::from(p[2]) << 16)
            | (u64::from(p[3]) << 24)
            | (u64::from(p[4]) << 32)
            | (u64::from(p[5]) << 40)
    }
}

// ---------------------------------------------------------------------------
// Decoded value types
// ---------------------------------------------------------------------------

/// A decoded crate value, ready for translation to `layerstack::doc::Value`.
#[derive(Clone, Debug)]
pub enum CrateValue {
    /// No value (e.g. `ValueBlock`).
    None,
    /// Boolean.
    Bool(bool),
    /// Unsigned 8-bit integer.
    UChar(u8),
    /// Signed 32-bit integer.
    Int(i32),
    /// Unsigned 32-bit integer.
    UInt(u32),
    /// Signed 64-bit integer.
    Int64(i64),
    /// Unsigned 64-bit integer.
    UInt64(u64),
    /// Half-precision float stored as raw `u16` bits.
    Half(u16),
    /// Single-precision float.
    Float(f32),
    /// Double-precision float.
    Double(f64),
    /// String value (resolved from STRINGS section).
    String(String),
    /// Token value (resolved from TOKENS section).
    Token(String),
    /// Asset path string.
    AssetPath(String),
    /// Specifier enum (0=Def, 1=Over, 2=Class).
    Specifier(u32),
    /// Variability enum (0=Varying, 1=Uniform).
    Variability(u32),
    /// Permission enum.
    Permission(u32),
    /// Opaque bytes tagged with value type (for math types, etc.).
    Opaque {
        /// The value type this data represents.
        value_type: ValueType,
        /// Raw element bytes.
        data: Vec<u8>,
    },
    /// An array of crate values.
    Array(Vec<Self>),
    /// A dictionary (string key → crate value).
    Dictionary(Vec<(String, Self)>),
    /// A list operation.
    ListOp(CrateListOp),
    /// Time samples (timecode → value).
    TimeSamples(Vec<(f64, Self)>),
    /// Variant selection map (variant set → selection).
    VariantSelectionMap(Vec<(String, String)>),
    /// A vector of paths (resolved strings).
    PathVector(Vec<String>),
    /// A vector of tokens (resolved strings).
    TokenVector(Vec<String>),
    /// A vector of doubles.
    DoubleVector(Vec<f64>),
    /// A vector of strings (resolved).
    StringVector(Vec<String>),
    /// A vector of layer offsets `(offset, scale)`.
    LayerOffsetVector(Vec<(f64, f64)>),
    /// Relocates map (source path → target path).
    RelocatesMap(Vec<(String, String)>),
    /// Decoded spline data (§16.3.10.33).
    Spline(SplineData),
}

/// A decoded list operation.
#[derive(Clone, Debug)]
pub struct CrateListOp {
    /// The list op type (which value type it operates on).
    pub op_type: ValueType,
    /// Explicit items (if set, replaces the list).
    pub explicit_items: Option<Vec<CrateValue>>,
    /// Prepended items.
    pub prepended_items: Vec<CrateValue>,
    /// Appended items.
    pub appended_items: Vec<CrateValue>,
    /// Deleted items.
    pub deleted_items: Vec<CrateValue>,
}

/// A decoded reference or payload.
#[derive(Clone, Debug)]
pub struct CrateReference {
    /// Asset path (from STRINGS).
    pub asset_path: String,
    /// Prim path (from PATHS).
    pub prim_path: String,
    /// Layer offset.
    pub layer_offset: f64,
    /// Layer scale.
    pub layer_scale: f64,
}

// ---------------------------------------------------------------------------
// Decoding entry point
// ---------------------------------------------------------------------------

/// Decodes a raw value representation into a `CrateValue`.
///
/// `data` is the full file byte slice (needed for offset-based reads).
/// `sections` provides the decoded token/string/path tables.
pub fn decode_value(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let vtype = rep.value_type()?;

    match vtype {
        ValueType::Unknown => Err(UsdcError::Inconsistent {
            message: "encountered Unknown value type",
        }),
        ValueType::ValueBlock => Ok(CrateValue::None),
        ValueType::Bool => decode_bool(rep, data),
        ValueType::UChar => decode_integer_u8(rep, data),
        ValueType::Int => decode_integer_i32(rep, data),
        ValueType::UInt => decode_integer_u32(rep, data),
        ValueType::Int64 => decode_integer_i64(rep, data),
        ValueType::UInt64 => decode_integer_u64(rep, data),
        ValueType::Half | ValueType::Float | ValueType::Double | ValueType::TimeCode => {
            decode_float(rep, data, vtype)
        }
        ValueType::String => decode_string(rep, data, sections),
        ValueType::Token => decode_token(rep, data, sections),
        ValueType::AssetPath | ValueType::PathExpression => decode_asset_path(rep, data, sections),
        ValueType::Specifier => {
            let v = decode_inlined_or_offset_u32(rep, data)?;
            Ok(CrateValue::Specifier(v))
        }
        ValueType::Variability => {
            let v = decode_inlined_or_offset_u32(rep, data)?;
            Ok(CrateValue::Variability(v))
        }
        ValueType::Permission => {
            let v = decode_inlined_or_offset_u32(rep, data)?;
            Ok(CrateValue::Permission(v))
        }
        ValueType::Dictionary => decode_dictionary(rep, data, sections),
        ValueType::VariantSelectionMap => decode_variant_selection_map(rep, data, sections),
        ValueType::Relocates => decode_relocates_map(rep, data, sections),
        ValueType::TokenListOp
        | ValueType::StringListOp
        | ValueType::PathListOp
        | ValueType::ReferenceListOp
        | ValueType::PayloadListOp
        | ValueType::IntListOp
        | ValueType::Int64ListOp
        | ValueType::UIntListOp
        | ValueType::UInt64ListOp
        | ValueType::UnregisteredValueListOp => decode_list_op(rep, data, sections),
        ValueType::TimeSamples => decode_time_samples(rep, data, sections),
        ValueType::PathVector => decode_path_vector(rep, data, sections),
        ValueType::TokenVector => decode_token_vector(rep, data, sections),
        ValueType::DoubleVector => decode_double_vector(rep, data),
        ValueType::StringVector => decode_string_vector(rep, data, sections),
        ValueType::LayerOffsetVector => decode_layer_offset_vector(rep, data),
        // Math types (vectors, quaternions, matrices) — read raw bytes.
        ValueType::Quatd
        | ValueType::Quatf
        | ValueType::Quath
        | ValueType::Vec2d
        | ValueType::Vec2f
        | ValueType::Vec2h
        | ValueType::Vec2i
        | ValueType::Vec3d
        | ValueType::Vec3f
        | ValueType::Vec3h
        | ValueType::Vec3i
        | ValueType::Vec4d
        | ValueType::Vec4f
        | ValueType::Vec4h
        | ValueType::Vec4i
        | ValueType::Matrix2d
        | ValueType::Matrix3d
        | ValueType::Matrix4d => decode_math_type(rep, data, vtype),
        ValueType::Value => decode_value_indirection(rep, data, sections),
        ValueType::UnregisteredValue => decode_unregistered_value(rep, data, sections),
        ValueType::Payload => decode_payload(rep, data, sections),
        ValueType::Spline => decode_spline(rep, data),
    }
}

// ---------------------------------------------------------------------------
// Integer decoders
// ---------------------------------------------------------------------------

fn decode_bool(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    if rep.is_inlined() && !rep.is_array() {
        return Ok(CrateValue::Bool(rep.payload()[0] != 0));
    }
    if rep.is_array() {
        let values = read_integer_array(rep, data, 1, false)?;
        let arr = values
            .into_iter()
            .map(|v| CrateValue::Bool(v != 0))
            .collect();
        return Ok(CrateValue::Array(arr));
    }
    let off = payload_offset_usize(rep)?;
    Ok(CrateValue::Bool(data[off] != 0))
}

fn decode_integer_u8(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    if rep.is_inlined() && !rep.is_array() {
        return Ok(CrateValue::UChar(rep.payload()[0]));
    }
    if rep.is_array() {
        let values = read_integer_array(rep, data, 1, false)?;
        let arr = values
            .into_iter()
            .map(|v| {
                #[allow(clippy::cast_possible_truncation, reason = "u8 range")]
                CrateValue::UChar(v as u8)
            })
            .collect();
        return Ok(CrateValue::Array(arr));
    }
    let off = payload_offset_usize(rep)?;
    Ok(CrateValue::UChar(data[off]))
}

fn decode_integer_i32(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    if rep.is_inlined() && !rep.is_array() {
        let p = rep.payload();
        let v = i32::from_le_bytes([p[0], p[1], p[2], p[3]]);
        return Ok(CrateValue::Int(v));
    }
    if rep.is_array() {
        let values = read_integer_array(rep, data, 4, true)?;
        #[allow(clippy::cast_possible_truncation, reason = "i32 range")]
        let arr = values
            .into_iter()
            .map(|v| CrateValue::Int(v as i32))
            .collect();
        return Ok(CrateValue::Array(arr));
    }
    let off = payload_offset_usize(rep)?;
    let v = i32::from_le_bytes(data[off..off + 4].try_into().unwrap());
    Ok(CrateValue::Int(v))
}

fn decode_integer_u32(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    if rep.is_inlined() && !rep.is_array() {
        let p = rep.payload();
        let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
        return Ok(CrateValue::UInt(v));
    }
    if rep.is_array() {
        let values = read_integer_array(rep, data, 4, false)?;
        #[allow(clippy::cast_possible_truncation, reason = "u32 range")]
        let arr = values
            .into_iter()
            .map(|v| CrateValue::UInt(v as u32))
            .collect();
        return Ok(CrateValue::Array(arr));
    }
    let off = payload_offset_usize(rep)?;
    let v = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
    Ok(CrateValue::UInt(v))
}

fn decode_integer_i64(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    if rep.is_inlined() && !rep.is_array() {
        let p = rep.payload();
        // Only 6 bytes available; sign-extend.
        let v = read_signed_le_6(&p);
        return Ok(CrateValue::Int64(v));
    }
    if rep.is_array() {
        let values = read_integer_array(rep, data, 8, true)?;
        let arr = values.into_iter().map(CrateValue::Int64).collect();
        return Ok(CrateValue::Array(arr));
    }
    let off = payload_offset_usize(rep)?;
    let v = i64::from_le_bytes(data[off..off + 8].try_into().unwrap());
    Ok(CrateValue::Int64(v))
}

fn decode_integer_u64(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    if rep.is_inlined() && !rep.is_array() {
        let v = rep.payload_offset(); // u48
        return Ok(CrateValue::UInt64(v));
    }
    if rep.is_array() {
        let values = read_integer_array(rep, data, 8, false)?;
        #[allow(clippy::cast_sign_loss, reason = "unsigned context")]
        let arr = values
            .into_iter()
            .map(|v| CrateValue::UInt64(v as u64))
            .collect();
        return Ok(CrateValue::Array(arr));
    }
    let off = payload_offset_usize(rep)?;
    let v = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
    Ok(CrateValue::UInt64(v))
}

/// Reads an array of integers from the file data at the payload offset.
fn read_integer_array(
    rep: &RawValueRep,
    data: &[u8],
    element_size: usize,
    _signed: bool,
) -> Result<Vec<i64>, UsdcError> {
    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(vec![]);
    }
    let num_elements = read_u64_at(data, off)? as usize;
    let arr_start = off + 8;

    if rep.is_compressed() {
        let (values, _) = read_compressed_ints(&data[arr_start..], num_elements, element_size)?;
        Ok(values)
    } else {
        let mut values = Vec::with_capacity(num_elements);
        for i in 0..num_elements {
            let elem_off = arr_start + i * element_size;
            let v = read_signed_le_n(data, elem_off, element_size);
            values.push(v);
        }
        Ok(values)
    }
}

// ---------------------------------------------------------------------------
// Float decoders
// ---------------------------------------------------------------------------

fn decode_float(rep: &RawValueRep, data: &[u8], vtype: ValueType) -> Result<CrateValue, UsdcError> {
    let (element_size, to_value): (usize, fn(f64) -> CrateValue) = match vtype {
        ValueType::Half => (2, |v| CrateValue::Half(f64_to_half_bits(v))),
        ValueType::Float => (4, |v| {
            #[allow(clippy::cast_possible_truncation, reason = "float32")]
            CrateValue::Float(v as f32)
        }),
        ValueType::Double | ValueType::TimeCode => (8, CrateValue::Double),
        _ => unreachable!(),
    };

    if rep.is_inlined() && !rep.is_array() {
        // Inlined doubles are read as floats (4 bytes).
        let read_size = if element_size > 4 { 4 } else { element_size };
        let p = rep.payload();
        let val = read_float_bytes(&p[..read_size])?;
        return Ok(to_value(val));
    }

    if !rep.is_array() {
        let off = payload_offset_usize(rep)?;
        let val = read_float_bytes(&data[off..off + element_size])?;
        return Ok(to_value(val));
    }

    // Array
    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(CrateValue::Array(vec![]));
    }
    let num_elements = read_u64_at(data, off)? as usize;
    let arr_start = off + 8;

    if !rep.is_compressed() {
        let mut arr = Vec::with_capacity(num_elements);
        for i in 0..num_elements {
            let elem_off = arr_start + i * element_size;
            let val = read_float_bytes(&data[elem_off..elem_off + element_size])?;
            arr.push(to_value(val));
        }
        return Ok(CrateValue::Array(arr));
    }

    // Compressed float array.
    let compression_type = data[arr_start];
    let rest = &data[arr_start + 1..];

    if compression_type == b'i' {
        // Integer-coded.
        let (int_values, _) = read_compressed_ints(rest, num_elements, element_size)?;
        let mut arr = Vec::with_capacity(num_elements);
        for v in int_values {
            let bytes = v.to_le_bytes();
            let fval = read_float_bytes(&bytes[..element_size])?;
            arr.push(to_value(fval));
        }
        Ok(CrateValue::Array(arr))
    } else if compression_type == b't' {
        // LUT compression.
        let lut_count = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let lut_start = 4;
        let mut luts = Vec::with_capacity(lut_count);
        for i in 0..lut_count {
            let loff = lut_start + i * element_size;
            let val = read_float_bytes(&rest[loff..loff + element_size])?;
            luts.push(val);
        }
        let indices_start = lut_start + lut_count * element_size;
        let (indices, _) = read_compressed_ints(&rest[indices_start..], num_elements, 4)?;
        let mut arr = Vec::with_capacity(num_elements);
        for idx in indices {
            let lut_idx = idx as usize;
            if lut_idx >= luts.len() {
                return Err(UsdcError::Inconsistent {
                    message: "float LUT index out of range",
                });
            }
            arr.push(to_value(luts[lut_idx]));
        }
        Ok(CrateValue::Array(arr))
    } else {
        Err(UsdcError::Inconsistent {
            message: "unsupported float compression type",
        })
    }
}

fn read_float_bytes(bytes: &[u8]) -> Result<f64, UsdcError> {
    match bytes.len() {
        2 => {
            let bits = u16::from_le_bytes(bytes.try_into().unwrap());
            Ok(half_to_f64(bits))
        }
        4 => {
            let v = f32::from_le_bytes(bytes.try_into().unwrap());
            Ok(f64::from(v))
        }
        8 => Ok(f64::from_le_bytes(bytes.try_into().unwrap())),
        _ => Err(UsdcError::Inconsistent {
            message: "unexpected float byte size",
        }),
    }
}

/// Convert half-precision (u16) to f64.
fn half_to_f64(bits: u16) -> f64 {
    f64::from(half_to_f32(bits))
}

/// Convert half-precision (u16) to f32.
fn half_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1F) as u32;
    let mant = (bits & 0x03FF) as u32;

    let f32_bits = if exp == 0 {
        if mant == 0 {
            sign << 31
        } else {
            // Denormalized: convert to normalized f32.
            let mut e = 0_i32;
            let mut m = mant;
            while m & 0x0400 == 0 {
                m <<= 1;
                e += 1;
            }
            m &= 0x03FF;
            (sign << 31) | (((127 - 15 + 1 - e as u32) & 0xFF) << 23) | (m << 13)
        }
    } else if exp == 31 {
        // Inf/NaN
        (sign << 31) | (0xFF << 23) | (mant << 13)
    } else {
        // Normalized
        (sign << 31) | ((exp + 127 - 15) << 23) | (mant << 13)
    };

    f32::from_bits(f32_bits)
}

/// Convert f64 to half-precision bits (u16).
#[allow(
    clippy::cast_possible_truncation,
    reason = "intentional conversion to u16"
)]
fn f64_to_half_bits(val: f64) -> u16 {
    #[allow(clippy::cast_possible_truncation, reason = "intentional narrowing")]
    let f = val as f32;
    let bits = f.to_bits();
    let sign = (bits >> 31) & 1;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let mant = bits & 0x007F_FFFF;

    if exp == 255 {
        // Inf/NaN
        let h_mant = if mant != 0 { 0x200 } else { 0 };
        ((sign << 15) | (0x1F << 10) | h_mant) as u16
    } else if exp > 127 + 15 {
        // Overflow → Inf
        ((sign << 15) | (0x1F << 10)) as u16
    } else if exp < 127 - 14 {
        if exp < 127 - 24 {
            (sign << 15) as u16
        } else {
            let m = (mant | 0x0080_0000) >> (1 + (127 - 14 - exp));
            ((sign << 15) | (m >> 13)) as u16
        }
    } else {
        let h_exp = (exp - 127 + 15) as u32;
        ((sign << 15) | (h_exp << 10) | (mant >> 13)) as u16
    }
}

// ---------------------------------------------------------------------------
// String / Token / Asset decoders
// ---------------------------------------------------------------------------

fn decode_string(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    if rep.is_array() {
        let indices = read_u32_array_or_inlined(rep, data)?;
        let arr = indices
            .into_iter()
            .map(|i| {
                let s = lookup_string(sections, i as usize);
                CrateValue::String(s)
            })
            .collect();
        return Ok(CrateValue::Array(arr));
    }
    let idx = decode_inlined_or_offset_u32(rep, data)?;
    Ok(CrateValue::String(lookup_string(sections, idx as usize)))
}

fn decode_token(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    if rep.is_array() {
        let indices = read_u32_array_or_inlined(rep, data)?;
        let arr = indices
            .into_iter()
            .map(|i| {
                let s = lookup_token(sections, i as usize);
                CrateValue::Token(s)
            })
            .collect();
        return Ok(CrateValue::Array(arr));
    }
    let idx = decode_inlined_or_offset_u32(rep, data)?;
    Ok(CrateValue::Token(lookup_token(sections, idx as usize)))
}

fn decode_asset_path(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    if rep.is_array() {
        let indices = read_u32_array_or_inlined(rep, data)?;
        let arr = indices
            .into_iter()
            .map(|i| {
                let s = lookup_string(sections, i as usize);
                CrateValue::AssetPath(s)
            })
            .collect();
        return Ok(CrateValue::Array(arr));
    }
    let idx = decode_inlined_or_offset_u32(rep, data)?;
    // Inlined asset paths use tokens; offset-based use strings.
    if rep.is_inlined() {
        Ok(CrateValue::AssetPath(lookup_token(sections, idx as usize)))
    } else {
        Ok(CrateValue::AssetPath(lookup_string(sections, idx as usize)))
    }
}

fn lookup_string(sections: &CrateSections, idx: usize) -> String {
    if idx < sections.strings.len() {
        let tok_idx = sections.strings[idx] as usize;
        if tok_idx < sections.tokens.len() {
            return sections.tokens[tok_idx].clone();
        }
    }
    String::new()
}

fn lookup_token(sections: &CrateSections, idx: usize) -> String {
    if idx < sections.tokens.len() {
        sections.tokens[idx].clone()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Math type decoder (opaque bytes)
// ---------------------------------------------------------------------------

fn math_type_info(vtype: ValueType) -> (usize, usize) {
    // Returns (element_count, element_byte_size).
    match vtype {
        ValueType::Vec2h => (2, 2),
        ValueType::Vec2f => (2, 4),
        ValueType::Vec2d => (2, 8),
        ValueType::Vec2i => (2, 4),
        ValueType::Vec3h => (3, 2),
        ValueType::Vec3f => (3, 4),
        ValueType::Vec3d => (3, 8),
        ValueType::Vec3i => (3, 4),
        ValueType::Vec4h => (4, 2),
        ValueType::Vec4f => (4, 4),
        ValueType::Vec4d => (4, 8),
        ValueType::Vec4i => (4, 4),
        ValueType::Quath => (4, 2),
        ValueType::Quatf => (4, 4),
        ValueType::Quatd => (4, 8),
        ValueType::Matrix2d => (4, 8),
        ValueType::Matrix3d => (9, 8),
        ValueType::Matrix4d => (16, 8),
        _ => (0, 0),
    }
}

fn decode_math_type(
    rep: &RawValueRep,
    data: &[u8],
    vtype: ValueType,
) -> Result<CrateValue, UsdcError> {
    let (elem_count, elem_size) = math_type_info(vtype);
    let total_bytes = elem_count * elem_size;

    if rep.is_inlined() && !rep.is_array() {
        // Inlined math: small types fit in 6 payload bytes.
        let p = rep.payload();
        let mut buf = vec![0_u8; total_bytes];
        let copy_len = total_bytes.min(p.len());
        buf[..copy_len].copy_from_slice(&p[..copy_len]);
        return Ok(CrateValue::Opaque {
            value_type: vtype,
            data: buf,
        });
    }

    let off = payload_offset_usize(rep)?;

    if !rep.is_array() {
        if off + total_bytes > data.len() {
            return Err(UsdcError::UnexpectedEof {
                section: "math value",
                offset: off as u64,
                expected: total_bytes as u64,
            });
        }
        return Ok(CrateValue::Opaque {
            value_type: vtype,
            data: data[off..off + total_bytes].to_vec(),
        });
    }

    // Array of math values.
    if off == 0 {
        return Ok(CrateValue::Array(vec![]));
    }
    let num_elements = read_u64_at(data, off)? as usize;
    let arr_start = off + 8;
    let mut arr = Vec::with_capacity(num_elements);
    for i in 0..num_elements {
        let elem_off = arr_start + i * total_bytes;
        if elem_off + total_bytes > data.len() {
            return Err(UsdcError::UnexpectedEof {
                section: "math array element",
                offset: elem_off as u64,
                expected: total_bytes as u64,
            });
        }
        arr.push(CrateValue::Opaque {
            value_type: vtype,
            data: data[elem_off..elem_off + total_bytes].to_vec(),
        });
    }
    Ok(CrateValue::Array(arr))
}

// ---------------------------------------------------------------------------
// Dictionary decoder
// ---------------------------------------------------------------------------

fn decode_dictionary(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    if off == 0 && rep.is_inlined() {
        let num = rep.payload_offset() as usize;
        if num == 0 {
            return Ok(CrateValue::Dictionary(vec![]));
        }
    }

    let num_items = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut entries = Vec::with_capacity(num_items);

    for _ in 0..num_items {
        // Key: u32 string index.
        let key_idx = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let key = lookup_string(sections, key_idx);

        // Value: u64 relative offset from the current position to the
        // 8-byte `ValueRep`. The Python reference does:
        //   seek_from = filehandle.tell()
        //   offset = read_int() + seek_from   # read u64, add position
        //   seek(offset); rep = read_int()    # read ValueRep at target
        //   current = filehandle.tell()       # right after ValueRep
        //   ... process ...
        //   seek(current)                     # next entry starts here
        //
        // So the relative offset is from the position of the offset field
        // itself, and the next entry continues from right after the ValueRep
        // at the target location.
        let seek_from = pos;
        let value_rel_offset = read_u64_at(data, pos)? as usize;
        let rep_offset = seek_from + value_rel_offset;
        if rep_offset + 8 > data.len() {
            return Err(UsdcError::UnexpectedEof {
                section: "dictionary value rep",
                offset: rep_offset as u64,
                expected: 8,
            });
        }

        let mut rep_bytes = [0_u8; 8];
        rep_bytes.copy_from_slice(&data[rep_offset..rep_offset + 8]);
        let child_rep = RawValueRep::new(rep_bytes);
        let child_val = decode_value(&child_rep, data, sections)?;

        // Advance pos to right after the ValueRep (mirroring Python's
        // `self.filehandle.seek(current)` where current = rep_offset + 8).
        pos = rep_offset + 8;

        entries.push((key, child_val));
    }

    Ok(CrateValue::Dictionary(entries))
}

// ---------------------------------------------------------------------------
// List op decoder
// ---------------------------------------------------------------------------

fn decode_list_op(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let vtype = rep.value_type()?;
    let off = payload_offset_usize(rep)?;

    if off == 0 {
        return Ok(CrateValue::ListOp(CrateListOp {
            op_type: vtype,
            explicit_items: None,
            prepended_items: vec![],
            appended_items: vec![],
            deleted_items: vec![],
        }));
    }

    // First byte: header flags.
    let header = data[off];
    let mut pos = off + 1;

    let make_explicit = header & (1 << 0) != 0;
    let add_explicit = header & (1 << 1) != 0;
    let add_items_flag = header & (1 << 2) != 0;
    let delete_flag = header & (1 << 3) != 0;
    // bit 4: reorder (deprecated)
    let reorder_flag = header & (1 << 4) != 0;
    let prepend_flag = header & (1 << 5) != 0;
    let append_flag = header & (1 << 6) != 0;

    let mut explicit_items = None;
    let mut added_items = vec![];
    let mut prepended_items = vec![];
    let mut appended_items = vec![];
    let mut deleted_items = vec![];

    if add_explicit {
        let (items, consumed) = read_list_op_items(vtype, data, pos, sections)?;
        explicit_items = Some(items);
        pos += consumed;
    } else if make_explicit {
        explicit_items = Some(vec![]);
    }

    if add_items_flag {
        let (items, consumed) = read_list_op_items(vtype, data, pos, sections)?;
        added_items = items;
        pos += consumed;
    }

    if prepend_flag {
        let (items, consumed) = read_list_op_items(vtype, data, pos, sections)?;
        prepended_items = items;
        pos += consumed;
    }

    if append_flag {
        let (items, consumed) = read_list_op_items(vtype, data, pos, sections)?;
        appended_items = items;
        pos += consumed;
    }

    if delete_flag {
        let (items, consumed) = read_list_op_items(vtype, data, pos, sections)?;
        deleted_items = items;
        pos += consumed;
    }

    if reorder_flag {
        // Deprecated; skip.
        let (_items, _consumed) = read_list_op_items(vtype, data, pos, sections)?;
    }

    // Map deprecated 'add' to 'append' when it's the only composable op.
    if !added_items.is_empty()
        && prepended_items.is_empty()
        && deleted_items.is_empty()
        && appended_items.is_empty()
    {
        appended_items = added_items;
    }

    Ok(CrateValue::ListOp(CrateListOp {
        op_type: vtype,
        explicit_items,
        prepended_items,
        appended_items,
        deleted_items,
    }))
}

/// Reads a list of items for a list op component.
///
/// Returns `(items, bytes_consumed)`.
fn read_list_op_items(
    vtype: ValueType,
    data: &[u8],
    pos: usize,
    sections: &CrateSections,
) -> Result<(Vec<CrateValue>, usize), UsdcError> {
    let num = read_u64_at(data, pos)? as usize;
    let mut cursor = pos + 8;

    let element_size = match vtype {
        ValueType::TokenListOp | ValueType::PathListOp | ValueType::StringListOp => 4,
        ValueType::IntListOp | ValueType::UIntListOp => 4,
        ValueType::Int64ListOp | ValueType::UInt64ListOp => 8,
        ValueType::ReferenceListOp => 36, // INDEX + INDEX + LAYER_OFFSET + VALUE_REP
        ValueType::PayloadListOp => 20,   // INDEX + INDEX + LAYER_OFFSET
        ValueType::UnregisteredValueListOp => 8,
        _ => {
            return Err(UsdcError::Inconsistent {
                message: "unsupported list op type",
            });
        }
    };

    let mut items = Vec::with_capacity(num);
    for _ in 0..num {
        let item_data = &data[cursor..cursor + element_size];
        let item = match vtype {
            ValueType::TokenListOp => {
                let idx = u32::from_le_bytes(item_data[..4].try_into().unwrap()) as usize;
                CrateValue::Token(lookup_token(sections, idx))
            }
            ValueType::PathListOp => {
                let idx = u32::from_le_bytes(item_data[..4].try_into().unwrap()) as usize;
                let path = if idx < sections.paths.len() {
                    sections.paths[idx].clone()
                } else {
                    String::new()
                };
                CrateValue::String(path)
            }
            ValueType::StringListOp => {
                let idx = u32::from_le_bytes(item_data[..4].try_into().unwrap()) as usize;
                CrateValue::String(lookup_string(sections, idx))
            }
            ValueType::IntListOp => {
                let v = i32::from_le_bytes(item_data[..4].try_into().unwrap());
                CrateValue::Int(v)
            }
            ValueType::UIntListOp => {
                let v = u32::from_le_bytes(item_data[..4].try_into().unwrap());
                CrateValue::UInt(v)
            }
            ValueType::Int64ListOp => {
                let v = i64::from_le_bytes(item_data[..8].try_into().unwrap());
                CrateValue::Int64(v)
            }
            ValueType::UInt64ListOp => {
                let v = u64::from_le_bytes(item_data[..8].try_into().unwrap());
                CrateValue::UInt64(v)
            }
            ValueType::ReferenceListOp | ValueType::PayloadListOp => {
                decode_reference_data(item_data, sections)?
            }
            ValueType::UnregisteredValueListOp => {
                let mut rb = [0_u8; 8];
                rb.copy_from_slice(item_data);
                let child_rep = RawValueRep::new(rb);
                decode_value(&child_rep, data, sections)?
            }
            _ => CrateValue::None,
        };
        items.push(item);
        cursor += element_size;
    }

    Ok((items, cursor - pos))
}

fn decode_reference_data(
    item_data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let asset_path_idx = u32::from_le_bytes(item_data[..4].try_into().unwrap()) as usize;
    let prim_path_idx = u32::from_le_bytes(item_data[4..8].try_into().unwrap()) as usize;

    let asset_path = lookup_string(sections, asset_path_idx);
    let prim_path = if prim_path_idx < sections.paths.len() {
        sections.paths[prim_path_idx].clone()
    } else {
        String::new()
    };

    let offset_bytes = &item_data[8..];
    let layer_offset = if offset_bytes.len() >= 8 {
        f64::from_le_bytes(offset_bytes[..8].try_into().unwrap())
    } else {
        0.0
    };
    let layer_scale = if offset_bytes.len() >= 16 {
        f64::from_le_bytes(offset_bytes[8..16].try_into().unwrap())
    } else {
        1.0
    };

    // Encode as a dictionary for the assembler to interpret.
    Ok(CrateValue::Dictionary(vec![
        (String::from("assetPath"), CrateValue::AssetPath(asset_path)),
        (String::from("primPath"), CrateValue::String(prim_path)),
        (
            String::from("layerOffset"),
            CrateValue::Double(layer_offset),
        ),
        (String::from("layerScale"), CrateValue::Double(layer_scale)),
    ]))
}

// ---------------------------------------------------------------------------
// Time samples decoder
// ---------------------------------------------------------------------------

fn decode_time_samples(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(CrateValue::TimeSamples(vec![]));
    }

    // Layout at `off` (Python reference: parse_timesamples):
    //   timecodes_offset: u64 — relative offset from `off` to the timecodes
    //                          `ValueRep` (8 bytes)
    // After the timecodes ValueRep:
    //   values_offset: u64 — relative offset from current position to the
    //                        values array
    // At values location:
    //   num_values: u64, then num_values × 8-byte ValueReps

    // 1. Read timecodes relative offset.
    let timecodes_rel = read_u64_at(data, off)? as usize;
    let tc_off = off + timecodes_rel;

    // 2. Read the 8-byte timecodes ValueRep.
    if tc_off + 8 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "timeSamples timecodes rep",
            offset: tc_off as u64,
            expected: 8,
        });
    }
    let mut tc_rep_bytes = [0_u8; 8];
    tc_rep_bytes.copy_from_slice(&data[tc_off..tc_off + 8]);
    let tc_rep = RawValueRep::new(tc_rep_bytes);

    // 3. Right after the timecodes ValueRep, read values relative offset.
    let current_offset = tc_off + 8;
    let values_rel = read_u64_at(data, current_offset)? as usize;
    let val_off = current_offset + values_rel;

    // 4. Decode timecodes (should be a Double array).
    let tc_value = decode_value(&tc_rep, data, sections)?;
    let timecodes: Vec<f64> = match &tc_value {
        CrateValue::Array(arr) => arr
            .iter()
            .filter_map(|v| match v {
                CrateValue::Double(d) => Some(*d),
                CrateValue::Float(f) => Some(f64::from(*f)),
                _ => None,
            })
            .collect(),
        CrateValue::Double(d) => vec![*d],
        _ => vec![],
    };

    // 5. Read value reps.
    let num_values = read_u64_at(data, val_off)? as usize;
    let mut samples = Vec::with_capacity(num_values);
    let reps_start = val_off + 8;

    for i in 0..num_values {
        let rep_off = reps_start + i * 8;
        if rep_off + 8 > data.len() {
            return Err(UsdcError::UnexpectedEof {
                section: "timeSamples value rep",
                offset: rep_off as u64,
                expected: 8,
            });
        }
        let mut vr_bytes = [0_u8; 8];
        vr_bytes.copy_from_slice(&data[rep_off..rep_off + 8]);
        let vr = RawValueRep::new(vr_bytes);
        let val = decode_value(&vr, data, sections)?;
        let tc = if i < timecodes.len() {
            timecodes[i]
        } else {
            0.0
        };
        samples.push((tc, val));
    }

    Ok(CrateValue::TimeSamples(samples))
}

// ---------------------------------------------------------------------------
// Vector decoders
// ---------------------------------------------------------------------------

fn decode_path_vector(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    let num = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut paths = Vec::with_capacity(num);
    for _ in 0..num {
        let idx = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let path = if idx < sections.paths.len() {
            sections.paths[idx].clone()
        } else {
            String::new()
        };
        paths.push(path);
        pos += 4;
    }
    Ok(CrateValue::PathVector(paths))
}

fn decode_token_vector(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    let num = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut tokens = Vec::with_capacity(num);
    for _ in 0..num {
        let idx = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        tokens.push(lookup_token(sections, idx));
        pos += 4;
    }
    Ok(CrateValue::TokenVector(tokens))
}

fn decode_double_vector(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    let num = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut doubles = Vec::with_capacity(num);
    for _ in 0..num {
        let v = f64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        doubles.push(v);
        pos += 8;
    }
    Ok(CrateValue::DoubleVector(doubles))
}

fn decode_string_vector(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    let num = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut strings = Vec::with_capacity(num);
    for _ in 0..num {
        let idx = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        strings.push(lookup_string(sections, idx));
        pos += 4;
    }
    Ok(CrateValue::StringVector(strings))
}

fn decode_layer_offset_vector(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    let num = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut offsets = Vec::with_capacity(num);
    for _ in 0..num {
        let offset = f64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        let scale = f64::from_le_bytes(data[pos + 8..pos + 16].try_into().unwrap());
        offsets.push((offset, scale));
        pos += 16;
    }
    Ok(CrateValue::LayerOffsetVector(offsets))
}

// ---------------------------------------------------------------------------
// Variant selection map decoder
// ---------------------------------------------------------------------------

fn decode_variant_selection_map(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(CrateValue::VariantSelectionMap(vec![]));
    }
    let num = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut pairs = Vec::with_capacity(num);
    for _ in 0..num {
        let key_idx = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let val_idx = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let key = lookup_string(sections, key_idx);
        let val = lookup_string(sections, val_idx);
        pairs.push((key, val));
        pos += 8;
    }
    Ok(CrateValue::VariantSelectionMap(pairs))
}

// ---------------------------------------------------------------------------
// Relocates map decoder
// ---------------------------------------------------------------------------

fn decode_relocates_map(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(CrateValue::RelocatesMap(vec![]));
    }
    let num = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut pairs = Vec::with_capacity(num);
    for _ in 0..num {
        let k_idx = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let v_idx = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let k = if k_idx < sections.paths.len() {
            sections.paths[k_idx].clone()
        } else {
            String::new()
        };
        let v = if v_idx < sections.paths.len() {
            sections.paths[v_idx].clone()
        } else {
            String::new()
        };
        pairs.push((k, v));
        pos += 8;
    }
    Ok(CrateValue::RelocatesMap(pairs))
}

// ---------------------------------------------------------------------------
// Value indirection & payload decoders
// ---------------------------------------------------------------------------

fn decode_value_indirection(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    if off + 8 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "Value indirection",
            offset: off as u64,
            expected: 8,
        });
    }
    let mut rb = [0_u8; 8];
    rb.copy_from_slice(&data[off..off + 8]);
    let child_rep = RawValueRep::new(rb);
    decode_value(&child_rep, data, sections)
}

fn decode_unregistered_value(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    let local_offset = read_u64_at(data, off)?;
    let target = off as u64 + local_offset;
    #[allow(clippy::cast_possible_truncation, reason = "offset within file bounds")]
    let target_off = target as usize;
    if target_off + 8 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "UnregisteredValue",
            offset: target,
            expected: 8,
        });
    }
    let mut rb = [0_u8; 8];
    rb.copy_from_slice(&data[target_off..target_off + 8]);
    let child_rep = RawValueRep::new(rb);
    decode_value(&child_rep, data, sections)
}

fn decode_payload(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(CrateValue::None);
    }
    // Payload: 20 bytes (INDEX + INDEX + LAYER_OFFSET).
    if off + 20 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "Payload",
            offset: off as u64,
            expected: 20,
        });
    }
    decode_reference_data(&data[off..off + 20], sections)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn payload_offset_usize(rep: &RawValueRep) -> Result<usize, UsdcError> {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "file offsets < 4 GiB supported"
    )]
    Ok(rep.payload_offset() as usize)
}

fn read_u64_at(data: &[u8], offset: usize) -> Result<u64, UsdcError> {
    if offset + 8 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "u64 read",
            offset: offset as u64,
            expected: 8,
        });
    }
    Ok(u64::from_le_bytes(
        data[offset..offset + 8].try_into().unwrap(),
    ))
}

fn read_signed_le_6(bytes: &[u8; 6]) -> i64 {
    let sign_ext = if bytes[5] & 0x80 != 0 { 0xFF } else { 0x00 };
    let mut buf = [sign_ext; 8];
    buf[..6].copy_from_slice(bytes);
    i64::from_le_bytes(buf)
}

fn read_signed_le_n(data: &[u8], offset: usize, size: usize) -> i64 {
    let bytes = &data[offset..offset + size];
    let sign_ext = if bytes[size - 1] & 0x80 != 0 {
        0xFF
    } else {
        0x00
    };
    let mut buf = [sign_ext; 8];
    buf[..size].copy_from_slice(bytes);
    i64::from_le_bytes(buf)
}

fn decode_inlined_or_offset_u32(rep: &RawValueRep, data: &[u8]) -> Result<u32, UsdcError> {
    if rep.is_inlined() {
        let p = rep.payload();
        Ok(u32::from_le_bytes([p[0], p[1], p[2], p[3]]))
    } else {
        let off = payload_offset_usize(rep)?;
        Ok(u32::from_le_bytes(data[off..off + 4].try_into().unwrap()))
    }
}

fn read_u32_array_or_inlined(rep: &RawValueRep, data: &[u8]) -> Result<Vec<i64>, UsdcError> {
    read_integer_array(rep, data, 4, false)
}

// ---------------------------------------------------------------------------
// Spline decoder (§16.3.10.33)
// ---------------------------------------------------------------------------

/// Decode a spline value from the USDC binary format (version 1).
///
/// Binary layout based on the reference implementation in `splines.py`:
///
/// - Header byte 1: version (bits 0–3), data type (bits 4–5), timed value
///   (bit 6), curve type (bit 7)
/// - Header byte 2: pre-extrapolation (bits 0–2), post-extrapolation
///   (bits 3–4), loop flag (bit 6)
/// - If sloped extrapolation: f64 slope value(s)
/// - If looping: proto\_start (f64), proto\_end (f64), num\_pre\_loops (i32),
///   num\_post\_loops (i32), value\_offset (f64)
/// - Knot count (u32)
/// - Per knot: flag byte + time (f64) + value + optional pre\_value +
///   optional tangent widths (Bézier only) + tangent slopes
fn decode_spline(rep: &RawValueRep, data: &[u8]) -> Result<CrateValue, UsdcError> {
    let off = payload_offset_usize(rep)?;
    if off == 0 || off >= data.len() {
        // Empty spline.
        return Ok(CrateValue::Spline(SplineData {
            data_type: SplineDataType::Unspecified,
            default_curve_type: CurveType::Bezier,
            pre_extrapolation: Extrapolation::Block,
            post_extrapolation: Extrapolation::Block,
            loop_params: None,
            knots: vec![],
        }));
    }

    let mut pos = off;

    // --- Header byte 1 ---
    let hdr1 = read_u8(data, &mut pos)?;
    let version = hdr1 & 0x0F;
    if version != 1 {
        // Unsupported spline version — treat as empty spline.
        return Ok(CrateValue::Spline(SplineData {
            data_type: SplineDataType::Unspecified,
            default_curve_type: CurveType::Bezier,
            pre_extrapolation: Extrapolation::Block,
            post_extrapolation: Extrapolation::Block,
            loop_params: None,
            knots: vec![],
        }));
    }
    let data_type = match (hdr1 & 0x30) >> 4 {
        0 => SplineDataType::Unspecified,
        1 => SplineDataType::Double,
        2 => SplineDataType::Float,
        3 => SplineDataType::Half,
        _ => SplineDataType::Unspecified,
    };
    // bit 6: timed_value (informational, not needed for decoding).
    let default_curve_type = if (hdr1 & 0x80) >> 7 == 1 {
        CurveType::Hermite
    } else {
        CurveType::Bezier
    };

    // --- Header byte 2 ---
    let hdr2 = read_u8(data, &mut pos)?;
    let pre_extrap_raw = hdr2 & 0x07;
    let mut pre_extrapolation = extrap_from_u8(pre_extrap_raw);
    if pre_extrap_raw == 3 {
        // Sloped: read f64 slope.
        let slope = read_f64_le(data, &mut pos)?;
        pre_extrapolation = Extrapolation::Sloped(slope);
    }

    let post_extrap_raw = (hdr2 & 0x18) >> 3;
    let mut post_extrapolation = extrap_from_u8(post_extrap_raw);
    if post_extrap_raw == 3 {
        let slope = read_f64_le(data, &mut pos)?;
        post_extrapolation = Extrapolation::Sloped(slope);
    }

    let has_loops = (hdr2 & 0x40) != 0;
    let loop_params = if has_loops {
        let proto_start = read_f64_le(data, &mut pos)?;
        let proto_end = read_f64_le(data, &mut pos)?;
        let num_pre_loops = read_i32_le(data, &mut pos)?;
        let num_post_loops = read_i32_le(data, &mut pos)?;
        let value_offset = read_f64_le(data, &mut pos)?;
        Some(LoopParams {
            proto_start,
            proto_end,
            num_pre_loops,
            num_post_loops,
            value_offset,
        })
    } else {
        None
    };

    // If data type is Unspecified, there are no knots.
    if data_type == SplineDataType::Unspecified {
        return Ok(CrateValue::Spline(SplineData {
            data_type,
            default_curve_type,
            pre_extrapolation,
            post_extrapolation,
            loop_params,
            knots: vec![],
        }));
    }

    // --- Knots ---
    let num_knots = read_u32_le(data, &mut pos)? as usize;
    let mut knots = Vec::with_capacity(num_knots);

    let is_hermite = default_curve_type == CurveType::Hermite;

    for _ in 0..num_knots {
        let flag = read_u8(data, &mut pos)?;
        let dual_valued = (flag & 0x01) != 0;
        let next_interp = knot_interp_from_u8((flag & 0x06) >> 1);
        let curve_type = if (flag & 0x08) >> 3 == 1 {
            CurveType::Hermite
        } else {
            CurveType::Bezier
        };
        let pre_tan_maya_form = (flag & 0x10) != 0;
        let post_tan_maya_form = (flag & 0x20) != 0;

        // Time is always f64.
        let time = read_f64_le(data, &mut pos)?;

        // Value: type-dependent.
        let value = read_typed_value(data, &mut pos, data_type)?;

        let pre_value = if dual_valued {
            Some(read_typed_value(data, &mut pos, data_type)?)
        } else {
            None
        };

        // Tangent widths (Bézier only; Hermite has no widths).
        let (pre_tan_width, post_tan_width) = if !is_hermite {
            let pre_w = read_f64_le(data, &mut pos)?;
            let post_w = read_f64_le(data, &mut pos)?;
            (pre_w, post_w)
        } else {
            (0.0, 0.0)
        };

        // Tangent slopes: type-dependent.
        let pre_tan_slope = read_typed_value(data, &mut pos, data_type)?;
        let post_tan_slope = read_typed_value(data, &mut pos, data_type)?;

        knots.push(Knot {
            time,
            value,
            pre_value,
            next_interp,
            curve_type,
            pre_tan_maya_form,
            post_tan_maya_form,
            pre_tan_width,
            post_tan_width,
            pre_tan_slope,
            post_tan_slope,
        });
    }

    Ok(CrateValue::Spline(SplineData {
        data_type,
        default_curve_type,
        pre_extrapolation,
        post_extrapolation,
        loop_params,
        knots,
    }))
}

/// Convert a 3-bit extrapolation mode to [`Extrapolation`].
fn extrap_from_u8(v: u8) -> Extrapolation {
    match v {
        0 => Extrapolation::Block,
        1 => Extrapolation::Held,
        2 => Extrapolation::Linear,
        // 3 (Sloped) is handled by the caller which reads the slope value.
        3 => Extrapolation::Sloped(0.0),
        4 => Extrapolation::LoopRepeat,
        5 => Extrapolation::LoopReset,
        6 => Extrapolation::LoopOscillate,
        _ => Extrapolation::Block,
    }
}

/// Convert a 2-bit interpolation mode to [`KnotInterp`].
fn knot_interp_from_u8(v: u8) -> KnotInterp {
    match v {
        0 => KnotInterp::Block,
        1 => KnotInterp::Held,
        2 => KnotInterp::Linear,
        3 => KnotInterp::Curve,
        _ => KnotInterp::Block,
    }
}

/// Read a single byte, advancing `pos`.
fn read_u8(data: &[u8], pos: &mut usize) -> Result<u8, UsdcError> {
    if *pos >= data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "spline",
            offset: *pos as u64,
            expected: 1,
        });
    }
    let v = data[*pos];
    *pos += 1;
    Ok(v)
}

/// Read a little-endian `f64`, advancing `pos`.
fn read_f64_le(data: &[u8], pos: &mut usize) -> Result<f64, UsdcError> {
    if *pos + 8 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "spline",
            offset: *pos as u64,
            expected: 8,
        });
    }
    let v = f64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap());
    *pos += 8;
    Ok(v)
}

/// Read a little-endian `f32`, advancing `pos`.
fn read_f32_le(data: &[u8], pos: &mut usize) -> Result<f32, UsdcError> {
    if *pos + 4 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "spline",
            offset: *pos as u64,
            expected: 4,
        });
    }
    let v = f32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    Ok(v)
}

/// Read a little-endian `i32`, advancing `pos`.
fn read_i32_le(data: &[u8], pos: &mut usize) -> Result<i32, UsdcError> {
    if *pos + 4 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "spline",
            offset: *pos as u64,
            expected: 4,
        });
    }
    let v = i32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    Ok(v)
}

/// Read a little-endian `u32`, advancing `pos`.
fn read_u32_le(data: &[u8], pos: &mut usize) -> Result<u32, UsdcError> {
    if *pos + 4 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "spline",
            offset: *pos as u64,
            expected: 4,
        });
    }
    let v = u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    Ok(v)
}

/// Read a value in the spline's data type, converting to `f64`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "half-float conversion intentional"
)]
fn read_typed_value(data: &[u8], pos: &mut usize, dt: SplineDataType) -> Result<f64, UsdcError> {
    match dt {
        SplineDataType::Double | SplineDataType::Unspecified => read_f64_le(data, pos),
        SplineDataType::Float => Ok(f64::from(read_f32_le(data, pos)?)),
        SplineDataType::Half => {
            if *pos + 2 > data.len() {
                return Err(UsdcError::UnexpectedEof {
                    section: "spline half",
                    offset: *pos as u64,
                    expected: 2,
                });
            }
            let bits = u16::from_le_bytes(data[*pos..*pos + 2].try_into().unwrap());
            *pos += 2;
            Ok(half_to_f64(bits))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_value_rep_flags() {
        // All flags set: is_array=1, is_inlined=1, is_compressed=1
        let mut bytes = [0_u8; 8];
        bytes[6] = 1; // ValueType::Bool
        bytes[7] = 0x80 | 0x40 | 0x20; // all flags
        let rep = RawValueRep::new(bytes);
        assert!(rep.is_array());
        assert!(rep.is_inlined());
        assert!(rep.is_compressed());
        assert_eq!(rep.value_type().unwrap(), ValueType::Bool);
    }

    #[test]
    fn inlined_bool() {
        let mut bytes = [0_u8; 8];
        bytes[0] = 1; // payload[0] = 1 (true)
        bytes[6] = 1; // ValueType::Bool
        bytes[7] = 0x40; // is_inlined
        let rep = RawValueRep::new(bytes);

        let sections = CrateSections {
            tokens: vec![],
            strings: vec![],
            fields: vec![],
            fieldsets: vec![],
            paths: vec![],
            specs: vec![],
        };

        let val = decode_value(&rep, &[], &sections).unwrap();
        match val {
            CrateValue::Bool(true) => {}
            other => panic!("expected Bool(true), got {other:?}"),
        }
    }

    #[test]
    fn inlined_int() {
        let mut bytes = [0_u8; 8];
        bytes[..4].copy_from_slice(&42_i32.to_le_bytes());
        bytes[6] = 3; // ValueType::Int
        bytes[7] = 0x40; // is_inlined
        let rep = RawValueRep::new(bytes);

        let sections = CrateSections {
            tokens: vec![],
            strings: vec![],
            fields: vec![],
            fieldsets: vec![],
            paths: vec![],
            specs: vec![],
        };

        let val = decode_value(&rep, &[], &sections).unwrap();
        match val {
            CrateValue::Int(42) => {}
            other => panic!("expected Int(42), got {other:?}"),
        }
    }

    #[test]
    fn half_to_f32_roundtrip() {
        // 1.0 in half = 0x3C00
        let f = half_to_f32(0x3C00);
        assert!((f - 1.0).abs() < 1e-6);

        // 0.0
        let f = half_to_f32(0x0000);
        assert!(f == 0.0);

        // -1.0 in half = 0xBC00
        let f = half_to_f32(0xBC00);
        assert!((f - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn payload_offset_extraction() {
        let mut bytes = [0_u8; 8];
        // Set payload to offset 0x0000_0100 = 256
        bytes[0] = 0x00;
        bytes[1] = 0x01;
        let rep = RawValueRep::new(bytes);
        assert_eq!(rep.payload_offset(), 256);
    }
}
