// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Common translation, pivot, Euler rotation and scale authoring and reading,
//! as OpenUSD's `UsdGeomXformCommonAPI` does them.

use crate::gf::{self, Matrix4, Rotation, decomposition};
use crate::xform::{INVERT_PREFIX, RESET_XFORM_STACK, op_matrix};
use crate::{
    SchemaEdit, Time, XformOpPrecision, XformOpType,
    usd_geom::{Xformable, XformableEdit},
};
use alloc::{string::String, vec::Vec};
use layerstack::{PropertyKind, Value};

/// The order in which a common transform applies its Euler rotations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[doc(alias = "UsdGeomXformCommonAPI::RotationOrder")]
pub enum RotationOrder {
    /// X, then Y, then Z.
    #[default]
    Xyz,
    /// X, then Z, then Y.
    Xzy,
    /// Y, then X, then Z.
    Yxz,
    /// Y, then Z, then X.
    Yzx,
    /// Z, then X, then Y.
    Zxy,
    /// Z, then Y, then X.
    Zyx,
}
impl RotationOrder {
    /// The three-axis rotate op type that applies this order
    /// (`rotateXYZ` for [`Self::Xyz`]).
    #[must_use]
    #[doc(alias = "UsdGeomXformCommonAPI::ConvertRotationOrderToOpType")]
    #[doc(alias = "ConvertRotationOrderToOpType")]
    pub fn op_type(self) -> XformOpType {
        match self {
            Self::Xyz => XformOpType::RotateXyz,
            Self::Xzy => XformOpType::RotateXzy,
            Self::Yxz => XformOpType::RotateYxz,
            Self::Yzx => XformOpType::RotateYzx,
            Self::Zxy => XformOpType::RotateZxy,
            Self::Zyx => XformOpType::RotateZyx,
        }
    }
    /// The order a three-axis rotate op type applies; `None` for any other
    /// op type, single-axis rotations and `orient` included.
    #[must_use]
    #[doc(alias = "UsdGeomXformCommonAPI::ConvertOpTypeToRotationOrder")]
    #[doc(alias = "UsdGeomXformCommonAPI::CanConvertOpTypeToRotationOrder")]
    #[doc(alias = "CanConvertOpTypeToRotationOrder")]
    pub fn from_op_type(op_type: XformOpType) -> Option<Self> {
        Some(match op_type {
            XformOpType::RotateXyz => Self::Xyz,
            XformOpType::RotateXzy => Self::Xzy,
            XformOpType::RotateYxz => Self::Yxz,
            XformOpType::RotateYzx => Self::Yzx,
            XformOpType::RotateZxy => Self::Zxy,
            XformOpType::RotateZyx => Self::Zyx,
            _ => return None,
        })
    }
    /// The rotation `degrees` (about X, Y and Z) make in this order, as the
    /// rotate op of [`Self::op_type`] with that value transforms row vectors.
    #[must_use]
    #[doc(alias = "UsdGeomXformCommonAPI::GetRotationTransform")]
    #[doc(alias = "GetRotationTransform")]
    pub fn rotation_matrix(self, degrees: [f32; 3]) -> [[f64; 4]; 4] {
        op_matrix(self.op_type(), &Value::Vec3f(degrees), false)
            .expect("a three-axis rotate takes a float3")
    }
}

/// Values of a common transform stack: translation, pivot, Euler rotation
/// and scale. The stack order is translation, pivot, rotation, scale,
/// inverse pivot; row vectors therefore apply scale and rotation about the
/// pivot first.
#[derive(Clone, Copy, Debug, PartialEq)]
#[doc(alias = "UsdGeomXformCommonAPI")]
pub struct CommonTransform {
    /// Translation, in double precision.
    pub translation: [f64; 3],
    /// Euler angles in degrees, about X/Y/Z respectively.
    pub rotation: [f32; 3],
    /// Scale along X/Y/Z.
    pub scale: [f32; 3],
    /// Pivot for rotation and scale.
    pub pivot: [f32; 3],
    /// Euler rotation order; an existing rotation op must agree.
    pub rotation_order: RotationOrder,
}
impl Default for CommonTransform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: [1.0; 3],
            pivot: [0.0; 3],
            rotation_order: RotationOrder::default(),
        }
    }
}

/// One component of a common transform, to author alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CommonComponent {
    /// The translation (`xformOp:translate`, `double3`).
    #[doc(alias = "UsdGeomXformCommonAPI::SetTranslate")]
    Translate([f64; 3]),
    /// The Euler rotation in degrees (`xformOp:rotate<order>`, `float3`).
    #[doc(alias = "UsdGeomXformCommonAPI::SetRotate")]
    Rotate {
        /// Degrees about X, Y and Z.
        degrees: [f32; 3],
        /// The order; an existing rotation op must agree.
        order: RotationOrder,
    },
    /// The scale (`xformOp:scale`, `float3`).
    #[doc(alias = "UsdGeomXformCommonAPI::SetScale")]
    Scale([f32; 3]),
    /// The pivot (`xformOp:translate:pivot` and its inverse, `float3`).
    #[doc(alias = "UsdGeomXformCommonAPI::SetPivot")]
    Pivot([f32; 3]),
}

/// Where a [`CommonTransformReading`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommonTransformSource {
    /// The prim's ops are a common stack; each value is its op's (identity
    /// for an absent op or one with no value of the op's common type).
    Ops,
    /// The ops are not a common stack, but reduce to one by multiplying
    /// adjacent ops of the same type (paired pivots, split translations or
    /// scales); the values are the reduced stack's.
    Accumulated,
    /// The ops are not a common stack: the values are factored from the
    /// local transform, with no pivot, in [`RotationOrder::Xyz`], which loses
    /// any shear.
    Decomposed,
}

/// A common transform read from a prim, and how it was read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommonTransformReading {
    /// The values.
    pub value: CommonTransform,
    /// Where they came from.
    pub source: CommonTransformSource,
}

/// A common-transform edit refused before authoring anything.
///
/// OpenUSD refuses the same calls (returning `false`, or with a coding
/// error), though it can leave ops it added before the refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommonTransformError {
    /// The ordered ops (after any reset, without entries naming no
    /// attribute) are not a common stack: more than five, out of order,
    /// another op type, an inverse op, or a pivot without its inverse.
    IncompatibleOps,
    /// An existing rotation op cannot change its order through this helper.
    RotationOrderMismatch {
        /// The existing order.
        existing: RotationOrder,
        /// The requested order.
        requested: RotationOrder,
    },
    /// An op to add is already listed in `xformOpOrder` (before a reset, or
    /// naming no attribute).
    OpAlreadyInOrder {
        /// The op, as `xformOpOrder` lists it.
        op: String,
    },
    /// An op's attribute does not have the value type the component is
    /// authored in (`double3` translation, `float3` pivot, rotation and
    /// scale): OpenUSD sets values without converting them.
    IncompatibleAttribute {
        /// The attribute that must be repaired or removed explicitly.
        name: String,
        /// The value type the component needs.
        expected: &'static str,
    },
}
impl core::fmt::Display for CommonTransformError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::IncompatibleOps => f.write_str("transform ops are not a compatible common stack"),
            Self::RotationOrderMismatch {
                existing,
                requested,
            } => write!(
                f,
                "rotation order {existing:?} differs from requested {requested:?}"
            ),
            Self::OpAlreadyInOrder { op } => write!(f, "xformOpOrder already lists {op}"),
            Self::IncompatibleAttribute { name, expected } => {
                write!(f, "{name} must be a {expected} attribute")
            }
        }
    }
}
impl core::error::Error for CommonTransformError {}

const TRANSLATE: &str = "xformOp:translate";
const PIVOT: &str = "xformOp:translate:pivot";
const INVERSE_PIVOT: &str = "!invert!xformOp:translate:pivot";
const SCALE: &str = "xformOp:scale";

/// One op of a prim's ordered ops, as the common stack reads it.
struct OrderedOp<'n> {
    attribute: &'n str,
    op_type: Option<XformOpType>,
    inverse: bool,
}

/// A common stack's ops, present or absent. The pivot is its pair.
#[derive(Clone, Copy, Debug, Default)]
struct CommonOps<'n> {
    translate: bool,
    pivot: bool,
    rotate: Option<(&'n str, RotationOrder)>,
    scale: bool,
}

/// `_GetCommonXformOps` in OpenUSD's `xformCommonAPI.cpp`: at most five
/// ordered ops, each optional but in the order translate, pivot, three-axis
/// rotate (any suffix), scale, inverse pivot, with the pivots paired.
fn common_ops<'n>(ops: &[OrderedOp<'n>]) -> Option<CommonOps<'n>> {
    if ops.len() > 5 {
        return None;
    }
    let mut rest = ops.iter().peekable();
    let mut take = |matches: &dyn Fn(&OrderedOp<'n>) -> bool| rest.next_if(|op| matches(op));
    let translate = take(&|op| op.attribute == TRANSLATE && !op.inverse).is_some();
    let pivot = take(&|op| op.attribute == PIVOT && !op.inverse).is_some();
    let rotate =
        take(&|op| !op.inverse && op.op_type.and_then(RotationOrder::from_op_type).is_some()).map(
            |op| {
                let order = op.op_type.and_then(RotationOrder::from_op_type);
                (op.attribute, order.expect("a three-axis rotate"))
            },
        );
    let scale = take(&|op| op.attribute == SCALE && !op.inverse).is_some();
    let inverse_pivot = take(&|op| op.attribute == PIVOT && op.inverse).is_some();
    (rest.next().is_none() && pivot == inverse_pivot).then_some(CommonOps {
        translate,
        pivot,
        rotate,
        scale,
    })
}

/// The ordered ops of `order`: after its last reset, without entries
/// naming no attribute, as `UsdGeomXformable::GetOrderedXformOps` lists
/// them. `has_attribute` says whether the prim has an attribute.
fn ordered<'n>(
    order: &[&'n str],
    mut has_attribute: impl FnMut(&str) -> bool,
) -> Vec<OrderedOp<'n>> {
    let start = order
        .iter()
        .rposition(|name| *name == RESET_XFORM_STACK)
        .map_or(0, |i| i + 1);
    order[start..]
        .iter()
        .filter_map(|name| {
            let (attribute, inverse) = match name.strip_prefix(INVERT_PREFIX) {
                Some(attribute) => (attribute, true),
                None => (*name, false),
            };
            has_attribute(attribute).then(|| OrderedOp {
                attribute,
                op_type: XformOpType::of_attribute(attribute),
                inverse,
            })
        })
        .collect()
}

/// What one common-transform edit authors.
#[derive(Clone, Copy, Debug, Default)]
struct Request {
    translate: Option<[f64; 3]>,
    pivot: Option<[f32; 3]>,
    rotate: Option<([f32; 3], RotationOrder)>,
    scale: Option<[f32; 3]>,
}

impl XformableEdit {
    /// Authors a common translation, pivot, rotation and scale stack at the
    /// default time, adding the ops a compatible stack lacks.
    ///
    /// As OpenUSD's `UsdGeomXformCommonAPI::SetXformVectors`: the ordered
    /// ops (after any reset, leaving out entries naming no attribute) must
    /// be a common stack, and an existing rotation op must have the
    /// requested order. Missing ops are added (translation in `double3`,
    /// the others in `float3`) and, when any is, `xformOpOrder` becomes the
    /// common stack, keeping the reset. Values are not converted: an
    /// existing op must already hold the component's value type. It neither
    /// decomposes matrices nor converts other stacks.
    ///
    /// # Errors
    /// [`CommonTransformError`] where OpenUSD refuses the call; nothing is
    /// authored.
    #[doc(alias = "UsdGeomXformCommonAPI::SetXformVectors")]
    #[doc(alias = "SetXformVectors")]
    pub fn set_common_transform(
        &self,
        edit: &mut SchemaEdit<'_>,
        value: &CommonTransform,
    ) -> Result<&Self, CommonTransformError> {
        self.write_common(edit, Request::all(value), None)
    }

    /// [`Self::set_common_transform`] authoring values at a numeric time.
    /// Op declarations and `xformOpOrder` remain uniform.
    ///
    /// # Errors
    /// As for [`Self::set_common_transform`], before authoring anything.
    #[doc(alias = "UsdGeomXformCommonAPI::SetXformVectors")]
    #[doc(alias = "SetXformVectors")]
    pub fn set_common_transform_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: &CommonTransform,
    ) -> Result<&Self, CommonTransformError> {
        self.write_common(edit, Request::all(value), Some(time))
    }

    /// Authors one component of a common stack at the default time, adding
    /// only its op (both pivot ops for a pivot) when the stack lacks it. A
    /// rotation's order must agree with an existing rotation op; the other
    /// components leave any rotation as it is.
    ///
    /// OpenUSD: `UsdGeomXformCommonAPI::SetTranslate`, `SetRotate`,
    /// `SetScale` and `SetPivot`.
    ///
    /// # Errors
    /// As for [`Self::set_common_transform`].
    #[doc(alias = "UsdGeomXformCommonAPI::SetTranslate")]
    #[doc(alias = "UsdGeomXformCommonAPI::SetRotate")]
    #[doc(alias = "UsdGeomXformCommonAPI::SetScale")]
    #[doc(alias = "UsdGeomXformCommonAPI::SetPivot")]
    pub fn set_common_component(
        &self,
        edit: &mut SchemaEdit<'_>,
        component: CommonComponent,
    ) -> Result<&Self, CommonTransformError> {
        self.write_common(edit, Request::one(component), None)
    }

    /// [`Self::set_common_component`] authoring its value at a numeric time.
    ///
    /// # Errors
    /// As for [`Self::set_common_transform`].
    #[doc(alias = "UsdGeomXformCommonAPI::SetTranslate")]
    #[doc(alias = "UsdGeomXformCommonAPI::SetRotate")]
    #[doc(alias = "UsdGeomXformCommonAPI::SetScale")]
    #[doc(alias = "UsdGeomXformCommonAPI::SetPivot")]
    pub fn set_common_component_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        component: CommonComponent,
    ) -> Result<&Self, CommonTransformError> {
        self.write_common(edit, Request::one(component), Some(time))
    }

    /// The ordered ops of `order` as the edit leaves the prim.
    fn edit_ordered<'n>(&self, edit: &mut SchemaEdit<'_>, order: &[&'n str]) -> Vec<OrderedOp<'n>> {
        ordered(order, |attribute| {
            edit.property_kind(self.path(), attribute) == Some(PropertyKind::Attribute)
        })
    }

    // `_GetOrAddCommonXformOps`, then `UsdGeomXformOp::Set` of each value.
    fn write_common(
        &self,
        edit: &mut SchemaEdit<'_>,
        request: Request,
        time: Option<f64>,
    ) -> Result<&Self, CommonTransformError> {
        let order = self.order(edit);
        let names: Vec<&str> = order.iter().map(String::as_str).collect();
        let ops = self.edit_ordered(edit, &names);
        let existing = common_ops(&ops).ok_or(CommonTransformError::IncompatibleOps)?;
        if let (Some((_, requested)), Some((_, existing))) = (request.rotate, existing.rotate)
            && requested != existing
        {
            return Err(CommonTransformError::RotationOrderMismatch {
                existing,
                requested,
            });
        }
        let rotate_name = existing
            .rotate
            .map(|(attribute, _)| String::from(attribute))
            .or_else(|| {
                request
                    .rotate
                    .map(|(_, order)| order.op_type().attribute_name(None))
            });
        // Each op to add must be new to `xformOpOrder` (`AddXformOp`).
        let mut added: Vec<&str> = Vec::new();
        if request.translate.is_some() && !existing.translate {
            added.push(TRANSLATE);
        }
        if request.pivot.is_some() && !existing.pivot {
            added.extend([PIVOT, INVERSE_PIVOT]);
        }
        if request.rotate.is_some() && existing.rotate.is_none() {
            added.push(rotate_name.as_deref().expect("a requested rotation"));
        }
        if request.scale.is_some() && !existing.scale {
            added.push(SCALE);
        }
        if let Some(op) = added.iter().find(|op| names.contains(op)) {
            return Err(CommonTransformError::OpAlreadyInOrder {
                op: String::from(*op),
            });
        }
        // `UsdAttribute::Set` refuses a value of another type.
        let mut checks: Vec<(&str, &'static str)> = Vec::new();
        if request.translate.is_some() {
            checks.push((TRANSLATE, "double3"));
        }
        if request.pivot.is_some() {
            checks.push((PIVOT, "float3"));
        }
        if request.rotate.is_some() {
            checks.push((
                rotate_name.as_deref().expect("a requested rotation"),
                "float3",
            ));
        }
        if request.scale.is_some() {
            checks.push((SCALE, "float3"));
        }
        for (name, expected) in checks {
            let found = edit.attribute_type(self.path(), name);
            let declared = edit.property_kind(self.path(), name).is_some();
            if found.as_ref().is_some_and(|ty| ty.type_name() != expected)
                || (found.is_none() && declared)
            {
                return Err(CommonTransformError::IncompatibleAttribute {
                    name: name.into(),
                    expected,
                });
            }
        }
        if !added.is_empty() {
            for &name in &added {
                let (op_type, suffix, precision) = match name {
                    TRANSLATE => (XformOpType::Translate, None, XformOpPrecision::Double),
                    PIVOT | INVERSE_PIVOT => (
                        XformOpType::Translate,
                        Some("pivot"),
                        XformOpPrecision::Float,
                    ),
                    SCALE => (XformOpType::Scale, None, XformOpPrecision::Float),
                    rotate => (
                        XformOpType::of_attribute(rotate).expect("a rotate op"),
                        rotate.splitn(3, ':').nth(2),
                        XformOpPrecision::Float,
                    ),
                };
                if name != INVERSE_PIVOT {
                    self.ensure_op_attribute(edit, op_type, precision, suffix);
                }
            }
            // `SetXformOpOrder(ops, resetXformStack)`: the common stack alone.
            let reset = names.contains(&RESET_XFORM_STACK);
            let has = |name: &str, before: bool| before || added.contains(&name);
            let mut stack: Vec<String> = Vec::with_capacity(6);
            if reset {
                stack.push(RESET_XFORM_STACK.into());
            }
            if has(TRANSLATE, existing.translate) {
                stack.push(TRANSLATE.into());
            }
            if has(PIVOT, existing.pivot) {
                stack.push(PIVOT.into());
            }
            if let Some(rotate) = rotate_name
                .as_deref()
                .filter(|r| has(r, existing.rotate.is_some()))
            {
                stack.push(rotate.into());
            }
            if has(SCALE, existing.scale) {
                stack.push(SCALE.into());
            }
            if has(PIVOT, existing.pivot) {
                stack.push(INVERSE_PIVOT.into());
            }
            self.set_order(edit, &stack);
        }
        let mut set = |name: &str, value: Value| edit.set_value(self.path(), name, time, value);
        if let Some(value) = request.translate {
            set(TRANSLATE, Value::Vec3d(value));
        }
        if let Some((degrees, _)) = request.rotate {
            set(
                rotate_name.as_deref().expect("a requested rotation"),
                Value::Vec3f(degrees),
            );
        }
        if let Some(value) = request.scale {
            set(SCALE, Value::Vec3f(value));
        }
        if let Some(value) = request.pivot {
            set(PIVOT, Value::Vec3f(value));
        }
        Ok(self)
    }
}

impl Request {
    fn all(value: &CommonTransform) -> Self {
        Self {
            translate: Some(value.translation),
            pivot: Some(value.pivot),
            rotate: Some((value.rotation, value.rotation_order)),
            scale: Some(value.scale),
        }
    }
    fn one(component: CommonComponent) -> Self {
        let mut request = Self::default();
        match component {
            CommonComponent::Translate(value) => request.translate = Some(value),
            CommonComponent::Rotate { degrees, order } => request.rotate = Some((degrees, order)),
            CommonComponent::Scale(value) => request.scale = Some(value),
            CommonComponent::Pivot(value) => request.pivot = Some(value),
        }
        request
    }
}

impl<'a> Xformable<'a> {
    /// Whether the prim's ordered ops (after any reset, without entries
    /// naming no attribute) are a common translation, pivot, rotation and
    /// scale stack, which [`XformableEdit::set_common_transform`] can author.
    ///
    /// OpenUSD: a valid `UsdGeomXformCommonAPI` (`_IsCompatible`).
    #[must_use]
    #[doc(alias = "UsdGeomXformCommonAPI::_IsCompatible")]
    pub fn is_common_transform_compatible(&self) -> bool {
        common_ops(&self.common_ordered()).is_some()
    }

    fn common_ordered(&self) -> Vec<OrderedOp<'a>> {
        let order = self.xform_op_order().unwrap_or_default();
        ordered(&order, |attribute| self.has_attribute(attribute))
    }

    /// The prim's common transform at `time`.
    ///
    /// As OpenUSD's `UsdGeomXformCommonAPI::GetXformVectors`: with a common
    /// stack, each component is its op's value, read without conversion (a
    /// `double3` translation, `float3` rotation and scale, a `double3` or
    /// `float3` pivot) and the identity when the op is absent or holds
    /// another type; the order is the rotation op's. Otherwise the local
    /// transform is factored into translation, `XYZ` rotation and scale,
    /// with no pivot ([`CommonTransformSource::Decomposed`]). The reset
    /// flag is the caller's to read.
    #[must_use]
    #[doc(alias = "UsdGeomXformCommonAPI::GetXformVectors")]
    #[doc(alias = "GetXformVectors")]
    pub fn common_transform(&self, time: Time) -> CommonTransformReading {
        let ops = self.common_ordered();
        let Some(common) = common_ops(&ops) else {
            return CommonTransformReading {
                value: decompose(&self.local_transform(time).matrix),
                source: CommonTransformSource::Decomposed,
            };
        };
        let double = |name: &str| match self.raw_value(name, time) {
            Some(Value::Vec3d(v)) => Some(v),
            _ => None,
        };
        let float = |name: &str| match self.raw_value(name, time) {
            Some(Value::Vec3f(v)) => Some(v),
            _ => None,
        };
        let value = CommonTransform {
            translation: common
                .translate
                .then(|| double(TRANSLATE))
                .flatten()
                .unwrap_or([0.0; 3]),
            rotation: common
                .rotate
                .and_then(|(attribute, _)| float(attribute))
                .unwrap_or([0.0; 3]),
            scale: common
                .scale
                .then(|| float(SCALE))
                .flatten()
                .unwrap_or([1.0; 3]),
            pivot: common
                .pivot
                .then(|| double(PIVOT).map(single).or_else(|| float(PIVOT)))
                .flatten()
                .unwrap_or([0.0; 3]),
            rotation_order: common.rotate.map_or(RotationOrder::Xyz, |(_, order)| order),
        };
        CommonTransformReading {
            value,
            source: CommonTransformSource::Ops,
        }
    }

    /// The prim's common transform at `time`, reducing a stack that is not a
    /// common one by multiplying adjacent ops of one type: split
    /// translations or scales, and pivot pairs (separate rotation and
    /// scale pivots at one place included) whose products cancel.
    ///
    /// As OpenUSD's `GetXformVectorsByAccumulation`: a common stack reads as
    /// [`Self::common_transform`]; a reducible one reads its reduced values
    /// ([`CommonTransformSource::Accumulated`]), the rotation decomposed in
    /// `XYZ` angles though its order is the rotation op's; any other falls
    /// back to [`Self::common_transform`]'s factoring.
    #[must_use]
    #[doc(alias = "UsdGeomXformCommonAPI::GetXformVectorsByAccumulation")]
    #[doc(alias = "GetXformVectorsByAccumulation")]
    pub fn common_transform_by_accumulation(&self, time: Time) -> CommonTransformReading {
        let ops = self.common_ordered();
        if common_ops(&ops).is_some() {
            return self.common_transform(time);
        }
        let is_rotate = |op: &OrderedOp<'_>| op.op_type.filter(|ty| is_rotate(*ty));
        // `_GetRotateOpType` takes the first rotate op's type, and
        // `_GetCommonOpTypesForOpOrder` the last's.
        let rotate_type = ops
            .iter()
            .find_map(is_rotate)
            .unwrap_or(XformOpType::RotateXyz);
        let last_rotate = ops.iter().rev().find_map(is_rotate);
        let has_scale = ops.iter().any(|op| op.op_type == Some(XformOpType::Scale));
        let inverse_translates = ops
            .iter()
            .filter(|op| op.op_type == Some(XformOpType::Translate) && op.inverse)
            .count();
        let mut types = alloc::vec![XformOpType::Translate, XformOpType::Translate];
        let (translate, pivot) = (0, 1);
        let rotate = last_rotate.map(|ty| {
            types.push(ty);
            types.len() - 1
        });
        let identity = (inverse_translates > 1).then(|| {
            types.push(XformOpType::Translate);
            types.len() - 1
        });
        let scale = has_scale.then(|| {
            types.push(XformOpType::Scale);
            types.len() - 1
        });
        types.push(XformOpType::Translate);
        let inverse_pivot = types.len() - 1;
        let mut matrices = alloc::vec![gf::IDENTITY; types.len()];
        // Scan backwards, so the inverse pivot accumulates first and shows
        // where the leading translations split into pivot and translation.
        let mut op_index = ops.len();
        let mut type_index = types.len();
        while op_index > 0 && type_index > translate {
            let op = &ops[op_index - 1];
            let ty = types[type_index - 1];
            if op.op_type != Some(ty) {
                type_index -= 1;
                continue;
            }
            matrices[type_index - 1] =
                gf::mul(&matrices[type_index - 1], &self.op_transform(op, time));
            op_index -= 1;
            // A rotation never accumulates; a translation moves on at an
            // inverse op, or at a pivot cancelled by the inverse pivot.
            let advance = ty == rotate_type
                || (ty == XformOpType::Translate
                    && (op.inverse
                        || (type_index - 1 == pivot
                            && inverses(&matrices[pivot], &matrices[inverse_pivot]))));
            if advance {
                type_index -= 1;
            }
        }
        let mut reducible = op_index == 0;
        if identity.is_some_and(|i| !is_identity(&matrices[i])) {
            reducible = false;
        }
        // Translations alone accumulate in the inverse pivot's place.
        if type_index == inverse_pivot + 1 {
            matrices[translate] = matrices[inverse_pivot];
            matrices[inverse_pivot] = gf::IDENTITY;
        }
        if !inverses(&matrices[pivot], &matrices[inverse_pivot]) {
            reducible = false;
        }
        if !reducible {
            return self.common_transform(time);
        }
        let row = |m: &Matrix4| [m[3][0], m[3][1], m[3][2]];
        let value = CommonTransform {
            translation: row(&matrices[translate]),
            pivot: single(row(&matrices[pivot])),
            rotation: rotate.map_or([0.0; 3], |i| {
                single(extract_rotation(&matrices[i]).decompose(gf::AXES))
            }),
            scale: scale.map_or([1.0; 3], |i| {
                let m = &matrices[i];
                single([m[0][0], m[1][1], m[2][2]])
            }),
            rotation_order: RotationOrder::from_op_type(rotate_type).unwrap_or_default(),
        };
        CommonTransformReading {
            value,
            source: CommonTransformSource::Accumulated,
        }
    }

    /// `UsdGeomXformOp::GetOpTransform(time)`: the identity when the op has
    /// no value of its type.
    fn op_transform(&self, op: &OrderedOp<'_>, time: Time) -> Matrix4 {
        op.op_type
            .zip(self.raw_value(op.attribute, time))
            .and_then(|(ty, value)| op_matrix(ty, &value, op.inverse).ok())
            .unwrap_or(gf::IDENTITY)
    }
}

fn is_rotate(ty: XformOpType) -> bool {
    matches!(
        ty,
        XformOpType::RotateX | XformOpType::RotateY | XformOpType::RotateZ
    ) || RotationOrder::from_op_type(ty).is_some()
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "common rotations, scales and pivots are float3, as OpenUSD's GfVec3f"
)]
fn single(v: [f64; 3]) -> [f32; 3] {
    v.map(|c| c as f32)
}

/// `_IsMatrixIdentity`: each row within `1e-6` of the identity's.
fn is_identity(m: &Matrix4) -> bool {
    m.iter().zip(gf::IDENTITY).all(|(row, identity)| {
        let delta: f64 = row
            .iter()
            .zip(identity)
            .map(|(a, b)| (a - b) * (a - b))
            .sum();
        delta <= 1e-6 * 1e-6
    })
}

/// `_MatricesAreInverses`.
fn inverses(a: &Matrix4, b: &Matrix4) -> bool {
    is_identity(&gf::mul(a, b))
}

/// `GfMatrix4d::ExtractRotation` of a matrix with a unit homogeneous corner.
fn extract_rotation(m: &Matrix4) -> Rotation {
    let upper = core::array::from_fn(|i| core::array::from_fn(|j| m[i][j]));
    let [r, x, y, z] = decomposition::quaternion(&upper);
    Rotation::from_quat([x, y, z, r])
}

/// `_ConvertMatrixToComponents`: `GfMatrix4d::Factor`'s scale, its rotation
/// orthonormalized and decomposed about Z, Y then X, and the translation.
fn decompose(m: &Matrix4) -> CommonTransform {
    let upper = core::array::from_fn(|i| core::array::from_fn(|j| m[i][j]));
    let (_, scale, rotation, _) = decomposition::factor(&upper, 1e-10);
    let rotation = decomposition::orthonormalize(rotation);
    let mut embedded = gf::IDENTITY;
    for (row, values) in embedded.iter_mut().zip(rotation) {
        row[..3].copy_from_slice(&values);
    }
    let [x, y, z] = gf::AXES;
    let [about_z, about_y, about_x] = extract_rotation(&embedded).decompose([z, y, x]);
    CommonTransform {
        translation: [m[3][0], m[3][1], m[3][2]],
        rotation: single([about_x, about_y, about_z]),
        scale: single(scale),
        pivot: [0.0; 3],
        rotation_order: RotationOrder::Xyz,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usd_geom::Xform;
    use alloc::sync::Arc;
    use layerstack::{
        InMemoryStore, Layer, LayerId, LiveStage, PropertyType, StageOptions, Value,
        edit::EditTarget,
    };

    #[test]
    fn malformed_common_stacks_are_rejected_before_authoring() {
        for case in 0..4 {
            let mut store = InMemoryStore::default();
            store.insert_layer(Layer::new(LayerId(1)));
            let path = store.path("/Rig");
            let options = StageOptions {
                schemas: Some(Arc::new(crate::openusd(&store.tokens))),
                ..StageOptions::default()
            };
            let live = LiveStage::compose(&mut store, LayerId(1), options);
            let mut edit =
                SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
            Xform::define(&mut edit, path).expect("outside any instance");
            let xform = XformableEdit::new(&edit, path)
                .expect("outside any instance")
                .unwrap();
            match case {
                0 => {
                    xform
                        .add_op(
                            &mut edit,
                            XformOpType::RotateXyz,
                            XformOpPrecision::Float,
                            None,
                            true,
                        )
                        .unwrap();
                }
                1 => {
                    xform.add_transform_op(&mut edit).unwrap();
                }
                2 => {
                    edit.create_attribute(
                        path,
                        "xformOp:scale",
                        PropertyType::parse("float3[]").unwrap(),
                    );
                    xform.set_xform_op_order(&mut edit, &["xformOp:scale"]);
                }
                _ => {
                    xform.set_xform_op_order(&mut edit, &["xformOp:translate"]);
                }
            }
            let before = edit.transaction().clone();
            assert!(
                xform
                    .set_common_transform(&mut edit, &CommonTransform::default())
                    .is_err()
            );
            assert_eq!(edit.transaction(), &before);
        }
    }

    #[test]
    fn other_vector_precisions_are_refused_as_openusd_sets_without_converting() {
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(LayerId(1)));
        let path = store.path("/Rig");
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&store.tokens))),
            ..StageOptions::default()
        };
        let live = LiveStage::compose(&mut store, LayerId(1), options);
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        Xform::define(&mut edit, path).expect("outside any instance");
        let xform = XformableEdit::new(&edit, path)
            .expect("outside any instance")
            .unwrap();
        xform
            .add_translate_op(&mut edit, XformOpPrecision::Half)
            .unwrap();
        xform
            .add_scale_op(&mut edit, XformOpPrecision::Double)
            .unwrap();
        let before = edit.transaction().clone();
        assert_eq!(
            xform.set_common_component(&mut edit, CommonComponent::Translate([1.0, 2.0, 3.0])),
            Err(CommonTransformError::IncompatibleAttribute {
                name: "xformOp:translate".into(),
                expected: "double3",
            })
        );
        assert_eq!(
            xform.set_common_component(&mut edit, CommonComponent::Scale([2.0; 3])),
            Err(CommonTransformError::IncompatibleAttribute {
                name: "xformOp:scale".into(),
                expected: "float3",
            })
        );
        assert_eq!(edit.transaction(), &before);
        xform
            .set_common_component(&mut edit, CommonComponent::Pivot([1.0; 3]))
            .unwrap();
        assert_eq!(
            edit.default_value(path, "xformOp:translate:pivot"),
            Some(Value::Vec3f([1.0; 3]))
        );
    }

    #[test]
    fn rotation_orders_convert_and_build_their_rotations() {
        for order in [
            RotationOrder::Xyz,
            RotationOrder::Xzy,
            RotationOrder::Yxz,
            RotationOrder::Yzx,
            RotationOrder::Zxy,
            RotationOrder::Zyx,
        ] {
            assert_eq!(RotationOrder::from_op_type(order.op_type()), Some(order));
        }
        assert_eq!(RotationOrder::from_op_type(XformOpType::RotateX), None);
        assert_eq!(RotationOrder::from_op_type(XformOpType::Orient), None);
        let quarter = RotationOrder::Xyz.rotation_matrix([0.0, 0.0, 90.0]);
        assert!((quarter[0][1] - 1.0).abs() < 1e-12, "X turns toward Y");
    }

    #[test]
    fn decomposition_recovers_scale_rotation_and_translation() {
        let rotation = RotationOrder::Xyz.rotation_matrix([10.0, 20.0, 30.0]);
        let mut matrix = gf::mul(&gf::scale([2.0, 3.0, 4.0]), &rotation);
        matrix[3] = [1.0, 2.0, 3.0, 1.0];
        let value = decompose(&matrix);
        assert_eq!(value.translation, [1.0, 2.0, 3.0]);
        for (got, want) in value.rotation.iter().zip([10.0, 20.0, 30.0]) {
            assert!((got - want).abs() < 1e-4, "{:?}", value.rotation);
        }
        for (got, want) in value.scale.iter().zip([2.0, 3.0, 4.0]) {
            assert!((got - want).abs() < 1e-5, "{:?}", value.scale);
        }
    }
}
