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
use crate::version::CrateVersion;

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
/// - bit 4 (0x10): `is_array_edit` (crate 0.14 and later)
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

    /// Whether this is a native array edit (`VtArrayEdit`), introduced in
    /// crate 0.14 (`_IsArrayEditBit`, `pxr/usd/sdf/crateFile.h:87`).
    #[must_use]
    pub fn is_array_edit(&self) -> bool {
        self.flags() & 0x10 != 0
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
    /// Path expression text (`SdfPathExpression`, crate version 0.10.0).
    PathExpression(String),
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
    /// A native array edit (crate 0.14 and later).
    ArrayEdit(CrateArrayEdit),
}

/// A decoded native array edit (`VtArrayEdit`).
///
/// Instructions refer to array elements by index: negative indices count
/// from the end, and [`CrateArrayEdit::END`] is the position past the last
/// element. Literal operands index into [`CrateArrayEdit::literals`]; the
/// decoder has already checked that they are in range.
#[derive(Clone, Debug)]
pub struct CrateArrayEdit {
    /// The element type of the edited array.
    pub element_type: ValueType,
    /// Literal elements referenced by the instructions.
    pub literals: Vec<CrateValue>,
    /// Instructions, in application order.
    pub ops: Vec<CrateArrayEditOp>,
}

impl CrateArrayEdit {
    /// The index past the last element (`Vt_ArrayEditOps::EndIndex`).
    pub const END: i64 = i64::MIN;
}

/// One array edit instruction (`Vt_ArrayEditOps::Op`,
/// `pxr/base/vt/arrayEditOps.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrateArrayEditOp {
    /// Overwrite the element at `index` with a literal.
    WriteLiteral {
        /// Index into [`CrateArrayEdit::literals`].
        literal: usize,
        /// Destination index.
        index: i64,
    },
    /// Overwrite the element at `index` with the element at `src`.
    WriteRef {
        /// Source index in the array being edited.
        src: i64,
        /// Destination index.
        index: i64,
    },
    /// Insert a literal at `index`.
    InsertLiteral {
        /// Index into [`CrateArrayEdit::literals`].
        literal: usize,
        /// Insertion index.
        index: i64,
    },
    /// Insert a copy of the element at `src` at `index`.
    InsertRef {
        /// Source index in the array being edited.
        src: i64,
        /// Insertion index.
        index: i64,
    },
    /// Erase the element at `index`.
    Erase {
        /// Index to erase.
        index: i64,
    },
    /// Grow to at least `len` elements with value-initialized elements.
    MinSize {
        /// Minimum length.
        len: u64,
    },
    /// Grow to at least `len` elements, filling with a literal.
    MinSizeFill {
        /// Minimum length.
        len: u64,
        /// Index into [`CrateArrayEdit::literals`] of the fill value.
        literal: usize,
    },
    /// Resize to `len` elements with value-initialized elements.
    SetSize {
        /// New length.
        len: u64,
    },
    /// Resize to `len` elements, filling with a literal.
    SetSizeFill {
        /// New length.
        len: u64,
        /// Index into [`CrateArrayEdit::literals`] of the fill value.
        literal: usize,
    },
    /// Shrink to at most `len` elements.
    MaxSize {
        /// Maximum length.
        len: u64,
    },
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
    if rep.is_array_edit() {
        return decode_array_edit(rep, data, sections, vtype);
    }

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
        ValueType::AssetPath => decode_asset_path(rep, data, sections),
        // Stored like an asset path (Core §16.3.10.14), but typed apart:
        // the text is an `SdfPathExpression`, not an asset to resolve.
        ValueType::PathExpression => {
            decode_asset_path(rep, data, sections).map(asset_path_to_path_expression)
        }
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
        ValueType::Spline => decode_spline(rep, data, sections),
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
        // Inlined int64 values are stored as an `int32` in the low four
        // payload bytes (`crateValueInliners.h`, `_EncodeInline` for integral
        // types); the upper payload bytes are zero, so sign-extend from 32
        // bits.
        let p = rep.payload();
        let v = i32::from_le_bytes([p[0], p[1], p[2], p[3]]);
        return Ok(CrateValue::Int64(i64::from(v)));
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
        // Inlined only when it fits a `uint32_t`, which is stored in the low
        // four payload bytes (`_EncodeInline`, `crateValueInliners.h`).
        let p = rep.payload();
        let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
        return Ok(CrateValue::UInt64(u64::from(v)));
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

    // Like float arrays, short arrays are stored uncompressed.
    if rep.is_compressed() && num_elements >= MIN_COMPRESSED_ARRAY_SIZE {
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

/// Arrays with fewer elements are never compressed
/// (`MinCompressedArraySize`, `pxr/usd/sdf/crateFile.cpp:1912`).
const MIN_COMPRESSED_ARRAY_SIZE: usize = 16;

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

    // Stored elements are read by bit pattern, so every half (subnormals
    // and NaN payloads included) and every float NaN payload survives.
    let element = |bytes: &[u8]| -> Result<CrateValue, UsdcError> {
        Ok(match (vtype, bytes.len()) {
            (ValueType::Half, 2) => CrateValue::Half(u16::from_le_bytes([bytes[0], bytes[1]])),
            (ValueType::Float, 4) => {
                CrateValue::Float(f32::from_le_bytes(bytes.try_into().unwrap()))
            }
            _ => to_value(read_float_bytes(bytes)?),
        })
    };

    if rep.is_inlined() && !rep.is_array() {
        // Inlined doubles are read as floats (4 bytes).
        let read_size = if element_size > 4 { 4 } else { element_size };
        let p = rep.payload();
        return element(&p[..read_size]);
    }

    if !rep.is_array() {
        let off = payload_offset_usize(rep)?;
        return element(&data[off..off + element_size]);
    }

    // Array
    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(CrateValue::Array(vec![]));
    }
    let num_elements = read_u64_at(data, off)? as usize;
    let arr_start = off + 8;

    // Arrays shorter than `MinCompressedArraySize` are stored uncompressed
    // even when flagged compressed (`_ReadPossiblyCompressedArray`,
    // `pxr/usd/sdf/crateFile.cpp:2259`).
    if !rep.is_compressed() || num_elements < MIN_COMPRESSED_ARRAY_SIZE {
        let mut arr = Vec::with_capacity(num_elements);
        for i in 0..num_elements {
            let elem_off = arr_start + i * element_size;
            arr.push(element(&data[elem_off..elem_off + element_size])?);
        }
        return Ok(CrateValue::Array(arr));
    }

    // Compressed float array.
    let compression_type = data[arr_start];
    let rest = &data[arr_start + 1..];

    if compression_type == b'i' {
        // Integer-coded: every element is an integral value, stored as
        // compressed `int32`s whatever the element width, and converted
        // numerically (`crateFile.cpp`, `_WritePossiblyCompressedArray` and
        // `_ReadPossiblyCompressedArray` for floating-point arrays).
        let (int_values, _) = read_compressed_ints(rest, num_elements, 4)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "values were written as exact int32 conversions"
        )]
        let arr = int_values.into_iter().map(|v| to_value(v as f64)).collect();
        Ok(CrateValue::Array(arr))
    } else if compression_type == b't' {
        // LUT compression.
        let lut_count = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let lut_start = 4;
        let mut luts = Vec::with_capacity(lut_count);
        for i in 0..lut_count {
            let loff = lut_start + i * element_size;
            luts.push(element(&rest[loff..loff + element_size])?);
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
            arr.push(luts[lut_idx].clone());
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
        // Inf/NaN, keeping a NaN's payload (and making it nonzero).
        let h_mant = match (mant, mant >> 13) {
            (0, _) => 0,
            (_, 0) => 0x200,
            (_, payload) => payload,
        };
        ((sign << 15) | (0x1F << 10) | h_mant) as u16
    } else if exp > 127 + 15 {
        // Overflow → Inf
        ((sign << 15) | (0x1F << 10)) as u16
    } else if exp < 127 - 14 {
        if exp < 127 - 24 {
            (sign << 15) as u16
        } else {
            // Subnormal half: the significand scaled by 2^24, so 2^-24
            // (exponent 103) is 1 and 2^-15 (exponent 112) is 0x200.
            let m = (mant | 0x0080_0000) >> (126 - exp);
            ((sign << 15) | m) as u16
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

/// Retypes decoded asset-path text as path-expression text.
fn asset_path_to_path_expression(value: CrateValue) -> CrateValue {
    match value {
        CrateValue::AssetPath(text) => CrateValue::PathExpression(text),
        CrateValue::Array(items) => CrateValue::Array(
            items
                .into_iter()
                .map(asset_path_to_path_expression)
                .collect(),
        ),
        other => other,
    }
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

/// Expands an inlined vector or matrix into its little-endian element bytes.
///
/// A `half2` is always inlined as its own four bytes. Otherwise, OpenUSD
/// inlines a vector whose components are all exactly representable
/// as `int8_t`, storing one `int8_t` per component, and a matrix that is
/// zero off the diagonal with `int8_t`-representable diagonal entries,
/// storing the diagonal (`pxr/usd/sdf/crateValueInliners.h:90`). Quaternions
/// are never inlined.
fn decode_inlined_math(
    rep: &RawValueRep,
    vtype: ValueType,
    elem_count: usize,
    elem_size: usize,
) -> Result<Vec<u8>, UsdcError> {
    let p = rep.payload();
    // A `GfVec2h` fits the payload, so OpenUSD always inlines it bitwise
    // (`_IsAlwaysInlined`, `pxr/usd/sdf/crateFile.cpp:281`).
    if vtype == ValueType::Vec2h {
        return Ok(p[..4].to_vec());
    }
    let component = |i: usize| f64::from(p[i].cast_signed());
    let components: Vec<f64> = match vtype {
        ValueType::Matrix2d | ValueType::Matrix3d | ValueType::Matrix4d => {
            let rows = match vtype {
                ValueType::Matrix2d => 2,
                ValueType::Matrix3d => 3,
                _ => 4,
            };
            let mut m = vec![0.0; rows * rows];
            for i in 0..rows {
                m[i * rows + i] = component(i);
            }
            m
        }
        ValueType::Quatd | ValueType::Quatf | ValueType::Quath => {
            return Err(UsdcError::Inconsistent {
                message: "quaternion values are never inlined",
            });
        }
        _ => (0..elem_count).map(component).collect(),
    };

    let mut buf = Vec::with_capacity(elem_count * elem_size);
    for value in components {
        match (vtype, elem_size) {
            (ValueType::Vec2i | ValueType::Vec3i | ValueType::Vec4i, _) => {
                #[allow(clippy::cast_possible_truncation, reason = "value is an int8")]
                buf.extend_from_slice(&(value as i32).to_le_bytes());
            }
            (_, 2) => buf.extend_from_slice(&f64_to_half_bits(value).to_le_bytes()),
            #[allow(clippy::cast_possible_truncation, reason = "value is an int8")]
            (_, 4) => buf.extend_from_slice(&(value as f32).to_le_bytes()),
            _ => buf.extend_from_slice(&value.to_le_bytes()),
        }
    }
    Ok(buf)
}

fn decode_math_type(
    rep: &RawValueRep,
    data: &[u8],
    vtype: ValueType,
) -> Result<CrateValue, UsdcError> {
    let (elem_count, elem_size) = math_type_info(vtype);
    let total_bytes = elem_count * elem_size;

    if rep.is_inlined() && !rep.is_array() {
        return Ok(CrateValue::Opaque {
            value_type: vtype,
            data: decode_inlined_math(rep, vtype, elem_count, elem_size)?,
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

    let (entries, _) = decode_dictionary_at(data, off, sections)?;
    Ok(CrateValue::Dictionary(entries))
}

/// Decodes a dictionary stored at `off`: a `u64` entry count, then per entry
/// a `u32` string index for the key and an `i64` offset, relative to the
/// offset field, to the value's `ValueRep`. Returns the entries and the
/// position after the last entry.
fn decode_dictionary_at(
    data: &[u8],
    off: usize,
    sections: &CrateSections,
) -> Result<(Vec<(String, CrateValue)>, usize), UsdcError> {
    let num_items = read_u64_at(data, off)? as usize;
    let mut pos = off + 8;
    let mut entries = Vec::new();

    for _ in 0..num_items {
        // Key: u32 string index.
        let key_idx = read_u32_le(data, &mut pos)? as usize;
        let key = lookup_string(sections, key_idx);

        // Value: a `VtValue` reached through a relative offset.
        let child_val = read_vt_value(data, &mut pos, sections)?;
        entries.push((key, child_val));
    }

    Ok((entries, pos))
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
    let mut pos = off;
    let header = read_u8(data, &mut pos)?;

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

/// Reads one list op component: a `u64` item count, then the items one
/// after another (`Read<vector<T>>`, `pxr/usd/sdf/crateFile.cpp:1146`).
///
/// Items are read sequentially because some have variable length: a
/// reference carries its `customData` dictionary, and an unregistered value
/// is a `VtValue` reached through a relative offset.
///
/// Returns `(items, bytes_consumed)`.
///
/// Spec: AOUSD Core §16.3.10.
fn read_list_op_items(
    vtype: ValueType,
    data: &[u8],
    pos: usize,
    sections: &CrateSections,
) -> Result<(Vec<CrateValue>, usize), UsdcError> {
    let num = read_u64_at(data, pos)?;
    let mut cursor = pos + 8;

    // Every item occupies at least four bytes, so a count the remaining data
    // cannot hold is malformed. Checking first bounds the allocation.
    if num > (data.len().saturating_sub(cursor) / 4) as u64 {
        return Err(UsdcError::UnexpectedEof {
            section: "list op items",
            offset: cursor as u64,
            expected: num.saturating_mul(4),
        });
    }
    let num = num as usize;

    let mut items = Vec::with_capacity(num);
    for _ in 0..num {
        let item = match vtype {
            ValueType::TokenListOp => {
                let idx = read_u32_le(data, &mut cursor)? as usize;
                CrateValue::Token(lookup_token(sections, idx))
            }
            ValueType::PathListOp => {
                let idx = read_u32_le(data, &mut cursor)? as usize;
                CrateValue::String(lookup_path(sections, idx))
            }
            ValueType::StringListOp => {
                let idx = read_u32_le(data, &mut cursor)? as usize;
                CrateValue::String(lookup_string(sections, idx))
            }
            ValueType::IntListOp => CrateValue::Int(read_i32_le(data, &mut cursor)?),
            ValueType::UIntListOp => CrateValue::UInt(read_u32_le(data, &mut cursor)?),
            ValueType::Int64ListOp => {
                let v = read_u64_at(data, cursor)?.cast_signed();
                cursor += 8;
                CrateValue::Int64(v)
            }
            ValueType::UInt64ListOp => {
                let v = read_u64_at(data, cursor)?;
                cursor += 8;
                CrateValue::UInt64(v)
            }
            ValueType::ReferenceListOp => decode_reference_at(data, &mut cursor, sections)?,
            ValueType::PayloadListOp => decode_payload_at(data, &mut cursor, sections)?,
            ValueType::UnregisteredValueListOp => read_vt_value(data, &mut cursor, sections)?,
            _ => {
                return Err(UsdcError::Inconsistent {
                    message: "unsupported list op type",
                });
            }
        };
        items.push(item);
    }

    Ok((items, cursor - pos))
}

/// Reads a `VtValue` stored in place (`Read<VtValue>`,
/// `pxr/usd/sdf/crateFile.cpp:1314`): an `i64` offset, relative to the
/// offset field, to the value's `ValueRep`. `pos` ends just past the
/// `ValueRep`, where the next item starts.
fn read_vt_value(
    data: &[u8],
    pos: &mut usize,
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let start = *pos;
    let relative = read_u64_at(data, start)?;
    let rep_offset = usize::try_from(relative)
        .ok()
        .and_then(|relative| start.checked_add(relative))
        .ok_or(UsdcError::UnexpectedEof {
            section: "VtValue value rep",
            offset: start as u64,
            expected: 8,
        })?;
    let child_rep = RawValueRep::new(read_u64_at(data, rep_offset)?.to_le_bytes());
    *pos = rep_offset + 8;
    decode_value(&child_rep, data, sections)
}

/// Looks up a path by index, or the empty string when out of range.
fn lookup_path(sections: &CrateSections, idx: usize) -> String {
    sections.paths.get(idx).cloned().unwrap_or_default()
}

/// Reads an `SdfReference` (`Write(SdfReference)`,
/// `pxr/usd/sdf/crateFile.cpp:1529`): a string index for the asset path, a
/// path index for the prim path, the layer offset as two `f64`s (offset,
/// scale), and the `customData` dictionary.
///
/// The custom data is decoded to validate it and to find the end of the
/// item, then dropped: `layerstack::doc::Reference` has no custom data.
fn decode_reference_at(
    data: &[u8],
    pos: &mut usize,
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let asset_path = lookup_string(sections, read_u32_le(data, pos)? as usize);
    let prim_path = lookup_path(sections, read_u32_le(data, pos)? as usize);
    let layer_offset = read_f64_le(data, pos)?;
    let layer_scale = read_f64_le(data, pos)?;
    let (_custom_data, end) = decode_dictionary_at(data, *pos, sections)?;
    *pos = end;
    Ok(reference_dictionary(
        asset_path,
        prim_path,
        layer_offset,
        layer_scale,
    ))
}

/// Reads an `SdfPayload` (`Write(SdfPayload)`,
/// `pxr/usd/sdf/crateFile.cpp:1535`): a string index for the asset path and
/// a path index for the prim path, then, from crate 0.8, the layer offset as
/// two `f64`s. Earlier files have no payload layer offsets
/// (`pxr/usd/sdf/crateFile.cpp:1303`).
fn decode_payload_at(
    data: &[u8],
    pos: &mut usize,
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let asset_path = lookup_string(sections, read_u32_le(data, pos)? as usize);
    let prim_path = lookup_path(sections, read_u32_le(data, pos)? as usize);
    let (layer_offset, layer_scale) = if sections.version.has(CrateVersion::PAYLOAD_LAYER_OFFSETS) {
        (read_f64_le(data, pos)?, read_f64_le(data, pos)?)
    } else {
        (0.0, 1.0)
    };
    Ok(reference_dictionary(
        asset_path,
        prim_path,
        layer_offset,
        layer_scale,
    ))
}

/// Encodes a reference or payload as the dictionary the assembler reads.
fn reference_dictionary(
    asset_path: String,
    prim_path: String,
    layer_offset: f64,
    layer_scale: f64,
) -> CrateValue {
    CrateValue::Dictionary(vec![
        (String::from("assetPath"), CrateValue::AssetPath(asset_path)),
        (String::from("primPath"), CrateValue::String(prim_path)),
        (
            String::from("layerOffset"),
            CrateValue::Double(layer_offset),
        ),
        (String::from("layerScale"), CrateValue::Double(layer_scale)),
    ])
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

    // 4. Decode timecodes. OpenUSD packs them as a `std::vector<double>`
    //    (`TimeSamples::times`, `pxr/usd/sdf/crateFile.cpp:1596`), which is
    //    the `DoubleVector` type, not a `double[]` array.
    let tc_value = decode_value(&tc_rep, data, sections)?;
    let timecodes: Vec<f64> = match tc_value {
        CrateValue::DoubleVector(times) => times,
        CrateValue::Array(arr) => arr
            .iter()
            .filter_map(|v| match v {
                CrateValue::Double(d) => Some(*d),
                CrateValue::Float(f) => Some(f64::from(*f)),
                _ => None,
            })
            .collect(),
        CrateValue::Double(d) => vec![d],
        _ => vec![],
    };

    // 5. Read value reps, one per time.
    let num_values = read_u64_at(data, val_off)? as usize;
    if num_values != timecodes.len() {
        return Err(UsdcError::Inconsistent {
            message: "timeSamples has a different number of times and values",
        });
    }
    let mut samples = Vec::with_capacity(num_values);
    let reps_start = val_off + 8;

    for (i, time) in timecodes.into_iter().enumerate() {
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
        samples.push((time, val));
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

/// Decodes a `VtValue` value (type 44). OpenUSD unpacks it with
/// `Read<VtValue>` at the payload offset (`pxr/usd/sdf/crateFile.cpp:1314`):
/// an offset to the value's `ValueRep`, relative to the offset field.
fn decode_value_indirection(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let mut pos = payload_offset_usize(rep)?;
    read_vt_value(data, &mut pos, sections)
}

/// Decodes an `SdfUnregisteredValue`, which OpenUSD stores as a `VtValue`
/// (`pxr/usd/sdf/crateFile.cpp:1261`).
fn decode_unregistered_value(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    let mut pos = payload_offset_usize(rep)?;
    read_vt_value(data, &mut pos, sections)
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
    decode_payload_at(data, &mut { off }, sections)
}

// ---------------------------------------------------------------------------
// Array edit decoder (crate 0.14)
// ---------------------------------------------------------------------------

/// Decodes a native array edit.
///
/// A zero payload is the identity edit. Otherwise the payload is the offset
/// of a literal-array `ValueRep`, an `int64[]` instruction `ValueRep`, and a
/// discarded byte (the former `isDense` flag), as written by
/// `_ValueHandler::PackArrayEdit` (`pxr/usd/sdf/crateFile.cpp:1793`). The
/// literal array must have the edit's element type. Instructions are decoded
/// by [`parse_array_edit_instructions`].
fn decode_array_edit(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
    element_type: ValueType,
) -> Result<CrateValue, UsdcError> {
    require_version(sections, CrateVersion::ARRAY_EDITS, "array edit")?;
    if rep.is_array() || rep.is_inlined() || rep.is_compressed() {
        return Err(UsdcError::Inconsistent {
            message: "array edit value rep has array, inlined or compressed flags",
        });
    }
    if matches!(
        element_type,
        ValueType::Unknown | ValueType::Relocates | ValueType::Spline
    ) || !element_type.supports_array()
    {
        return Err(UsdcError::Inconsistent {
            message: "array edit of a type that has no arrays",
        });
    }

    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(CrateValue::ArrayEdit(CrateArrayEdit {
            element_type,
            literals: Vec::new(),
            ops: Vec::new(),
        }));
    }

    let literals_rep = RawValueRep::new(read_u64_at(data, off)?.to_le_bytes());
    let indexes_rep = RawValueRep::new(read_u64_at(data, off + 8)?.to_le_bytes());
    // The former `isDense` flag; OpenUSD reads and discards it.
    read_u8(data, &mut (off + 16))?;

    let is_plain_array = |rep: &RawValueRep, vtype: ValueType| {
        rep.is_array() && !rep.is_array_edit() && rep.value_type().ok() == Some(vtype)
    };
    if !is_plain_array(&literals_rep, element_type) {
        return Err(UsdcError::Inconsistent {
            message: "array edit literals are not an array of the element type",
        });
    }
    if !is_plain_array(&indexes_rep, ValueType::Int64) {
        return Err(UsdcError::Inconsistent {
            message: "array edit instructions are not an int64 array",
        });
    }

    let CrateValue::Array(literals) = decode_value(&literals_rep, data, sections)? else {
        return Err(UsdcError::Inconsistent {
            message: "array edit literals did not decode to an array",
        });
    };
    let CrateValue::Array(indexes) = decode_value(&indexes_rep, data, sections)? else {
        return Err(UsdcError::Inconsistent {
            message: "array edit instructions did not decode to an array",
        });
    };
    let instructions = indexes
        .into_iter()
        .map(|value| match value {
            CrateValue::Int64(v) => Ok(v),
            _ => Err(UsdcError::Inconsistent {
                message: "array edit instruction is not an int64",
            }),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ops = parse_array_edit_instructions(&instructions, literals.len())?;

    Ok(CrateValue::ArrayEdit(CrateArrayEdit {
        element_type,
        literals,
        ops,
    }))
}

/// Decodes the `int64` instruction stream of an array edit.
///
/// The stream is a sequence of groups. Each group starts with a header whose
/// low 56 bits are a repeat count and whose high 8 bits are the op
/// (`Vt_ArrayEditOps::OpAndCount`), followed by `count` argument tuples of
/// the op's arity (`pxr/base/vt/arrayEditOps.h`). A stream with an unknown
/// op, a non-positive count, missing arguments, an out-of-range literal
/// index or a negative size is malformed and rejected. Element indices are
/// not range-checked here: like OpenUSD, instructions whose indices fall
/// outside the array being edited are skipped when the edit is applied.
fn parse_array_edit_instructions(
    instructions: &[i64],
    num_literals: usize,
) -> Result<Vec<CrateArrayEditOp>, UsdcError> {
    let malformed = |message| UsdcError::Inconsistent { message };
    let literal = |index: i64| {
        usize::try_from(index)
            .ok()
            .filter(|i| *i < num_literals)
            .ok_or(malformed("array edit literal index out of range"))
    };
    let size = |len: i64| u64::try_from(len).map_err(|_| malformed("array edit size is negative"));

    let mut ops = Vec::new();
    let mut rest = instructions;
    while let Some((&header, tail)) = rest.split_first() {
        let op = header.to_le_bytes()[7];
        // Sign-extend the 56-bit count field.
        let count = (header << 8) >> 8;
        let arity: usize = match op {
            0..=3 | 6 | 8 => 2,
            4 | 5 | 7 | 9 => 1,
            _ => return Err(malformed("unknown array edit op")),
        };
        if count <= 0 {
            return Err(malformed("array edit op count is not positive"));
        }
        let needed = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(arity))
            .filter(|needed| *needed <= tail.len())
            .ok_or(malformed("array edit op is missing arguments"))?;
        let (args, next) = tail.split_at(needed);
        for tuple in args.chunks_exact(arity) {
            let a1 = tuple[0];
            let a2 = tuple.get(1).copied().unwrap_or(-1);
            ops.push(match op {
                0 => CrateArrayEditOp::WriteLiteral {
                    literal: literal(a1)?,
                    index: a2,
                },
                1 => CrateArrayEditOp::WriteRef { src: a1, index: a2 },
                2 => CrateArrayEditOp::InsertLiteral {
                    literal: literal(a1)?,
                    index: a2,
                },
                3 => CrateArrayEditOp::InsertRef { src: a1, index: a2 },
                4 => CrateArrayEditOp::Erase { index: a1 },
                5 => CrateArrayEditOp::MinSize { len: size(a1)? },
                6 => CrateArrayEditOp::MinSizeFill {
                    len: size(a1)?,
                    literal: literal(a2)?,
                },
                7 => CrateArrayEditOp::SetSize { len: size(a1)? },
                8 => CrateArrayEditOp::SetSizeFill {
                    len: size(a1)?,
                    literal: literal(a2)?,
                },
                _ => CrateArrayEditOp::MaxSize { len: size(a1)? },
            });
        }
        rest = next;
    }
    Ok(ops)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Fails with [`UsdcError::FeatureRequiresVersion`] unless the file's version
/// includes `introduced`.
fn require_version(
    sections: &CrateSections,
    introduced: CrateVersion,
    feature: &'static str,
) -> Result<(), UsdcError> {
    if sections.version.has(introduced) {
        Ok(())
    } else {
        Err(UsdcError::FeatureRequiresVersion {
            feature,
            required: introduced,
            found: sections.version,
        })
    }
}

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

fn empty_spline() -> SplineData {
    SplineData {
        data_type: SplineDataType::Unspecified,
        default_curve_type: CurveType::Bezier,
        pre_extrapolation: Extrapolation::Block,
        post_extrapolation: Extrapolation::Block,
        loop_params: None,
        knots: vec![],
    }
}

/// Decodes a spline value.
///
/// The crate stores a spline as a `u64` byte count, that many bytes of Ts
/// binary data, and a map from knot time to custom-data dictionary
/// (`Write(const TsSpline &)` and the `TsSpline` branch of `Read`,
/// `pxr/usd/sdf/crateFile.cpp:1614` and `:1382`). The blob is parsed by
/// [`parse_ts_spline`]. Knot custom data is decoded to validate it and then
/// dropped, because [`Knot`] has no custom data.
///
/// Spec: AOUSD Core §16.3.10.33; OpenUSD v26.08 for crate 0.13 and later.
fn decode_spline(
    rep: &RawValueRep,
    data: &[u8],
    sections: &CrateSections,
) -> Result<CrateValue, UsdcError> {
    require_version(sections, CrateVersion::SPLINES, "spline value")?;
    let off = payload_offset_usize(rep)?;
    if off == 0 {
        return Ok(CrateValue::Spline(empty_spline()));
    }

    let blob_len = read_u64_at(data, off)?;
    let blob_start = off + 8;
    let blob_end = usize::try_from(blob_len)
        .ok()
        .and_then(|len| blob_start.checked_add(len))
        .filter(|end| *end <= data.len())
        .ok_or(UsdcError::UnexpectedEof {
            section: "spline data",
            offset: blob_start as u64,
            expected: blob_len,
        })?;
    let spline = parse_ts_spline(&data[..blob_end], blob_start, sections.version)?;

    // Knot custom data: `u64` count, then per knot a `f64` time and a
    // dictionary.
    let count = read_u64_at(data, blob_end)?;
    let mut pos = blob_end + 8;
    for _ in 0..count {
        read_f64_le(data, &mut pos)?;
        let (_, end) = decode_dictionary_at(data, pos, sections)?;
        pos = end;
    }

    Ok(CrateValue::Spline(spline))
}

/// Parses Ts binary spline data in formats 1 to 3 from `data[pos..]`.
///
/// Follows `Ts_BinaryDataAccess::_ParseV1_3` (`pxr/base/ts/binary.cpp`,
/// OpenUSD v26.08). Format 2 (crate 0.13) adds a tangent-algorithm byte per
/// knot. It is validated and dropped: OpenUSD also stores the tangents the
/// algorithm produced, and those are kept. Format 3 (crate 0.15) widens the
/// value-type field and adds a third header byte for `loopBoundaryTime`.
/// `GfTimeCode`-valued splines and `loopBoundaryTime` have no
/// [`SplineData`] representation and fail with
/// [`UsdcError::UnsupportedFeature`]. The data must be consumed exactly.
fn parse_ts_spline(
    data: &[u8],
    mut pos: usize,
    version: CrateVersion,
) -> Result<SplineData, UsdcError> {
    // An empty blob is an empty spline.
    if pos == data.len() {
        return Ok(empty_spline());
    }

    // Header byte 1: format (bits 0-3), value type (bits 4-5, or 4-6 from
    // format 3), legacy time-valued flag (bit 6, formats 1-2), curve type
    // (bit 7).
    let hdr1 = read_u8(data, &mut pos)?;
    let format = hdr1 & 0x0F;
    let required = match format {
        1 => CrateVersion::SPLINES,
        2 => CrateVersion::SPLINE_TANGENT_ALGORITHMS,
        3 => CrateVersion::SPLINE_LOOP_BOUNDARY_AND_TIMECODE,
        0 => {
            return Err(UsdcError::Inconsistent {
                message: "spline data has format 0",
            });
        }
        _ => {
            return Err(UsdcError::UnsupportedFeature {
                feature: "spline binary format newer than 3",
            });
        }
    };
    if !version.has(required) {
        return Err(UsdcError::FeatureRequiresVersion {
            feature: "spline binary format",
            required,
            found: version,
        });
    }
    let descriptor = if format > 2 {
        (hdr1 & 0x70) >> 4
    } else {
        if hdr1 & 0x40 != 0 {
            return Err(UsdcError::UnsupportedFeature {
                feature: "time-valued spline",
            });
        }
        (hdr1 & 0x30) >> 4
    };
    let data_type = match descriptor {
        0 => SplineDataType::Unspecified,
        1 => SplineDataType::Double,
        2 => SplineDataType::Float,
        3 => SplineDataType::Half,
        4 => {
            return Err(UsdcError::UnsupportedFeature {
                feature: "time-valued spline",
            });
        }
        _ => {
            return Err(UsdcError::Inconsistent {
                message: "unknown spline value type",
            });
        }
    };
    let default_curve_type = if hdr1 & 0x80 != 0 {
        CurveType::Hermite
    } else {
        CurveType::Bezier
    };

    // Header byte 2: pre-extrapolation (bits 0-2), post-extrapolation
    // (bits 3-5), inner loops (bit 6).
    let hdr2 = read_u8(data, &mut pos)?;
    let pre_mode = hdr2 & 0x07;
    let post_mode = (hdr2 & 0x38) >> 3;
    let has_loops = hdr2 & 0x40 != 0;

    // Header byte 3 (format 3): `loopBoundaryTime` presence for pre (bit 0)
    // and post (bit 1) looping extrapolation.
    if format > 2 {
        let hdr3 = read_u8(data, &mut pos)?;
        if hdr3 & 0x03 != 0 {
            return Err(UsdcError::UnsupportedFeature {
                feature: "spline loopBoundaryTime",
            });
        }
        if hdr3 != 0 {
            return Err(UsdcError::Inconsistent {
                message: "unknown spline header flags",
            });
        }
    }

    let pre_extrapolation = read_extrapolation(data, &mut pos, pre_mode)?;
    let post_extrapolation = read_extrapolation(data, &mut pos, post_mode)?;

    let loop_params = if has_loops {
        Some(LoopParams {
            proto_start: read_f64_le(data, &mut pos)?,
            proto_end: read_f64_le(data, &mut pos)?,
            num_pre_loops: read_i32_le(data, &mut pos)?,
            num_post_loops: read_i32_le(data, &mut pos)?,
            value_offset: read_f64_le(data, &mut pos)?,
        })
    } else {
        None
    };

    // An untyped spline has no knot data.
    let mut knots = Vec::new();
    if data_type != SplineDataType::Unspecified || pos != data.len() {
        let num_knots = read_u32_le(data, &mut pos)?;
        let is_hermite = default_curve_type == CurveType::Hermite;
        for _ in 0..num_knots {
            knots.push(read_knot(data, &mut pos, data_type, is_hermite, format)?);
        }
    }

    if pos != data.len() {
        return Err(UsdcError::Inconsistent {
            message: "trailing bytes after spline data",
        });
    }

    Ok(SplineData {
        data_type,
        default_curve_type,
        pre_extrapolation,
        post_extrapolation,
        loop_params,
        knots,
    })
}

/// Reads one knot of Ts binary spline data.
fn read_knot(
    data: &[u8],
    pos: &mut usize,
    data_type: SplineDataType,
    is_hermite: bool,
    format: u8,
) -> Result<Knot, UsdcError> {
    // Flag byte: dual-valued (bit 0), next interpolation (bits 1-2), curve
    // type (bit 3). Bits 4-5 are the Maya tangent forms of the AOUSD
    // supplemental reference (`splines.py`); OpenUSD v26.08 writes zero.
    let flag = read_u8(data, pos)?;
    let dual_valued = flag & 0x01 != 0;
    let next_interp = match (flag & 0x06) >> 1 {
        0 => KnotInterp::Block,
        1 => KnotInterp::Held,
        2 => KnotInterp::Linear,
        _ => KnotInterp::Curve,
    };
    let curve_type = if flag & 0x08 != 0 {
        CurveType::Hermite
    } else {
        CurveType::Bezier
    };

    let time = read_f64_le(data, pos)?;
    let value = read_typed_value(data, pos, data_type)?;
    let pre_value = if dual_valued {
        Some(read_typed_value(data, pos, data_type)?)
    } else {
        None
    };
    // Tangent widths are stored only for Bézier splines.
    let (pre_tan_width, post_tan_width) = if is_hermite {
        (0.0, 0.0)
    } else {
        (read_f64_le(data, pos)?, read_f64_le(data, pos)?)
    };
    let pre_tan_slope = read_typed_value(data, pos, data_type)?;
    let post_tan_slope = read_typed_value(data, pos, data_type)?;

    // Format 2 and later: tangent algorithms, pre (bits 0-3) and post
    // (bits 4-7): None, Custom or AutoEase (`TsTangentAlgorithm`).
    if format > 1 {
        let algorithms = read_u8(data, pos)?;
        if algorithms & 0x0F > 2 || algorithms >> 4 > 2 {
            return Err(UsdcError::Inconsistent {
                message: "unknown spline tangent algorithm",
            });
        }
    }

    Ok(Knot {
        time,
        value,
        pre_value,
        next_interp,
        curve_type,
        pre_tan_maya_form: flag & 0x10 != 0,
        post_tan_maya_form: flag & 0x20 != 0,
        pre_tan_width,
        post_tan_width,
        pre_tan_slope,
        post_tan_slope,
    })
}

/// Reads an extrapolation mode (`TsExtrapMode`), and its slope when sloped.
fn read_extrapolation(data: &[u8], pos: &mut usize, mode: u8) -> Result<Extrapolation, UsdcError> {
    Ok(match mode {
        0 => Extrapolation::Block,
        1 => Extrapolation::Held,
        2 => Extrapolation::Linear,
        3 => Extrapolation::Sloped(read_f64_le(data, pos)?),
        4 => Extrapolation::LoopRepeat,
        5 => Extrapolation::LoopReset,
        6 => Extrapolation::LoopOscillate,
        _ => {
            return Err(UsdcError::Inconsistent {
                message: "unknown spline extrapolation mode",
            });
        }
    })
}

/// Read a single byte, advancing `pos`.
fn read_u8(data: &[u8], pos: &mut usize) -> Result<u8, UsdcError> {
    if *pos >= data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "value data",
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
            section: "value data",
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
            section: "value data",
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
            section: "value data",
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
            section: "value data",
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
                    section: "value data",
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
            version: CrateVersion::NEWEST_READABLE,
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
            version: CrateVersion::NEWEST_READABLE,
        };

        let val = decode_value(&rep, &[], &sections).unwrap();
        match val {
            CrateValue::Int(42) => {}
            other => panic!("expected Int(42), got {other:?}"),
        }
    }

    #[test]
    fn every_half_decodes_to_its_bits() {
        // Subnormals, infinities and NaN payloads included.
        for bits in 0..=u16::MAX {
            let [lo, hi] = bits.to_le_bytes();
            let rep = inlined(ValueType::Half, [lo, hi, 0, 0]);
            match decode_float(&rep, &[], ValueType::Half) {
                Ok(CrateValue::Half(read)) => assert_eq!(read, bits, "{bits:#06x}"),
                other => panic!("expected Half, got {other:?}"),
            }
            // Numeric conversion is exact for every value but a NaN.
            let widened = half_to_f64(bits);
            if !widened.is_nan() {
                assert_eq!(f64_to_half_bits(widened), bits, "{bits:#06x}");
            }
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

    fn inlined(vtype: ValueType, payload: [u8; 4]) -> RawValueRep {
        let mut bytes = [0_u8; 8];
        bytes[..4].copy_from_slice(&payload);
        bytes[6] = vtype as u8;
        bytes[7] = 0x40; // is_inlined
        RawValueRep::new(bytes)
    }

    #[test]
    fn inlined_vectors_hold_int8_components() {
        let rep = inlined(ValueType::Vec3f, [1, 2, 0xFD, 0]);
        let (count, size) = math_type_info(ValueType::Vec3f);
        let bytes = decode_inlined_math(&rep, ValueType::Vec3f, count, size).unwrap();
        let expected: Vec<u8> = [1.0_f32, 2.0, -3.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(bytes, expected);

        let rep = inlined(ValueType::Vec2i, [0xFF, 7, 0, 0]);
        let (count, size) = math_type_info(ValueType::Vec2i);
        let bytes = decode_inlined_math(&rep, ValueType::Vec2i, count, size).unwrap();
        let expected: Vec<u8> = [-1_i32, 7].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn inlined_half2_holds_its_bits() {
        // (0, 1) as two halves.
        let rep = inlined(ValueType::Vec2h, [0x00, 0x00, 0x00, 0x3C]);
        let (count, size) = math_type_info(ValueType::Vec2h);
        let bytes = decode_inlined_math(&rep, ValueType::Vec2h, count, size).unwrap();
        assert_eq!(bytes, [0x00, 0x00, 0x00, 0x3C]);
    }

    /// An inlined `uint64` is the `uint32` in the low payload bytes; OpenUSD
    /// truncates the payload to it (`_DecodeInline`,
    /// `pxr/usd/sdf/crateValueInliners.h`), so higher bytes do not count.
    #[test]
    fn inlined_uint64_holds_32_bits() {
        let sections = sections_with(CrateVersion::NEWEST_READABLE);
        let mut bytes = [0_u8; 8];
        bytes[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[4] = 0x01;
        bytes[6] = ValueType::UInt64 as u8;
        bytes[7] = 0x40; // inlined
        match decode_value(&RawValueRep::new(bytes), &[], &sections) {
            Ok(CrateValue::UInt64(v)) => assert_eq!(v, u64::from(u32::MAX)),
            other => panic!("expected UInt64, got {other:?}"),
        }
    }

    #[test]
    fn inlined_matrices_hold_the_diagonal() {
        let rep = inlined(ValueType::Matrix2d, [2, 0xFF, 0, 0]);
        let (count, size) = math_type_info(ValueType::Matrix2d);
        let bytes = decode_inlined_math(&rep, ValueType::Matrix2d, count, size).unwrap();
        let expected: Vec<u8> = [2.0_f64, 0.0, 0.0, -1.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn inlined_quaternions_are_rejected() {
        let rep = inlined(ValueType::Quatf, [1, 0, 0, 0]);
        let (count, size) = math_type_info(ValueType::Quatf);
        assert!(decode_inlined_math(&rep, ValueType::Quatf, count, size).is_err());
    }

    /// Ts binary data for a Bézier double spline with held extrapolation and
    /// one held knot, in `format` with the given header byte 3 (format 3)
    /// and tangent-algorithm byte (formats 2 and 3).
    fn ts_blob(format: u8, hdr3: u8, algorithms: u8) -> Vec<u8> {
        let mut blob = vec![format | (1 << 4), 0x01 | (0x01 << 3)];
        if format > 2 {
            blob.push(hdr3);
        }
        blob.extend_from_slice(&1_u32.to_le_bytes());
        blob.push(1 << 1); // held
        for v in [2.0_f64, 5.0, 1.0, 1.0, 0.0, 0.0] {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        if format > 1 {
            blob.push(algorithms);
        }
        blob
    }

    fn parse(blob: &[u8], version: CrateVersion) -> Result<SplineData, UsdcError> {
        parse_ts_spline(blob, 0, version)
    }

    #[test]
    fn ts_formats_parse() {
        let newest = CrateVersion::SPLINE_LOOP_BOUNDARY_AND_TIMECODE;
        for format in 1..=3 {
            let spline = parse(&ts_blob(format, 0, 0x21), newest).unwrap();
            assert_eq!(spline.data_type, SplineDataType::Double);
            assert_eq!(spline.pre_extrapolation, Extrapolation::Held);
            assert_eq!(spline.post_extrapolation, Extrapolation::Held);
            assert_eq!(spline.knots.len(), 1);
            assert_eq!(spline.knots[0].time, 2.0);
            assert_eq!(spline.knots[0].value, 5.0);
            assert_eq!(spline.knots[0].next_interp, KnotInterp::Held);
        }
        assert!(parse(&[], CrateVersion::SPLINES).unwrap().knots.is_empty());
    }

    #[test]
    fn ts_format_must_fit_the_crate_version() {
        assert_eq!(
            parse(&ts_blob(3, 0, 0), CrateVersion::ARRAY_EDITS).err(),
            Some(UsdcError::FeatureRequiresVersion {
                feature: "spline binary format",
                required: CrateVersion::SPLINE_LOOP_BOUNDARY_AND_TIMECODE,
                found: CrateVersion::ARRAY_EDITS,
            })
        );
    }

    #[test]
    fn ts_unrepresentable_features_are_reported() {
        let newest = CrateVersion::SPLINE_LOOP_BOUNDARY_AND_TIMECODE;
        let unsupported = |feature| Some(UsdcError::UnsupportedFeature { feature });
        assert_eq!(
            parse(&ts_blob(3, 0x02, 0), newest).err(),
            unsupported("spline loopBoundaryTime")
        );
        let mut time_valued = ts_blob(3, 0, 0);
        time_valued[0] = 3 | (4 << 4);
        assert_eq!(
            parse(&time_valued, newest).err(),
            unsupported("time-valued spline")
        );
        let mut legacy_time_valued = ts_blob(1, 0, 0);
        legacy_time_valued[0] |= 0x40;
        assert_eq!(
            parse(&legacy_time_valued, newest).err(),
            unsupported("time-valued spline")
        );
        let mut future = ts_blob(3, 0, 0);
        future[0] = (future[0] & 0xF0) | 4;
        assert_eq!(
            parse(&future, newest).err(),
            unsupported("spline binary format newer than 3")
        );
    }

    #[test]
    fn ts_malformed_data_is_rejected() {
        let newest = CrateVersion::SPLINE_LOOP_BOUNDARY_AND_TIMECODE;
        let mut trailing = ts_blob(1, 0, 0);
        trailing.push(0);
        let mut bad_extrapolation = ts_blob(1, 0, 0);
        bad_extrapolation[1] = 0x07;
        let mut truncated = ts_blob(2, 0, 0);
        truncated.pop();
        for blob in [
            trailing,
            bad_extrapolation,
            truncated,
            ts_blob(2, 0, 0x03),
            ts_blob(3, 0x04, 0),
        ] {
            assert!(parse(&blob, newest).is_err(), "{blob:?}");
        }
    }

    /// An `OpAndCount` header: count in the low 56 bits, op in the high 8.
    fn op_header(op: u8, count: i64) -> i64 {
        (i64::from(op) << 56) | (count & 0x00FF_FFFF_FFFF_FFFF)
    }

    #[test]
    fn array_edit_instructions_decode() {
        let end = CrateArrayEdit::END;
        let instructions = [
            op_header(7, 1),
            1024,
            op_header(0, 2),
            0,
            2,
            1,
            4,
            op_header(1, 1),
            5,
            6,
            op_header(4, 2),
            9,
            -1,
            op_header(2, 1),
            0,
            end,
            op_header(6, 1),
            3,
            1,
        ];
        let ops = parse_array_edit_instructions(&instructions, 2).unwrap();
        assert_eq!(
            ops,
            [
                CrateArrayEditOp::SetSize { len: 1024 },
                CrateArrayEditOp::WriteLiteral {
                    literal: 0,
                    index: 2
                },
                CrateArrayEditOp::WriteLiteral {
                    literal: 1,
                    index: 4
                },
                CrateArrayEditOp::WriteRef { src: 5, index: 6 },
                CrateArrayEditOp::Erase { index: 9 },
                CrateArrayEditOp::Erase { index: -1 },
                CrateArrayEditOp::InsertLiteral {
                    literal: 0,
                    index: end
                },
                CrateArrayEditOp::MinSizeFill { len: 3, literal: 1 },
            ]
        );
        assert!(parse_array_edit_instructions(&[], 0).unwrap().is_empty());
    }

    #[test]
    fn malformed_array_edit_instructions_are_rejected() {
        for (instructions, literals) in [
            // Unknown op.
            (vec![op_header(10, 1), 0], 0),
            // Non-positive counts.
            (vec![op_header(4, 0)], 0),
            (vec![op_header(4, -1), 0], 0),
            // Missing arguments.
            (vec![op_header(0, 2), 0, 1, 0], 1),
            (vec![op_header(4, 1)], 0),
            // Literal index out of range.
            (vec![op_header(2, 1), 1, 0], 1),
            (vec![op_header(8, 1), 4, -1], 1),
            // Negative size.
            (vec![op_header(9, 1), -3], 0),
        ] {
            assert!(
                parse_array_edit_instructions(&instructions, literals).is_err(),
                "{instructions:?}"
            );
        }
    }

    #[test]
    fn array_edit_needs_crate_0_14() {
        let mut bytes = [0_u8; 8];
        bytes[6] = ValueType::Int as u8;
        bytes[7] = 0x10; // is_array_edit, identity
        let rep = RawValueRep::new(bytes);
        let mut sections = CrateSections {
            tokens: vec![],
            strings: vec![],
            fields: vec![],
            fieldsets: vec![],
            paths: vec![],
            specs: vec![],
            version: CrateVersion::SPLINE_TANGENT_ALGORITHMS,
        };
        assert_eq!(
            decode_value(&rep, &[], &sections).err(),
            Some(UsdcError::FeatureRequiresVersion {
                feature: "array edit",
                required: CrateVersion::ARRAY_EDITS,
                found: CrateVersion::SPLINE_TANGENT_ALGORITHMS,
            })
        );
        sections.version = CrateVersion::ARRAY_EDITS;
        match decode_value(&rep, &[], &sections) {
            Ok(CrateValue::ArrayEdit(edit)) => assert!(edit.ops.is_empty()),
            other => panic!("expected the identity edit, got {other:?}"),
        }
        // An array edit cannot also be an array.
        bytes[7] |= 0x80;
        assert!(decode_value(&RawValueRep::new(bytes), &[], &sections).is_err());
    }

    fn sections_with(version: CrateVersion) -> CrateSections {
        CrateSections {
            tokens: vec!["./ref.usd".into(), "note".into()],
            strings: vec![0, 1],
            fields: vec![],
            fieldsets: vec![],
            paths: vec!["/".into(), "/Ref".into()],
            specs: vec![],
            version,
        }
    }

    fn list_op_rep(vtype: ValueType, offset: u8) -> RawValueRep {
        let mut bytes = [0_u8; 8];
        bytes[0] = offset;
        bytes[6] = vtype as u8;
        RawValueRep::new(bytes)
    }

    fn reference_fields(value: &CrateValue) -> (String, String, f64, f64) {
        let CrateValue::Dictionary(entries) = value else {
            panic!("expected a reference dictionary, got {value:?}");
        };
        match entries.as_slice() {
            [
                (_, CrateValue::AssetPath(asset)),
                (_, CrateValue::String(prim)),
                (_, CrateValue::Double(offset)),
                (_, CrateValue::Double(scale)),
            ] => (asset.clone(), prim.clone(), *offset, *scale),
            other => panic!("unexpected reference entries {other:?}"),
        }
    }

    /// References are variable-length: each carries its `customData`
    /// dictionary after the layer offset (`Write(SdfReference)`,
    /// `pxr/usd/sdf/crateFile.cpp:1529`).
    #[test]
    fn reference_list_op_items_carry_custom_data() {
        let mut data = vec![0_u8; 8];
        data.push(1 << 5); // prepended items
        data.extend_from_slice(&2_u64.to_le_bytes());
        // Reference 1: custom data { note: 7 }.
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&1_u32.to_le_bytes());
        data.extend_from_slice(&10.0_f64.to_le_bytes());
        data.extend_from_slice(&2.0_f64.to_le_bytes());
        data.extend_from_slice(&1_u64.to_le_bytes());
        data.extend_from_slice(&1_u32.to_le_bytes());
        data.extend_from_slice(&8_i64.to_le_bytes());
        data.extend_from_slice(&[7, 0, 0, 0, 0, 0, ValueType::Int as u8, 0x40]);
        // Reference 2: no custom data.
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&1_u32.to_le_bytes());
        data.extend_from_slice(&45.0_f64.to_le_bytes());
        data.extend_from_slice(&0.5_f64.to_le_bytes());
        data.extend_from_slice(&0_u64.to_le_bytes());

        let sections = sections_with(CrateVersion::NEWEST_READABLE);
        let rep = list_op_rep(ValueType::ReferenceListOp, 8);
        let Ok(CrateValue::ListOp(op)) = decode_value(&rep, &data, &sections) else {
            panic!("expected a list op");
        };
        let refs: Vec<_> = op.prepended_items.iter().map(reference_fields).collect();
        let r = |offset, scale| {
            (
                String::from("./ref.usd"),
                String::from("/Ref"),
                offset,
                scale,
            )
        };
        assert_eq!(refs, [r(10.0, 2.0), r(45.0, 0.5)]);

        // Truncated anywhere, the list op fails without panicking.
        for len in 0..data.len() {
            assert!(
                decode_value(&rep, &data[..len], &sections).is_err(),
                "{len}"
            );
        }
    }

    /// Payloads have layer offsets from crate 0.8 (`Write(SdfPayload)`,
    /// `pxr/usd/sdf/crateFile.cpp:1535`).
    #[test]
    fn payload_list_op_items_have_offsets() {
        let mut data = vec![0_u8; 8];
        data.push(1 << 6); // appended items
        data.extend_from_slice(&1_u64.to_le_bytes());
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&1_u32.to_le_bytes());
        data.extend_from_slice(&3.0_f64.to_le_bytes());
        data.extend_from_slice(&4.0_f64.to_le_bytes());

        let sections = sections_with(CrateVersion::NEWEST_READABLE);
        let rep = list_op_rep(ValueType::PayloadListOp, 8);
        let Ok(CrateValue::ListOp(op)) = decode_value(&rep, &data, &sections) else {
            panic!("expected a list op");
        };
        let payloads: Vec<_> = op.appended_items.iter().map(reference_fields).collect();
        assert_eq!(
            payloads,
            [(String::from("./ref.usd"), String::from("/Ref"), 3.0, 4.0)]
        );

        // A crate 0.7 payload is only the asset and prim path.
        let payload = &data[17..25];
        let mut old = vec![0_u8; 8];
        old.extend_from_slice(payload);
        let rep = list_op_rep(ValueType::Payload, 8);
        let sections = sections_with(CrateVersion::OLDEST_READABLE);
        let value = decode_value(&rep, &old, &sections).unwrap();
        assert_eq!(
            reference_fields(&value),
            (String::from("./ref.usd"), String::from("/Ref"), 0.0, 1.0)
        );
    }

    /// A `VtValue` value is reached through an offset relative to its
    /// payload offset, like the values of a dictionary.
    #[test]
    fn vt_values_are_read_through_their_relative_offset() {
        let sections = sections_with(CrateVersion::NEWEST_READABLE);
        let mut data = vec![0_u8; 8];
        data.extend_from_slice(&16_i64.to_le_bytes());
        data.extend_from_slice(&[0xEE; 8]); // Not the value.
        data.extend_from_slice(&[5, 0, 0, 0, 0, 0, ValueType::Int as u8, 0x40]);
        for vtype in [ValueType::Value, ValueType::UnregisteredValue] {
            let rep = list_op_rep(vtype, 8);
            match decode_value(&rep, &data, &sections) {
                Ok(CrateValue::Int(5)) => {}
                other => panic!("expected Int(5), got {other:?}"),
            }
        }
    }

    /// Appends a compressed-ints block encoding `count` values with a
    /// constant delta: the size, the chunk byte, and the LZ4 block.
    fn compressed_ints(data: &mut Vec<u8>, delta: i32, count: usize) {
        let mut encoded = delta.to_le_bytes().to_vec();
        encoded.resize(4 + (count * 2).div_ceil(8), 0);
        let mut block = vec![0_u8];
        block.extend_from_slice(&lz4_flex::compress(&encoded));
        data.extend_from_slice(&(block.len() as u64).to_le_bytes());
        data.extend_from_slice(&block);
    }

    /// Integral float arrays are compressed as `int32_t` values, converted
    /// by value (`_WritePossiblyCompressedArray`,
    /// `pxr/usd/sdf/crateFile.cpp:1990`).
    #[test]
    fn integer_coded_float_arrays_convert_by_value() {
        let sections = sections_with(CrateVersion::NEWEST_READABLE);
        for (vtype, expect) in [
            (ValueType::Double, CrateValue::Double(-32.0)),
            (ValueType::Float, CrateValue::Float(-32.0)),
            (ValueType::Half, CrateValue::Half(f64_to_half_bits(-32.0))),
        ] {
            let mut data = vec![0_u8; 8];
            data.extend_from_slice(&32_u64.to_le_bytes());
            data.push(b'i');
            compressed_ints(&mut data, -1, 32);
            let mut bytes = [0_u8; 8];
            bytes[0] = 8;
            bytes[6] = vtype as u8;
            bytes[7] = 0x80 | 0x20; // array, compressed
            let Ok(CrateValue::Array(values)) =
                decode_value(&RawValueRep::new(bytes), &data, &sections)
            else {
                panic!("expected an array");
            };
            assert_eq!(values.len(), 32);
            assert_eq!(
                alloc::format!("{:?}", values[31]),
                alloc::format!("{expect:?}")
            );
        }
    }

    /// Arrays shorter than 16 elements are stored uncompressed even when
    /// flagged compressed.
    #[test]
    fn short_compressed_arrays_are_stored_plainly() {
        let sections = sections_with(CrateVersion::NEWEST_READABLE);
        let mut data = vec![0_u8; 8];
        data.extend_from_slice(&2_u64.to_le_bytes());
        data.extend_from_slice(&1.5_f64.to_le_bytes());
        data.extend_from_slice(&(-2.5_f64).to_le_bytes());
        let mut bytes = [0_u8; 8];
        bytes[0] = 8;
        bytes[6] = ValueType::Double as u8;
        bytes[7] = 0x80 | 0x20;
        let Ok(CrateValue::Array(values)) =
            decode_value(&RawValueRep::new(bytes), &data, &sections)
        else {
            panic!("expected an array");
        };
        assert_eq!(alloc::format!("{values:?}"), "[Double(1.5), Double(-2.5)]");
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
