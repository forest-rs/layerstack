// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scatter trees and rocks over a field and write them to a `.usdz`
//! package as one `PointInstancer`.
//!
//! Each prototype is written once: a tree (an `Xform` of a trunk and a
//! crown, each with its own material) and a rock (a bare mesh). Every
//! placement starts as an affine transform, which
//! `PointInstancer::push_affine` splits into a position, a rotation and a
//! (possibly mirrored) scale; with a stable id, that is all an instance
//! costs, so a thousand trees are a few numbers each rather than a copy of
//! their geometry.
//!
//! The package uses the `ARKit` / AR Quick Look profile (a single USDC root
//! layer) unless `generic` is given, which writes a USDA root layer. Apple's
//! viewers do not draw `PointInstancer` instances, so the `ARKit` package
//! writes each instance as a reference to its prototype instead; the
//! generic one keeps the `PointInstancer`.
//!
//! ```sh
//! cargo run -p layerstack_examples --example scatter_to_usdz -- field.usdz [arkit|generic]
//! usdchecker field.usdz   # optional, if OpenUSD is installed
//! ```

use layerstack_mesh_export::{
    Faces, Material, Mesh, PointInstancer, Scene, StageSettings, Transform, UpAxis, UsdzProfile,
    Xform,
};

/// A square trunk, 0.3 m wide and 1 m tall, standing on the origin.
const TRUNK: [[f32; 3]; 8] = [
    [-0.15, -0.15, 0.0],
    [0.15, -0.15, 0.0],
    [0.15, 0.15, 0.0],
    [-0.15, 0.15, 0.0],
    [-0.15, -0.15, 1.0],
    [0.15, -0.15, 1.0],
    [0.15, 0.15, 1.0],
    [-0.15, 0.15, 1.0],
];
/// Six counter-clockwise quads (seen from outside): -Z, +Z, -Y, +X, +Y, -X.
const BOX_INDICES: [u32; 24] = [
    0, 3, 2, 1, 4, 5, 6, 7, 0, 1, 5, 4, 1, 2, 6, 5, 2, 3, 7, 6, 3, 0, 4, 7,
];

/// A square pyramid crown from z = 0.8 to z = 2.6.
const CROWN: [[f32; 3]; 5] = [
    [-0.7, -0.7, 0.8],
    [0.7, -0.7, 0.8],
    [0.7, 0.7, 0.8],
    [-0.7, 0.7, 0.8],
    [0.0, 0.0, 2.6],
];
const CROWN_COUNTS: [u32; 5] = [4, 3, 3, 3, 3];
const CROWN_INDICES: [u32; 16] = [0, 3, 2, 1, 0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4];

/// A flattened octahedron.
const ROCK: [[f32; 3]; 6] = [
    [0.6, 0.0, 0.3],
    [-0.6, 0.0, 0.3],
    [0.0, 0.45, 0.3],
    [0.0, -0.45, 0.3],
    [0.0, 0.0, 0.75],
    [0.0, 0.0, 0.0],
];
const ROCK_INDICES: [u32; 24] = [
    0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 4, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3, 5,
];

const TREE: u32 = 0;
const ROCKS: u32 = 1;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "field.usdz".into());
    let profile = match args.next().as_deref() {
        None | Some("arkit") => UsdzProfile::Arkit,
        Some("generic") => UsdzProfile::Generic,
        Some(other) => return Err(format!("unknown profile {other:?}").into()),
    };

    let tree = Xform::new("Tree")
        .with_mesh(
            Mesh::new("Trunk", &TRUNK, Faces::polygons(&[4; 6], &BOX_INDICES))
                .with_material("Bark"),
        )
        .with_mesh(
            Mesh::new(
                "Crown",
                &CROWN,
                Faces::polygons(&CROWN_COUNTS, &CROWN_INDICES),
            )
            .with_material("Needles"),
        );
    let rock = Mesh::new("Rock", &ROCK, Faces::triangles(&ROCK_INDICES)).with_material("Stone");
    // Prototype order is the `protoIndices` numbering: the tree is 0.
    let mut field = PointInstancer::new("Field", Vec::new(), Vec::new())
        .with_prototype(tree)
        .with_prototype(rock);

    // A 30 x 30 grid, 3 m apart, jittered by a small fixed hash so the
    // output is reproducible; every fifth placement is a rock. Each
    // placement is the affine transform a modeling kernel would hold,
    // column-vector rows `[R · S | t]`; `push_affine` splits it into the
    // instancer's position, orientation and scale.
    let mut ids = Vec::new();
    for row in 0..30_u16 {
        for column in 0..30_u16 {
            let i = u32::from(row) * 30 + u32::from(column);
            let hash = i.wrapping_mul(2_654_435_761).to_le_bytes();
            let unit = |byte: usize| f64::from(hash[byte]) / 255.0;
            let rock = i % 5 == 0;
            let (sin, cos) = (unit(2) * std::f64::consts::TAU).sin_cos();
            let size = 0.7 + 0.6 * unit(3);
            // Rocks are stretched along their own X, and every other one
            // is mirrored, which becomes a negative scale.
            let (sx, sy) = if rock {
                let mirror = if i % 2 == 0 { -1.0 } else { 1.0 };
                (mirror * size * 1.4, size)
            } else {
                (size, size)
            };
            let x = (f64::from(column) - 14.5) * 3.0 + unit(0) - 0.5;
            let y = (f64::from(row) - 14.5) * 3.0 + unit(1) - 0.5;
            let placement = Transform::from_affine_3x4([
                [cos * sx, -sin * sy, 0.0, x],
                [sin * sx, cos * sy, 0.0, y],
                [0.0, 0.0, size, 0.0],
            ]);
            field.push_affine(if rock { ROCKS } else { TREE }, &placement)?;
            // Stable ids, such as the placement's key in the source model.
            ids.push(10_000 + i64::from(i));
        }
    }
    let field = field.with_ids(ids);
    let rocks = field.proto_indices.iter().filter(|&&p| p == ROCKS).count();
    let instances = field.proto_indices.len();
    let scene = Scene::new(
        StageSettings::new(UpAxis::Z, 1.0),
        Xform::new("Root")
            .with_kind("assembly")
            .with_point_instancer(field),
    )
    .with_material(Material::new("Bark").with_diffuse_color([0.35, 0.2, 0.08]))
    .with_material(Material::new("Needles").with_diffuse_color([0.05, 0.45, 0.1]))
    .with_material(Material::new("Stone").with_diffuse_color([0.5, 0.5, 0.48]));

    let usda = scene.to_usda()?;
    assert!(
        usda.contains(
            "rel prototypes = [</Root/Field/Prototypes/Tree>, </Root/Field/Prototypes/Rock>]"
        ),
        "the prototypes relationship keeps the prototype order"
    );
    assert!(
        usda.contains("quatf[] orientationsf = ["),
        "orientations are single-precision quaternions"
    );
    assert!(
        usda.contains("float3[] scales = [(-"),
        "the first rock is mirrored"
    );

    let bytes = scene.to_usdz(profile, &[])?;
    std::fs::write(&out, &bytes)?;
    println!(
        "wrote {out} ({profile:?} profile, root layer {}): {} bytes, {} trees and {rocks} rocks",
        profile.root_layer_path(),
        bytes.len(),
        instances - rocks,
    );
    Ok(())
}
