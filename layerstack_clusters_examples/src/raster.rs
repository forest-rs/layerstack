// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A small software rasterizer and PNG writer, enough to compare a cut
//! with the full mesh pixel by pixel without a GPU or an image crate.

use std::io::Write;

/// A perspective camera looking at a point.
#[derive(Clone, Copy, Debug)]
pub(crate) struct View {
    pub(crate) eye: [f32; 3],
    pub(crate) target: [f32; 3],
    /// `1 / tan(fov_y / 2)`.
    pub(crate) projection_y: f32,
    pub(crate) z_near: f32,
    pub(crate) size: usize,
}

/// A grey image and its depth buffer; depth `f32::INFINITY` is empty.
pub(crate) struct Image {
    pub(crate) size: usize,
    pub(crate) depth: Vec<f32>,
    pub(crate) shade: Vec<u8>,
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    if l > 0.0 {
        [a[0] / l, a[1] / l, a[2] / l]
    } else {
        a
    }
}

/// Renders `indices` over `positions` (with per-vertex `normals`, or flat
/// ones) into `image` as two-sided Lambert grey with a headlight, depth
/// tested against what it holds. Triangles reaching behind the near plane
/// are skipped.
#[expect(
    clippy::cast_possible_truncation,
    reason = "pixel coordinates clamped to the image, grey clamped to a byte"
)]
pub(crate) fn render(
    view: &View,
    image: &mut Image,
    positions: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
    indices: &[u32],
) {
    let size = view.size;
    let forward = normalize(sub(view.target, view.eye));
    let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
    let up = cross(right, forward);
    let half = size as f32 * 0.5;
    let scale = view.projection_y * half;
    let project = |p: [f32; 3]| {
        let d = sub(p, view.eye);
        let z = dot(d, forward);
        [
            half + dot(d, right) / z * scale,
            half - dot(d, up) / z * scale,
            z,
        ]
    };
    for triangle in indices.as_chunks::<3>().0 {
        let [a, b, c] = triangle.map(|v| positions[v as usize]);
        let [pa, pb, pc] = [a, b, c].map(project);
        if pa[2] < view.z_near || pb[2] < view.z_near || pc[2] < view.z_near {
            continue;
        }
        let area = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pc[0] - pa[0]) * (pb[1] - pa[1]);
        if area == 0.0 {
            continue;
        }
        let flat = normalize(cross(sub(b, a), sub(c, a)));
        let corner_normals = match normals {
            Some(n) => triangle.map(|v| n[v as usize]),
            None => [flat; 3],
        };
        let min_x = pa[0].min(pb[0]).min(pc[0]).floor().max(0.0) as usize;
        let max_x = (pa[0].max(pb[0]).max(pc[0]).ceil() as usize).min(size);
        let min_y = pa[1].min(pb[1]).min(pc[1]).floor().max(0.0) as usize;
        let max_y = (pa[1].max(pb[1]).max(pc[1]).ceil() as usize).min(size);
        for y in min_y..max_y {
            let py = y as f32 + 0.5;
            for x in min_x..max_x {
                let px = x as f32 + 0.5;
                let w0 = ((pb[0] - px) * (pc[1] - py) - (pc[0] - px) * (pb[1] - py)) / area;
                let w1 = ((pc[0] - px) * (pa[1] - py) - (pa[0] - px) * (pc[1] - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                // Perspective-correct depth and normal.
                let inverse_z = w0 / pa[2] + w1 / pb[2] + w2 / pc[2];
                let z = 1.0 / inverse_z;
                let i = y * size + x;
                if z >= image.depth[i] {
                    continue;
                }
                let k = [w0 / pa[2] * z, w1 / pb[2] * z, w2 / pc[2] * z];
                let n = normalize([0, 1, 2].map(|axis| {
                    k[0] * corner_normals[0][axis]
                        + k[1] * corner_normals[1][axis]
                        + k[2] * corner_normals[2][axis]
                }));
                let lambert = dot(n, forward).abs();
                image.depth[i] = z;
                image.shade[i] = (40.0 + 215.0 * lambert).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

impl Image {
    /// An empty image `size` pixels square.
    pub(crate) fn new(size: usize) -> Self {
        Self {
            size,
            depth: vec![f32::INFINITY; size * size],
            shade: vec![0; size * size],
        }
    }
}

/// How two filtered renders differ.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Difference {
    /// Pixels either render covers at all.
    pub(crate) covered: usize,
    /// Pixels whose grey differs by more than 8 of 255.
    pub(crate) over_8: usize,
    /// Pixels whose grey differs by more than 32 of 255.
    pub(crate) over_32: usize,
    /// The mean absolute grey difference over covered pixels.
    pub(crate) mean: f64,
}

/// Box-filters a render made `factor` times larger in each direction down
/// to its final size: each pixel the mean of its samples' grey, empty
/// samples black.
pub(crate) fn downsample(image: &Image, factor: usize) -> Vec<u8> {
    let size = image.size / factor;
    let mut out = vec![0; size * size];
    for (i, value) in out.iter_mut().enumerate() {
        let (x, y) = (i % size, i / size);
        let mut sum = 0_u32;
        for sy in y * factor..(y + 1) * factor {
            for sx in x * factor..(x + 1) * factor {
                sum += u32::from(image.shade[sy * image.size + sx]);
            }
        }
        let samples = u32::try_from(factor * factor).expect("a small factor");
        *value = u8::try_from((sum + samples / 2) / samples).expect("a mean of bytes");
    }
    out
}

/// Compares two filtered renders of one view, and draws the differences:
/// the first render dimmed where they agree within 8 of 255, blue up to
/// 32, red beyond.
pub(crate) fn compare(a: &[u8], b: &[u8]) -> (Difference, Vec<[u8; 3]>) {
    let mut d = Difference::default();
    let mut total = 0_u64;
    let mut picture = vec![[0_u8; 3]; a.len()];
    for ((&x, &y), pixel) in a.iter().zip(b).zip(&mut picture) {
        if x == 0 && y == 0 {
            continue;
        }
        d.covered += 1;
        let delta = x.abs_diff(y);
        total += u64::from(delta);
        *pixel = if delta > 32 {
            d.over_32 += 1;
            d.over_8 += 1;
            [255, 0, 0]
        } else if delta > 8 {
            d.over_8 += 1;
            [0, 0, 255]
        } else {
            [x / 2; 3]
        };
    }
    d.mean = total as f64 / d.covered.max(1) as f64;
    (d, picture)
}

/// Writes an 8-bit RGB PNG with stored (uncompressed) deflate blocks.
pub(crate) fn write_png(
    path: &std::path::Path,
    size: usize,
    rgb: &[[u8; 3]],
) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity(size * (size * 3 + 1));
    for row in rgb.chunks(size) {
        raw.push(0);
        raw.extend(row.iter().flatten());
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        let len = u16::try_from(block.len()).expect("blocks of at most 65535 bytes");
        zlib.extend(len.to_le_bytes());
        zlib.extend((!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut s1, mut s2) = (1_u32, 0_u32);
    for &byte in &raw {
        s1 = (s1 + u32::from(byte)) % 65521;
        s2 = (s2 + s1) % 65521;
    }
    zlib.extend(((s2 << 16) | s1).to_be_bytes());

    let side = u32::try_from(size).expect("an image side that fits 32 bits");
    let mut header = Vec::new();
    header.extend(side.to_be_bytes());
    header.extend(side.to_be_bytes());
    header.extend([8, 2, 0, 0, 0]);
    let mut file = std::fs::File::create(path)?;
    file.write_all(b"\x89PNG\r\n\x1a\n")?;
    for (kind, data) in [(b"IHDR", &header), (b"IDAT", &zlib), (b"IEND", &Vec::new())] {
        let len = u32::try_from(data.len()).expect("a chunk under 4 GiB");
        file.write_all(&len.to_be_bytes())?;
        file.write_all(kind)?;
        file.write_all(data)?;
        let mut crc = crc32(0xFFFF_FFFF, kind);
        crc = crc32(crc, data);
        file.write_all(&(!crc).to_be_bytes())?;
    }
    Ok(())
}

fn crc32(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// A grey image as RGB.
pub(crate) fn grey(shade: &[u8]) -> Vec<[u8; 3]> {
    shade.iter().map(|&s| [s; 3]).collect()
}
