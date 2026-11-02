// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The editor's cluster cache: each mesh's set, built here and written as
//! a `layerstack_clusters` file named by a key the editor computes again
//! from the buffers it draws.
//!
//! The editor never holds a mesh's `f32` positions: `@forest-rs/layerstack_stagehand`
//! encodes them to its `compact` vertex format as the buffers are copied
//! out of WebAssembly. So the key hashes what both sides have bit for bit,
//! the triangulated indices and the compact encoding of the positions and
//! normals, which this module reproduces exactly (`f64` arithmetic and
//! JavaScript's `Math.round`, as `vertices.ts` computes them):
//!
//! - `KEY_VERSION`, `layerstack_clusters::file::FILE_VERSION`, the vertex
//!   count and the triangle count, as `u32` words;
//! - every index, a word each;
//! - every position as `snorm16` x4 (the fourth zero), two words, low
//!   component in the low half;
//! - the position decode's scale and offset (`f64`), two words each, low
//!   word first;
//! - every normal as `snorm8` x4 (the fourth zero), one word, x in the low
//!   byte.
//!
//! The words go through two lanes of `MurmurHash3`'s 32-bit block step
//! (seeds 0 and `0x9e3779b9`), each finished with its length; the key is
//! the two lanes in hex, high lane first. Texture coordinates and colors
//! are left out: they weigh into the simplification but do not change
//! which vertices a set's indices name, so a set built from other ones is
//! still a valid set for the mesh.

use layerstack_clusters::file::FILE_VERSION;
use layerstack_stagehand::MeshBuffers;

/// The version of the key and of the configuration sets are built with:
/// bumped when either changes, so the editor stops finding stale sets.
pub(crate) const KEY_VERSION: u32 = 1;

/// The cache key of a triangulated mesh: 16 hex digits.
pub(crate) fn cache_key(buffers: &MeshBuffers) -> String {
    let mut hash = Hasher::default();
    let count = u32::try_from(buffers.positions.len()).expect("32-bit vertex counts");
    let triangles = u32::try_from(buffers.indices.len() / 3).expect("32-bit index counts");
    for word in [KEY_VERSION, FILE_VERSION, count, triangles] {
        hash.word(word);
    }
    for &index in &buffers.indices {
        hash.word(index);
    }
    let (quantized, scale, offset) = quantize_positions(&buffers.positions);
    for q in &quantized {
        let [x, y, z] = q.map(i16::to_le_bytes);
        hash.word(u32::from_le_bytes([x[0], x[1], y[0], y[1]]));
        hash.word(u32::from_le_bytes([z[0], z[1], 0, 0]));
    }
    for value in [scale, offset[0], offset[1], offset[2]] {
        let bits = value.to_bits().to_le_bytes();
        hash.word(u32::from_le_bytes([bits[0], bits[1], bits[2], bits[3]]));
        hash.word(u32::from_le_bytes([bits[4], bits[5], bits[6], bits[7]]));
    }
    for n in &buffers.normals {
        let [x, y, z] = n.map(|c| {
            let q = i8::try_from(snorm(f64::from(c) * 127.0, 127.0)).expect("clamped to snorm8");
            q.to_le_bytes()[0]
        });
        hash.word(u32::from_le_bytes([x, y, z, 0]));
    }
    hash.finish()
}

/// JavaScript's `Math.round`: to the nearest integer, halves up.
fn js_round(x: f64) -> f64 {
    let floor = x.floor();
    if x - floor >= 0.5 { floor + 1.0 } else { floor }
}

/// `value` rounded as `Math.round` does and clamped to `[-limit, limit]`.
fn snorm(value: f64, limit: f64) -> i32 {
    let q = js_round(value).clamp(-limit, limit);
    // A whole number within ±32767 after the clamp: the cast is exact.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to a small whole number"
    )]
    let q = q as i32;
    q
}

/// `quantizePositions` of `vertices.ts`: `snorm16` positions about the
/// bounds' center, scaled by half the largest extent, with that scale and
/// center.
fn quantize_positions(positions: &[[f32; 3]]) -> (Vec<[i16; 3]>, f64, [f64; 3]) {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for p in positions {
        for axis in 0..3 {
            let value = f64::from(p[axis]);
            if value < min[axis] {
                min[axis] = value;
            }
            if value > max[axis] {
                max[axis] = value;
            }
        }
    }
    if positions.is_empty() {
        return (Vec::new(), 1.0, [0.0; 3]);
    }
    let offset = [0, 1, 2].map(|axis| (min[axis] + max[axis]) / 2.0);
    let half = (max[0] - min[0]).max(max[1] - min[1]).max(max[2] - min[2]) / 2.0;
    let scale = if half > 0.0 && half.is_finite() {
        half
    } else {
        1.0
    };
    let to_unit = 32767.0 / scale;
    let quantized = positions
        .iter()
        .map(|p| {
            [0, 1, 2].map(|axis| {
                let q = snorm((f64::from(p[axis]) - offset[axis]) * to_unit, 32767.0);
                i16::try_from(q).expect("clamped to snorm16")
            })
        })
        .collect();
    (quantized, scale, offset)
}

/// Two lanes of `MurmurHash3`'s 32-bit block step over words.
struct Hasher {
    lanes: [u32; 2],
    words: u32,
}

impl Default for Hasher {
    fn default() -> Self {
        Self {
            lanes: [0, 0x9e37_79b9],
            words: 0,
        }
    }
}

impl Hasher {
    fn word(&mut self, word: u32) {
        let k = word
            .wrapping_mul(0xcc9e_2d51)
            .rotate_left(15)
            .wrapping_mul(0x1b87_3593);
        for (lane, salt) in self.lanes.iter_mut().zip([0, 0x85eb_ca6b]) {
            *lane = (*lane ^ k ^ salt)
                .rotate_left(13)
                .wrapping_mul(5)
                .wrapping_add(0xe654_6b64);
        }
        self.words = self.words.wrapping_add(1);
    }

    fn finish(&self) -> String {
        let [low, high] = self.lanes.map(|lane| {
            let mut h = lane ^ self.words.wrapping_mul(4);
            h ^= h >> 16;
            h = h.wrapping_mul(0x85eb_ca6b);
            h ^= h >> 13;
            h = h.wrapping_mul(0xc2b2_ae35);
            h ^ (h >> 16)
        });
        format!("{high:08x}{low:08x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_as_javascript_does() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(-2.6), -3.0);
        assert_eq!(js_round(0.499_999_999_999_999_94), 0.0);
    }

    #[test]
    fn a_known_mesh_has_a_known_key() {
        // The editor's `clusterKey` test hashes the same triangle and
        // expects this key: the two must agree bit for bit.
        let buffers = MeshBuffers {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 2.0, 0.5]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            indices: vec![0, 1, 2],
            ..MeshBuffers::default()
        };
        assert_eq!(cache_key(&buffers), "71129f20029f413d");
    }
}
