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

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::ops::Bound;

use layerstack::{HashMap, Path, PathId, Value, half};

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
#[derive(Clone, Debug, PartialEq, Eq)]
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
    let mut problems = ordered.problems.clone();
    let mut matrix = gf::IDENTITY;
    let ops = &ordered.ops;
    let mut i = ops.len();
    while i > 0 {
        let op = &ops[i - 1];
        if i >= 2 {
            let next = &ops[i - 2];
            if next.attribute == op.attribute && next.inverse != op.inverse {
                i -= 2;
                continue;
            }
        }
        i -= 1;
        let Some(op_type) = op.op_type else {
            problems.push(XformProblem {
                op: op.name.into(),
                kind: XformProblemKind::UnknownOpType,
            });
            continue;
        };
        let Some(value) = values.get(i).and_then(Option::as_ref) else {
            problems.push(XformProblem {
                op: op.name.into(),
                kind: XformProblemKind::NoValue,
            });
            continue;
        };
        let op_matrix = match op_matrix(op_type, value, op.inverse) {
            Ok(m) => m,
            Err(kind) => {
                problems.push(XformProblem {
                    op: op.name.into(),
                    kind,
                });
                match (kind, value) {
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
        resets_xform_stack: ordered.resets_xform_stack,
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

#[derive(Clone, Debug, Default)]
struct Entry {
    local: Option<LocalTransform>,
    world: Option<Matrix4>,
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
/// After an edit, drop what it changed: a structural change
/// (`LiveStage::apply` reports subtree roots in `changes.resynced`) or an edit of a
/// prim's `xformOpOrder` or `xformOp:*` attributes changes that prim's
/// transform and its descendants' world transforms, which
/// [`XformCache::invalidate`] drops; [`XformCache::clear`] drops
/// everything. Nothing is invalidated by itself.
///
/// OpenUSD: `UsdGeomXformCache`.
#[derive(Clone, Debug)]
pub struct XformCache {
    time: Time,
    entries: HashMap<PathId, Entry>,
    // Path's segment ordering places a prefix and its descendants together.
    // This index also covers locally queried entries whose parents were never
    // cached, and survives deletion of the subtree from the composed stage.
    namespace: BTreeMap<Path, PathId>,
    stats: XformCacheStats,
}

impl XformCache {
    /// An empty cache of transforms at `time`.
    #[must_use]
    pub fn new(time: Time) -> Self {
        Self {
            time,
            entries: HashMap::new(),
            namespace: BTreeMap::new(),
            stats: XformCacheStats::default(),
        }
    }

    /// The time it holds transforms at.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }

    /// Holds transforms at `time` instead, dropping everything held.
    pub fn set_time(&mut self, time: Time) {
        self.time = time;
        self.clear();
    }

    /// Drops everything held, and the statistics.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.namespace.clear();
        self.stats = XformCacheStats::default();
    }

    /// Drops what it holds for `path` and its namespace descendants.
    ///
    /// An ordered namespace index limits the search to cached descendants,
    /// even when the subtree has already been removed from the stage. The
    /// index retains one owned path per cached prim.
    pub fn invalidate(&mut self, scene: &Scene<'_>, path: PathId) {
        let paths = scene.store().paths();
        let root = paths.resolve(path);
        let removed: Vec<_> = self
            .namespace
            .range::<Path, _>((Bound::Included(root), Bound::Unbounded))
            .take_while(|(held, _)| root.is_prefix_of(held))
            .map(|(_, id)| *id)
            .collect();
        for id in removed {
            self.namespace.remove(paths.resolve(id));
            self.entries.remove(&id);
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
        let time = self.time;
        let entry = self.entries.entry(path).or_insert_with(|| {
            self.namespace
                .insert(scene.store().paths().resolve(path).clone(), path);
            Entry::default()
        });
        if entry.local.is_some() {
            self.stats.hits += 1;
        } else {
            entry.local = Some(LocalTransformInputs::read(scene, path, time).evaluate());
            self.stats.local_computed += 1;
        }
        entry.local.as_ref()
    }

    /// The transform from the prim at `path` to the world, or `None` if no
    /// prim is there. The pseudo-root's is the identity.
    ///
    /// OpenUSD: `UsdGeomXformCache::GetLocalToWorldTransform`.
    pub fn local_to_world(&mut self, scene: &Scene<'_>, path: PathId) -> Option<[[f64; 4]; 4]> {
        if !scene.stage().has_prim(path) {
            return None;
        }
        // The prims up to the nearest ancestor whose world transform is
        // held, or the pseudo-root.
        let mut chain = Vec::new();
        let mut at = Some(path);
        let mut above = gf::IDENTITY;
        while let Some(current) = at {
            if scene.parent(current).is_none() {
                // The pseudo-root: the identity.
                break;
            }
            if let Some(world) = self.entries.get(&current).and_then(|e| e.world) {
                self.stats.hits += 1;
                above = world;
                break;
            }
            chain.push(current);
            // OpenUSD UsdGeomXformCache::_GetCtm does not query a parent
            // above a reset. Evaluate while walking so a cold reset subtree
            // does not populate unrelated ancestor transforms.
            if self.local_transform(scene, current)?.resets_xform_stack {
                break;
            }
            at = scene.parent(current);
        }
        for current in chain.into_iter().rev() {
            let world = self
                .entries
                .get(&current)?
                .local
                .as_ref()?
                .local_to_world(&above);
            self.entries.entry(current).or_default().world = Some(world);
            self.stats.world_computed += 1;
            above = world;
        }
        Some(above)
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

#[cfg(test)]
mod cache_tests {
    use super::*;
    use layerstack::{
        EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, StageOptions, Transaction,
    };

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
        assert_eq!(cache.namespace.len(), 2);
        live.apply(&mut store, &applied.inverse).unwrap();
        cache.local_transform(&Scene::new(live.stage(), &store), child);
        cache.invalidate(&Scene::new(live.stage(), &store), root);
        assert_eq!(cache.len(), 1);
        assert!(cache.entries.contains_key(&other));
        let pseudo = store.path("/");
        cache.invalidate(&Scene::new(live.stage(), &store), pseudo);
        assert!(cache.is_empty());
        assert!(cache.namespace.is_empty());
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
        assert!(cache.namespace.is_empty());
        cache.local_to_world(&scene, path);
        cache.invalidate(&scene, path);
        assert!(cache.is_empty());
    }
}
