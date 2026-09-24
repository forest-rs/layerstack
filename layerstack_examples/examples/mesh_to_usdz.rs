// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Write a unit cube with normals and UVs to a `.usdz` package.
//!
//! The cube shares its 8 corner points between faces. Normals and UVs are
//! `faceVarying` (one value per face corner), which is how hard edges and
//! UV seams are expressed without duplicating points: every face is its own
//! UV island, so no face-varying indices are shared across an edge.
//!
//! ```sh
//! cargo run -p layerstack_examples --example mesh_to_usdz -- cube.usdz
//! usdchecker cube.usdz   # optional, if OpenUSD is installed
//! ```

use layerstack_mesh_export::{
    Faces, Mesh, Primvar, Scene, StageSettings, Transform, UpAxis, Xform,
};

const POINTS: [[f32; 3]; 8] = [
    [-0.5, -0.5, -0.5],
    [0.5, -0.5, -0.5],
    [0.5, 0.5, -0.5],
    [-0.5, 0.5, -0.5],
    [-0.5, -0.5, 0.5],
    [0.5, -0.5, 0.5],
    [0.5, 0.5, 0.5],
    [-0.5, 0.5, 0.5],
];

/// Six counter-clockwise quads (seen from outside), matching USD's
/// `rightHanded` orientation.
const FACES: [[u32; 4]; 6] = [
    [0, 3, 2, 1], // -Z
    [4, 5, 6, 7], // +Z
    [0, 1, 5, 4], // -Y
    [2, 3, 7, 6], // +Y
    [1, 2, 6, 5], // +X
    [3, 0, 4, 7], // -X
];

const FACE_NORMALS: [[f32; 3]; 6] = [
    [0.0, 0.0, -1.0],
    [0.0, 0.0, 1.0],
    [0.0, -1.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
];

const UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "cube.usdz".into());

    let counts = [4_u32; 6];
    let indices: Vec<u32> = FACES.iter().flatten().copied().collect();
    // One normal per face corner: each face's flat normal, four times.
    let normals: Vec<[f32; 3]> = FACE_NORMALS
        .iter()
        .flat_map(|n| std::iter::repeat_n(*n, 4))
        .collect();
    // Each face maps its corners to the unit square (its own UV island).
    let uvs: Vec<[f32; 2]> = (0..6).flat_map(|_| UVS).collect();

    let cube = Mesh::new(
        "Cube",
        &POINTS,
        Faces::Polygons {
            counts: &counts,
            indices: &indices,
        },
    )
    .with_normals(Primvar::face_varying(&normals[..]))
    .with_uvs(Primvar::face_varying(&uvs[..]));

    // Lift the cube so it rests on the ground plane (Z up, meters).
    let root = Xform::new("Root")
        .with_kind("component")
        .with_transform(Transform::from_translation([0.0, 0.0, 0.5]))
        .with_mesh(cube);
    let scene = Scene::new(StageSettings::new(UpAxis::Z, 1.0), root);

    let bytes = scene.to_usdz(&[])?;
    std::fs::write(&out, &bytes)?;
    println!(
        "wrote {out}: {} bytes, {} points, {} faces, {} face corners",
        bytes.len(),
        POINTS.len(),
        counts.len(),
        indices.len()
    );
    Ok(())
}
