// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authoring transform ops: an `Xformable` prim's `xformOp:*` attributes and
//! its `xformOpOrder`, as OpenUSD's `UsdGeomXformable::AddXformOp` and
//! `UsdGeomXformOp::Set` author them.

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::fmt;

use layerstack::{PathId, PropertyType, Value, half};

use crate::edit::SchemaEdit;
use crate::usd_geom::{Xformable, XformableEdit};
use crate::xform::{INVERT_PREFIX, RESET_XFORM_STACK, XformOpType};

/// The precision of a transform op's value.
///
/// OpenUSD: `UsdGeomXformOp::Precision`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum XformOpPrecision {
    /// `double`, `double3`, `quatd` (and `matrix4d`, the only precision a
    /// `transform` op has).
    Double,
    /// `float`, `float3`, `quatf`.
    Float,
    /// `half`, `half3`, `quath`.
    Half,
}

impl XformOpPrecision {
    /// The precision of the value type `type_name`, if it is an op's.
    ///
    /// OpenUSD: `UsdGeomXformOp::GetPrecisionFromValueTypeName`.
    #[must_use]
    pub fn of_type(type_name: &str) -> Option<Self> {
        match type_name {
            "double" | "double3" | "quatd" | "matrix4d" => Some(Self::Double),
            "float" | "float3" | "quatf" => Some(Self::Float),
            "half" | "half3" | "quath" => Some(Self::Half),
            _ => None,
        }
    }
}

/// The value type of an op of `op_type` in `precision`: a `transform` op is
/// always `matrix4d`.
///
/// OpenUSD: `UsdGeomXformOp::GetValueTypeName`.
fn value_type(op_type: XformOpType, precision: XformOpPrecision) -> PropertyType {
    use XformOpPrecision as P;
    use XformOpType as T;
    let (name, zero) = match (op_type, precision) {
        (T::Transform, _) => (
            "matrix4d",
            Value::Matrix4d(alloc::boxed::Box::new([0.0; 16])),
        ),
        (T::Orient, P::Double) => ("quatd", Value::Quatd([0.0; 4])),
        (T::Orient, P::Float) => ("quatf", Value::Quatf([0.0; 4])),
        (T::Orient, P::Half) => ("quath", Value::Quath([0; 4])),
        (op, precision) if op.is_scalar() => match precision {
            P::Double => ("double", Value::Double(0.0)),
            P::Float => ("float", Value::Float(0.0)),
            P::Half => ("half", Value::Half(0)),
        },
        (_, P::Double) => ("double3", Value::Vec3d([0.0; 3])),
        (_, P::Float) => ("float3", Value::Vec3f([0.0; 3])),
        (_, P::Half) => ("half3", Value::Vec3h([0; 3])),
    };
    PropertyType::new(name, false, zero)
}

impl XformOpType {
    /// Whether its value is a scalar (`translateX`, `scaleY`, `rotateZ`, …).
    #[must_use]
    pub fn is_scalar(self) -> bool {
        matches!(
            self,
            Self::TranslateX
                | Self::TranslateY
                | Self::TranslateZ
                | Self::ScaleX
                | Self::ScaleY
                | Self::ScaleZ
                | Self::RotateX
                | Self::RotateY
                | Self::RotateZ
        )
    }

    /// The name of the attribute an op of this type with `suffix` reads
    /// (`xformOp:translate:pivot`).
    ///
    /// OpenUSD: `UsdGeomXformOp::GetOpName(opType, opSuffix)`.
    #[must_use]
    pub fn attribute_name(self, suffix: Option<&str>) -> String {
        match suffix {
            Some(suffix) if !suffix.is_empty() => {
                format!("xformOp:{}:{suffix}", self.as_str())
            }
            _ => format!("xformOp:{}", self.as_str()),
        }
    }
}

/// A value for a transform op, in double precision; setting it converts it
/// to the op's precision.
#[derive(Clone, Debug, PartialEq)]
pub enum XformOpValue {
    /// The value of a scalar op: an offset, a scale or degrees.
    Scalar(f64),
    /// The value of `translate`, `scale` or a three-axis rotate (degrees
    /// about each axis).
    Vector([f64; 3]),
    /// The value of `orient`: a quaternion `[i, j, k, r]`.
    Orientation([f64; 4]),
    /// The value of `transform`: rows, transforming row vectors.
    Matrix([[f64; 4]; 4]),
}

impl From<f64> for XformOpValue {
    fn from(value: f64) -> Self {
        Self::Scalar(value)
    }
}

impl From<[f64; 3]> for XformOpValue {
    fn from(value: [f64; 3]) -> Self {
        Self::Vector(value)
    }
}

impl From<[[f64; 4]; 4]> for XformOpValue {
    fn from(value: [[f64; 4]; 4]) -> Self {
        Self::Matrix(value)
    }
}

/// Why a transform op edit was refused. Nothing is authored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XformOpError {
    /// `xformOpOrder` already lists the op.
    ///
    /// OpenUSD: `UsdGeomXformable::AddXformOp`'s "already exists in
    /// xformOpOrder" error.
    AlreadyInOrder {
        /// The op, as `xformOpOrder` would list it.
        op: String,
    },
    /// The value is not of the kind the op takes (a vector for a scalar
    /// op, …).
    ValueKind {
        /// The op's type.
        op_type: XformOpType,
    },
    /// The op is an inverse op, whose value is its forward op's: set the
    /// forward op's instead.
    ///
    /// OpenUSD: `UsdGeomXformOp::Set`'s "Cannot set a value on the inverse
    /// xformOp" error.
    InverseOp {
        /// The op, as `xformOpOrder` lists it.
        op: String,
    },
}

impl fmt::Display for XformOpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInOrder { op } => write!(f, "xformOpOrder already lists {op}"),
            Self::InverseOp { op } => {
                write!(f, "{op} is an inverse op; set its forward op instead")
            }
            Self::ValueKind { op_type } => {
                write!(
                    f,
                    "the value is not of the kind a {} op takes",
                    op_type.as_str()
                )
            }
        }
    }
}

/// One transform op of a prim, to set the value of. Had from
/// [`XformableEdit::add_op`] (or its conveniences).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XformOpEdit {
    path: PathId,
    attribute: Arc<str>,
    op_type: XformOpType,
    precision: XformOpPrecision,
    inverse: bool,
}

impl XformOpEdit {
    /// The prim's path.
    #[must_use]
    pub fn path(&self) -> PathId {
        self.path
    }

    /// The attribute it reads (`xformOp:translate:pivot`).
    #[must_use]
    pub fn attribute(&self) -> &str {
        &self.attribute
    }

    /// The op as `xformOpOrder` lists it (`!invert!xformOp:translate:pivot`).
    #[must_use]
    pub fn name(&self) -> String {
        if self.inverse {
            format!("{INVERT_PREFIX}{}", self.attribute)
        } else {
            self.attribute.to_string()
        }
    }

    /// Its type.
    #[must_use]
    pub fn op_type(&self) -> XformOpType {
        self.op_type
    }

    /// The precision of its attribute: an existing attribute keeps its own.
    #[must_use]
    pub fn precision(&self) -> XformOpPrecision {
        self.precision
    }

    /// Whether it applies its inverse.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.inverse
    }

    /// Authors `value` as the op's default.
    ///
    /// OpenUSD: `UsdGeomXformOp::Set` at the default time.
    ///
    /// # Errors
    ///
    /// Nothing is authored, with [`XformOpError::InverseOp`] for an inverse
    /// op (its value is its forward op's, which setting it would change),
    /// or [`XformOpError::ValueKind`] when the value is not of the kind the
    /// op takes.
    pub fn set(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: impl Into<XformOpValue>,
    ) -> Result<&Self, XformOpError> {
        self.write(edit, None, value.into())
    }

    /// Authors `value` as a time sample at the time code `time`.
    ///
    /// OpenUSD: `UsdGeomXformOp::Set` at a time code.
    ///
    /// # Errors
    ///
    /// As for [`XformOpEdit::set`].
    pub fn set_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: impl Into<XformOpValue>,
    ) -> Result<&Self, XformOpError> {
        self.write(edit, Some(time), value.into())
    }

    fn write(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: Option<f64>,
        value: XformOpValue,
    ) -> Result<&Self, XformOpError> {
        if self.inverse {
            return Err(XformOpError::InverseOp { op: self.name() });
        }
        let value =
            encode(self.op_type, self.precision, &value).ok_or(XformOpError::ValueKind {
                op_type: self.op_type,
            })?;
        edit.set_value(self.path, &self.attribute, time, value);
        Ok(self)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a float or half op holds its value at that precision, as OpenUSD's does"
)]
fn narrow(v: f64) -> f32 {
    v as f32
}

/// `value` as the op's attribute holds it.
fn encode(
    op_type: XformOpType,
    precision: XformOpPrecision,
    value: &XformOpValue,
) -> Option<Value> {
    use XformOpPrecision as P;
    use XformOpType as T;
    Some(match (op_type, value) {
        (T::Transform, XformOpValue::Matrix(m)) => {
            Value::Matrix4d(alloc::boxed::Box::new(core::array::from_fn(|i| {
                m[i / 4][i % 4]
            })))
        }
        (T::Orient, XformOpValue::Orientation(q)) => match precision {
            P::Double => Value::Quatd(*q),
            P::Float => Value::Quatf(q.map(narrow)),
            P::Half => Value::Quath(q.map(|c| half::from_f32(narrow(c)))),
        },
        (op, XformOpValue::Scalar(v)) if op.is_scalar() => match precision {
            P::Double => Value::Double(*v),
            P::Float => Value::Float(narrow(*v)),
            P::Half => Value::Half(half::from_f32(narrow(*v))),
        },
        (op, XformOpValue::Vector(v))
            if !op.is_scalar() && !matches!(op, T::Transform | T::Orient) =>
        {
            match precision {
                P::Double => Value::Vec3d(*v),
                P::Float => Value::Vec3f(v.map(narrow)),
                P::Half => Value::Vec3h(v.map(|c| half::from_f32(narrow(c)))),
            }
        }
        _ => return None,
    })
}

impl XformableEdit {
    /// The prim's `xformOpOrder` as the edit leaves it.
    fn order(&self, edit: &mut SchemaEdit<'_>) -> Vec<String> {
        let tokens = |edit: &mut SchemaEdit<'_>, value: Value| match value {
            Value::Array(items) => items
                .iter()
                .filter_map(|item| match item {
                    Value::Token(token) => Some(edit.tokens().resolve(*token).to_string()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        edit.default_value(self.path(), Xformable::XFORM_OP_ORDER)
            .map(|value| tokens(edit, value))
            .unwrap_or_default()
    }

    fn set_order(&self, edit: &mut SchemaEdit<'_>, order: &[String]) {
        let order: Vec<&str> = order.iter().map(String::as_str).collect();
        self.set_xform_op_order(edit, &order);
    }

    /// Adds an op of `op_type` to the end of the prim's `xformOpOrder`
    /// (`!invert!` first for an inverse op), creating its attribute
    /// (`xformOp:<type>[:<suffix>]`, of the value type `precision` gives)
    /// unless the prim has it, in which case the attribute keeps its own
    /// type. A `transform` op is always `matrix4d`.
    ///
    /// OpenUSD: `UsdGeomXformable::AddXformOp`.
    ///
    /// # Errors
    ///
    /// [`XformOpError::AlreadyInOrder`] when `xformOpOrder` lists the op.
    pub fn add_op(
        &self,
        edit: &mut SchemaEdit<'_>,
        op_type: XformOpType,
        precision: XformOpPrecision,
        suffix: Option<&str>,
        inverse: bool,
    ) -> Result<XformOpEdit, XformOpError> {
        let attribute = op_type.attribute_name(suffix);
        let name = if inverse {
            format!("{INVERT_PREFIX}{attribute}")
        } else {
            attribute.clone()
        };
        let mut order = self.order(edit);
        if order.contains(&name) {
            return Err(XformOpError::AlreadyInOrder { op: name });
        }
        let precision = match edit.attribute_type(self.path(), &attribute) {
            Some(existing) => XformOpPrecision::of_type(&existing.type_name).unwrap_or(precision),
            None => {
                let ty = value_type(op_type, precision);
                let precision = XformOpPrecision::of_type(&ty.type_name).unwrap_or(precision);
                edit.create_attribute(self.path(), &attribute, ty);
                precision
            }
        };
        order.push(name);
        self.set_order(edit, &order);
        Ok(XformOpEdit {
            path: self.path(),
            attribute: Arc::from(attribute),
            op_type,
            precision,
            inverse,
        })
    }

    /// The op of `op_type` (with `suffix`, inverse or not) that the prim's
    /// `xformOpOrder` lists, as the edit leaves it, with the precision of
    /// its attribute; `None` when the order does not list it or the prim
    /// has no such attribute.
    ///
    /// OpenUSD: `UsdGeomXformable::GetXformOp`.
    #[must_use]
    pub fn get_op(
        &self,
        edit: &mut SchemaEdit<'_>,
        op_type: XformOpType,
        suffix: Option<&str>,
        inverse: bool,
    ) -> Option<XformOpEdit> {
        let attribute = op_type.attribute_name(suffix);
        let name = if inverse {
            format!("{INVERT_PREFIX}{attribute}")
        } else {
            attribute.clone()
        };
        if !self.order(edit).contains(&name) {
            return None;
        }
        let precision =
            XformOpPrecision::of_type(&edit.attribute_type(self.path(), &attribute)?.type_name)?;
        Some(XformOpEdit {
            path: self.path(),
            attribute: Arc::from(attribute),
            op_type,
            precision,
            inverse,
        })
    }

    /// Adds a `translate` op ([`XformableEdit::add_op`]).
    ///
    /// # Errors
    ///
    /// As for [`XformableEdit::add_op`].
    pub fn add_translate_op(
        &self,
        edit: &mut SchemaEdit<'_>,
        precision: XformOpPrecision,
    ) -> Result<XformOpEdit, XformOpError> {
        self.add_op(edit, XformOpType::Translate, precision, None, false)
    }

    /// Adds a `scale` op ([`XformableEdit::add_op`]).
    ///
    /// # Errors
    ///
    /// As for [`XformableEdit::add_op`].
    pub fn add_scale_op(
        &self,
        edit: &mut SchemaEdit<'_>,
        precision: XformOpPrecision,
    ) -> Result<XformOpEdit, XformOpError> {
        self.add_op(edit, XformOpType::Scale, precision, None, false)
    }

    /// Adds a `rotateXYZ` op ([`XformableEdit::add_op`]).
    ///
    /// # Errors
    ///
    /// As for [`XformableEdit::add_op`].
    pub fn add_rotate_xyz_op(
        &self,
        edit: &mut SchemaEdit<'_>,
        precision: XformOpPrecision,
    ) -> Result<XformOpEdit, XformOpError> {
        self.add_op(edit, XformOpType::RotateXyz, precision, None, false)
    }

    /// Adds an `orient` op ([`XformableEdit::add_op`]).
    ///
    /// # Errors
    ///
    /// As for [`XformableEdit::add_op`].
    pub fn add_orient_op(
        &self,
        edit: &mut SchemaEdit<'_>,
        precision: XformOpPrecision,
    ) -> Result<XformOpEdit, XformOpError> {
        self.add_op(edit, XformOpType::Orient, precision, None, false)
    }

    /// Adds a `transform` op, always `matrix4d` ([`XformableEdit::add_op`]).
    ///
    /// # Errors
    ///
    /// As for [`XformableEdit::add_op`].
    pub fn add_transform_op(&self, edit: &mut SchemaEdit<'_>) -> Result<XformOpEdit, XformOpError> {
        self.add_op(
            edit,
            XformOpType::Transform,
            XformOpPrecision::Double,
            None,
            false,
        )
    }

    /// Authors an empty `xformOpOrder`: the prim's local transform is the
    /// identity, and it no longer resets the transform stack. Its op
    /// attributes are kept.
    ///
    /// OpenUSD: `UsdGeomXformable::ClearXformOpOrder`.
    pub fn clear_xform_op_order(&self, edit: &mut SchemaEdit<'_>) -> &Self {
        self.set_order(edit, &[]);
        self
    }

    /// Makes the prim's transform ignore its ancestors' (`!resetXformStack!`
    /// first in `xformOpOrder`), or not (every op before the last reset is
    /// dropped with it). Nothing is authored when the order already says
    /// so.
    ///
    /// OpenUSD: `UsdGeomXformable::SetResetXformStack`.
    pub fn set_reset_xform_stack(&self, edit: &mut SchemaEdit<'_>, reset: bool) -> &Self {
        let order = self.order(edit);
        let has_reset = order.iter().any(|name| name == RESET_XFORM_STACK);
        if reset && !has_reset {
            let mut new = Vec::with_capacity(order.len() + 1);
            new.push(String::from(RESET_XFORM_STACK));
            new.extend(order);
            self.set_order(edit, &new);
        } else if !reset && has_reset {
            let last = order
                .iter()
                .rposition(|name| name == RESET_XFORM_STACK)
                .unwrap_or(0);
            self.set_order(edit, &order[last + 1..]);
        }
        self
    }
}
