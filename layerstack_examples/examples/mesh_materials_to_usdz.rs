// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Write a cube with two materials, one of them textured, to a `.usdz`.
//!
//! The cube is the one from `mesh_to_usdz`. Its faces are split between
//! two materials by a `materialBind` partition of `GeomSubset`s:
//!
//! - `Checker` on +Z, +X and -Z: a `UsdPreviewSurface` whose base color
//!   is a texture (read as sRGB) and whose roughness comes from the green
//!   channel of a second, raw data texture;
//! - `Paint` on -Y, +Y and -X: constant blue.
//!
//! Both textures are generated here and stored in the package, which is
//! self-contained: every authored asset path names a package file.
//!
//! ```sh
//! cargo run -p layerstack_examples --example mesh_materials_to_usdz -- cube.usdz
//! usdchecker cube.usdz   # optional, if OpenUSD is installed
//! usdview cube.usdz      # or usdrecord, through a layer that adds a camera
//! ```

use layerstack_mesh_export::{
    Channel, ColorInput, Faces, FamilyType, FloatInput, Material, Mesh, PackageFile, Primvar,
    Scene, StageSettings, Texture, Transform, UpAxis, Xform,
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
    [0, 3, 2, 1], // 0: -Z
    [4, 5, 6, 7], // 1: +Z
    [0, 1, 5, 4], // 2: -Y
    [2, 3, 7, 6], // 3: +Y
    [1, 2, 6, 5], // 4: +X
    [3, 0, 4, 7], // 5: -X
];

const FACE_NORMALS: [[f32; 3]; 6] = [
    [0.0, 0.0, -1.0],
    [0.0, 0.0, 1.0],
    [0.0, -1.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
];

/// Texture coordinates of each face's corners: `st` (0, 0) is the lower
/// left of the image as displayed.
const UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// Faces bound to `Checker` and to `Paint`; together they cover every face
/// once, as the partition requires.
const CHECKER_FACES: [u32; 3] = [1, 4, 0];
const PAINT_FACES: [u32; 3] = [2, 3, 5];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "cube.usdz".into());

    let counts = [4_u32; 6];
    let indices: Vec<u32> = FACES.iter().flatten().copied().collect();
    let uvs: Vec<[f32; 2]> = (0..6).flat_map(|_| UVS).collect();

    let cube = Mesh::new(
        "Cube",
        &POINTS,
        Faces::Polygons {
            counts: &counts,
            indices: &indices,
        },
    )
    .with_normals(Primvar::uniform(&FACE_NORMALS[..]))
    .with_uvs(Primvar::face_varying(&uvs[..]))
    .with_material_subset("CheckerFaces", &CHECKER_FACES, "Checker")
    .with_material_subset("PaintFaces", &PAINT_FACES, "Paint")
    .with_subset_family(FamilyType::Partition);

    // A 4x4 orange and teal checker for the base color, and a data image
    // whose green channel varies the roughness per cell.
    let albedo = rgb_png(64, 64, |x, y| {
        if (x / 16 + y / 16) % 2 == 0 {
            [230, 120, 20]
        } else {
            [20, 150, 160]
        }
    });
    let roughness = rgb_png(64, 64, |x, y| {
        let g = if (x / 16 + y / 16) % 2 == 0 { 60 } else { 220 };
        [0, g, 0]
    });
    let checker = Material::new("Checker")
        .with_diffuse_color(ColorInput::texture(Texture::new("textures/albedo.png")))
        .with_roughness(FloatInput::texture(
            Texture::new("textures/roughness.png"),
            Channel::G,
        ))
        .with_metallic(0.0);
    let paint = Material::new("Paint")
        .with_diffuse_color([0.05, 0.12, 0.7])
        .with_roughness(0.35)
        .with_metallic(0.0);

    // Lift the cube so it rests on the ground plane (Z up, meters).
    let root = Xform::new("Root")
        .with_kind("component")
        .with_transform(Transform::from_translation([0.0, 0.0, 0.5]))
        .with_mesh(cube);
    let scene = Scene::new(StageSettings::new(UpAxis::Z, 1.0), root)
        .with_material(checker)
        .with_material(paint);

    let bytes = scene.to_usdz(&[
        PackageFile::new("textures/albedo.png", &albedo),
        PackageFile::new("textures/roughness.png", &roughness),
    ])?;
    std::fs::write(&out, &bytes)?;
    println!(
        "wrote {out}: {} bytes, {} faces, materials Checker (faces {CHECKER_FACES:?}) and Paint (faces {PAINT_FACES:?})",
        bytes.len(),
        counts.len(),
    );
    Ok(())
}

/// Encodes an 8-bit RGB PNG with a stored (uncompressed) deflate stream,
/// which keeps the example free of image dependencies.
fn rgb_png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = layerstack_usdz::crc32::crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0); // filter: none
        for x in 0..width {
            raw.extend_from_slice(&pixel(x, y));
        }
    }
    // A zlib stream holding one stored block, then the Adler-32 checksum.
    let len = u16::try_from(raw.len()).expect("image fits one stored block");
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend_from_slice(&len.to_le_bytes());
    zlib.extend_from_slice(&(!len).to_le_bytes());
    zlib.extend_from_slice(&raw);
    let (mut a, mut b) = (1_u32, 0_u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib);
    chunk(&mut png, b"IEND", &[]);
    png
}
