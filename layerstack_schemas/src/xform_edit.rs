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

use layerstack::{
    AuthoringInspectionError, AuthoringSource, PathId, PropertyType, Time, Value, half,
};

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
    let name = match (op_type, precision) {
        (T::Transform, _) => "matrix4d",
        (T::Orient, P::Double) => "quatd",
        (T::Orient, P::Float) => "quatf",
        (T::Orient, P::Half) => "quath",
        (op, precision) if op.is_scalar() => match precision {
            P::Double => "double",
            P::Float => "float",
            P::Half => "half",
        },
        (_, P::Double) => "double3",
        (_, P::Float) => "float3",
        (_, P::Half) => "half3",
    };
    PropertyType::parse(name).expect("a registered value type")
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

/// A compound matrix replacement could not be authored as a contributing edit.
/// Rejection leaves the collected transaction and its schema overlay unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MatrixXformError {
    /// The prospective destination could not be inspected.
    Inspection(AuthoringInspectionError),
    /// The mapped edit target is not in the composed prim's active sources.
    InactiveTarget,
    /// A stronger value-bearing source can mask or alter the replacement.
    StrongerSource {
        /// `xformOpOrder` or `xformOp:transform`.
        attribute: &'static str,
        /// Stronger authoring site and slot, retaining diagnostic provenance.
        source: AuthoringSource,
    },
    /// An existing declaration cannot represent the replacement.
    IncompatibleType {
        /// The attribute with the incompatible declaration.
        attribute: &'static str,
        /// Its existing USD type name.
        type_name: String,
    },
    /// The supplied matrix contains a nonfinite element.
    NonfiniteMatrix,
}

impl fmt::Display for MatrixXformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection(error) => error.fmt(f),
            Self::InactiveTarget => f.write_str("matrix edit target does not contribute"),
            Self::StrongerSource { attribute, source } => write!(
                f,
                "{attribute} has a stronger source in {:?} at {:?}",
                source.key.layer_id, source.key.spec_path
            ),
            Self::IncompatibleType {
                attribute,
                type_name,
            } => write!(f, "{attribute} has incompatible type {type_name}"),
            Self::NonfiniteMatrix => f.write_str("matrix transform requires finite elements"),
        }
    }
}

impl core::error::Error for MatrixXformError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Inspection(error) => Some(error),
            _ => None,
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
    /// Replaces the local transform stack with one double-precision matrix op.
    ///
    /// Preflights both `xformOpOrder` and `xformOp:transform` at this edit target,
    /// then authors the order, declaration and matrix in one rollback group.
    /// Existing op attributes remain available but no longer appear in the order.
    /// Only the matrix default changes; existing numeric samples are retained.
    /// Requires a composed prim; newly collected definitions must be applied first.
    ///
    /// This deliberately rejects any stronger value-bearing source, including
    /// sparse sources that could alter the proposed result. Raw op/order setters
    /// remain available for deliberate masked authoring. It accepts only
    /// where [`Self::make_matrix_xform`] (OpenUSD's `MakeMatrixXform`) does,
    /// and then authors what it and setting its op author; it also refuses
    /// a stronger `xformOp:transform`, which would mask the matrix.
    ///
    /// AOUSD Core §12.3 (value strength), §13.3.1 (transformable prims).
    ///
    /// # Errors
    /// Returns [`MatrixXformError`] for an inactive or stronger destination,
    /// incompatible existing declarations, or nonfinite matrix elements.
    pub fn set_matrix_transform(
        &self,
        edit: &mut SchemaEdit<'_>,
        matrix: [[f64; 4]; 4],
    ) -> Result<XformOpEdit, MatrixXformError> {
        self.set_matrix_transform_impl(edit, None, matrix)
    }

    /// As [`Self::set_matrix_transform`], authoring the matrix at stage `time`.
    /// The uniform order is always authored and inspected at default time.
    /// Replaces one sample and retains the default and other authored samples.
    ///
    /// # Errors
    /// As [`Self::set_matrix_transform`]; nonfinite times are rejected too.
    pub fn set_matrix_transform_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        matrix: [[f64; 4]; 4],
    ) -> Result<XformOpEdit, MatrixXformError> {
        self.set_matrix_transform_impl(edit, Some(time), matrix)
    }

    fn set_matrix_transform_impl(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: Option<f64>,
        matrix: [[f64; 4]; 4],
    ) -> Result<XformOpEdit, MatrixXformError> {
        if matrix.iter().flatten().any(|element| !element.is_finite()) {
            return Err(MatrixXformError::NonfiniteMatrix);
        }
        const TRANSFORM: &str = "xformOp:transform";
        edit.group(|edit| {
            for (attribute, query, expected) in [
                (Xformable::XFORM_OP_ORDER, Time::Default, "token[]"),
                (TRANSFORM, time.map_or(Time::Default, Time::at), "matrix4d"),
            ] {
                let inspection = edit
                    .inspect_authoring(self.path(), attribute, query)
                    .map_err(MatrixXformError::Inspection)?;
                if inspection.target.is_none() {
                    return Err(MatrixXformError::InactiveTarget);
                }
                if let Some(source) = inspection.stronger_sources.into_iter().next() {
                    return Err(MatrixXformError::StrongerSource { attribute, source });
                }
                match edit.attribute_type(self.path(), attribute) {
                    Some(ty) if ty.type_name() != expected => {
                        return Err(MatrixXformError::IncompatibleType {
                            attribute,
                            type_name: ty.type_name().to_string(),
                        });
                    }
                    None if edit.property_kind(self.path(), attribute).is_some() => {
                        return Err(MatrixXformError::IncompatibleType {
                            attribute,
                            type_name: "untyped attribute or relationship".into(),
                        });
                    }
                    _ => {}
                }
            }
            if edit.attribute_type(self.path(), TRANSFORM).is_none() {
                edit.create_attribute(
                    self.path(),
                    TRANSFORM,
                    value_type(XformOpType::Transform, XformOpPrecision::Double),
                );
            }
            self.set_order(edit, &[TRANSFORM.into()]);
            let value = Value::Matrix4d(alloc::boxed::Box::new(core::array::from_fn(|i| {
                matrix[i / 4][i % 4]
            })));
            edit.set_value(self.path(), TRANSFORM, time, value);
            Ok(XformOpEdit {
                path: self.path(),
                attribute: TRANSFORM.into(),
                op_type: XformOpType::Transform,
                precision: XformOpPrecision::Double,
                inverse: false,
            })
        })
    }

    /// Creates the attribute of an op of `op_type` with `suffix` in
    /// `precision`, unless the prim has it, as `UsdGeomXformOp`'s
    /// constructor does for `AddXformOp`. The order is not changed.
    pub(crate) fn ensure_op_attribute(
        &self,
        edit: &mut SchemaEdit<'_>,
        op_type: XformOpType,
        precision: XformOpPrecision,
        suffix: Option<&str>,
    ) {
        let attribute = op_type.attribute_name(suffix);
        if edit.attribute_type(self.path(), &attribute).is_none() {
            edit.create_attribute(self.path(), &attribute, value_type(op_type, precision));
        }
    }

    /// Replaces the local transform stack with one `transform` op, as
    /// OpenUSD's `UsdGeomXformable::MakeMatrixXform` does, and returns that
    /// op to set the matrix through.
    ///
    /// `xformOpOrder` is cleared at the edit target, then the op is added
    /// to the order that then composes: `[xformOp:transform]`, or a stronger
    /// order with it appended when that order has no ops after its reset.
    /// The op attribute is created as `matrix4d` unless the prim has it.
    /// Existing op attributes are kept, out of the order. Unlike
    /// [`Self::set_matrix_transform`], a stronger `xformOp:transform` value
    /// is not refused, and the value is the caller's to set.
    ///
    /// # Errors
    /// Nothing is authored, where OpenUSD refuses (it can leave the cleared
    /// order behind): [`MatrixXformError::StrongerSource`] when a stronger
    /// `xformOpOrder` still has ops after the clear (`MakeMatrixXform`'s
    /// "Could not clear xformOpOrder"), [`MatrixXformError::IncompatibleType`]
    /// when `xformOp:transform` exists with another type or the stronger
    /// order already lists it, [`MatrixXformError::InactiveTarget`] and
    /// [`MatrixXformError::Inspection`] when the target cannot be inspected
    /// or does not contribute.
    #[doc(alias = "UsdGeomXformable::MakeMatrixXform")]
    #[doc(alias = "MakeMatrixXform")]
    pub fn make_matrix_xform(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<XformOpEdit, MatrixXformError> {
        const TRANSFORM: &str = "xformOp:transform";
        let inspection = edit
            .inspect_authoring(self.path(), Xformable::XFORM_OP_ORDER, Time::Default)
            .map_err(MatrixXformError::Inspection)?;
        if inspection.target.is_none() {
            return Err(MatrixXformError::InactiveTarget);
        }
        // The order after clearing the target's: a stronger one, else none.
        let mut order = if inspection.stronger_sources.is_empty() {
            Vec::new()
        } else {
            let scene = edit.scene();
            Xformable::new(&scene, self.path())
                .and_then(|prim| prim.xform_op_order())
                .unwrap_or_default()
                .into_iter()
                .map(String::from)
                .collect()
        };
        let reset = order
            .iter()
            .rposition(|name| name == RESET_XFORM_STACK)
            .map_or(0, |i| i + 1);
        let has_attribute = |edit: &mut SchemaEdit<'_>, name: &str| {
            let attribute = name.strip_prefix(INVERT_PREFIX).unwrap_or(name);
            edit.property_kind(self.path(), attribute).is_some()
        };
        if let Some(source) = inspection.stronger_sources.into_iter().next()
            && order[reset..].iter().any(|name| has_attribute(edit, name))
        {
            return Err(MatrixXformError::StrongerSource {
                attribute: Xformable::XFORM_OP_ORDER,
                source,
            });
        }
        let existing = edit.attribute_type(self.path(), TRANSFORM);
        if existing
            .as_ref()
            .is_some_and(|ty| ty.type_name() != "matrix4d")
            || (existing.is_none() && edit.property_kind(self.path(), TRANSFORM).is_some())
            || order.iter().any(|name| name == TRANSFORM)
        {
            return Err(MatrixXformError::IncompatibleType {
                attribute: TRANSFORM,
                type_name: existing.map_or_else(
                    || String::from("untyped attribute, relationship or listed op"),
                    |ty| ty.type_name().to_string(),
                ),
            });
        }
        self.ensure_op_attribute(edit, XformOpType::Transform, XformOpPrecision::Double, None);
        order.push(TRANSFORM.into());
        self.set_order(edit, &order);
        Ok(XformOpEdit {
            path: self.path(),
            attribute: TRANSFORM.into(),
            op_type: XformOpType::Transform,
            precision: XformOpPrecision::Double,
            inverse: false,
        })
    }

    /// The prim's `xformOpOrder` as the edit leaves it.
    pub(crate) fn order(&self, edit: &mut SchemaEdit<'_>) -> Vec<String> {
        let tokens = |edit: &mut SchemaEdit<'_>, value: Value| match value.array_ref() {
            Some(items) => items
                .iter()
                .filter_map(|item| match *item {
                    Value::Token(token) => Some(edit.tokens().resolve(token).to_string()),
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        };
        edit.default_value(self.path(), Xformable::XFORM_OP_ORDER)
            .map(|value| tokens(edit, value))
            .unwrap_or_default()
    }

    pub(crate) fn set_order(&self, edit: &mut SchemaEdit<'_>, order: &[String]) {
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
            Some(existing) => XformOpPrecision::of_type(existing.type_name()).unwrap_or(precision),
            None => {
                let ty = value_type(op_type, precision);
                let precision = XformOpPrecision::of_type(ty.type_name()).unwrap_or(precision);
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
            XformOpPrecision::of_type(edit.attribute_type(self.path(), &attribute)?.type_name())?;
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
