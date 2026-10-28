// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The value types an attribute may declare: OpenUSD's Sdf value type
//! registry.
//!
//! Each [`ValueType`] is a scalar type, such as `float3` or `color3f`;
//! every one but `opaque` and `group` also has an array form, its name
//! followed by `[]` (`color3f[]`). A role (`color3f` is a `float3` with the
//! color role) changes what a value means, not how it is stored:
//! [`ValueType::storage`] names the role-less type whose values it holds.
//!
//! [`PropertyType::parse`](crate::PropertyType::parse) takes a type name
//! through this table; the USDA and USDC readers and writers use it too.
//!
//! OpenUSD: `Sdf_ValueTypeRegistry`, filled by `_RegisterStandardTypes`
//! (`pxr/usd/sdf/schema.cpp`) from `SDF_VALUE_TYPES` (`pxr/usd/sdf/types.h`);
//! Python's `Sdf.ValueTypeNames`. The capitalized aliases OpenUSD still
//! registers for a few types (`Vec3f`, `PointFloat`, `Color`, ...) are not
//! accepted: OpenUSD writes the names here, and so do layerstack's writers.
//! Four of its legacy types have no current name and keep their own:
//! `Transform`, a `matrix4d` with the transform role, and `PointIndex`,
//! `EdgeIndex` and `FaceIndex`, `int`s with an index role
//! (`_AddLegacyTypesToRegistry`).
//!
//! Spec: AOUSD Core §6.2 (scalar types), §6.3 (dimensioned types), §6.5
//! (semantic roles), §7.6.4.1.1 (`typeName`).

use alloc::sync::Arc;
use core::fmt;

use crate::{doc::Value, interner::TokenId};

/// The role of a [`ValueType`]: what its values mean beyond their storage.
///
/// OpenUSD: `SdfValueRoleNames`.
///
/// Spec: AOUSD Core §6.5 (semantic roles).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueRole {
    /// A color (`color3f`).
    Color,
    /// A coordinate frame (`frame4d`).
    Frame,
    /// A group of properties (`group`).
    Group,
    /// A surface normal (`normal3f`).
    Normal,
    /// A position (`point3f`).
    Point,
    /// A texture coordinate (`texCoord2f`).
    TextureCoordinate,
    /// A direction (`vector3f`).
    Vector,
    /// A transform (the legacy `Transform`).
    Transform,
    /// The index of a point (the legacy `PointIndex`).
    PointIndex,
    /// The index of an edge (the legacy `EdgeIndex`).
    EdgeIndex,
    /// The index of a face (the legacy `FaceIndex`).
    FaceIndex,
}

impl ValueRole {
    /// The role's name, as OpenUSD spells it (`TextureCoordinate`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Color => "Color",
            Self::Frame => "Frame",
            Self::Group => "Group",
            Self::Normal => "Normal",
            Self::Point => "Point",
            Self::TextureCoordinate => "TextureCoordinate",
            Self::Vector => "Vector",
            Self::Transform => "Transform",
            Self::PointIndex => "PointIndex",
            Self::EdgeIndex => "EdgeIndex",
            Self::FaceIndex => "FaceIndex",
        }
    }
}

macro_rules! value_types {
    ($($variant:ident = $name:literal, $array:expr, $role:expr, $storage:ident;)*) => {
        /// A scalar value type of OpenUSD's value type registry.
        ///
        /// See the [module docs](self).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ValueType {
            $(
                #[doc = concat!("`", $name, "`.")]
                $variant,
            )*
        }

        impl ValueType {
            /// Every value type: the role-less types, then the types with
            /// a role.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// The type's name (`color3f`).
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// The name of the type's array form (`color3f[]`), or `None`
            /// for `opaque` and `group`, which have none.
            #[must_use]
            pub const fn array_name(self) -> Option<&'static str> {
                match self {
                    $(Self::$variant => $array,)*
                }
            }

            /// The type's role, or `None` for a type without one.
            #[must_use]
            pub const fn role(self) -> Option<ValueRole> {
                match self {
                    $(Self::$variant => $role,)*
                }
            }

            /// The role-less type whose values this type's values are:
            /// `float3` for `color3f`, `matrix4d` for `frame4d`, `opaque`
            /// for `group`, and the type itself for a type without a role.
            ///
            /// OpenUSD: `SdfValueTypeName::GetType`, which types sharing
            /// a storage share.
            #[must_use]
            pub const fn storage(self) -> Self {
                match self {
                    $(Self::$variant => Self::$storage,)*
                }
            }

            /// The value type named `name`, a scalar name (`color3f`, not
            /// `color3f[]`); `None` when no registered type has the name.
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                Some(match name {
                    $($name => Self::$variant,)*
                    _ => return None,
                })
            }
        }
    };
}

use ValueRole::{
    Color, EdgeIndex, FaceIndex, Frame, Group, Normal, Point, PointIndex, TextureCoordinate,
    Transform, Vector,
};

value_types! {
    Bool = "bool", Some("bool[]"), None, Bool;
    UChar = "uchar", Some("uchar[]"), None, UChar;
    Int = "int", Some("int[]"), None, Int;
    UInt = "uint", Some("uint[]"), None, UInt;
    Int64 = "int64", Some("int64[]"), None, Int64;
    UInt64 = "uint64", Some("uint64[]"), None, UInt64;
    Half = "half", Some("half[]"), None, Half;
    Float = "float", Some("float[]"), None, Float;
    Double = "double", Some("double[]"), None, Double;
    TimeCode = "timecode", Some("timecode[]"), None, TimeCode;
    Duration = "duration", Some("duration[]"), None, Duration;
    String = "string", Some("string[]"), None, String;
    Token = "token", Some("token[]"), None, Token;
    Asset = "asset", Some("asset[]"), None, Asset;
    PathExpression = "pathExpression", Some("pathExpression[]"), None, PathExpression;
    Opaque = "opaque", None, None, Opaque;
    Int2 = "int2", Some("int2[]"), None, Int2;
    Int3 = "int3", Some("int3[]"), None, Int3;
    Int4 = "int4", Some("int4[]"), None, Int4;
    Half2 = "half2", Some("half2[]"), None, Half2;
    Half3 = "half3", Some("half3[]"), None, Half3;
    Half4 = "half4", Some("half4[]"), None, Half4;
    Float2 = "float2", Some("float2[]"), None, Float2;
    Float3 = "float3", Some("float3[]"), None, Float3;
    Float4 = "float4", Some("float4[]"), None, Float4;
    Double2 = "double2", Some("double2[]"), None, Double2;
    Double3 = "double3", Some("double3[]"), None, Double3;
    Double4 = "double4", Some("double4[]"), None, Double4;
    Matrix2d = "matrix2d", Some("matrix2d[]"), None, Matrix2d;
    Matrix3d = "matrix3d", Some("matrix3d[]"), None, Matrix3d;
    Matrix4d = "matrix4d", Some("matrix4d[]"), None, Matrix4d;
    Quath = "quath", Some("quath[]"), None, Quath;
    Quatf = "quatf", Some("quatf[]"), None, Quatf;
    Quatd = "quatd", Some("quatd[]"), None, Quatd;
    Point3h = "point3h", Some("point3h[]"), Some(Point), Half3;
    Point3f = "point3f", Some("point3f[]"), Some(Point), Float3;
    Point3d = "point3d", Some("point3d[]"), Some(Point), Double3;
    Vector3h = "vector3h", Some("vector3h[]"), Some(Vector), Half3;
    Vector3f = "vector3f", Some("vector3f[]"), Some(Vector), Float3;
    Vector3d = "vector3d", Some("vector3d[]"), Some(Vector), Double3;
    Normal3h = "normal3h", Some("normal3h[]"), Some(Normal), Half3;
    Normal3f = "normal3f", Some("normal3f[]"), Some(Normal), Float3;
    Normal3d = "normal3d", Some("normal3d[]"), Some(Normal), Double3;
    Color3h = "color3h", Some("color3h[]"), Some(Color), Half3;
    Color3f = "color3f", Some("color3f[]"), Some(Color), Float3;
    Color3d = "color3d", Some("color3d[]"), Some(Color), Double3;
    Color4h = "color4h", Some("color4h[]"), Some(Color), Half4;
    Color4f = "color4f", Some("color4f[]"), Some(Color), Float4;
    Color4d = "color4d", Some("color4d[]"), Some(Color), Double4;
    Frame4d = "frame4d", Some("frame4d[]"), Some(Frame), Matrix4d;
    TexCoord2h = "texCoord2h", Some("texCoord2h[]"), Some(TextureCoordinate), Half2;
    TexCoord2f = "texCoord2f", Some("texCoord2f[]"), Some(TextureCoordinate), Float2;
    TexCoord2d = "texCoord2d", Some("texCoord2d[]"), Some(TextureCoordinate), Double2;
    TexCoord3h = "texCoord3h", Some("texCoord3h[]"), Some(TextureCoordinate), Half3;
    TexCoord3f = "texCoord3f", Some("texCoord3f[]"), Some(TextureCoordinate), Float3;
    TexCoord3d = "texCoord3d", Some("texCoord3d[]"), Some(TextureCoordinate), Double3;
    Group = "group", None, Some(Group), Opaque;
    Transform = "Transform", Some("Transform[]"), Some(Transform), Matrix4d;
    PointIndex = "PointIndex", Some("PointIndex[]"), Some(PointIndex), Int;
    EdgeIndex = "EdgeIndex", Some("EdgeIndex[]"), Some(EdgeIndex), Int;
    FaceIndex = "FaceIndex", Some("FaceIndex[]"), Some(FaceIndex), Int;
}

impl ValueType {
    /// The value type of `type_name`, a scalar name or an array name
    /// (`color3f[]`), and whether it names the array form; `None` when no
    /// registered type has the name, including `opaque[]` and `group[]`.
    #[must_use]
    pub fn parse(type_name: &str) -> Option<(Self, bool)> {
        match type_name.strip_suffix("[]") {
            Some(scalar) => {
                let ty = Self::from_name(scalar)?;
                ty.array_name().is_some().then_some((ty, true))
            }
            None => Some((Self::from_name(type_name)?, false)),
        }
    }

    /// One element of the type's zero value, the empty token
    /// ([`TokenId::EMPTY`]) for `token`: what a sparse array edit appends
    /// when it grows an array of this type, as `VtArray::resize`
    /// value-initializes its elements. `opaque` and `group` hold no value
    /// ([`Value::Null`]).
    ///
    /// OpenUSD's registry default differs for the matrix and quaternion
    /// types, whose default values are the identity.
    #[must_use]
    pub fn zero(self) -> Value {
        match self.storage() {
            Self::Bool => Value::Bool(false),
            Self::UChar => Value::UChar(0),
            Self::Int => Value::Int(0),
            Self::UInt => Value::UInt(0),
            Self::Int64 => Value::Int64(0),
            Self::UInt64 => Value::UInt64(0),
            Self::Half => Value::Half(0),
            Self::Float => Value::Float(0.0),
            Self::Double => Value::Double(0.0),
            Self::TimeCode => Value::TimeCode(0.0),
            Self::Duration => Value::Duration(0.0),
            Self::String => Value::String(Arc::from("")),
            Self::Token => Value::Token(TokenId::EMPTY),
            Self::Asset => Value::Asset(Arc::from("")),
            Self::PathExpression => Value::PathExpression(Arc::from("")),
            Self::Int2 => Value::Vec2i([0; 2]),
            Self::Int3 => Value::Vec3i([0; 3]),
            Self::Int4 => Value::Vec4i([0; 4]),
            Self::Half2 => Value::Vec2h([0; 2]),
            Self::Half3 => Value::Vec3h([0; 3]),
            Self::Half4 => Value::Vec4h([0; 4]),
            Self::Float2 => Value::Vec2f([0.0; 2]),
            Self::Float3 => Value::Vec3f([0.0; 3]),
            Self::Float4 => Value::Vec4f([0.0; 4]),
            Self::Double2 => Value::Vec2d([0.0; 2]),
            Self::Double3 => Value::Vec3d([0.0; 3]),
            Self::Double4 => Value::Vec4d([0.0; 4]),
            Self::Matrix2d => Value::Matrix2d(alloc::boxed::Box::new([0.0; 4])),
            Self::Matrix3d => Value::Matrix3d(alloc::boxed::Box::new([0.0; 9])),
            Self::Matrix4d => Value::Matrix4d(alloc::boxed::Box::new([0.0; 16])),
            Self::Quath => Value::Quath([0; 4]),
            Self::Quatf => Value::Quatf([0.0; 4]),
            Self::Quatd => Value::Quatd([0.0; 4]),
            _ => Value::Null,
        }
    }

    /// [`Self::zero`] as a constant, for the types whose zero owns no
    /// allocation; `None` for the text types (`string`, `asset`,
    /// `pathExpression`) and the matrices, whose zeros own their contents.
    pub(crate) fn constant_zero(self) -> Option<&'static Value> {
        Some(match self.storage() {
            Self::Bool => &Value::Bool(false),
            Self::UChar => &Value::UChar(0),
            Self::Int => &Value::Int(0),
            Self::UInt => &Value::UInt(0),
            Self::Int64 => &Value::Int64(0),
            Self::UInt64 => &Value::UInt64(0),
            Self::Half => &Value::Half(0),
            Self::Float => &Value::Float(0.0),
            Self::Double => &Value::Double(0.0),
            Self::TimeCode => &Value::TimeCode(0.0),
            Self::Duration => &Value::Duration(0.0),
            Self::Token => &Value::Token(TokenId::EMPTY),
            Self::Int2 => &Value::Vec2i([0; 2]),
            Self::Int3 => &Value::Vec3i([0; 3]),
            Self::Int4 => &Value::Vec4i([0; 4]),
            Self::Half2 => &Value::Vec2h([0; 2]),
            Self::Half3 => &Value::Vec3h([0; 3]),
            Self::Half4 => &Value::Vec4h([0; 4]),
            Self::Float2 => &Value::Vec2f([0.0; 2]),
            Self::Float3 => &Value::Vec3f([0.0; 3]),
            Self::Float4 => &Value::Vec4f([0.0; 4]),
            Self::Double2 => &Value::Vec2d([0.0; 2]),
            Self::Double3 => &Value::Vec3d([0.0; 3]),
            Self::Double4 => &Value::Vec4d([0.0; 4]),
            Self::Quath => &Value::Quath([0; 4]),
            Self::Quatf => &Value::Quatf([0.0; 4]),
            Self::Quatd => &Value::Quatd([0.0; 4]),
            Self::String
            | Self::Asset
            | Self::PathExpression
            | Self::Matrix2d
            | Self::Matrix3d
            | Self::Matrix4d => return None,
            _ => &Value::Null,
        })
    }
}

/// The name of the role-less type `value` is a value of (`double`,
/// `float3[]`), as an edit that rejects it names it; `dictionary` for a
/// dictionary, and `value` for anything else no type holds.
pub(crate) fn offered_type_name(value: &Value) -> &'static str {
    fn scalar(value: &Value) -> Option<ValueType> {
        let kind = core::mem::discriminant(value);
        ValueType::ALL
            .iter()
            .copied()
            .filter(|ty| ty.storage() == *ty)
            .find(|ty| {
                let zero = ty.zero();
                !matches!(zero, Value::Null) && core::mem::discriminant(&zero) == kind
            })
    }
    let element = match value {
        Value::Dictionary(_) => return "dictionary",
        Value::Array(items) => match items.first() {
            Some(item) => item,
            None => return "array",
        },
        Value::TypedArray(items) => {
            return scalar(&items.element_kind())
                .and_then(ValueType::array_name)
                .unwrap_or("array");
        }
        value => return scalar(value).map_or("value", ValueType::name),
    };
    scalar(element)
        .and_then(ValueType::array_name)
        .unwrap_or("array")
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A type name no registered value type has: not one of [`ValueType`]'s
/// names, nor the array form of one that has an array form.
///
/// Spec: AOUSD Core §6.2 (value types), §7.6.4.1.1 (`typeName`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownValueType {
    type_name: Arc<str>,
}

impl UnknownValueType {
    pub(crate) fn new(type_name: &str) -> Self {
        Self {
            type_name: Arc::from(type_name),
        }
    }

    /// The type name.
    #[must_use]
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    pub(crate) fn into_type_name(self) -> Arc<str> {
        self.type_name
    }
}

impl fmt::Display for UnknownValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not a registered value type", self.type_name)
    }
}

impl core::error::Error for UnknownValueType {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse_back_to_their_types() {
        assert_eq!(ValueType::ALL.len(), 61);
        for &ty in ValueType::ALL {
            assert_eq!(ValueType::parse(ty.name()), Some((ty, false)), "{ty}");
            if let Some(array) = ty.array_name() {
                assert_eq!(array, alloc::format!("{}[]", ty.name()));
                assert_eq!(ValueType::parse(array), Some((ty, true)), "{array}");
            }
            assert_eq!(ty.storage().role(), None, "{ty}");
            assert_eq!(ty.storage().storage(), ty.storage(), "{ty}");
        }
    }

    #[test]
    fn constant_zeros_are_the_zeros() {
        for &ty in ValueType::ALL {
            match ty.constant_zero() {
                Some(zero) => assert_eq!(zero, &ty.zero(), "{ty}"),
                None => assert!(
                    matches!(
                        ty.zero(),
                        Value::String(_)
                            | Value::Asset(_)
                            | Value::PathExpression(_)
                            | Value::Matrix2d(_)
                            | Value::Matrix3d(_)
                            | Value::Matrix4d(_)
                    ),
                    "{ty}"
                ),
            }
        }
    }

    #[test]
    fn unregistered_names_do_not_parse() {
        for name in [
            "float5",
            "",
            "[]",
            "Vec3f",
            "PointFloat",
            "dictionary",
            "opaque[]",
            "group[]",
            "float[][]",
            "float []",
        ] {
            assert_eq!(ValueType::parse(name), None, "{name}");
        }
    }

    #[test]
    fn roles_keep_their_storage() {
        assert_eq!(ValueType::Color3f.storage(), ValueType::Float3);
        assert_eq!(ValueType::Color3f.role(), Some(Color));
        assert_eq!(ValueType::Frame4d.storage(), ValueType::Matrix4d);
        assert_eq!(ValueType::Transform.storage(), ValueType::Matrix4d);
        assert_eq!(ValueType::Transform.role(), Some(Transform));
        assert_eq!(ValueType::FaceIndex.storage(), ValueType::Int);
        assert_eq!(
            ValueType::parse("PointIndex[]"),
            Some((ValueType::PointIndex, true))
        );
        assert_eq!(ValueType::Group.storage(), ValueType::Opaque);
        assert_eq!(ValueType::TexCoord2h.zero(), Value::Vec2h([0; 2]));
        assert_eq!(ValueType::Group.zero(), Value::Null);
        let tokens = crate::TokenInterner::default();
        assert_eq!(ValueType::Token.zero(), Value::Token(tokens.intern("")));
    }
}
