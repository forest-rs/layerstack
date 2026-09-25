// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A scattered field of trees, boulders and shrubs, for checking
//! `PointInstancer` export against OpenUSD.
//!
//! [`Field::scene`] writes it as one `PointInstancer` with three
//! prototypes; [`Field::duplicated_scene`] writes the same placements as
//! one transformed copy of the prototype per instance, the baseline that
//! instancing is measured against. [`Field::instance_transforms`] gives
//! the transform each instance should get, computed from the inputs, for
//! comparison with `UsdGeomPointInstancer::ComputeInstanceTransformsAtTime`.

use std::f64::consts::PI;

use layerstack_mesh_export::{
    Faces, FamilyType, Material, Mesh, Node, PointInstancer, Scene, StageSettings, Transform,
    UpAxis, Xform,
};

/// A square prism from `(-0.15, -0.15, 0)` to `(0.15, 0.15, 1)`.
const TRUNK_POINTS: [[f32; 3]; 8] = [
    [-0.15, -0.15, 0.0],
    [0.15, -0.15, 0.0],
    [0.15, 0.15, 0.0],
    [-0.15, 0.15, 0.0],
    [-0.15, -0.15, 1.0],
    [0.15, -0.15, 1.0],
    [0.15, 0.15, 1.0],
    [-0.15, 0.15, 1.0],
];
/// The six faces of a box with [`TRUNK_POINTS`]' corner order, outward
/// and counter-clockwise: -Z, +Z, -Y, +X, +Y, -X.
const BOX_COUNTS: [u32; 6] = [4; 6];
const BOX_INDICES: [u32; 24] = [
    0, 3, 2, 1, 4, 5, 6, 7, 0, 1, 5, 4, 1, 2, 6, 5, 2, 3, 7, 6, 3, 0, 4, 7,
];
const BOX: Faces<'static> = Faces::Polygons {
    counts: &BOX_COUNTS,
    indices: &BOX_INDICES,
};

/// A square pyramid: base at z = 0.8, apex at z = 2.6.
const CROWN_POINTS: [[f32; 3]; 5] = [
    [-0.7, -0.7, 0.8],
    [0.7, -0.7, 0.8],
    [0.7, 0.7, 0.8],
    [-0.7, 0.7, 0.8],
    [0.0, 0.0, 2.6],
];
const CROWN_COUNTS: [u32; 5] = [4, 3, 3, 3, 3];
const CROWN_INDICES: [u32; 16] = [0, 3, 2, 1, 0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4];
/// The crown's four sloped faces.
pub const CROWN_SIDES: [u32; 4] = [1, 2, 3, 4];
/// The crown's base.
pub const CROWN_BASE: [u32; 1] = [0];

/// A flattened octahedron.
const BOULDER_POINTS: [[f32; 3]; 6] = [
    [0.6, 0.0, 0.3],
    [-0.6, 0.0, 0.3],
    [0.0, 0.45, 0.3],
    [0.0, -0.45, 0.3],
    [0.0, 0.0, 0.75],
    [0.0, 0.0, 0.0],
];
const BOULDER_INDICES: [u32; 24] = [
    0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 4, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3, 5,
];

/// A unit box from `(-1, -1, 0)` to `(1, 1, 2)`, scaled down by the shrub
/// prototype's own transform.
const LEAVES_POINTS: [[f32; 3]; 8] = [
    [-1.0, -1.0, 0.0],
    [1.0, -1.0, 0.0],
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [-1.0, -1.0, 2.0],
    [1.0, -1.0, 2.0],
    [1.0, 1.0, 2.0],
    [-1.0, 1.0, 2.0],
];

/// Prototype indices: the order of [`prototypes`].
pub const PINE: u32 = 0;
/// See [`PINE`].
pub const BOULDER: u32 = 1;
/// See [`PINE`].
pub const SHRUB: u32 = 2;

/// Local transform of the shrub prototype's root: halve, then lift by 0.1.
pub fn shrub_transform() -> Transform {
    Transform::from_usd_rows([
        [0.5, 0.0, 0.0, 0.0],
        [0.0, 0.5, 0.0, 0.0],
        [0.0, 0.0, 0.4, 0.0],
        [0.0, 0.0, 0.1, 1.0],
    ])
}

/// The three prototypes, in [`PINE`], [`BOULDER`], [`SHRUB`] order:
///
/// - `Pine`, an `Xform` of a `Trunk` bound to `Bark` and a `Crown` whose
///   faces are partitioned between `Needles` (the sides) and `Shade` (the
///   base) by `GeomSubset`s;
/// - `Boulder`, a mesh without a material;
/// - `Shrub`, an `Xform` with its own transform ([`shrub_transform`])
///   around `Leaves` bound to `Needles`.
pub fn prototypes() -> Vec<Node<'static>> {
    let pine = Xform::new("Pine")
        .with_mesh(Mesh::new("Trunk", &TRUNK_POINTS, BOX).with_material("Bark"))
        .with_mesh(
            Mesh::new(
                "Crown",
                &CROWN_POINTS,
                Faces::Polygons {
                    counts: &CROWN_COUNTS,
                    indices: &CROWN_INDICES,
                },
            )
            .with_material_subset("Sides", &CROWN_SIDES, "Needles")
            .with_material_subset("Base", &CROWN_BASE, "Shade")
            .with_subset_family(FamilyType::Partition),
        );
    let boulder = Mesh::new(
        "Boulder",
        &BOULDER_POINTS,
        Faces::Triangles(&BOULDER_INDICES),
    );
    let shrub = Xform::new("Shrub")
        .with_transform(shrub_transform())
        .with_mesh(Mesh::new("Leaves", &LEAVES_POINTS, BOX).with_material("Needles"));
    vec![pine.into(), boulder.into(), shrub.into()]
}

/// The materials the prototypes bind.
pub fn materials() -> Vec<Material<'static>> {
    vec![
        Material::new("Bark")
            .with_diffuse_color([0.35, 0.2, 0.08])
            .with_roughness(0.9),
        Material::new("Needles")
            .with_diffuse_color([0.05, 0.45, 0.1])
            .with_roughness(0.7),
        Material::new("Shade")
            .with_diffuse_color([0.02, 0.2, 0.05])
            .with_roughness(0.7),
    ]
}

/// Per-instance arrays of a scattered field.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// Prototype of each instance.
    pub proto_indices: Vec<u32>,
    /// Position of each instance.
    pub positions: Vec<[f32; 3]>,
    /// Orientation of each instance, `[x, y, z, w]`.
    pub orientations: Vec<[f32; 4]>,
    /// Scale of each instance.
    pub scales: Vec<[f32; 3]>,
    /// Id of each instance: unique, not in instance order.
    pub ids: Vec<i64>,
}

/// Hamilton product `a * b` of `[x, y, z, w]` quaternions.
fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

impl Field {
    /// `columns * rows` instances on a jittered grid with 4 m spacing,
    /// centered on the origin: mostly pines, some boulders (tilted, with
    /// non-uniform scales) and shrubs, each turned about +Z.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the field is generated in f64 and exported as the f32 USD stores"
    )]
    pub fn new(columns: usize, rows: usize) -> Self {
        // A fixed linear congruential generator keeps the field
        // reproducible.
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 11) as f64 / (1_u64 << 53) as f64
        };
        let mut field = Self {
            proto_indices: Vec::new(),
            positions: Vec::new(),
            orientations: Vec::new(),
            scales: Vec::new(),
            ids: Vec::new(),
        };
        for row in 0..rows {
            for column in 0..columns {
                let pick = next();
                let proto = if pick < 0.55 {
                    PINE
                } else if pick < 0.8 {
                    BOULDER
                } else {
                    SHRUB
                };
                let x = (column as f64 - (columns as f64 - 1.0) / 2.0) * 4.0 + next() - 0.5;
                let y = (row as f64 - (rows as f64 - 1.0) / 2.0) * 4.0 + next() - 0.5;
                let yaw = next() * 2.0 * PI;
                let tilt = if proto == BOULDER {
                    (next() - 0.5) * 0.6
                } else {
                    0.0
                };
                let q = quat_mul(
                    [0.0, 0.0, (yaw / 2.0).sin(), (yaw / 2.0).cos()],
                    [(tilt / 2.0).sin(), 0.0, 0.0, (tilt / 2.0).cos()],
                );
                let size = 0.7 + 0.6 * next();
                let scale = match proto {
                    PINE => [size, size, size * (0.9 + 0.4 * next())],
                    BOULDER => [size * 1.4, size, size * (0.6 + 0.6 * next())],
                    _ => [size; 3],
                };
                let instance = field.ids.len() as i64;
                field.proto_indices.push(proto);
                field.positions.push([x as f32, y as f32, 0.0]);
                field.orientations.push(q.map(|c| c as f32));
                field.scales.push(scale.map(|c| c as f32));
                field.ids.push((instance * 7919) % 100_003 + 1_000);
            }
        }
        field
    }

    /// The field as one `PointInstancer` named `Field` under `/Root`.
    pub fn scene(&self) -> Scene<'_> {
        let mut instancer = PointInstancer::new("Field", &self.proto_indices, &self.positions)
            .with_orientations(&self.orientations)
            .with_scales(&self.scales)
            .with_ids(&self.ids);
        instancer.prototypes = prototypes();
        let mut scene = Scene::new(
            StageSettings::new(UpAxis::Z, 1.0),
            Xform::new("Root")
                .with_kind("assembly")
                .with_point_instancer(instancer),
        );
        scene.materials = materials();
        scene
    }

    /// Prim names for [`Self::duplicated_scene`], one per instance.
    pub fn instance_names(&self) -> Vec<String> {
        (0..self.proto_indices.len())
            .map(|i| format!("Instance_{i}"))
            .collect()
    }

    /// The same placements with the geometry duplicated: under
    /// `/Root/Field`, one `Xform` per instance, named by `names`
    /// ([`Self::instance_names`]) and carrying the instance's scale,
    /// rotation and translation, around a copy of its prototype.
    pub fn duplicated_scene<'a>(&self, names: &'a [String]) -> Scene<'a> {
        let prototypes = prototypes();
        let mut field = Xform::new("Field");
        for (i, name) in names.iter().enumerate() {
            let mut placed = Xform::new(name).with_transform(Transform::from_usd_rows(self.srt(i)));
            placed
                .children
                .push(prototypes[self.proto_indices[i] as usize].clone());
            field = field.with_xform(placed);
        }
        let mut scene = Scene::new(
            StageSettings::new(UpAxis::Z, 1.0),
            Xform::new("Root").with_kind("assembly").with_xform(field),
        );
        scene.materials = materials();
        scene
    }

    /// Instance `i`'s scale, rotation (at the half precision `quath`
    /// stores) and translation, `S · R · T` in USD's row-vector form.
    fn srt(&self, i: usize) -> [[f64; 4]; 4] {
        let [x, y, z, r] = self.orientations[i]
            .map(|c| f64::from(layerstack::half::to_f32(layerstack::half::from_f32(c))));
        // The rotation matrix of a unit quaternion, row-vector form.
        let rotation = [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y + z * r),
                2.0 * (x * z - y * r),
            ],
            [
                2.0 * (x * y - z * r),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z + x * r),
            ],
            [
                2.0 * (x * z + y * r),
                2.0 * (y * z - x * r),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ];
        let s = self.scales[i];
        let t = self.positions[i];
        let mut m = [[0.0; 4]; 4];
        for (row, rotation_row) in rotation.iter().enumerate() {
            for column in 0..3 {
                m[row][column] = f64::from(s[row]) * rotation_row[column];
            }
        }
        m[3] = [f64::from(t[0]), f64::from(t[1]), f64::from(t[2]), 1.0];
        m
    }

    /// The transform of each instance from its prototype root's space to
    /// the instancer's: the prototype root's own transform, then the
    /// instance's scale, rotation and translation (`UsdGeomPointInstancer`,
    /// "Computing an Instance Transform").
    pub fn instance_transforms(&self) -> Vec<[[f64; 4]; 4]> {
        (0..self.proto_indices.len())
            .map(|i| {
                let srt = self.srt(i);
                if self.proto_indices[i] != SHRUB {
                    return srt;
                }
                let p = shrub_transform().usd_rows();
                std::array::from_fn(|r| {
                    std::array::from_fn(|c| (0..4).map(|k| p[r][k] * srt[k][c]).sum())
                })
            })
            .collect()
    }
}
