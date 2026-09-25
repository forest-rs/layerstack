// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Repeated geometry: `UsdGeomPointInstancer`.

use alloc::vec::Vec;

use layerstack_usda::writer::Value;

use crate::{CustomAttribute, InstancerProblem, Mesh, Node, Transform, Xform};

/// Name of the `Scope` under a [`PointInstancer`] that holds its
/// prototypes.
pub const PROTOTYPES_SCOPE: &str = "Prototypes";

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
    pub name: &'a str,
    /// Local transform of the instancer relative to its parent prim,
    /// applied after (less locally than) every instance transform.
    pub transform: Option<Transform>,
    /// Prototype roots, in `protoIndices` order.
    pub prototypes: Vec<Node<'a>>,
    /// Prototype of each instance (`protoIndices`), an index into
    /// [`Self::prototypes`].
    pub proto_indices: &'a [u32],
    /// Position of each instance (`positions`), in the instancer's space.
    pub positions: &'a [[f32; 3]],
    /// Rotation of each instance (`orientations`), as unit quaternions
    /// `[x, y, z, w]` (imaginary part first, real part last). They are
    /// stored at half precision (`quath[]`), as `UsdGeomPointInstancer`
    /// defines them, so each component is rounded to the nearest `half`.
    pub orientations: Option<&'a [[f32; 4]]>,
    /// Scale of each instance along the prototype's axes (`scales`),
    /// applied before the rotation.
    pub scales: Option<&'a [[f32; 3]]>,
    /// Stable identifier of each instance (`ids`), unique within the
    /// instancer.
    pub ids: Option<&'a [i64]>,
    /// Custom attributes. Names of `PointInstancer` schema properties are
    /// rejected.
    pub attributes: Vec<CustomAttribute<'a>>,
}

impl<'a> PointInstancer<'a> {
    /// An instancer without prototypes, drawing `proto_indices[i]` at
    /// `positions[i]` for each instance `i`.
    pub fn new(name: &'a str, proto_indices: &'a [u32], positions: &'a [[f32; 3]]) -> Self {
        Self {
            name,
            transform: None,
            prototypes: Vec::new(),
            proto_indices,
            positions,
            orientations: None,
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
    pub fn with_orientations(mut self, orientations: &'a [[f32; 4]]) -> Self {
        self.orientations = Some(orientations);
        self
    }

    /// Sets the per-instance scales.
    #[must_use]
    pub fn with_scales(mut self, scales: &'a [[f32; 3]]) -> Self {
        self.scales = Some(scales);
        self
    }

    /// Sets the per-instance ids.
    #[must_use]
    pub fn with_ids(mut self, ids: &'a [i64]) -> Self {
        self.ids = Some(ids);
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
    pub fn with_attribute(mut self, name: &'a str, value: Value) -> Self {
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
    pub(crate) fn name(&self) -> &'a str {
        match self {
            Self::Xform(x) => x.name,
            Self::Mesh(m) => m.name,
            Self::PointInstancer(p) => p.name,
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
    /// `orientations` as half bits, `[i, j, k, r]`.
    pub(crate) orientations: Option<Vec<[u16; 4]>>,
}

/// Validates `instancer` as `UsdGeomPointInstancer` requires: indices in
/// range, one element per instance in every array, finite values, unit
/// orientations and unique ids. Nothing is authored unless it passes.
pub(crate) fn check(instancer: &PointInstancer<'_>) -> Result<Checked, InstancerProblem> {
    for attribute in &instancer.attributes {
        let name = attribute.name;
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
        ("orientations", instancer.orientations.map(<[_]>::len)),
        ("scales", instancer.scales.map(<[_]>::len)),
        ("ids", instancer.ids.map(<[_]>::len)),
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
    finite("positions", instancer.positions)?;
    finite("scales", instancer.scales.unwrap_or_default())?;
    let orientations = instancer
        .orientations
        .map(|quats| quats.iter().enumerate().map(orientation).collect())
        .transpose()?;
    if let Some(ids) = instancer.ids {
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
        orientations,
    })
}

fn finite(name: &'static str, values: &[[f32; 3]]) -> Result<(), InstancerProblem> {
    match values.iter().position(|v| !v.iter().all(|c| c.is_finite())) {
        Some(instance) => Err(InstancerProblem::NonFinite { name, instance }),
        None => Ok(()),
    }
}

/// Checks one `[x, y, z, w]` orientation and rounds it to `quath` bits,
/// which are stored in the same order (`GfQuath`'s `[i, j, k, r]`).
fn orientation((instance, q): (usize, &[f32; 4])) -> Result<[u16; 4], InstancerProblem> {
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
    Ok(q.map(layerstack::half::from_f32))
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
        Node::Mesh(mesh) => points_bounds(mesh.points),
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

/// The transform of one instance, before the prototype root's own
/// transform: scale, then rotation, then translation, as
/// `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime` builds it
/// (`pxr/usd/usdGeom/pointInstancer.cpp`), with the orientation read back
/// at the half precision it is stored with.
fn instance_matrix(
    position: [f32; 3],
    orientation: Option<[u16; 4]>,
    scale: Option<[f32; 3]>,
) -> Matrix {
    let mut m = Transform::IDENTITY.usd_rows();
    if let Some(s) = scale {
        for axis in 0..3 {
            m[axis][axis] = f64::from(s[axis]);
        }
    }
    if let Some(q) = orientation {
        let [x, y, z, r] = q.map(|bits| f64::from(layerstack::half::to_f32(bits)));
        // `GfMatrix4d::SetRotate(GfQuatd)` (`pxr/base/gf/matrix4d.cpp`).
        let rotation = [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y + z * r),
                2.0 * (z * x - y * r),
                0.0,
            ],
            [
                2.0 * (x * y - z * r),
                1.0 - 2.0 * (z * z + x * x),
                2.0 * (y * z + x * r),
                0.0,
            ],
            [
                2.0 * (z * x + y * r),
                2.0 * (y * z - x * r),
                1.0 - 2.0 * (y * y + x * x),
                0.0,
            ],
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
                checked.orientations.as_ref().map(|q| q[instance]),
                instancer.scales.map(|s| s[instance]),
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
