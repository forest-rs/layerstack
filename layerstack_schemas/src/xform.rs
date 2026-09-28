// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transforms: a prim's local transform from its `xformOpOrder`, and the
//! transforms down namespace to the world.
//!
//! Matrices are `[[f64; 4]; 4]` rows (`m[row][column]`) that transform row
//! vectors (`p' = p * M`), as USD's are: the translation is the last row,
//! and in `A * B` `A` applies first. A prim's local-to-world transform is
//! its local transform times its parent's local-to-world transform, unless
//! its `xformOpOrder` resets the transform stack.
//!
//! Every value is computed as OpenUSD computes it (`UsdGeomXformOp`,
//! `UsdGeomXformable`, `UsdGeomXformCache`, with `GfMatrix4d` and
//! `GfRotation` arithmetic step for step), so results agree with OpenUSD's
//! to within the rounding of the platform's trigonometry and fused
//! multiply-adds.

use alloc::{
    borrow::Cow, boxed::Box, collections::BTreeMap, format, string::String, sync::Arc, vec::Vec,
};
use core::{num::NonZeroUsize, ops::Bound};

use layerstack::{HashMap, Path, PathId, ResolvedValue, TokenId, Value, half};

use crate::gf::{self, Matrix4, Rotation};
use crate::usd_geom::{Imageable, Xformable};
use crate::view::{PrimView, Scene, Time};

/// The `xformOpOrder` entry that makes a prim's transform ignore its
/// ancestors': the ops before it are ignored too.
pub const RESET_XFORM_STACK: &str = "!resetXformStack!";

/// The prefix of an `xformOpOrder` entry that applies an op's inverse
/// (`!invert!xformOp:translate:pivot`).
pub const INVERT_PREFIX: &str = "!invert!";

/// The kind of a transform op, from the second component of its attribute
/// name (`xformOp:rotateXYZ:tilt` is a [`XformOpType::RotateXyz`]).
///
/// OpenUSD: `UsdGeomXformOp::Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum XformOpType {
    /// `translate`: a `double3`, `float3` or `half3` offset.
    Translate,
    /// `translateX`: a scalar offset along X.
    TranslateX,
    /// `translateY`: a scalar offset along Y.
    TranslateY,
    /// `translateZ`: a scalar offset along Z.
    TranslateZ,
    /// `scale`: a three-component scale.
    Scale,
    /// `scaleX`: a scalar scale along X.
    ScaleX,
    /// `scaleY`: a scalar scale along Y.
    ScaleY,
    /// `scaleZ`: a scalar scale along Z.
    ScaleZ,
    /// `rotateX`: degrees about X.
    RotateX,
    /// `rotateY`: degrees about Y.
    RotateY,
    /// `rotateZ`: degrees about Z.
    RotateZ,
    /// `rotateXYZ`: degrees about X, then Y, then Z.
    RotateXyz,
    /// `rotateXZY`: degrees about X, then Z, then Y.
    RotateXzy,
    /// `rotateYXZ`: degrees about Y, then X, then Z.
    RotateYxz,
    /// `rotateYZX`: degrees about Y, then Z, then X.
    RotateYzx,
    /// `rotateZXY`: degrees about Z, then X, then Y.
    RotateZxy,
    /// `rotateZYX`: degrees about Z, then Y, then X.
    RotateZyx,
    /// `orient`: a quaternion.
    Orient,
    /// `transform`: a `matrix4d`.
    Transform,
}

impl XformOpType {
    /// Every op type, with its token.
    pub const ALL: [(Self, &'static str); 19] = [
        (Self::Translate, "translate"),
        (Self::TranslateX, "translateX"),
        (Self::TranslateY, "translateY"),
        (Self::TranslateZ, "translateZ"),
        (Self::Scale, "scale"),
        (Self::ScaleX, "scaleX"),
        (Self::ScaleY, "scaleY"),
        (Self::ScaleZ, "scaleZ"),
        (Self::RotateX, "rotateX"),
        (Self::RotateY, "rotateY"),
        (Self::RotateZ, "rotateZ"),
        (Self::RotateXyz, "rotateXYZ"),
        (Self::RotateXzy, "rotateXZY"),
        (Self::RotateYxz, "rotateYXZ"),
        (Self::RotateYzx, "rotateYZX"),
        (Self::RotateZxy, "rotateZXY"),
        (Self::RotateZyx, "rotateZYX"),
        (Self::Orient, "orient"),
        (Self::Transform, "transform"),
    ];

    /// The op type `token` names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, t)| *t == token)
            .map(|(op, _)| *op)
    }

    /// The op type of the attribute `name`: its second `:` component
    /// (`xformOp:translate:pivot` is a translate).
    #[must_use]
    pub fn of_attribute(name: &str) -> Option<Self> {
        name.split(':').nth(1).and_then(Self::from_token)
    }

    /// Its token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(op, _)| *op == self)
            .map_or("", |(_, t)| t)
    }
}

/// One op of a prim's `xformOpOrder`, after `!resetXformStack!`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XformOp<'a> {
    /// The entry as `xformOpOrder` lists it (`!invert!xformOp:translate:pivot`).
    pub name: &'a str,
    /// The attribute it reads (`xformOp:translate:pivot`).
    pub attribute: &'a str,
    /// Its type; `None` when the attribute name names none, which OpenUSD
    /// evaluates as the identity.
    pub op_type: Option<XformOpType>,
    /// Whether the op applies its inverse.
    pub inverse: bool,
}

/// The ops a prim's `xformOpOrder` lists, as OpenUSD orders them
/// (`UsdGeomXformable::GetOrderedXformOps`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XformOps<'a> {
    /// The ops, first to last: the last applies first.
    pub ops: Vec<XformOp<'a>>,
    /// Whether `xformOpOrder` resets the transform stack: the prim's
    /// transform ignores its ancestors', and the ops before the reset.
    pub resets_xform_stack: bool,
    /// Entries left out because the prim has no attribute of that name.
    pub problems: Vec<XformProblem>,
}

/// Why an op of `xformOpOrder` did not contribute as written. OpenUSD
/// evaluates such an op as the identity (or, for
/// [`XformProblemKind::Singular`], a huge scale) and warns; so does this
/// crate, and reports it here.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct XformProblem {
    /// The `xformOpOrder` entry.
    pub op: String,
    /// What is wrong with it.
    pub kind: XformProblemKind,
}

/// What is wrong with an op (see [`XformProblem`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum XformProblemKind {
    /// The prim has no attribute of that name; the op is skipped.
    NoAttribute,
    /// The attribute name names no op type; the op is the identity.
    UnknownOpType,
    /// The attribute has no value at the time; the op is the identity.
    NoValue,
    /// The value's type is not one the op type takes; the op is the
    /// identity.
    ValueType,
    /// An inverted `transform` op's matrix is singular (its determinant is
    /// within `1e-9` of 0); the op is `GfMatrix4d::GetInverse`'s result, a
    /// scale by `f32::MAX` when exactly singular.
    Singular,
}

/// A prim's local transform at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalTransform {
    /// The product of its ops, rows first.
    pub matrix: [[f64; 4]; 4],
    /// Whether it resets the transform stack (see
    /// [`XformOps::resets_xform_stack`]).
    pub resets_xform_stack: bool,
    /// The ops that did not contribute as written.
    pub problems: Vec<XformProblem>,
}

impl LocalTransform {
    /// The identity: what a prim that is not `Xformable` contributes.
    #[must_use]
    pub fn identity() -> Self {
        Self {
            matrix: gf::IDENTITY,
            resets_xform_stack: false,
            problems: Vec::new(),
        }
    }

    /// The local-to-world transform of the prim this is the local
    /// transform of, given its parent's (the identity for the
    /// pseudo-root): `local * parent_to_world`, or `local` alone when it
    /// resets the transform stack.
    ///
    /// OpenUSD: `UsdGeomXformCache::_GetCtm`.
    #[must_use]
    pub fn local_to_world(&self, parent_to_world: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
        if self.resets_xform_stack {
            self.matrix
        } else {
            gf::mul(&self.matrix, parent_to_world)
        }
    }
}

/// What one prim contributes to its local transform at a time: every stage
/// read the local transform makes for it.
///
/// It reads the prim's type (`Xformable` or not); its `xformOpOrder`; for
/// each entry, whether the prim has that attribute; and for each op of a
/// known type, its value at the time
/// ([`LocalTransformInputs::properties`]). [`LocalTransformInputs::evaluate`]
/// is a pure function of these, so a caller that tracks dependencies (an
/// incremental graph) can record the reads and rerun it.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalTransformInputs<'a> {
    /// Whether the prim is `Xformable`; if not, it has no ops and its local
    /// transform is the identity.
    pub xformable: bool,
    /// Its `xformOpOrder` as it lists them, the reset and inverse markers
    /// included.
    pub xform_op_order: Vec<&'a str>,
    /// Its ops.
    pub ops: XformOps<'a>,
    /// Each op's value at the time, in the order of `ops.ops`: `None` for
    /// an op with no value or of no known type (which is not read).
    pub values: Vec<Option<Value>>,
}

impl<'a> LocalTransformInputs<'a> {
    /// Reads the inputs of the prim at `path` at `time`.
    #[must_use]
    pub fn read(scene: &Scene<'a>, path: PathId, time: Time) -> Self {
        if !scene.is_a(path, Xformable::SCHEMA) {
            return Self {
                xformable: false,
                xform_op_order: Vec::new(),
                ops: XformOps::default(),
                values: Vec::new(),
            };
        }
        let prim = Xformable::from_view(PrimView::new(*scene, path));
        let xform_op_order = prim.xform_op_order().unwrap_or_default();
        let ops = ordered_ops(&prim, &xform_op_order);
        let values = ops
            .ops
            .iter()
            .map(|op| {
                op.op_type?;
                prim.raw_value(op.attribute, time)
            })
            .collect();
        Self {
            xformable: true,
            xform_op_order,
            ops,
            values,
        }
    }

    /// The prim's properties it reads: `xformOpOrder` and every attribute
    /// an entry names (whether or not the prim has it).
    pub fn properties(&self) -> impl Iterator<Item = &'a str> + '_ {
        let listed = self
            .xform_op_order
            .iter()
            .filter(|name| **name != RESET_XFORM_STACK)
            .map(|name| name.strip_prefix(INVERT_PREFIX).unwrap_or(name));
        self.xformable
            .then_some(XFORM_OP_ORDER)
            .into_iter()
            .chain(listed)
    }

    /// The local transform: the product of the ops, the last applying
    /// first. Pure: it reads nothing more.
    ///
    /// OpenUSD: `UsdGeomXformable::XformQuery::GetLocalTransformation`, as
    /// `UsdGeomXformCache` evaluates it: the ops in reverse, each
    /// multiplied on the right, an op and its adjacent inverse cancelling.
    ///
    /// Spec: `UsdGeomXformable` (`pxr/usd/usdGeom/xformable.h`), "Xform Op
    /// Ordering".
    #[must_use]
    pub fn evaluate(&self) -> LocalTransform {
        evaluate(&self.ops, &self.values)
    }
}

/// `xformOpOrder`.
const XFORM_OP_ORDER: &str = "xformOpOrder";

/// The ops of `prim`'s `xformOpOrder`, `order`.
fn ordered_ops<'a>(prim: &Xformable<'a>, order: &[&'a str]) -> XformOps<'a> {
    let mut out = XformOps::default();
    for &name in order {
        if name == RESET_XFORM_STACK {
            out.resets_xform_stack = true;
            out.ops.clear();
            continue;
        }
        let (attribute, inverse) = match name.strip_prefix(INVERT_PREFIX) {
            Some(attribute) => (attribute, true),
            None => (name, false),
        };
        if !prim.has_attribute(attribute) {
            out.problems.push(XformProblem {
                op: name.into(),
                kind: XformProblemKind::NoAttribute,
            });
            continue;
        }
        out.ops.push(XformOp {
            name,
            attribute,
            op_type: XformOpType::of_attribute(attribute),
            inverse,
        });
    }
    out
}

fn scalar(value: &Value) -> Option<f64> {
    match value {
        Value::Double(v) => Some(*v),
        Value::Float(v) => Some(f64::from(*v)),
        Value::Half(bits) => Some(f64::from(half::to_f32(*bits))),
        _ => None,
    }
}

fn vec3(value: &Value) -> Option<[f64; 3]> {
    match value {
        Value::Vec3d(v) => Some(*v),
        Value::Vec3f(v) => Some(v.map(f64::from)),
        Value::Vec3h(v) => Some(v.map(|bits| f64::from(half::to_f32(bits)))),
        _ => None,
    }
}

fn quat(value: &Value) -> Option<[f64; 4]> {
    match value {
        Value::Quatd(q) => Some(*q),
        Value::Quatf(q) => Some(q.map(f64::from)),
        Value::Quath(q) => Some(q.map(|bits| f64::from(half::to_f32(bits)))),
        _ => None,
    }
}

/// The matrix of one op with the value `value`.
///
/// OpenUSD: `UsdGeomXformOp::GetOpTransform(opType, opVal, isInverseOp)`.
fn op_matrix(
    op_type: XformOpType,
    value: &Value,
    inverse: bool,
) -> Result<Matrix4, XformProblemKind> {
    use XformOpType as T;
    let sign = if inverse { -1.0 } else { 1.0 };
    match op_type {
        T::Transform => {
            let Value::Matrix4d(m) = value else {
                return Err(XformProblemKind::ValueType);
            };
            let m: Matrix4 = core::array::from_fn(|i| core::array::from_fn(|j| m[i * 4 + j]));
            if !inverse {
                return Ok(m);
            }
            let (inverse, det) = gf::inverse(&m);
            if det.abs() < 1e-9 {
                return Err(XformProblemKind::Singular);
            }
            Ok(inverse)
        }
        T::TranslateX | T::TranslateY | T::TranslateZ => {
            let v = scalar(value).ok_or(XformProblemKind::ValueType)? * sign;
            let axis = op_type as usize - T::TranslateX as usize;
            let mut t = [0.0; 3];
            t[axis] = v;
            Ok(gf::translate(t))
        }
        T::ScaleX | T::ScaleY | T::ScaleZ => {
            let mut v = scalar(value).ok_or(XformProblemKind::ValueType)?;
            // A scalar inverse is the negation (OpenUSD's `-doubleVal`
            // for every scalar op).
            v *= sign;
            let axis = op_type as usize - T::ScaleX as usize;
            let mut s = [1.0; 3];
            s[axis] = v;
            Ok(gf::scale(s))
        }
        T::RotateX | T::RotateY | T::RotateZ => {
            let v = scalar(value).ok_or(XformProblemKind::ValueType)? * sign;
            let axis = op_type as usize - T::RotateX as usize;
            Ok(Rotation::new(gf::AXES[axis], v).matrix())
        }
        T::Translate => {
            let v = vec3(value).ok_or(XformProblemKind::ValueType)?;
            Ok(gf::translate(v.map(|c| c * sign)))
        }
        T::Scale => {
            let v = vec3(value).ok_or(XformProblemKind::ValueType)?;
            Ok(gf::scale(if inverse { v.map(|c| 1.0 / c) } else { v }))
        }
        T::RotateXyz | T::RotateXzy | T::RotateYxz | T::RotateYzx | T::RotateZxy | T::RotateZyx => {
            let v = vec3(value)
                .ok_or(XformProblemKind::ValueType)?
                .map(|c| c * sign);
            let order = match op_type {
                T::RotateXyz => [0, 1, 2],
                T::RotateXzy => [0, 2, 1],
                T::RotateYxz => [1, 0, 2],
                T::RotateYzx => [1, 2, 0],
                T::RotateZxy => [2, 0, 1],
                _ => [2, 1, 0],
            };
            // Inv(ABC) = Inv(C) * Inv(B) * Inv(A).
            let order = if inverse {
                [order[2], order[1], order[0]]
            } else {
                order
            };
            Ok(gf::euler(v, order))
        }
        T::Orient => {
            let q = quat(value).ok_or(XformProblemKind::ValueType)?;
            let rotation = Rotation::from_quat(q);
            Ok(if inverse {
                rotation.inverse()
            } else {
                rotation
            }
            .matrix())
        }
    }
}

/// The product of `ops`, with `values` their values in order.
fn evaluate(ordered: &XformOps<'_>, values: &[Option<Value>]) -> LocalTransform {
    evaluate_ops(
        ordered.ops.len(),
        ordered.resets_xform_stack,
        ordered.problems.clone(),
        |i| ordered.ops[i].clone(),
        |i| values.get(i).and_then(Option::as_ref).map(Cow::Borrowed),
        |i| ordered.ops[i].name.into(),
    )
}

fn evaluate_ops<'a, 'v>(
    count: usize,
    resets_xform_stack: bool,
    mut problems: Vec<XformProblem>,
    get_op: impl Fn(usize) -> XformOp<'a>,
    get_value: impl Fn(usize) -> Option<Cow<'v, Value>>,
    get_name: impl Fn(usize) -> String,
) -> LocalTransform {
    let mut matrix = gf::IDENTITY;
    let mut i = count;
    while i > 0 {
        let op = get_op(i - 1);
        if i >= 2 {
            let next = get_op(i - 2);
            if next.attribute == op.attribute && next.inverse != op.inverse {
                i -= 2;
                continue;
            }
        }
        i -= 1;
        let Some(op_type) = op.op_type else {
            problems.push(XformProblem {
                op: get_name(i),
                kind: XformProblemKind::UnknownOpType,
            });
            continue;
        };
        let Some(value) = get_value(i) else {
            problems.push(XformProblem {
                op: get_name(i),
                kind: XformProblemKind::NoValue,
            });
            continue;
        };
        let op_matrix = match op_matrix(op_type, &value, op.inverse) {
            Ok(m) => m,
            Err(kind) => {
                problems.push(XformProblem {
                    op: get_name(i),
                    kind,
                });
                match (kind, value.as_ref()) {
                    // OpenUSD still applies `GetInverse`'s result.
                    (XformProblemKind::Singular, Value::Matrix4d(m)) => {
                        let m: Matrix4 =
                            core::array::from_fn(|i| core::array::from_fn(|j| m[i * 4 + j]));
                        gf::inverse(&m).0
                    }
                    _ => gf::IDENTITY,
                }
            }
        };
        if op_matrix != gf::IDENTITY {
            matrix = gf::mul(&matrix, &op_matrix);
        }
    }
    LocalTransform {
        matrix,
        resets_xform_stack,
        problems,
    }
}

impl<'a> Xformable<'a> {
    /// The ops the prim's `xformOpOrder` lists, after any
    /// `!resetXformStack!`, with entries naming no attribute reported and
    /// left out.
    ///
    /// OpenUSD: `UsdGeomXformable::GetOrderedXformOps`.
    #[must_use]
    pub fn ordered_xform_ops(&self) -> XformOps<'a> {
        ordered_ops(self, &self.xform_op_order().unwrap_or_default())
    }

    /// Whether an effective local transform op might vary across numeric times.
    ///
    /// OpenUSD: `UsdGeomXformable::TransformMightBeTimeVarying`. Two or more
    /// samples, or a spline, count as varying even if their values agree.
    /// Stronger defaults and blocks mask weaker animation. Only operations
    /// after the last reset contribute; ancestors are not inspected.
    /// A single sample returns false, although it may differ from the default
    /// value. This query is not sufficient to invalidate a cache when moving
    /// between default and numeric time.
    #[must_use]
    pub fn transform_might_be_time_varying(&self) -> bool {
        self.ordered_xform_ops().ops.iter().any(|op| {
            op.op_type.is_some()
                && transform_op_source(self, op.attribute).is_some_and(|source| {
                    source.value.spline().is_some()
                        || source
                            .value
                            .time_samples()
                            .is_some_and(|samples| samples.len() > 1)
                })
        })
    }

    /// Sorted, unique stage-time samples of the effective local transform ops.
    ///
    /// OpenUSD: `UsdGeomXformable::GetTimeSamples`. Uses composed value
    /// sources and accumulated layer offsets (AOUSD Core §12.3.2.1); defaults
    /// and blocks mask weaker samples. Ops before the last reset and ancestor
    /// transforms are excluded. Splines have no discrete time samples; use
    /// [`Self::transform_might_be_time_varying`] to detect their variability.
    #[must_use]
    pub fn transform_time_samples(&self) -> Vec<f64> {
        let mut times = Vec::new();
        for op in self.ordered_xform_ops().ops {
            if op.op_type.is_none() {
                continue;
            }
            if let Some(source) = transform_op_source(self, op.attribute)
                && let Some(samples) = source.value.time_samples()
            {
                times.extend(samples.iter().map(|(time, _)| {
                    time * source.layer_offset.scale + source.layer_offset.offset
                }));
            }
        }
        times.sort_by(f64::total_cmp);
        times.dedup_by(|a, b| *a == *b);
        times
    }

    /// [`Self::transform_time_samples`] restricted to an inclusive interval.
    /// Reversed or NaN endpoints produce an empty result.
    #[must_use]
    pub fn transform_time_samples_in_interval(&self, start: f64, end: f64) -> Vec<f64> {
        if start > end || start.is_nan() || end.is_nan() {
            return Vec::new();
        }
        let mut times = self.transform_time_samples();
        times.retain(|time| *time >= start && *time <= end);
        times
    }

    /// The stage reads the prim's local transform at `time` depends on
    /// ([`LocalTransformInputs`]).
    #[must_use]
    pub fn local_transform_inputs(&self, time: Time) -> LocalTransformInputs<'a> {
        LocalTransformInputs::read(&self.scene(), self.path(), time)
    }

    /// The prim's local transform at `time`: the product of its ops, the
    /// last applying first, with whether it resets the transform stack.
    ///
    /// OpenUSD: `UsdGeomXformable::GetLocalTransformation`.
    ///
    /// Spec: `UsdGeomXformable` (`pxr/usd/usdGeom/xformable.h`), "Xform Op
    /// Ordering".
    #[must_use]
    pub fn local_transform(&self, time: Time) -> LocalTransform {
        self.local_transform_inputs(time).evaluate()
    }
}

// Transform op values are atomic scalars/vectors/matrices, never sparse-array
// opinions. Numeric source precedence is samples, spline, then default at each
// site (AOUSD Core §12.3). Keep this separate from conservative cache dependency
// classification, which must also cover default/numeric transitions.
fn transform_op_source<'a>(prim: &Xformable<'a>, name: &str) -> Option<&'a layerstack::Opinion> {
    prim.scene()
        .stage()
        .explain_property_path(prim.property_path(name)?)?
        .iter()
        .find(|opinion| {
            opinion.value.time_samples().is_some()
                || opinion.value.spline().is_some()
                || opinion.value.default_value().is_some()
        })
}

impl<'a> Imageable<'a> {
    /// The transform from the prim's space to the world at `time`: its
    /// local transform times its parent's local-to-world transform, unless
    /// it resets the transform stack.
    ///
    /// OpenUSD: `UsdGeomImageable::ComputeLocalToWorldTransform`. For many
    /// prims use an [`XformCache`], which shares the ancestors' work.
    #[must_use]
    pub fn compute_local_to_world(&self, time: Time) -> [[f64; 4]; 4] {
        XformCache::new(time)
            .local_to_world(&self.scene(), self.path())
            .unwrap_or(gf::IDENTITY)
    }

    /// The local-to-world transform of the prim's parent at `time`.
    ///
    /// OpenUSD: `UsdGeomImageable::ComputeParentToWorldTransform`.
    #[must_use]
    pub fn compute_parent_to_world(&self, time: Time) -> [[f64; 4]; 4] {
        XformCache::new(time)
            .parent_to_world(&self.scene(), self.path())
            .unwrap_or(gf::IDENTITY)
    }
}

/// Local operations composed from a prim toward a requested ancestor.
/// A reset stops the walk, so the result is then world-relative rather than
/// relative to an ancestor above the reset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RelativeTransform {
    /// Row-vector transform from the queried prim's coordinates.
    pub matrix: [[f64; 4]; 4],
    /// Whether the walk encountered a reset before reaching the ancestor.
    pub resets_xform_stack: bool,
}

/// What an [`XformCache`] has done since it was made or cleared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XformCacheStats {
    /// Local transforms computed.
    pub local_computed: usize,
    /// Local-to-world transforms computed.
    pub world_computed: usize,
    /// Queries answered from the cache.
    pub hits: usize,
}

// Slots never escape the cache. Vec relocation preserves indices, and subtree
// retirement removes every incoming parent link before a slot can be reused.
type Slot = NonZeroUsize;

#[derive(Clone, Copy, Debug)]
enum Parent {
    Unresolved,
    Root,
    Cached(Slot),
    Vacant(Option<Slot>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeDependency {
    Unknown,
    Static,
    Varying,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    path: PathId,
    parent: Parent,
    local_stamp: u64,
    world_stamp: u64,
    validated: u64,
    world_valid: bool,
    world_time_dependent: bool,
    time_dependency: TimeDependency,
}

#[derive(Clone, Debug)]
struct Transforms {
    local: Option<LocalTransform>,
    world: Matrix4,
}

// Only sampled transforms need a retained operation recipe. Static and empty
// stacks keep their evaluated local result without an extra per-node allocation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PreparedTransform {
    ops: Box<[PreparedOp]>,
    resets: bool,
    problems: Vec<XformProblem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PreparedOp {
    attribute: TokenId,
    kind: Option<XformOpType>,
    inverse: bool,
}

impl PreparedTransform {
    fn new(scene: &Scene<'_>, inputs: &LocalTransformInputs<'_>) -> Option<Self> {
        let ops: Option<Vec<_>> = inputs
            .ops
            .ops
            .iter()
            .map(|op| {
                Some(PreparedOp {
                    attribute: scene.store().tokens().lookup(op.attribute)?,
                    kind: op.op_type,
                    inverse: op.inverse,
                })
            })
            .collect();
        Some(Self {
            ops: ops?.into_boxed_slice(),
            resets: inputs.ops.resets_xform_stack,
            problems: inputs.ops.problems.clone(),
        })
    }

    fn evaluate(&self, scene: &Scene<'_>, path: PathId, time: Time) -> LocalTransform {
        evaluate_ops(
            self.ops.len(),
            self.resets,
            self.problems.clone(),
            |i| {
                let op = self.ops[i];
                let attribute = scene.store().tokens().resolve(op.attribute);
                XformOp {
                    name: attribute,
                    attribute,
                    op_type: op.kind,
                    inverse: op.inverse,
                }
            },
            |i| {
                let token = self.ops[i].attribute;
                let value = match time {
                    Time::Default => match scene
                        .stage()
                        .resolve_value_with_schema(path, token, scene.store())?
                        .value
                    {
                        ResolvedValue::Scalar(value) => value,
                        _ => return None,
                    },
                    Time::At {
                        code,
                        interpolation,
                    } => {
                        scene
                            .stage()
                            .resolve_value_at_time_with_schema(
                                path,
                                token,
                                code,
                                interpolation,
                                scene.store(),
                            )?
                            .value
                    }
                };
                Some(Cow::Owned(value))
            },
            |i| {
                let op = self.ops[i];
                let attribute = scene.store().tokens().resolve(op.attribute);
                if op.inverse {
                    format!("{INVERT_PREFIX}{attribute}")
                } else {
                    attribute.into()
                }
            },
        )
    }
}

// Recipes contain no prim-specific values. Share equal op layouts within a
// cache, but count this cache's users explicitly: Arc counts would also include
// cloned caches and would retain dead recipes here while another clone lives.
#[derive(Clone, Debug, Default)]
struct PreparedCache {
    by_slot: HashMap<Slot, Arc<PreparedTransform>>,
    recipes: HashMap<Arc<PreparedTransform>, usize>,
}
impl PreparedCache {
    fn get(&self, slot: &Slot) -> Option<&PreparedTransform> {
        self.by_slot.get(slot).map(Arc::as_ref)
    }
    fn insert(&mut self, slot: Slot, prepared: PreparedTransform) {
        self.remove(&slot);
        let recipe = self
            .recipes
            .get_key_value(&prepared)
            .map(|(recipe, _)| Arc::clone(recipe))
            .unwrap_or_else(|| Arc::new(prepared));
        *self.recipes.entry(Arc::clone(&recipe)).or_default() += 1;
        self.by_slot.insert(slot, recipe);
    }
    fn remove(&mut self, slot: &Slot) {
        if let Some(recipe) = self.by_slot.remove(slot) {
            let users = self
                .recipes
                .get_mut(&recipe)
                .expect("live recipe has a user count");
            *users -= 1;
            if *users == 0 {
                self.recipes.remove(&recipe);
            }
        }
    }
    fn clear(&mut self) {
        self.by_slot.clear();
        self.recipes.clear();
    }
}

/// Local and local-to-world transforms of many prims at one time, sharing
/// each ancestor's work.
///
/// It is one caller of the computations, not their owner: it reads each
/// prim's [`LocalTransformInputs`], evaluates them, and composes each world
/// transform with [`LocalTransform::local_to_world`], as any other caller
/// (an incremental graph among them) can.
///
/// The caller owns it and decides its lifetime; nothing is global. It
/// holds results for one [`Time`] and for the [`Scene`] it is queried
/// with: query it with one scene, or [`XformCache::clear`] it between
/// scenes.
///
/// After `LiveStage::apply`, pass its change report to [`Self::apply_changes`].
/// This reuses composed topology and exact removal inventories. For manual
/// notifications, drop what changed: a structural change
/// (`LiveStage::apply` reports subtree roots in `changes.resynced`) or an edit of a
/// prim's `xformOpOrder` or `xformOp:*` attributes changes that prim's
/// transform and its descendants' world transforms, which
/// [`XformCache::invalidate`] drops; [`XformCache::clear`] drops
/// everything. For transform-only changes, [`XformCache::invalidate_transform`]
/// retains unchanged local transforms and independent reset subtrees. Nothing
/// is invalidated by itself.
///
/// Cached parents are private arena links. Structural invalidation retires all
/// cached descendants before reusing their slots. Storage is reused across edits
/// and clearing; retained arena capacity follows peak demand.
///
/// OpenUSD: `UsdGeomXformCache`.
#[derive(Clone, Debug)]
pub struct XformCache {
    time: Time,
    entries: HashMap<PathId, Slot>,
    // Keep traversal metadata apart from the much larger transform payloads.
    nodes: Vec<Node>,
    transforms: Vec<Transforms>,
    prepared: PreparedCache,
    free: Option<Slot>,
    // Path's segment ordering places a prefix and its descendants together.
    // Manual invalidation builds this lazily, including sparse local-only
    // entries and deleted subtrees. Report users share stage topology instead.
    namespace: Option<BTreeMap<Path, PathId>>,
    chain: Vec<Slot>,
    epoch: u64,
    time_stamp: u64,
    stats: XformCacheStats,
}

impl XformCache {
    /// An empty cache of transforms at `time`.
    #[must_use]
    pub fn new(time: Time) -> Self {
        Self {
            time,
            entries: HashMap::new(),
            nodes: Vec::new(),
            transforms: Vec::new(),
            prepared: PreparedCache::default(),
            free: None,
            namespace: None,
            chain: Vec::new(),
            epoch: 0,
            time_stamp: 0,
            stats: XformCacheStats::default(),
        }
    }

    /// The time it holds transforms at.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }

    /// Evaluates subsequent queries at `time`, retaining topology and static locals.
    ///
    /// This advances a timestamp without visiting cached nodes. On lookup,
    /// locals with any sampled or spline opinion are reevaluated conservatively;
    /// descendant worlds validate lazily. This includes single samples because
    /// numeric and default time can differ. Statistics remain cumulative.
    /// Setting the same time is a no-op. Source edits still require notification.
    ///
    /// Spec: AOUSD Core §12.3 (default values and time samples), §12.5
    /// (interpolation). Unlike source opinions, evaluated cache values may be
    /// discarded without changing authored state.
    pub fn set_time(&mut self, time: Time) {
        if time == self.time {
            return;
        }
        self.time = time;
        if self.epoch == u64::MAX {
            self.clear();
            return;
        }
        self.epoch += 1;
        self.time_stamp = self.epoch;
    }

    /// Drops everything held, and the statistics.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.nodes.clear();
        self.transforms.clear();
        self.prepared.clear();
        self.free = None;
        self.namespace = None;
        self.chain.clear();
        self.epoch = 0;
        self.time_stamp = 0;
        self.stats = XformCacheStats::default();
    }

    /// Drops what it holds for `path` and its namespace descendants.
    ///
    /// An ordered namespace index limits the search to cached descendants,
    /// even when the subtree has already been removed from the stage. The
    /// index retains one owned path per cached prim. It is built lazily on the
    /// first call, which visits the whole cache. Subsequent calls search only
    /// the indexed subtree. [`Self::apply_changes`] uses stage topology instead
    /// and avoids maintaining this index.
    pub fn invalidate(&mut self, scene: &Scene<'_>, path: PathId) {
        let paths = scene.store().paths();
        let root = paths.resolve(path);
        let namespace = self.namespace.get_or_insert_with(|| {
            self.entries
                .keys()
                .map(|&id| (paths.resolve(id).clone(), id))
                .collect()
        });
        let removed: Vec<_> = namespace
            .range::<Path, _>((Bound::Included(root), Bound::Unbounded))
            .take_while(|(held, _)| root.is_prefix_of(held))
            .map(|(_, id)| *id)
            .collect();
        for id in removed {
            self.remove_cached(scene, id);
        }
        self.finish_retirement();
    }

    /// Applies a complete successful `LiveStage::apply` change report against
    /// the resulting scene. Pass every report before querying changed results.
    ///
    /// Exact removed paths retire old entries; resyncs use composed child lists.
    /// For a sparse cache, traversal stops after a budget equal to its entry
    /// count and filters the remaining cache by namespace instead. Child lists
    /// are borrowed lazily, so wide scenes allocate no wide traversal frontier.
    /// Info-only changes invalidate the local transform when transform fields
    /// changed or the report lacks a complete property inventory;
    /// descendant worlds validate lazily. No namespace index is constructed.
    ///
    /// Spec: AOUSD Core §11 (population), §12 (composed changes).
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &layerstack::Changes) {
        for &path in &changes.removed {
            self.remove_cached(scene, path);
        }
        for &root in &changes.resynced {
            if self.entries.is_empty() {
                break;
            }
            let budget = self.entries.len();
            for (visited, path) in scene.stage().traverse(root).enumerate() {
                if visited == budget {
                    let paths = scene.store().paths();
                    let prefix = paths.resolve(root);
                    let nodes = &mut self.nodes;
                    let transforms = &mut self.transforms;
                    let free = &mut self.free;
                    let prepared = &mut self.prepared;
                    self.entries.retain(|path, slot| {
                        if !prefix.is_prefix_of(paths.resolve(*path)) {
                            return true;
                        }
                        prepared.remove(slot);
                        Self::retire_slot(nodes, transforms, free, *slot);
                        false
                    });
                    if let Some(namespace) = &mut self.namespace {
                        namespace.retain(|path, _| !prefix.is_prefix_of(path));
                    }
                    break;
                }
                self.remove_cached(scene, path);
            }
        }
        self.finish_retirement();
        for &path in &changes.changed_info_only {
            if changes.properties_for(path).is_none_or(|fields| {
                fields
                    .iter()
                    .any(|field| transform_property(scene.store().tokens().resolve(field.name)))
            }) {
                self.invalidate_transform(path);
            }
        }
    }

    fn remove_cached(&mut self, scene: &Scene<'_>, path: PathId) {
        if let Some(slot) = self.entries.remove(&path) {
            self.prepared.remove(&slot);
            Self::retire_slot(&mut self.nodes, &mut self.transforms, &mut self.free, slot);
        }
        if let Some(namespace) = &mut self.namespace {
            namespace.remove(scene.store().paths().resolve(path));
        }
    }

    fn retire_slot(
        nodes: &mut [Node],
        transforms: &mut [Transforms],
        free: &mut Option<Slot>,
        slot: Slot,
    ) {
        let index = slot.get() - 1;
        transforms[index].local = None;
        nodes[index].world_valid = false;
        nodes[index].parent = Parent::Vacant(*free);
        *free = Some(slot);
    }

    fn finish_retirement(&mut self) {
        // A complete structural batch retires all incoming descendant links
        // before any slot can be reused by a query.
        self.chain.clear();
        if self.entries.is_empty() {
            self.nodes.clear();
            self.transforms.clear();
            self.free = None;
        }
    }

    /// Invalidates a transform-only edit on `path`, retaining descendant locals.
    ///
    /// Use this only when the prim namespace, types and applied schemas are
    /// unchanged, and only this prim's `xformOpOrder` or `xformOp:*` attributes
    /// changed. This includes adding/removing ops or toggling its reset marker.
    /// If a source edit changes several composed prims, invalidate each one.
    /// Use [`Self::invalidate`] for structural, type or schema changes instead.
    ///
    /// Only the edited prim loses its local and world values. Descendant worlds
    /// are validated lazily on lookup, stopping at a current ancestor or reset.
    /// Unchanged dependencies advance the validation stamp without multiplying
    /// matrices. Notifications do not walk or allocate for descendants.
    ///
    /// This cache owns its validation clock; it is not a source revision.
    /// Structural changes still require [`Self::invalidate`].
    pub fn invalidate_transform(&mut self, path: PathId) {
        if self.epoch == u64::MAX {
            // Never let an old validation stamp compare equal after wraparound.
            self.clear();
        }
        self.epoch += 1;
        if let Some(slot) = self.entries.get(&path) {
            self.prepared.remove(slot);
            let index = slot.get() - 1;
            self.transforms[index].local = None;
            self.nodes[index].world_valid = false;
            self.nodes[index].local_stamp = self.epoch;
            self.nodes[index].time_dependency = TimeDependency::Unknown;
        }
    }

    /// The number of prims it holds something for.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What it has computed and reused.
    #[must_use]
    pub fn stats(&self) -> XformCacheStats {
        self.stats
    }

    /// The local transform of the prim at `path` (the identity for a prim
    /// that is not `Xformable`), or `None` if no prim is there.
    ///
    /// OpenUSD: `UsdGeomXformCache::GetLocalTransformation`.
    pub fn local_transform(&mut self, scene: &Scene<'_>, path: PathId) -> Option<&LocalTransform> {
        if !scene.stage().has_prim(path) {
            return None;
        }
        let slot = self.ensure_slot(scene, path);
        self.ensure_local(scene, slot);
        self.transforms[slot.get() - 1].local.as_ref()
    }

    // The caller has checked existence, or followed a retained parent link in
    // this cache's scene. Structural edits require subtree invalidation.
    fn ensure_slot(&mut self, scene: &Scene<'_>, path: PathId) -> Slot {
        *self.entries.entry(path).or_insert_with(|| {
            let node = Node {
                path,
                parent: Parent::Unresolved,
                local_stamp: self.epoch,
                world_stamp: 0,
                validated: 0,
                world_valid: false,
                world_time_dependent: false,
                time_dependency: TimeDependency::Unknown,
            };
            let transforms = Transforms {
                local: None,
                world: gf::IDENTITY,
            };
            let slot = if let Some(slot) = self.free {
                let index = slot.get() - 1;
                let Parent::Vacant(next) = self.nodes[index].parent else {
                    unreachable!("free list contains only retired slots")
                };
                self.free = next;
                self.nodes[index] = node;
                self.transforms[index] = transforms;
                slot
            } else {
                self.nodes.push(node);
                self.transforms.push(transforms);
                Slot::new(self.nodes.len()).expect("pushed slot is nonzero")
            };
            if let Some(namespace) = &mut self.namespace {
                namespace.insert(scene.store().paths().resolve(path).clone(), path);
            }
            slot
        })
    }

    fn ensure_local(&mut self, scene: &Scene<'_>, slot: Slot) {
        let index = slot.get() - 1;
        // Time is a dependency of sampled locals only. Reuse the edit stamp
        // as their evaluation stamp, so advancing time needs no cache scan or
        // separate per-node clock. Source edits also advance this same epoch.
        if self.nodes[index].time_dependency == TimeDependency::Varying
            && self.nodes[index].local_stamp < self.time_stamp
        {
            self.transforms[index].local = None;
            self.nodes[index].world_valid = false;
            self.nodes[index].local_stamp = self.time_stamp;
        }
        if self.transforms[index].local.is_some() {
            self.stats.hits += 1;
        } else {
            let path = self.nodes[index].path;
            if let Some(prepared) = self.prepared.get(&slot) {
                self.transforms[index].local = Some(prepared.evaluate(scene, path, self.time));
                self.stats.local_computed += 1;
                return;
            }
            let inputs = LocalTransformInputs::read(scene, path, self.time);
            // Check all opinions, including weaker masked samples. This is a
            // conservative dependency classification, not a second resolver.
            // Single samples and splines can differ from default-time values.
            let view = PrimView::new(*scene, path);
            if self.nodes[index].time_dependency == TimeDependency::Unknown {
                let varying = inputs
                    .ops
                    .ops
                    .iter()
                    .any(|op| view.property_might_vary(op.attribute));
                self.nodes[index].time_dependency = if varying {
                    TimeDependency::Varying
                } else {
                    TimeDependency::Static
                };
            }
            if self.nodes[index].time_dependency == TimeDependency::Varying
                && let Some(prepared) = PreparedTransform::new(scene, &inputs)
            {
                self.prepared.insert(slot, prepared);
            }
            self.transforms[index].local = Some(inputs.evaluate());
            self.stats.local_computed += 1;
        }
    }

    // None is the identity pseudo-root. Only the first traversal resolves a
    // namespace parent; later walks use the arena link, including after edits.
    fn parent_slot(&mut self, scene: &Scene<'_>, slot: Slot) -> Option<Slot> {
        let index = slot.get() - 1;
        match self.nodes[index].parent {
            Parent::Cached(parent) => Some(parent),
            Parent::Root => None,
            Parent::Unresolved => {
                let parent = scene.parent(self.nodes[index].path).and_then(|path| {
                    (!scene.store().paths().resolve(path).segments().is_empty())
                        .then(|| self.ensure_slot(scene, path))
                });
                self.nodes[index].parent = parent.map_or(Parent::Root, Parent::Cached);
                parent
            }
            Parent::Vacant(_) => unreachable!("live nodes never link to retired slots"),
        }
    }

    /// The transform from the prim at `path` to the world, or `None` if no
    /// prim is there. The pseudo-root's is the identity.
    ///
    /// OpenUSD: `UsdGeomXformCache::GetLocalToWorldTransform`.
    pub fn local_to_world(&mut self, scene: &Scene<'_>, path: PathId) -> Option<[[f64; 4]; 4]> {
        if !scene.stage().has_prim(path) {
            return None;
        }
        if scene.store().paths().resolve(path).segments().is_empty() {
            return Some(gf::IDENTITY);
        }
        self.chain.clear();
        let mut at = Some(self.ensure_slot(scene, path));
        let mut above = gf::IDENTITY;
        let mut above_stamp = 0;
        let mut above_time_dependent = false;
        while let Some(slot) = at {
            let index = slot.get() - 1;
            let node = &self.nodes[index];
            if node.validated == self.epoch && node.world_valid {
                self.stats.hits += 1;
                above = self.transforms[index].world;
                above_stamp = node.world_stamp;
                above_time_dependent = node.world_time_dependent;
                break;
            }
            self.chain.push(slot);
            self.ensure_local(scene, slot);
            // OpenUSD UsdGeomXformCache::_GetCtm stops at reset. Do not even
            // allocate a parent slot above a cold reset subtree.
            if self.transforms[index].local.as_ref()?.resets_xform_stack {
                break;
            }
            at = self.parent_slot(scene, slot);
        }
        while let Some(slot) = self.chain.pop() {
            let index = slot.get() - 1;
            let node = &mut self.nodes[index];
            let transforms = &mut self.transforms[index];
            let local = transforms.local.as_ref()?;
            let stamp = if local.resets_xform_stack {
                node.local_stamp
            } else {
                node.local_stamp.max(above_stamp)
            };
            if !node.world_valid || node.world_stamp != stamp {
                transforms.world = local.local_to_world(&above);
                node.world_valid = true;
                node.world_stamp = stamp;
                self.stats.world_computed += 1;
            } else {
                self.stats.hits += 1;
            }
            node.world_time_dependent = node.time_dependency == TimeDependency::Varying
                || (!local.resets_xform_stack && above_time_dependent);
            above_time_dependent = node.world_time_dependent;
            node.validated = self.epoch;
            above = transforms.world;
            above_stamp = stamp;
        }
        Some(above)
    }

    /// Compose local operations up to, but excluding, `ancestor`.
    ///
    /// OpenUSD: `UsdGeomXformCache::ComputeRelativeTransform`. Stops at a
    /// reset and reports it; this is not a world-matrix quotient. If the
    /// requested ancestor is outside the prim's ancestry, the walk ends at
    /// the pseudo-root and returns the world transform. Equal paths return
    /// identity without a reset. Returns `None` if either prim is missing.
    pub fn relative_transform(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
        ancestor: PathId,
    ) -> Option<RelativeTransform> {
        if !scene.stage().has_prim(path) || !scene.stage().has_prim(ancestor) {
            return None;
        }
        let mut result = RelativeTransform {
            matrix: gf::IDENTITY,
            resets_xform_stack: false,
        };
        if path == ancestor || scene.store().paths().resolve(path).segments().is_empty() {
            return Some(result);
        }
        let mut at = Some(self.ensure_slot(scene, path));
        while let Some(slot) = at {
            let index = slot.get() - 1;
            if self.nodes[index].path == ancestor {
                break;
            }
            self.ensure_local(scene, slot);
            let local = self.transforms[index].local.as_ref()?;
            result.matrix = gf::mul(&result.matrix, &local.matrix);
            if local.resets_xform_stack {
                result.resets_xform_stack = true;
                break;
            }
            at = self.parent_slot(scene, slot);
        }
        Some(result)
    }

    // Called after a successful world query by bounds reduction. The world
    // dependency follows reset boundaries exactly like the matrix itself.
    pub(crate) fn world_might_vary(&self, path: PathId) -> bool {
        self.entries
            .get(&path)
            .is_some_and(|slot| self.nodes[slot.get() - 1].world_time_dependent)
    }

    /// The local-to-world transform of the parent of the prim at `path`,
    /// or `None` if no prim is there.
    ///
    /// OpenUSD: `UsdGeomXformCache::GetParentToWorldTransform`.
    pub fn parent_to_world(&mut self, scene: &Scene<'_>, path: PathId) -> Option<[[f64; 4]; 4]> {
        if !scene.stage().has_prim(path) {
            return None;
        }
        match scene.parent(path) {
            Some(parent) => self.local_to_world(scene, parent),
            None => Some(gf::IDENTITY),
        }
    }
}

// UsdGeomXformable reads only xformOpOrder and the named xformOp attributes.
// Match the whole namespace conservatively, including currently unused ops.
pub(crate) fn transform_property(name: &str) -> bool {
    name == "xformOpOrder" || name.starts_with("xformOp:")
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use layerstack::{
        EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, StageOptions, Transaction,
    };

    fn node(cache: &XformCache, path: PathId) -> &Node {
        &cache.nodes[cache.entries[&path].get() - 1]
    }

    fn local(cache: &XformCache, path: PathId) -> Option<&LocalTransform> {
        cache.transforms[cache.entries[&path].get() - 1]
            .local
            .as_ref()
    }

    fn world(cache: &XformCache, path: PathId) -> Option<Matrix4> {
        node(cache, path)
            .world_valid
            .then(|| cache.transforms[cache.entries[&path].get() - 1].world)
    }

    #[test]
    fn invalidation_finds_sparse_locals_after_source_removal() {
        let mut store = InMemoryStore::default();
        // Intern siblings out of lexical order; namespace index order is token order.
        let other = store.path("/Other");
        let child = store.path("/Root/Z/Leaf");
        let branch = store.path("/Root/Z");
        let sibling = store.path("/Root/A");
        let root = store.path("/Root");
        let mut layer = Layer::new(LayerId(1));
        for path in [other, child, branch, sibling, root] {
            layer.insert_prim(path, PrimSpec::def());
        }
        store.insert_layer(layer);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let mut cache = XformCache::new(Time::Default);
        for path in [other, child, sibling] {
            cache.local_transform(&Scene::new(live.stage(), &store), path);
        }
        assert_eq!(cache.len(), 3);
        // No parent was queried and the removed subtree no longer exists in Stage.
        let mut txn = Transaction::new();
        txn.remove_spec(EditTarget::for_layer(LayerId(1)).prim(branch));
        let applied = live.apply(&mut store, &txn).unwrap();
        cache.invalidate(&Scene::new(live.stage(), &store), branch);
        assert_eq!(cache.len(), 2);
        assert!(!cache.entries.contains_key(&child));
        assert_eq!(cache.namespace.as_ref().unwrap().len(), 2);
        live.apply(&mut store, &applied.inverse).unwrap();
        cache.local_transform(&Scene::new(live.stage(), &store), child);
        cache.invalidate(&Scene::new(live.stage(), &store), root);
        assert_eq!(cache.len(), 1);
        assert!(cache.entries.contains_key(&other));
        let pseudo = store.path("/");
        cache.invalidate(&Scene::new(live.stage(), &store), pseudo);
        assert!(cache.is_empty());
        assert!(cache.namespace.as_ref().is_none_or(BTreeMap::is_empty));
    }

    #[test]
    fn cold_reset_does_not_evaluate_ancestors() {
        use alloc::{sync::Arc, vec};
        use layerstack::PropertySpec;
        let mut store = InMemoryStore::default();
        let root = store.path("/Root");
        let child = store.path("/Root/Child");
        let xform = store.tokens.intern("Xform");
        let order = store.tokens.intern("xformOpOrder");
        let reset = store.tokens.intern("!resetXformStack!");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(root, PrimSpec::def().with_type_name(xform));
        layer.insert_prim(
            child,
            PrimSpec::def().with_type_name(xform).with_property(
                order,
                PropertySpec::attribute().with_default(Value::Array(vec![Value::Token(reset)])),
            ),
        );
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        let live = LiveStage::compose(&mut store, LayerId(1), options);
        let scene = Scene::new(live.stage(), &store);
        let mut cache = XformCache::new(Time::Default);
        assert_eq!(cache.local_to_world(&scene, child), Some(gf::IDENTITY));
        assert_eq!(cache.stats().local_computed, 1);
        assert_eq!(cache.stats().world_computed, 1);
        assert_eq!(cache.len(), 1);
        assert!(!cache.entries.contains_key(&root));
    }

    fn transform_scene() -> (InMemoryStore, LiveStage, [PathId; 5]) {
        use alloc::{sync::Arc, vec};
        use layerstack::PropertySpec;
        let mut store = InMemoryStore::default();
        let paths = [
            "/Root",
            "/Root/Normal",
            "/Root/Reset",
            "/Root/Reset/Leaf",
            "/Other",
        ]
        .map(|name| store.path(name));
        let xform = store.tokens.intern("Xform");
        let order = store.tokens.intern("xformOpOrder");
        let translate = store.tokens.intern("xformOp:translate");
        let reset = store.tokens.intern(RESET_XFORM_STACK);
        let mut layer = Layer::new(LayerId(1));
        for (i, &path) in paths.iter().enumerate() {
            let ops = if i == 2 {
                vec![Value::Token(reset), Value::Token(translate)]
            } else {
                vec![Value::Token(translate)]
            };
            layer.insert_prim(
                path,
                PrimSpec::def()
                    .with_type_name(xform)
                    .with_property(
                        order,
                        PropertySpec::typed_attribute(layerstack::PropertyType::new(
                            "token",
                            true,
                            Value::Token(translate),
                        ))
                        .with_default(Value::Array(ops)),
                    )
                    .with_property(
                        translate,
                        PropertySpec::typed_attribute(layerstack::PropertyType::new(
                            "double3",
                            false,
                            Value::Vec3d([0.0; 3]),
                        ))
                        .with_default(Value::Vec3d([
                            i as f64 + 1.0,
                            0.0,
                            0.0,
                        ])),
                    ),
            );
        }
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        let live = LiveStage::compose(&mut store, LayerId(1), options);
        (store, live, paths)
    }

    #[test]
    fn change_reports_reuse_stage_topology_for_sparse_reads_and_undo() {
        let (mut store, mut live, [root, normal, reset, leaf, other]) = transform_scene();
        let mut cache = XformCache::new(Time::Default);
        let scene = Scene::new(live.stage(), &store);
        cache.local_transform(&scene, leaf).unwrap();
        cache.local_transform(&scene, other).unwrap();
        // More stage descendants than cached entries: take the bounded sparse
        // path, including a local-only query with no cached ancestors.
        cache.apply_changes(
            &scene,
            &layerstack::Changes {
                resynced: alloc::vec![root],
                ..layerstack::Changes::default()
            },
        );
        assert_eq!(cache.len(), 1);
        assert!(cache.entries.contains_key(&other));
        assert!(cache.namespace.is_none());
        assert_arena_links(&cache, &scene);
        for path in [normal, leaf] {
            cache.local_to_world(&scene, path).unwrap();
        }
        let mut remove = Transaction::new();
        remove.remove_spec(EditTarget::for_layer(LayerId(1)).prim(root));
        let removed = live.apply(&mut store, &remove).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &removed.changes);
        assert!(!cache.entries.contains_key(&leaf));
        assert!(!cache.entries.contains_key(&reset));
        let restored = live.apply(&mut store, &removed.inverse).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &restored.changes);
        let translate = store.tokens.intern("xformOp:translate");
        let order = store.tokens.intern("xformOpOrder");
        for round in 0..6 {
            let mut edit = Transaction::new();
            edit.set_default(
                EditTarget::for_layer(LayerId(1))
                    .property(layerstack::PropertyPath::new(root, translate)),
                Value::Vec3d([round as f64, 2.0, 3.0]),
            );
            edit.set_default(
                EditTarget::for_layer(LayerId(1))
                    .property(layerstack::PropertyPath::new(reset, order)),
                Value::Array(if round % 2 == 0 {
                    alloc::vec![Value::Token(translate)]
                } else {
                    alloc::vec![
                        Value::Token(store.tokens.intern(RESET_XFORM_STACK)),
                        Value::Token(translate)
                    ]
                }),
            );
            let applied = live.apply(&mut store, &edit).unwrap();
            let scene = Scene::new(live.stage(), &store);
            cache.apply_changes(&scene, &applied.changes);
            let mut fresh = XformCache::new(Time::Default);
            for path in [root, normal, reset, leaf, other] {
                assert_eq!(
                    cache.local_to_world(&scene, path),
                    fresh.local_to_world(&scene, path)
                );
            }
        }
        assert!(cache.namespace.is_none());
        // Mixing manual and report-driven invalidation keeps the optional
        // compatibility index coherent too.
        cache.invalidate(&Scene::new(live.stage(), &store), leaf);
        cache.local_transform(&Scene::new(live.stage(), &store), leaf);
        let removed = live.apply(&mut store, &remove).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &removed.changes);
        assert_eq!(cache.namespace.as_ref().unwrap().len(), cache.len());
        assert!(!cache.entries.contains_key(&leaf));
    }

    #[test]
    fn transform_edits_reuse_locals_and_preserve_reset_worlds() {
        use alloc::vec;
        use layerstack::PropertyPath;
        let (mut store, mut live, [root, normal, reset, leaf, other]) = transform_scene();
        let mut cache = XformCache::new(Time::Default);
        let scene = Scene::new(live.stage(), &store);
        for path in [normal, leaf, other] {
            cache.local_to_world(&scene, path).unwrap();
        }
        let held_reset = world(&cache, reset);
        let held_leaf = world(&cache, leaf);
        let held_other = world(&cache, other);
        let initial = cache.stats();
        let translate = store.tokens.intern("xformOp:translate");
        let mut tx = Transaction::new();
        tx.set_default(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(root, translate)),
            Value::Vec3d([100.0, 0.0, 0.0]),
        );
        live.apply(&mut store, &tx).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.invalidate_transform(root);
        assert!(local(&cache, root).is_none());
        assert!(world(&cache, root).is_none());
        assert!(local(&cache, normal).is_some());
        assert!(world(&cache, normal).is_some(), "validation is lazy");
        assert_eq!(world(&cache, reset), held_reset);
        assert_eq!(world(&cache, leaf), held_leaf);
        assert_eq!(world(&cache, other), held_other);
        for path in [root, normal, reset, leaf, other] {
            assert_eq!(
                cache.local_to_world(&scene, path),
                XformCache::new(Time::Default).local_to_world(&scene, path)
            );
        }
        assert_eq!(cache.stats().local_computed - initial.local_computed, 1);
        assert_eq!(cache.stats().world_computed - initial.world_computed, 2);

        // Turning off this boundary invalidates its world subtree while keeping
        // both the leaf's local transform and unrelated cached branches.
        let order = store.tokens.intern("xformOpOrder");
        let mut tx = Transaction::new();
        tx.set_default(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(reset, order)),
            Value::Array(vec![Value::Token(translate)]),
        );
        let applied = live.apply(&mut store, &tx).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.invalidate_transform(reset);
        assert!(local(&cache, reset).is_none());
        assert!(local(&cache, leaf).is_some());
        assert!(world(&cache, leaf).is_some(), "validation is lazy");
        let now = cache.local_to_world(&scene, leaf);
        assert_ne!(now, held_leaf);
        assert_eq!(
            now,
            XformCache::new(Time::Default).local_to_world(&scene, leaf)
        );
        live.apply(&mut store, &applied.inverse).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.invalidate_transform(reset);
        assert_eq!(cache.local_to_world(&scene, leaf), held_leaf);
    }

    #[test]
    fn transform_invalidation_keeps_sparse_locals_but_structural_removal_evicts() {
        let (mut store, mut live, [root, normal, reset, leaf, _]) = transform_scene();
        let mut cache = XformCache::new(Time::Default);
        let scene = Scene::new(live.stage(), &store);
        for path in [normal, leaf] {
            cache.local_transform(&scene, path).unwrap();
        }
        assert!(!cache.entries.contains_key(&root));
        assert!(!cache.entries.contains_key(&reset));
        let initial = cache.stats();
        cache.invalidate_transform(root);
        assert_eq!(cache.len(), 2);
        assert!(local(&cache, leaf).is_some());
        assert_eq!(
            cache.stats(),
            initial,
            "invalidation performs no property evaluation"
        );
        let mut tx = Transaction::new();
        tx.remove_spec(EditTarget::for_layer(LayerId(1)).prim(root));
        live.apply(&mut store, &tx).unwrap();
        cache.invalidate(&Scene::new(live.stage(), &store), root);
        assert!(cache.entries.is_empty());
        assert!(cache.namespace.as_ref().is_none_or(BTreeMap::is_empty));
    }

    #[test]
    fn unrelated_edits_validate_without_recomposing() {
        let (store, live, [root, normal, _, leaf, other]) = transform_scene();
        let scene = Scene::new(live.stage(), &store);
        let mut cache = XformCache::new(Time::Default);
        let expected = cache.local_to_world(&scene, normal);
        cache.local_to_world(&scene, leaf);
        let before = cache.stats();
        cache.invalidate_transform(other);
        assert_eq!(cache.local_to_world(&scene, normal), expected);
        assert_eq!(cache.stats().world_computed, before.world_computed);
        assert_eq!(cache.stats().local_computed, before.local_computed);
        assert_eq!(node(&cache, root).validated, cache.epoch);
        assert_eq!(node(&cache, normal).validated, cache.epoch);
        let mut cloned = cache.clone();
        cloned.invalidate_transform(root);
        assert_eq!(cloned.local_to_world(&scene, normal), expected);
        assert_eq!(cache.local_to_world(&scene, normal), expected);
        assert_ne!(cloned.epoch, cache.epoch);
    }

    #[test]
    fn batched_transform_stamps_match_fresh_results() {
        use alloc::vec;
        use layerstack::PropertyPath;
        let (mut store, mut live, paths) = transform_scene();
        let mut cache = XformCache::new(Time::Default);
        let translate = store.tokens.intern("xformOp:translate");
        let order = store.tokens.intern("xformOpOrder");
        let reset = store.tokens.intern(RESET_XFORM_STACK);
        // Several notifications before querying, including an ancestor and a
        // descendant, reset toggles, sparse reads, and interleaved local reads.
        for round in 0..60 {
            for edit in 0..3 {
                let path = paths[(round + edit * 2) % paths.len()];
                let mut tx = Transaction::new();
                tx.set_default(
                    EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(path, translate)),
                    Value::Vec3d([round as f64, edit as f64, -1.0]),
                );
                if edit == 1 {
                    let ops = if round % 2 == 0 {
                        vec![Value::Token(reset), Value::Token(translate)]
                    } else {
                        vec![Value::Token(translate)]
                    };
                    tx.set_default(
                        EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(path, order)),
                        Value::Array(ops),
                    );
                }
                live.apply(&mut store, &tx).unwrap();
                cache.invalidate_transform(path);
                if edit == 0 {
                    cache.local_transform(&Scene::new(live.stage(), &store), path);
                }
            }
            let scene = Scene::new(live.stage(), &store);
            for i in 0..paths.len() {
                let path = paths[(round + i) % paths.len()];
                assert_eq!(
                    cache.local_to_world(&scene, path),
                    XformCache::new(Time::Default).local_to_world(&scene, path),
                    "round {round}, path {path:?}",
                );
            }
        }
    }

    #[test]
    fn validation_clock_wrap_clears_old_stamps() {
        let (store, live, [root, normal, _, _, _]) = transform_scene();
        let scene = Scene::new(live.stage(), &store);
        let mut cache = XformCache::new(Time::Default);
        let expected = cache.local_to_world(&scene, normal);
        cache.epoch = u64::MAX;
        cache.invalidate_transform(root);
        assert!(cache.is_empty());
        assert_eq!(cache.epoch, 1);
        assert_eq!(cache.local_to_world(&scene, normal), expected);
        cache.clear();
        assert_eq!(cache.epoch, 0);
        assert_eq!(cache.local_to_world(&scene, normal), expected);
    }

    fn assert_arena_links(cache: &XformCache, scene: &Scene<'_>) {
        use alloc::collections::BTreeSet;
        assert_eq!(cache.nodes.len(), cache.transforms.len());
        if let Some(namespace) = &cache.namespace {
            assert_eq!(cache.entries.len(), namespace.len());
        }
        let mut seen = BTreeSet::new();
        for (&path, &slot) in &cache.entries {
            assert!(seen.insert(slot));
            let node = &cache.nodes[slot.get() - 1];
            assert_eq!(node.path, path);
            if let Some(namespace) = &cache.namespace {
                assert_eq!(namespace[scene.store().paths().resolve(path)], path);
            }
            if let Parent::Cached(parent) = node.parent {
                let parent_node = &cache.nodes[parent.get() - 1];
                assert_eq!(scene.parent(path), Some(parent_node.path));
                assert_eq!(cache.entries.get(&parent_node.path), Some(&parent));
            }
            assert!(!matches!(node.parent, Parent::Vacant(_)));
        }
        for slot in cache.prepared.by_slot.keys() {
            assert!(seen.contains(slot), "a recipe belongs to a live slot");
        }
        assert_eq!(
            cache.prepared.recipes.values().sum::<usize>(),
            cache.prepared.by_slot.len()
        );
        for (recipe, users) in &cache.prepared.recipes {
            assert_eq!(
                *users,
                cache
                    .prepared
                    .by_slot
                    .values()
                    .filter(|held| Arc::ptr_eq(held, recipe))
                    .count()
            );
        }
        let mut at = cache.free;
        while let Some(slot) = at {
            assert!(seen.insert(slot), "free list is disjoint and acyclic");
            let index = slot.get() - 1;
            assert!(cache.transforms[index].local.is_none());
            let Parent::Vacant(next) = cache.nodes[index].parent else {
                panic!("free slot is live")
            };
            at = next;
        }
        assert_eq!(seen.len(), cache.nodes.len());
    }

    #[test]
    fn retired_slots_can_host_unrelated_prims_before_undo() {
        for report in [false, true] {
            let (mut store, mut live, [root, normal, reset, leaf, other]) = transform_scene();
            let replacement = store.path("/Replacement");
            let mut cache = XformCache::new(Time::Default);
            for path in [normal, leaf, other] {
                cache
                    .local_to_world(&Scene::new(live.stage(), &store), path)
                    .unwrap();
            }
            let old_slots: Vec<_> = [root, normal, reset, leaf]
                .map(|path| cache.entries[&path])
                .into();
            let retained_other = cache.entries[&other];
            let mut tx = Transaction::new();
            tx.remove_spec(EditTarget::for_layer(LayerId(1)).prim(root));
            tx.create_prim(
                EditTarget::for_layer(LayerId(1)).prim(replacement),
                layerstack::Specifier::Def,
                None,
            );
            let applied = live.apply(&mut store, &tx).unwrap();
            let scene = Scene::new(live.stage(), &store);
            if report {
                cache.apply_changes(&scene, &applied.changes);
            } else {
                cache.invalidate(&scene, root);
            }
            assert_arena_links(&cache, &scene);
            assert_eq!(
                cache.local_to_world(&scene, replacement),
                Some(gf::IDENTITY)
            );
            // A report may conservatively resync the pseudo-root for this
            // create/remove batch. Manual invalidation keeps the other branch.
            assert!(old_slots.contains(&cache.entries[&replacement]));
            if !report {
                assert_eq!(cache.entries[&other], retained_other);
            }
            assert_arena_links(&cache, &scene);
            let restored = live.apply(&mut store, &applied.inverse).unwrap();
            let scene = Scene::new(live.stage(), &store);
            cache.apply_changes(&scene, &restored.changes);
            for path in [leaf, normal, other] {
                assert_eq!(
                    cache.local_to_world(&scene, path),
                    XformCache::new(Time::Default).local_to_world(&scene, path)
                );
            }
            assert_arena_links(&cache, &scene);
        }
    }

    #[test]
    fn sparse_queries_and_churn_keep_parent_links_and_storage_bounded() {
        let (store, live, [root, normal, reset, leaf, other]) = transform_scene();
        let scene = Scene::new(live.stage(), &store);
        let mut cache = XformCache::new(Time::Default);
        cache.local_transform(&scene, normal);
        assert!(matches!(node(&cache, normal).parent, Parent::Unresolved));
        assert!(!cache.entries.contains_key(&root));
        cache.local_to_world(&scene, normal);
        assert!(matches!(node(&cache, normal).parent, Parent::Cached(_)));
        for path in [leaf, other] {
            cache.local_to_world(&scene, path);
        }
        let capacity = (cache.nodes.capacity(), cache.transforms.capacity());
        let length = cache.nodes.len();
        for round in 0..100 {
            // Keep the other branch alive, forcing real free-list reuse instead
            // of the empty-cache shortcut. Include sparse local-only rebuilds.
            if round % 2 == 0 {
                cache.apply_changes(
                    &scene,
                    &layerstack::Changes {
                        resynced: alloc::vec![root],
                        ..layerstack::Changes::default()
                    },
                );
            } else {
                cache.invalidate(&scene, root);
            }
            cache.local_transform(&scene, leaf);
            cache.local_to_world(&scene, normal);
            cache.local_to_world(&scene, leaf);
            cache.invalidate_transform(root);
            for path in [other, normal, reset, leaf] {
                assert_eq!(
                    cache.local_to_world(&scene, path),
                    XformCache::new(Time::Default).local_to_world(&scene, path)
                );
            }
            assert_arena_links(&cache, &scene);
            assert_eq!(cache.nodes.len(), length);
            assert_eq!(
                (cache.nodes.capacity(), cache.transforms.capacity()),
                capacity
            );
        }
        let mut cloned = cache.clone();
        cloned.invalidate(&scene, root);
        assert_arena_links(&cloned, &scene);
        assert_arena_links(&cache, &scene);
        cache.set_time(Time::at(1.0));
        assert_eq!(cache.nodes.len(), length);
        assert_arena_links(&cache, &scene);
        assert_eq!(
            cache.local_to_world(&scene, normal),
            XformCache::new(Time::at(1.0)).local_to_world(&scene, normal)
        );
    }

    #[test]
    fn time_changes_retain_static_locals_and_follow_animated_ancestors() {
        let (mut store, mut live, [root, normal, reset, leaf, other]) = transform_scene();
        let translate = store.tokens.intern("xformOp:translate");
        let address = EditTarget::for_layer(LayerId(1))
            .property(layerstack::PropertyPath::new(root, translate));
        let mut tx = Transaction::new();
        tx.set_time_sample(address.clone(), 0.0, Value::Vec3d([10.0, 0.0, 0.0]));
        let single = live.apply(&mut store, &tx).unwrap();
        let mut cache = XformCache::new(Time::Default);
        let paths = [root, normal, reset, leaf, other];
        let scene = Scene::new(live.stage(), &store);
        for path in paths {
            cache.local_to_world(&scene, path);
        }
        let slots = paths.map(|path| cache.entries[&path]);
        let before = cache.stats();
        cache.set_time(Time::at(5.0));
        // Advancing time touches no node and evaluates nothing until queried.
        assert_eq!(cache.stats(), before);
        assert!(local(&cache, root).is_some());
        assert!(world(&cache, normal).is_some());
        for path in paths {
            assert_eq!(
                cache.local_to_world(&scene, path),
                XformCache::new(Time::at(5.0)).local_to_world(&scene, path)
            );
        }
        assert_eq!(cache.stats().local_computed - before.local_computed, 1);
        assert_eq!(cache.stats().world_computed - before.world_computed, 2);
        assert_eq!(paths.map(|path| cache.entries[&path]), slots);
        let before = cache.stats();
        cache.set_time(Time::at(5.0));
        assert_eq!(cache.stats(), before);
        assert_eq!(cache.epoch, node(&cache, normal).validated);
        let mut tx = Transaction::new();
        tx.set_time_sample(address, 10.0, Value::Vec3d([20.0, 0.0, 0.0]));
        let edited = live.apply(&mut store, &tx).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &edited.changes);
        for time in [
            Time::at(5.0),
            Time::held(5.0),
            Time::Default,
            Time::at(10.0),
        ] {
            cache.set_time(time);
            let mut fresh = XformCache::new(time);
            for path in paths {
                assert_eq!(
                    cache.local_to_world(&scene, path),
                    fresh.local_to_world(&scene, path)
                );
            }
            assert_arena_links(&cache, &scene);
        }
        // Remove all samples through authored undo; the cached classification
        // must become static again rather than retain historical animation.
        let undone = live.apply(&mut store, &edited.inverse).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &undone.changes);
        let undone = live.apply(&mut store, &single.inverse).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &undone.changes);
        for path in paths {
            cache.local_to_world(&scene, path);
        }
        assert_eq!(node(&cache, root).time_dependency, TimeDependency::Static);
        let before = cache.stats();
        cache.set_time(Time::Default);
        for path in paths {
            cache.local_to_world(&scene, path);
        }
        assert_eq!(cache.stats().local_computed, before.local_computed);
        assert_eq!(cache.stats().world_computed, before.world_computed);
        // Clock wrap is still an explicit complete reset.
        cache.epoch = u64::MAX;
        cache.set_time(Time::at(3.0));
        assert!(cache.is_empty());
        assert_eq!(
            cache.local_to_world(&scene, normal),
            XformCache::new(Time::at(3.0)).local_to_world(&scene, normal)
        );
    }

    #[test]
    fn equal_recipes_share_storage_and_retire_independently_across_clones() {
        let (mut store, mut live, paths) = transform_scene();
        let translate = store.tokens.intern("xformOp:translate");
        let mut tx = Transaction::new();
        for path in paths {
            let address = EditTarget::for_layer(LayerId(1))
                .property(layerstack::PropertyPath::new(path, translate));
            tx.set_time_sample(address, 0.0, Value::Vec3d([1.0; 3]));
        }
        live.apply(&mut store, &tx).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let mut cache = XformCache::new(Time::at(0.0));
        for path in paths {
            cache.local_to_world(&scene, path);
        }
        assert_eq!(cache.prepared.by_slot.len(), 5);
        assert_eq!(
            cache.prepared.recipes.len(),
            2,
            "reset and ordinary layouts"
        );
        assert_arena_links(&cache, &scene);
        let mut cloned = cache.clone();
        for path in paths {
            cache.invalidate_transform(path);
        }
        assert!(
            cache.prepared.recipes.is_empty(),
            "other clone cannot retain local dead recipes"
        );
        assert_eq!(cloned.prepared.recipes.len(), 2);
        for path in paths {
            cloned.invalidate(&scene, path);
        }
        assert!(cloned.prepared.recipes.is_empty());
        for path in paths {
            cache.local_to_world(&scene, path);
        }
        assert_eq!(cache.prepared.recipes.len(), 2);
        assert_arena_links(&cache, &scene);
    }

    #[test]
    fn prepared_recipes_follow_source_edits_and_retirement() {
        let (mut store, mut live, [root, normal, _, _, other]) = transform_scene();
        let translate = store.tokens.intern("xformOp:translate");
        let order = store.tokens.intern("xformOpOrder");
        let target = EditTarget::for_layer(LayerId(1));
        let mut tx = Transaction::new();
        tx.set_time_sample(
            target.property(layerstack::PropertyPath::new(root, translate)),
            0.0,
            Value::Vec3d([10.0, 0.0, 0.0]),
        );
        live.apply(&mut store, &tx).unwrap();
        let mut cache = XformCache::new(Time::at(0.0));
        cache.local_to_world(&Scene::new(live.stage(), &store), normal);
        assert_eq!(cache.prepared.by_slot.len(), 1);
        let capacity = cache.prepared.by_slot[&cache.entries[&root]].ops.as_ptr();
        for time in [Time::at(3.0), Time::held(2.0), Time::Default] {
            cache.set_time(time);
            let scene = Scene::new(live.stage(), &store);
            assert_eq!(
                cache.local_to_world(&scene, normal),
                XformCache::new(time).local_to_world(&scene, normal)
            );
            assert_eq!(
                cache.prepared.by_slot[&cache.entries[&root]].ops.as_ptr(),
                capacity
            );
        }
        let mut edit = Transaction::new();
        edit.set_default(
            target.property(layerstack::PropertyPath::new(root, order)),
            Value::Array(alloc::vec![
                Value::Token(translate),
                Value::Token(store.tokens.intern("!invert!xformOp:translate"))
            ]),
        );
        let edited = live.apply(&mut store, &edit).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &edited.changes);
        assert!(cache.prepared.by_slot.is_empty());
        cache.set_time(Time::at(4.0));
        assert_eq!(
            cache.local_transform(&scene, root).unwrap().matrix,
            gf::IDENTITY
        );
        let mut copied = cache.clone();
        copied.set_time(Time::at(5.0));
        assert_eq!(
            copied.local_to_world(&scene, normal),
            XformCache::new(Time::at(5.0)).local_to_world(&scene, normal)
        );
        let mut remove = Transaction::new();
        remove.remove_spec(target.prim(root));
        let removed = live.apply(&mut store, &remove).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &removed.changes);
        assert!(cache.prepared.by_slot.is_empty());
        cache.local_to_world(&scene, other);
        assert_arena_links(&cache, &scene);
        let restored = live.apply(&mut store, &removed.inverse).unwrap();
        let scene = Scene::new(live.stage(), &store);
        cache.apply_changes(&scene, &restored.changes);
        assert_eq!(
            cache.local_to_world(&scene, normal),
            XformCache::new(cache.time()).local_to_world(&scene, normal)
        );
        assert_arena_links(&cache, &scene);
    }

    #[test]
    fn deep_parent_walk_survives_vector_growth_and_is_iterative() {
        let mut store = InMemoryStore::default();
        let mut layer = Layer::new(LayerId(1));
        let mut name = String::new();
        let mut paths = Vec::new();
        for _ in 0..256 {
            name.push_str("/N");
            let path = store.path(&name);
            layer.insert_prim(path, PrimSpec::def());
            paths.push(path);
        }
        store.insert_layer(layer);
        let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let scene = Scene::new(live.stage(), &store);
        let mut cache = XformCache::new(Time::Default);
        let leaf = *paths.last().unwrap();
        assert_eq!(cache.local_to_world(&scene, leaf), Some(gf::IDENTITY));
        assert_eq!(cache.len(), 256);
        assert_arena_links(&cache, &scene);
        let before = cache.stats();
        cache.invalidate_transform(paths[128]);
        assert_eq!(cache.local_to_world(&scene, leaf), Some(gf::IDENTITY));
        assert_eq!(cache.stats().local_computed - before.local_computed, 1);
        assert_eq!(cache.stats().world_computed - before.world_computed, 128);
        assert_arena_links(&cache, &scene);
    }

    #[test]
    fn clear_and_clone_keep_namespace_index_in_sync() {
        let mut store = InMemoryStore::default();
        let path = store.path("/Root");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(path, PrimSpec::def());
        store.insert_layer(layer);
        let live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let scene = Scene::new(live.stage(), &store);
        let mut cache = XformCache::new(Time::Default);
        cache.local_to_world(&scene, path);
        let mut copied = cache.clone();
        copied.invalidate(&scene, path);
        assert!(copied.is_empty());
        assert_eq!(cache.len(), 1);
        cache.set_time(Time::at(1.0));
        assert!(cache.namespace.as_ref().is_none_or(BTreeMap::is_empty));
        cache.local_to_world(&scene, path);
        cache.invalidate(&scene, path);
        assert!(cache.is_empty());
    }
}
