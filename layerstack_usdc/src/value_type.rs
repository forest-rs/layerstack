// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDC value type and spec form enumerations.
//!
//! Spec: AOUSD Core §16.3.9 (value representations), §16.3.10 (value types).

use crate::error::UsdcError;

/// USDC value types encoded in [`RawValueRep`](crate::value_rep::RawValueRep) byte 6.
///
/// Spec: AOUSD Core §16.3.10.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ValueType {
    /// Unknown / unset value type.
    Unknown = 0,
    /// Boolean.
    Bool = 1,
    /// Unsigned 8-bit integer.
    UChar = 2,
    /// Signed 32-bit integer.
    Int = 3,
    /// Unsigned 32-bit integer.
    UInt = 4,
    /// Signed 64-bit integer.
    Int64 = 5,
    /// Unsigned 64-bit integer.
    UInt64 = 6,
    /// IEEE 754 half-precision float (16-bit).
    Half = 7,
    /// IEEE 754 single-precision float (32-bit).
    Float = 8,
    /// IEEE 754 double-precision float (64-bit).
    Double = 9,
    /// String value (index into STRINGS section).
    String = 10,
    /// Token value (index into TOKENS section).
    Token = 11,
    /// Asset path value.
    AssetPath = 12,
    /// 2×2 double-precision matrix.
    Matrix2d = 13,
    /// 3×3 double-precision matrix.
    Matrix3d = 14,
    /// 4×4 double-precision matrix.
    Matrix4d = 15,
    /// Double-precision quaternion.
    Quatd = 16,
    /// Single-precision quaternion.
    Quatf = 17,
    /// Half-precision quaternion.
    Quath = 18,
    /// 2-component double vector.
    Vec2d = 19,
    /// 2-component float vector.
    Vec2f = 20,
    /// 2-component half vector.
    Vec2h = 21,
    /// 2-component integer vector.
    Vec2i = 22,
    /// 3-component double vector.
    Vec3d = 23,
    /// 3-component float vector.
    Vec3f = 24,
    /// 3-component half vector.
    Vec3h = 25,
    /// 3-component integer vector.
    Vec3i = 26,
    /// 4-component double vector.
    Vec4d = 27,
    /// 4-component float vector.
    Vec4f = 28,
    /// 4-component half vector.
    Vec4h = 29,
    /// 4-component integer vector.
    Vec4i = 30,
    /// Dictionary (string-keyed value map).
    Dictionary = 31,
    /// Token list operation.
    TokenListOp = 32,
    /// String list operation.
    StringListOp = 33,
    /// Path list operation.
    PathListOp = 34,
    /// Reference list operation.
    ReferenceListOp = 35,
    /// Int list operation.
    IntListOp = 36,
    /// Int64 list operation.
    Int64ListOp = 37,
    /// `UInt` list operation.
    UIntListOp = 38,
    /// `UInt64` list operation.
    UInt64ListOp = 39,
    /// Vector of paths.
    PathVector = 40,
    /// Vector of tokens.
    TokenVector = 41,
    /// Specifier enum value (`Def`/`Over`/`Class`).
    Specifier = 42,
    /// Permission enum value (`Public`/`Private`).
    Permission = 43,
    /// Variability enum value (`Varying`/`Uniform`).
    Variability = 44,
    /// Variant selection map (string key → string value).
    VariantSelectionMap = 45,
    /// Time samples (timecode → value pairs).
    TimeSamples = 46,
    /// Payload.
    Payload = 47,
    /// Vector of doubles.
    DoubleVector = 48,
    /// Vector of layer offsets.
    LayerOffsetVector = 49,
    /// Vector of strings.
    StringVector = 50,
    /// Value block sentinel (suppresses weaker opinions).
    ValueBlock = 51,
    /// Wrapped value.
    Value = 52,
    /// Unregistered value.
    UnregisteredValue = 53,
    /// Unregistered value list operation.
    UnregisteredValueListOp = 54,
    /// Payload list operation.
    PayloadListOp = 55,
    /// Timecode.
    TimeCode = 56,
    /// Path expression.
    PathExpression = 57,
    /// Relocates map.
    Relocates = 58,
    /// Spline.
    Spline = 59,
}

impl ValueType {
    /// Whether this value type supports arrays (i.e. the `is_array` flag is
    /// meaningful).
    #[must_use]
    pub fn supports_array(self) -> bool {
        let v = self as u8;
        !(31..=55).contains(&v)
    }
}

impl TryFrom<u8> for ValueType {
    type Error = UsdcError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Bool),
            2 => Ok(Self::UChar),
            3 => Ok(Self::Int),
            4 => Ok(Self::UInt),
            5 => Ok(Self::Int64),
            6 => Ok(Self::UInt64),
            7 => Ok(Self::Half),
            8 => Ok(Self::Float),
            9 => Ok(Self::Double),
            10 => Ok(Self::String),
            11 => Ok(Self::Token),
            12 => Ok(Self::AssetPath),
            13 => Ok(Self::Matrix2d),
            14 => Ok(Self::Matrix3d),
            15 => Ok(Self::Matrix4d),
            16 => Ok(Self::Quatd),
            17 => Ok(Self::Quatf),
            18 => Ok(Self::Quath),
            19 => Ok(Self::Vec2d),
            20 => Ok(Self::Vec2f),
            21 => Ok(Self::Vec2h),
            22 => Ok(Self::Vec2i),
            23 => Ok(Self::Vec3d),
            24 => Ok(Self::Vec3f),
            25 => Ok(Self::Vec3h),
            26 => Ok(Self::Vec3i),
            27 => Ok(Self::Vec4d),
            28 => Ok(Self::Vec4f),
            29 => Ok(Self::Vec4h),
            30 => Ok(Self::Vec4i),
            31 => Ok(Self::Dictionary),
            32 => Ok(Self::TokenListOp),
            33 => Ok(Self::StringListOp),
            34 => Ok(Self::PathListOp),
            35 => Ok(Self::ReferenceListOp),
            36 => Ok(Self::IntListOp),
            37 => Ok(Self::Int64ListOp),
            38 => Ok(Self::UIntListOp),
            39 => Ok(Self::UInt64ListOp),
            40 => Ok(Self::PathVector),
            41 => Ok(Self::TokenVector),
            42 => Ok(Self::Specifier),
            43 => Ok(Self::Permission),
            44 => Ok(Self::Variability),
            45 => Ok(Self::VariantSelectionMap),
            46 => Ok(Self::TimeSamples),
            47 => Ok(Self::Payload),
            48 => Ok(Self::DoubleVector),
            49 => Ok(Self::LayerOffsetVector),
            50 => Ok(Self::StringVector),
            51 => Ok(Self::ValueBlock),
            52 => Ok(Self::Value),
            53 => Ok(Self::UnregisteredValue),
            54 => Ok(Self::UnregisteredValueListOp),
            55 => Ok(Self::PayloadListOp),
            56 => Ok(Self::TimeCode),
            57 => Ok(Self::PathExpression),
            58 => Ok(Self::Relocates),
            59 => Ok(Self::Spline),
            _ => Err(UsdcError::UnknownValueType { type_byte: value }),
        }
    }
}

/// Spec forms identifying the kind of spec in the SPECS section.
///
/// Spec: AOUSD Core §16.3.8.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SpecForm {
    /// Unknown form.
    Unknown = 0,
    /// Attribute spec.
    Attribute = 1,
    /// Connection spec.
    Connection = 2,
    /// Expression spec.
    Expression = 3,
    /// Mapper spec.
    Mapper = 4,
    /// Mapper argument spec.
    MapperArg = 5,
    /// Prim spec.
    Prim = 6,
    /// Pseudo-root spec (layer-level metadata).
    PseudoRoot = 7,
    /// Relationship spec.
    Relationship = 8,
    /// Relationship target spec.
    RelationshipTarget = 9,
    /// Variant spec.
    Variant = 10,
    /// Variant set spec.
    VariantSet = 11,
}

impl TryFrom<u32> for SpecForm {
    type Error = UsdcError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Attribute),
            2 => Ok(Self::Connection),
            3 => Ok(Self::Expression),
            4 => Ok(Self::Mapper),
            5 => Ok(Self::MapperArg),
            6 => Ok(Self::Prim),
            7 => Ok(Self::PseudoRoot),
            8 => Ok(Self::Relationship),
            9 => Ok(Self::RelationshipTarget),
            10 => Ok(Self::Variant),
            11 => Ok(Self::VariantSet),
            _ => Err(UsdcError::UnknownSpecForm { form: value }),
        }
    }
}
