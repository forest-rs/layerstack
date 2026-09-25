// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Repeated geometry: `UsdGeomPointInstancer`.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use layerstack_usda::writer::Value;

use crate::{CustomAttribute, InstancerProblem, Mesh, Node, Transform, Xform};

/// Name of the `Scope` under a [`PointInstancer`] that holds its
/// prototypes.
pub const PROTOTYPES_SCOPE: &str = "Prototypes";

/// The attributes that store a [`PointInstancer`]'s orientations.
///
/// `UsdGeomPointInstancer` has two: `orientations` (`quath[]`, half
/// precision, the original) and `orientationsf` (`quatf[]`, added in
/// OpenUSD 24.03). When `orientationsf` is authored and not empty, readers
/// that know it use it and ignore `orientations`
/// (`UsdGeomPointInstancer::UsesOrientationsf`,
/// `pxr/usd/usdGeom/pointInstancer.cpp`); older readers see only
/// `orientations`.
///
/// Half precision keeps about three significant digits: rounding a unit
/// quaternion moves a point a meter from the prototype's origin by up to
/// a millimeter or so ([`PointInstancer::half_orientation_error`]), which
/// shows as gaps between parts that should touch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OrientationPrecision {
    /// `orientationsf` only: exact, for OpenUSD 24.03 and later. The
    /// default, since those releases are what current tools ship, and a
    /// single authored rotation cannot disagree with itself.
    #[default]
    Float,
    /// `orientations` only: half the size, rounded to `half`.
    Half,
    /// Both: exact for readers that know `orientationsf`, rounded for
    /// older ones.
    FloatAndHalf,
}

impl OrientationPrecision {
    /// Whether `orientationsf` is authored.
    pub fn writes_float(self) -> bool {
        matches!(self, Self::Float | Self::FloatAndHalf)
    }

    /// Whether `orientations` is authored.
    pub fn writes_half(self) -> bool {
        matches!(self, Self::Half | Self::FloatAndHalf)
    }
}

/// Many placements of a few prototypes, written as a `PointInstancer` prim.
///
/// Each prototype is an ordinary [`Mesh`] or [`Xform`] subtree (or a nested
/// instancer), with its materials and face subsets. The prototypes are
/// written in order under a `Prototypes` scope ([`PROTOTYPES_SCOPE`]) below
/// the instancer, and the `prototypes` relationship targets them in that
/// order, so prototype `k` is the `k`-th one added. Instance `i` draws
/// prototype `proto_indices[i]` at `positions[i]`, rotated by
/// `orientations[i]` and scaled by `scales[i]` when those are given.
///
/// Every per-instance array has one element per instance, and is checked
/// before anything is written: indices must name a prototype, lengths must
/// agree, values must be finite, orientations unit length, and `ids`
/// unique. The instancer's `extent` is computed from the prototypes'
/// points and the instance transforms.
///
/// The export is static: values are authored as defaults, with no time
/// samples, and the motion attributes (`velocities`, `accelerations`,
/// `angularVelocities`) and masking (`invisibleIds`, the `inactiveIds`
/// metadata) are not authored. Custom attributes that would author them
/// are rejected ([`InstancerProblem::UnsupportedProperty`]).
///
/// Spec: `UsdGeomPointInstancer`
/// (<https://openusd.org/dev/api/class_usd_geom_point_instancer.html>;
/// `pxr/usd/usdGeom/pointInstancer.h`, "Computing an Instance Transform").
///
/// [`InstancerProblem::UnsupportedProperty`]: crate::InstancerProblem::UnsupportedProperty
#[derive(Clone, Debug, PartialEq)]
pub struct PointInstancer<'a> {
    /// Prim name; must be a USD identifier.
    pub name: Cow<'a, str>,
    /// Local transform of the instancer relative to its parent prim,
    /// applied after (less locally than) every instance transform.
    pub transform: Option<Transform>,
    /// Prototype roots, in `protoIndices` order.
    pub prototypes: Vec<Node<'a>>,
    /// Prototype of each instance (`protoIndices`), an index into
    /// [`Self::prototypes`].
    pub proto_indices: Cow<'a, [u32]>,
    /// Position of each instance (`positions`), in the instancer's space.
    pub positions: Cow<'a, [[f32; 3]]>,
    /// Rotation of each instance, as unit quaternions `[x, y, z, w]`
    /// (imaginary part first, real part last), authored as
    /// [`Self::orientation_precision`] says.
    pub orientations: Option<Cow<'a, [[f32; 4]]>>,
    /// Which attributes store [`Self::orientations`].
    pub orientation_precision: OrientationPrecision,
    /// Scale of each instance along the prototype's axes (`scales`),
    /// applied before the rotation.
    pub scales: Option<Cow<'a, [[f32; 3]]>>,
    /// Stable identifier of each instance (`ids`), unique within the
    /// instancer.
    pub ids: Option<Cow<'a, [i64]>>,
    /// Custom attributes. Names of `PointInstancer` schema properties are
    /// rejected.
    pub attributes: Vec<CustomAttribute<'a>>,
}

impl<'a> PointInstancer<'a> {
    /// An instancer without prototypes, drawing `proto_indices[i]` at
    /// `positions[i]` for each instance `i`.
    pub fn new(
        name: impl Into<Cow<'a, str>>,
        proto_indices: impl Into<Cow<'a, [u32]>>,
        positions: impl Into<Cow<'a, [[f32; 3]]>>,
    ) -> Self {
        Self {
            name: name.into(),
            transform: None,
            prototypes: Vec::new(),
            proto_indices: proto_indices.into(),
            positions: positions.into(),
            orientations: None,
            orientation_precision: OrientationPrecision::default(),
            scales: None,
            ids: None,
            attributes: Vec::new(),
        }
    }

    /// Appends a prototype; its index is the number of prototypes added
    /// before it.
    #[must_use]
    pub fn with_prototype(mut self, prototype: impl Into<Node<'a>>) -> Self {
        self.prototypes.push(prototype.into());
        self
    }

    /// Sets the per-instance orientations, as unit quaternions
    /// `[x, y, z, w]`.
    #[must_use]
    pub fn with_orientations(mut self, orientations: impl Into<Cow<'a, [[f32; 4]]>>) -> Self {
        self.orientations = Some(orientations.into());
        self
    }

    /// Sets which attributes store the orientations.
    #[must_use]
    pub fn with_orientation_precision(mut self, precision: OrientationPrecision) -> Self {
        self.orientation_precision = precision;
        self
    }

    /// The largest error that rounding the orientations to `half`
    /// (`quath`) introduces, as a fraction of distance from the prototype's
    /// origin: an instance point `d` away from it moves by at most
    /// `error * d` when a reader uses `orientations` instead of
    /// `orientationsf`. `0.0` without orientations.
    ///
    /// The bound is the Frobenius norm of the difference between the
    /// rotation matrices OpenUSD builds from the `float` and the `half`
    /// quaternion (`GfMatrix4d::SetRotate`, which does not renormalize),
    /// which is at least the spectral norm. Unit quaternions round to
    /// errors of about `1e-3`, so a 2 m wide prototype can be off by a
    /// millimeter or two. Orientations are not validated here.
    pub fn half_orientation_error(&self) -> f64 {
        self.orientations
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|q| {
                let exact = rotation(q.map(f64::from));
                let rounded =
                    rotation(q.map(|c| {
                        f64::from(layerstack::half::to_f32(layerstack::half::from_f32(c)))
                    }));
                let mut sum = 0.0;
                for (a, b) in exact.iter().zip(&rounded) {
                    for (x, y) in a.iter().zip(b) {
                        sum += (x - y) * (x - y);
                    }
                }
                libm::sqrt(sum)
            })
            .fold(0.0, f64::max)
    }

    /// Sets the per-instance scales.
    #[must_use]
    pub fn with_scales(mut self, scales: impl Into<Cow<'a, [[f32; 3]]>>) -> Self {
        self.scales = Some(scales.into());
        self
    }

    /// Sets the per-instance ids.
    #[must_use]
    pub fn with_ids(mut self, ids: impl Into<Cow<'a, [i64]>>) -> Self {
        self.ids = Some(ids.into());
        self
    }

    /// Sets the instancer's local transform.
    #[must_use]
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = Some(transform);
        self
    }

    /// Adds a custom attribute.
    #[must_use]
    pub fn with_attribute(mut self, name: impl Into<Cow<'a, str>>, value: Value) -> Self {
        self.attributes.push(CustomAttribute::new(name, value));
        self
    }
}

impl<'a> From<Mesh<'a>> for Node<'a> {
    fn from(mesh: Mesh<'a>) -> Self {
        Self::Mesh(mesh)
    }
}

impl<'a> From<Xform<'a>> for Node<'a> {
    fn from(xform: Xform<'a>) -> Self {
        Self::Xform(xform)
    }
}

impl<'a> From<PointInstancer<'a>> for Node<'a> {
    fn from(instancer: PointInstancer<'a>) -> Self {
        Self::PointInstancer(instancer)
    }
}

impl<'a> Node<'a> {
    /// The node's prim name.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Xform(x) => &x.name,
            Self::Mesh(m) => &m.name,
            Self::PointInstancer(p) => &p.name,
        }
    }

    /// The node's local transform.
    fn transform(&self) -> Option<Transform> {
        match self {
            Self::Xform(x) => x.transform,
            Self::Mesh(m) => m.transform,
            Self::PointInstancer(p) => p.transform,
        }
    }
}

/// Custom attribute names that would author motion or masking, which the
/// static export does not support (`pxr/usd/usdGeom/pointInstancer.h`:
/// `velocities`, `accelerations`, `angularVelocities`, `invisibleIds`).
const UNSUPPORTED: [&str; 4] = [
    "velocities",
    "accelerations",
    "angularVelocities",
    "invisibleIds",
];

/// Schema properties the exporter authors itself, or that would override
/// what it authors (`orientationsf` takes precedence over `orientations`
/// when authored).
const RESERVED: [&str; 9] = [
    "prototypes",
    "protoIndices",
    "ids",
    "positions",
    "orientations",
    "orientationsf",
    "scales",
    "extent",
    "xformOpOrder",
];

/// The per-instance arrays in their authored form, after validation.
pub(crate) struct Checked {
    /// `protoIndices` as USD `int`s.
    pub(crate) proto_indices: Vec<i32>,
    /// `orientations` as half bits, `[i, j, k, r]`, when authored.
    pub(crate) half_orientations: Option<Vec<[u16; 4]>>,
    /// The rotation readers apply to each instance, `[x, y, z, w]`: the
    /// `float` quaternion when `orientationsf` is authored, else the
    /// `half` one read back.
    pub(crate) rotations: Option<Vec<[f64; 4]>>,
}

/// Validates `instancer` as `UsdGeomPointInstancer` requires: indices in
/// range, one element per instance in every array, finite values, unit
/// orientations and unique ids. Nothing is authored unless it passes.
pub(crate) fn check(instancer: &PointInstancer<'_>) -> Result<Checked, InstancerProblem> {
    for attribute in &instancer.attributes {
        let name = &*attribute.name;
        if UNSUPPORTED.contains(&name) {
            return Err(InstancerProblem::UnsupportedProperty { name: name.into() });
        }
        if RESERVED.contains(&name) || name.starts_with("xformOp:") {
            return Err(InstancerProblem::ReservedProperty { name: name.into() });
        }
    }
    let prototypes = instancer.prototypes.len();
    if prototypes == 0 {
        return Err(InstancerProblem::NoPrototypes);
    }
    let instances = instancer.proto_indices.len();
    let lengths = [
        ("positions", Some(instancer.positions.len())),
        (
            "orientations",
            instancer.orientations.as_deref().map(<[_]>::len),
        ),
        ("scales", instancer.scales.as_deref().map(<[_]>::len)),
        ("ids", instancer.ids.as_deref().map(<[_]>::len)),
    ];
    for (name, len) in lengths {
        if let Some(actual) = len.filter(|&len| len != instances) {
            return Err(InstancerProblem::LengthMismatch {
                name,
                expected: instances,
                actual,
            });
        }
    }
    let proto_indices = instancer
        .proto_indices
        .iter()
        .enumerate()
        .map(|(instance, &index)| {
            i32::try_from(index)
                .ok()
                .filter(|_| (index as usize) < prototypes)
                .ok_or(InstancerProblem::ProtoIndexOutOfRange {
                    instance,
                    index,
                    prototypes,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    finite("positions", &instancer.positions)?;
    finite("scales", instancer.scales.as_deref().unwrap_or_default())?;
    let orientations: Option<&[[f32; 4]]> = instancer.orientations.as_deref();
    if let Some(quats) = orientations {
        quats.iter().enumerate().try_for_each(orientation)?;
    }
    let precision = instancer.orientation_precision;
    let half_orientations = orientations
        .filter(|_| precision.writes_half())
        .map(|quats| {
            quats
                .iter()
                .map(|q| q.map(layerstack::half::from_f32))
                .collect()
        });
    let rotations = orientations.map(|quats| {
        quats
            .iter()
            .map(|q| {
                if precision.writes_float() {
                    q.map(f64::from)
                } else {
                    q.map(|c| f64::from(layerstack::half::to_f32(layerstack::half::from_f32(c))))
                }
            })
            .collect()
    });
    if let Some(ids) = &instancer.ids {
        let mut sorted: Vec<(i64, usize)> = ids.iter().copied().zip(0..).collect();
        sorted.sort_unstable();
        if let Some(pair) = sorted.windows(2).find(|w| w[0].0 == w[1].0) {
            return Err(InstancerProblem::DuplicateId {
                id: pair[0].0,
                first: pair[0].1,
                second: pair[1].1,
            });
        }
    }
    Ok(Checked {
        proto_indices,
        half_orientations,
        rotations,
    })
}

fn finite(name: &'static str, values: &[[f32; 3]]) -> Result<(), InstancerProblem> {
    match values.iter().position(|v| !v.iter().all(|c| c.is_finite())) {
        Some(instance) => Err(InstancerProblem::NonFinite { name, instance }),
        None => Ok(()),
    }
}

/// Checks one `[x, y, z, w]` orientation: finite and unit length.
fn orientation((instance, q): (usize, &[f32; 4])) -> Result<(), InstancerProblem> {
    if !q.iter().all(|c| c.is_finite()) {
        return Err(InstancerProblem::NonFinite {
            name: "orientations",
            instance,
        });
    }
    let norm2: f32 = q.iter().map(|c| c * c).sum();
    if (norm2 - 1.0).abs() > 1e-3 {
        return Err(InstancerProblem::NonUnitOrientation { instance });
    }
    Ok(())
}

// ── Bounds ──────────────────────────────────────────────────────────────

/// A row-vector affine matrix (`p' = p · M`), as USD composes transforms.
type Matrix = [[f64; 4]; 4];

/// An axis-aligned box `[min, max]`.
pub(crate) type Aabb = [[f64; 3]; 2];

fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    core::array::from_fn(|i| core::array::from_fn(|j| (0..4).map(|k| a[i][k] * b[k][j]).sum()))
}

fn matrix(transform: Option<Transform>) -> Matrix {
    transform.unwrap_or(Transform::IDENTITY).usd_rows()
}

/// A box of a prototype's geometry and the matrix from its space to the
/// prototype root's.
struct Leaf {
    bounds: Aabb,
    to_root: Matrix,
}

/// The boxes of `node`'s geometry, each with the matrix from its space to
/// the prototype root's: each mesh's `points` bounds, and each nested
/// instancer's extent.
fn leaves(node: &Node<'_>, to_root: Matrix, out: &mut Vec<Leaf>) {
    let bounds = match node {
        Node::Xform(xform) => {
            for child in &xform.children {
                leaves(child, mul(&matrix(child.transform()), &to_root), out);
            }
            return;
        }
        Node::Mesh(mesh) => points_bounds(&mesh.points),
        Node::PointInstancer(instancer) => check(instancer)
            .ok()
            .and_then(|checked| extent(instancer, &checked)),
    };
    if let Some(bounds) = bounds {
        out.push(Leaf { bounds, to_root });
    }
}

fn points_bounds(points: &[[f32; 3]]) -> Option<Aabb> {
    let mut bounds: Option<Aabb> = None;
    for p in points {
        include(&mut bounds, p.map(f64::from));
    }
    bounds
}

fn include(bounds: &mut Option<Aabb>, p: [f64; 3]) {
    let [lo, hi] = bounds.get_or_insert([p, p]);
    for axis in 0..3 {
        lo[axis] = lo[axis].min(p[axis]);
        hi[axis] = hi[axis].max(p[axis]);
    }
}

/// The 3×3 rotation `GfMatrix4d::SetRotate(GfQuatd)` builds from an
/// `[x, y, z, w]` quaternion, in row-vector form, without renormalizing
/// (`pxr/base/gf/matrix4d.cpp`).
pub(crate) fn rotation([x, y, z, r]: [f64; 4]) -> [[f64; 3]; 3] {
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + z * r),
            2.0 * (z * x - y * r),
        ],
        [
            2.0 * (x * y - z * r),
            1.0 - 2.0 * (z * z + x * x),
            2.0 * (y * z + x * r),
        ],
        [
            2.0 * (z * x + y * r),
            2.0 * (y * z - x * r),
            1.0 - 2.0 * (y * y + x * x),
        ],
    ]
}

/// The transform of one instance, before the prototype root's own
/// transform: scale, then rotation, then translation, as
/// `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime` builds it
/// (`pxr/usd/usdGeom/pointInstancer.cpp`), with the rotation readers
/// apply ([`Checked::rotations`]).
pub(crate) fn instance_matrix(
    position: [f32; 3],
    orientation: Option<[f64; 4]>,
    scale: Option<[f32; 3]>,
) -> Matrix {
    let mut m = Transform::IDENTITY.usd_rows();
    if let Some(s) = scale {
        for axis in 0..3 {
            m[axis][axis] = f64::from(s[axis]);
        }
    }
    if let Some(q) = orientation {
        let r = rotation(q);
        let rotation = [
            [r[0][0], r[0][1], r[0][2], 0.0],
            [r[1][0], r[1][1], r[1][2], 0.0],
            [r[2][0], r[2][1], r[2][2], 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        m = mul(&m, &rotation);
    }
    m[3] = [
        f64::from(position[0]),
        f64::from(position[1]),
        f64::from(position[2]),
        1.0,
    ];
    m
}

/// The instancer's `extent`: the axis-aligned box, in the instancer's own
/// space, of every instance's prototype geometry. Each prototype mesh's
/// point bounds (and each nested instancer's extent) is carried through the
/// prototype's transforms and the instance transform corner by corner.
/// `None` when nothing is drawn.
///
/// Spec: `pxr/usd/usdGeom/boundable.h` (`extent` is in local space,
/// without the prim's own transform); `pxr/usd/usdGeom/pointInstancer.h`,
/// `ComputeExtentAtTime`.
pub(crate) fn extent(instancer: &PointInstancer<'_>, checked: &Checked) -> Option<Aabb> {
    let prototypes: Vec<(Matrix, Vec<Leaf>)> = instancer
        .prototypes
        .iter()
        .map(|prototype| {
            let mut out = Vec::new();
            leaves(prototype, Transform::IDENTITY.usd_rows(), &mut out);
            (matrix(prototype.transform()), out)
        })
        .collect();
    let mut bounds: Option<Aabb> = None;
    for (instance, &index) in checked.proto_indices.iter().enumerate() {
        let (root, leaves) = &prototypes[index as usize];
        if leaves.is_empty() {
            continue;
        }
        let placed = mul(
            root,
            &instance_matrix(
                instancer.positions[instance],
                checked.rotations.as_ref().map(|q| q[instance]),
                instancer.scales.as_deref().map(|s| s[instance]),
            ),
        );
        for leaf in leaves {
            let m = mul(&leaf.to_root, &placed);
            for corner in 0..8 {
                let p: [f64; 3] =
                    core::array::from_fn(|axis| leaf.bounds[(corner >> axis) & 1][axis]);
                include(
                    &mut bounds,
                    core::array::from_fn(|j| {
                        p[0] * m[0][j] + p[1] * m[1][j] + p[2] * m[2][j] + m[3][j]
                    }),
                );
            }
        }
    }
    bounds
}
