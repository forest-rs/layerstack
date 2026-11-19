// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! LZ4 framing and integer coding, the inverse of [`crate::compression`].
//!
//! Spec: AOUSD Core §16.3.4; OpenUSD `pxr/base/tf/fastCompression.cpp`
//! (`TfFastCompression::CompressToBuffer`) and `pxr/usd/sdf/integerCoding.cpp`
//! (`_EncodeIntegers`).

use alloc::vec::Vec;
use hashbrown::HashMap;

/// Largest input one LZ4 block may hold (`LZ4_MAX_INPUT_SIZE`).
const LZ4_MAX_INPUT_SIZE: usize = 0x7E00_0000;

/// Appends a size-prefixed `TfFastCompression` block. Only the current LZ4
/// block needs scratch space. The caller can append several columns to one
/// destination without retaining the framed columns separately.
pub(super) fn append_compressed(out: &mut Vec<u8>, input: &[u8]) {
    let size_at = out.len();
    out.extend_from_slice(&0_u64.to_le_bytes());
    append_chunks(out, input, LZ4_MAX_INPUT_SIZE);
    let size = (out.len() - size_at - 8) as u64;
    out[size_at..size_at + 8].copy_from_slice(&size.to_le_bytes());
}

fn append_chunks(out: &mut Vec<u8>, input: &[u8], max_chunk: usize) {
    if input.len() <= max_chunk {
        out.push(0);
        out.extend_from_slice(&lz4_flex::compress(input));
        return;
    }
    let chunks = input.chunks(max_chunk);
    // Callers bound the input to 127 chunks. Each compressed chunk fits i32.
    out.push(u8::try_from(chunks.len()).expect("at most 127 chunks"));
    for chunk in chunks {
        let block = lz4_flex::compress(chunk);
        let size = i32::try_from(block.len()).expect("bounded LZ4 block");
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&block);
    }
}

/// The largest input [`append_compressed`] accepts (127 chunks,
/// `TfFastCompression::GetMaxInputSize`).
pub(crate) const LZ4_MAX_TOTAL_INPUT: u64 = 127 * LZ4_MAX_INPUT_SIZE as u64;

/// Width of the integers being coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntWidth {
    /// 32-bit integers (`Sdf_IntegerCompression`): 8/16/32-bit codes.
    W32,
    /// 64-bit integers (`Sdf_IntegerCompression64`): 16/32/64-bit codes.
    W64,
}

impl IntWidth {
    fn bytes(self) -> usize {
        match self {
            Self::W32 => 4,
            Self::W64 => 8,
        }
    }
}

/// Delta + 2-bit-code encodes `values` (already sign-reinterpreted into
/// `i64`; 32-bit inputs must lie in `i32` range).
///
/// Output: the most common delta, then 2-bit codes (four per byte, first
/// integer in the low bits), then each non-common delta at the narrowest of
/// three widths. The most common delta breaks ties toward the larger value,
/// as OpenUSD does.
pub(crate) fn encode_integers(values: &[i64], width: IntWidth) -> Vec<u8> {
    if values.is_empty() {
        return Vec::new();
    }
    let delta = |prev: i64, cur: i64| match width {
        #[allow(clippy::cast_possible_truncation, reason = "32-bit inputs")]
        IntWidth::W32 => i64::from((cur as i32).wrapping_sub(prev as i32)),
        IntWidth::W64 => cur.wrapping_sub(prev),
    };

    // Hashed: the most common delta below is the greatest by count, then
    // by value, whatever order the counts are visited in.
    let mut counts: HashMap<i64, usize> = HashMap::new();
    // AOUSD Core §16.3.4: count every delta exactly. Keep the first inter-element
    // delta in a separate counter: ramps and repeated ranges avoid a tree lookup
    // for their usual step, without a preliminary pass or a guessed mode.
    let cached_delta = values
        .get(1)
        .map_or_else(|| delta(0, values[0]), |&v| delta(values[0], v));
    let mut cached_count = 0;
    let mut prev = 0;
    for &v in values {
        let d = delta(prev, v);
        if d == cached_delta {
            cached_count += 1;
        } else {
            *counts.entry(d).or_insert(0) += 1;
        }
        prev = v;
    }
    // Highest count; among equal counts the largest delta, including the
    // cached candidate, whether or not it is actually the most frequent one.
    let mut common = cached_delta;
    let mut best = cached_count;
    for (&value, &count) in &counts {
        if (count, value) > (best, common) {
            best = count;
            common = value;
        }
    }

    let n = values.len();
    let code_bytes = (n * 2).div_ceil(8);
    let mut out = Vec::with_capacity(width.bytes() * (n + 1) + code_bytes);
    push_int(&mut out, common, width.bytes());
    let codes_at = out.len();
    out.resize(codes_at + code_bytes, 0);

    let (small, medium): (u32, u32) = match width {
        IntWidth::W32 => (1, 2),
        IntWidth::W64 => (2, 4),
    };
    let fits = |v: i64, bytes: u32| {
        let bits = 8 * bytes;
        let min = -(1_i64 << (bits - 1));
        let max = (1_i64 << (bits - 1)) - 1;
        (min..=max).contains(&v)
    };
    let mut prev = 0;
    for (i, &v) in values.iter().enumerate() {
        let d = delta(prev, v);
        prev = v;
        let code: u8 = if d == common {
            0
        } else if fits(d, small) {
            push_int(&mut out, d, small as usize);
            1
        } else if fits(d, medium) {
            push_int(&mut out, d, medium as usize);
            2
        } else {
            push_int(&mut out, d, width.bytes());
            3
        };
        out[codes_at + i / 4] |= code << (2 * (i % 4));
    }
    out
}

/// Appends the low `bytes` bytes of `v`, little-endian.
fn push_int(out: &mut Vec<u8>, v: i64, bytes: usize) {
    out.extend_from_slice(&v.to_le_bytes()[..bytes]);
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::vec;

    use super::*;
    use crate::compression::{decode_integer_array, lz4_decompress, read_compressed_ints};

    /// The worked example in `integerCoding.cpp`: deltas
    /// `[123, 1, 1, 100000, 0, 1, 0]`, most common delta `1`.
    #[test]
    fn integer_coding_matches_openusd_example() {
        let input = [123, 124, 125, 100_125, 100_125, 100_126, 100_126];
        let encoded = encode_integers(&input, IntWidth::W32);
        let expected = [
            1, 0, 0, 0, // common delta: int32(1)
            0xC1, 0x11, // codes 01 00 00 11 | 01 00 01 (low bits first)
            123,  // int8(123)
            0xA0, 0x86, 0x01, 0x00, // int32(100000)
            0,    // int8(0)
            0,    // int8(0)
        ];
        assert_eq!(encoded, expected, "golden encoding");
        let decoded = decode_integer_array(&encoded, input.len(), 4).unwrap();
        assert_eq!(decoded, input, "reader decodes it");
    }

    #[test]
    fn integer_coding_ties_pick_the_larger_delta() {
        // Deltas 5, 5, -1, -1: tie between 5 and -1.
        let encoded = encode_integers(&[5, 10, 9, 8], IntWidth::W32);
        assert_eq!(&encoded[..4], &5_i32.to_le_bytes(), "common delta");
    }

    #[test]
    fn integer_coding_cached_counts_preserve_modes_ties_and_wrapping() {
        for width in [IntWidth::W32, IntWidth::W64] {
            for len in [1, 3, 4, 5, 15, 16, 17, 64, 257, 4096] {
                for deltas in [
                    &[1][..],
                    &[5, 5, -1, -1],
                    &[-1, -1, 5, 5],
                    &[5, 1, 1, 1],
                    &[0, 0, 0, i64::MAX, i64::MIN],
                    &[i64::from(i32::MAX), i64::from(i32::MIN), -1],
                ] {
                    let mut value = 0_i64;
                    let values: Vec<_> = (0..len)
                        .map(|i| {
                            value = value.wrapping_add(deltas[i % deltas.len()]);
                            match width {
                                IntWidth::W32 => i64::from(i32::from_le_bytes(
                                    value.to_le_bytes()[..4].try_into().unwrap(),
                                )),
                                IntWidth::W64 => value,
                            }
                        })
                        .collect();
                    let mut counts = BTreeMap::new();
                    let mut prev = 0_i64;
                    for &value in &values {
                        let delta = match width {
                            IntWidth::W32 => {
                                let narrow = |v: i64| {
                                    i32::from_le_bytes(v.to_le_bytes()[..4].try_into().unwrap())
                                };
                                i64::from(narrow(value).wrapping_sub(narrow(prev)))
                            }
                            IntWidth::W64 => value.wrapping_sub(prev),
                        };
                        *counts.entry(delta).or_insert(0_usize) += 1;
                        prev = value;
                    }
                    let common = counts
                        .iter()
                        .max_by_key(|&(delta, count)| (count, delta))
                        .unwrap()
                        .0;
                    let encoded = encode_integers(&values, width);
                    assert_eq!(
                        &encoded[..width.bytes()],
                        &common.to_le_bytes()[..width.bytes()]
                    );
                    assert_eq!(
                        decode_integer_array(&encoded, len, width.bytes()).unwrap(),
                        values
                    );
                }
            }
        }
    }

    #[test]
    fn integer_coding_round_trips_extremes() {
        let w32 = [
            i64::from(i32::MIN),
            i64::from(i32::MAX),
            0,
            -1,
            1,
            300,
            -40_000,
        ];
        let decoded = decode_integer_array(&encode_integers(&w32, IntWidth::W32), 7, 4).unwrap();
        assert_eq!(decoded, w32, "32-bit wrapping deltas");
        let w64 = [i64::MIN, i64::MAX, 0, -1, 70_000, -(1 << 40), 5];
        let decoded = decode_integer_array(&encode_integers(&w64, IntWidth::W64), 7, 8).unwrap();
        assert_eq!(decoded, w64, "64-bit wrapping deltas");
        // u32 values are coded by their `int32` bit pattern.
        let u32s = [i64::from(u32::MAX as i32), 7];
        let decoded = decode_integer_array(&encode_integers(&u32s, IntWidth::W32), 2, 4).unwrap();
        assert_eq!(decoded, u32s, "u32 reinterpreted");
    }

    #[test]
    fn compressed_ints_read_back() {
        let values: Vec<i64> = (0..1000).map(|i| (i * 7) % 13 - 6).collect();
        let mut bytes = Vec::new();
        append_compressed(&mut bytes, &encode_integers(&values, IntWidth::W32));
        let (decoded, consumed) = read_compressed_ints(&bytes, values.len(), 4).unwrap();
        assert_eq!(decoded, values, "values");
        assert_eq!(consumed, bytes.len(), "size prefix covers the payload");
        let start = bytes.len();
        append_compressed(&mut bytes, &[]);
        assert!(bytes.len() - start > 8, "empty input");
    }

    #[test]
    fn single_chunk_framing() {
        let input = b"hello hello hello hello hello hello";
        let mut framed = Vec::new();
        append_chunks(&mut framed, input, LZ4_MAX_INPUT_SIZE);
        assert_eq!(framed[0], 0, "one-chunk marker");
        assert_eq!(lz4_decompress(&framed, input.len()).unwrap(), input);
    }

    #[test]
    fn multi_chunk_framing() {
        let input: Vec<u8> = (0..1000_u32).map(|i| (i % 251) as u8).collect();
        let mut framed = Vec::new();
        append_chunks(&mut framed, &input, 300);
        assert_eq!(framed[0], 4, "chunk count");
        // Decode per `TfFastCompression::DecompressFromBuffer`.
        let mut out = vec![];
        let mut rest = &framed[1..];
        for _ in 0..framed[0] {
            let size = i32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
            out.extend(lz4_flex::decompress(&rest[4..4 + size], 300).unwrap());
            rest = &rest[4 + size..];
        }
        assert!(rest.is_empty(), "no trailing bytes");
        assert_eq!(out, input, "chunks concatenate to the input");
    }
}
