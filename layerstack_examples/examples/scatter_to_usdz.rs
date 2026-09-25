// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scatter trees and rocks over a field and write them to a `.usdz`
//! package as one `PointInstancer`.
//!
//! Each prototype is written once: a tree (an `Xform` of a trunk and a
//! crown, each with its own material) and a rock (a bare mesh). Every
//! placement is only an index into the prototypes, a position, a turn
//! about +Z, a scale and a stable id, so a thousand trees cost a few
//! numbers each rather than a copy of their geometry.
//!
//! The package uses the `ARKit` / AR Quick Look profile (a single USDC root
//! layer) unless `generic` is given, which writes a USDA root layer.
//!
//! ```sh
//! cargo run -p layerstack_examples --example scatter_to_usdz -- field.usdz [arkit|generic]
//! usdchecker field.usdz   # optional, if OpenUSD is installed
//! ```

use layerstack_mesh_export::{
    Faces, Material, Mesh, PointInstancer, Scene, StageSettings, UpAxis, UsdzProfile, Xform,
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

    // A 30 x 30 grid, 3 m apart, jittered by a small fixed hash so the
    // output is reproducible; every fifth placement is a rock.
    let mut proto_indices = Vec::new();
    let mut positions = Vec::new();
    let mut orientations = Vec::new();
    let mut scales = Vec::new();
    let mut ids = Vec::new();
    for row in 0..30_u16 {
        for column in 0..30_u16 {
            let i = u32::from(row) * 30 + u32::from(column);
            let hash = i.wrapping_mul(2_654_435_761).to_le_bytes();
            let unit = |byte: usize| f32::from(hash[byte]) / 255.0;
            proto_indices.push(if i % 5 == 0 { ROCKS } else { TREE });
            positions.push([
                (f32::from(column) - 14.5) * 3.0 + unit(0) - 0.5,
                (f32::from(row) - 14.5) * 3.0 + unit(1) - 0.5,
                0.0,
            ]);
            // A turn about +Z as a unit quaternion `[x, y, z, w]`.
            let half_angle = unit(2) * std::f32::consts::PI;
            orientations.push([0.0, 0.0, half_angle.sin(), half_angle.cos()]);
            let size = 0.7 + 0.6 * unit(3);
            scales.push([size, size, size]);
            // Stable ids, such as the placement's key in the source model.
            ids.push(10_000 + i64::from(i));
        }
    }

    let tree = Xform::new("Tree")
        .with_mesh(
            Mesh::new(
                "Trunk",
                &TRUNK,
                Faces::Polygons {
                    counts: &[4; 6],
                    indices: &BOX_INDICES,
                },
            )
            .with_material("Bark"),
        )
        .with_mesh(
            Mesh::new(
                "Crown",
                &CROWN,
                Faces::Polygons {
                    counts: &CROWN_COUNTS,
                    indices: &CROWN_INDICES,
                },
            )
            .with_material("Needles"),
        );
    let rock = Mesh::new("Rock", &ROCK, Faces::Triangles(&ROCK_INDICES)).with_material("Stone");
    // Prototype order is the `protoIndices` numbering: the tree is 0.
    let field = PointInstancer::new("Field", &proto_indices, &positions)
        .with_prototype(tree)
        .with_prototype(rock)
        .with_orientations(&orientations)
        .with_scales(&scales)
        .with_ids(&ids);
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
        usda.contains("quath[] orientations = ["),
        "orientations are half-precision quaternions"
    );

    let bytes = scene.to_usdz(profile, &[])?;
    std::fs::write(&out, &bytes)?;
    let rocks = proto_indices.iter().filter(|&&p| p == ROCKS).count();
    println!(
        "wrote {out} ({profile:?} profile, root layer {}): {} bytes, {} trees and {rocks} rocks",
        profile.root_layer_path(),
        bytes.len(),
        proto_indices.len() - rocks,
    );
    Ok(())
}
