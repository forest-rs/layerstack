// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! LZ4 block decompression and integer array decoding.
//!
//! USDC uses LZ4 compression for token strings, field value reps, and
//! compressed integer arrays. On top of LZ4, integer arrays use an
//! additional delta + 2-bit code encoding.
//!
//! Spec: AOUSD Core §16.3.4.

use alloc::vec;
use alloc::vec::Vec;

use crate::error::UsdcError;

// ---------------------------------------------------------------------------
// LZ4 block decompression
// ---------------------------------------------------------------------------

/// Decompresses a block written by OpenUSD's `TfFastCompression`.
///
/// The first byte is a chunk count (`TfFastCompression::DecompressFromBuffer`,
/// `pxr/base/tf/fastCompression.cpp`):
/// - `0` → the rest of `data` is one LZ4 block.
/// - `n > 0` → `n` chunks, each an `i32` compressed size and that many bytes
///   of LZ4 block, for outputs larger than one LZ4 block can hold.
///
/// `output_size` is the largest decompressed size the caller expects. It
/// comes from the file, so no more is allocated than the compressed bytes
/// can expand to.
///
/// Spec: AOUSD Core §16.3.4.
pub fn lz4_decompress(data: &[u8], output_size: usize) -> Result<Vec<u8>, UsdcError> {
    let Some((&num_chunks, payload)) = data.split_first() else {
        return Err(UsdcError::DecompressionFailed {
            context: "empty LZ4 input",
        });
    };
    let output_size = output_size.min(max_lz4_output(payload.len()));

    if num_chunks == 0 {
        // Single-block decompress.
        return lz4_flex::decompress(payload, output_size).map_err(|_| {
            UsdcError::DecompressionFailed {
                context: "LZ4 single-block decompress",
            }
        });
    }

    let mut out = Vec::with_capacity(output_size);
    let mut cursor = payload;
    for _ in 0..num_chunks {
        let Some((size, rest)) = cursor.split_first_chunk::<4>() else {
            return Err(UsdcError::DecompressionFailed {
                context: "LZ4 chunked: missing chunk size",
            });
        };
        let chunk_size = usize::try_from(i32::from_le_bytes(*size)).map_err(|_| {
            UsdcError::DecompressionFailed {
                context: "LZ4 chunked: negative chunk size",
            }
        })?;
        if rest.len() < chunk_size {
            return Err(UsdcError::DecompressionFailed {
                context: "LZ4 chunked: chunk data truncated",
            });
        }
        let (chunk, rest) = rest.split_at(chunk_size);
        cursor = rest;
        let remaining = (output_size - out.len()).min(LZ4_MAX_INPUT_SIZE);
        let decompressed =
            lz4_flex::decompress(chunk, remaining).map_err(|_| UsdcError::DecompressionFailed {
                context: "LZ4 chunked: chunk decompress",
            })?;
        if decompressed.len() > output_size - out.len() {
            return Err(UsdcError::DecompressionFailed {
                context: "LZ4 chunked: output exceeds the expected size",
            });
        }
        out.extend_from_slice(&decompressed);
    }
    Ok(out)
}

/// The largest input one LZ4 block holds (`LZ4_MAX_INPUT_SIZE`), which is
/// also the most one chunk decompresses to.
const LZ4_MAX_INPUT_SIZE: usize = 0x7E00_0000;

/// The most that `compressed` bytes of LZ4 blocks can decompress to.
///
/// Each byte of a sequence's match-length extension adds at most 255 output
/// bytes, so no block expands by more than a factor of 255, plus the fixed
/// overhead of one sequence.
fn max_lz4_output(compressed: usize) -> usize {
    compressed.saturating_mul(255).saturating_add(64)
}

// ---------------------------------------------------------------------------
// Integer array decoding (delta + 2-bit codes)
// ---------------------------------------------------------------------------

/// Two-bit code values for the integer array encoder; the fourth code (3)
/// is a full-width value.
const CODE_COMMON: u8 = 0;
const CODE_QUARTER: u8 = 1;
const CODE_HALF: u8 = 2;

/// Decodes a USDC-compressed integer array from `data`.
///
/// `count` is the number of elements to decode. `int_size` is the byte width
/// of each element (typically 4 for `i32` or 8 for `i64`). Returns signed
/// 64-bit values to accommodate both `i32` and `i64` elements.
///
/// Format: `common_value` (signed, `int_size` bytes), then code bytes
/// (2 bits per element, packed 4 per byte), then value bytes (variable
/// width per the code).
///
/// Spec: AOUSD Core §16.3.4.
pub fn decode_integer_array(
    data: &[u8],
    count: usize,
    int_size: usize,
) -> Result<Vec<i64>, UsdcError> {
    if count == 0 {
        return Ok(vec![]);
    }

    if data.len() < int_size {
        return Err(UsdcError::IntegerArrayDecode {
            context: "data too short for common value",
        });
    }

    // Read the common (most frequent) delta value (signed, int_size bytes).
    let common_value = read_signed_le(&data[..int_size]);
    let rest = &data[int_size..];

    let num_code_bytes = count.div_ceil(4);
    if rest.len() < num_code_bytes {
        return Err(UsdcError::IntegerArrayDecode {
            context: "data too short for code bytes",
        });
    }

    let code_bytes = &rest[..num_code_bytes];
    let value_bytes = &rest[num_code_bytes..];

    let quarter_size = int_size / 4;
    let half_size = int_size / 2;

    let mut elements = Vec::with_capacity(count);
    let mut prev: i64 = 0;
    let mut value_offset = 0;

    for i in 0..count {
        let code_byte_idx = i / 4;
        let bit_shift = (i % 4) * 2;
        let code = (code_bytes[code_byte_idx] >> bit_shift) & 3;
        // Each code selects a width; the value bytes are checked below.

        let delta = match code {
            CODE_COMMON => common_value,
            CODE_QUARTER => {
                let end = value_offset + quarter_size;
                if end > value_bytes.len() {
                    return Err(UsdcError::IntegerArrayDecode {
                        context: "value bytes truncated (quarter)",
                    });
                }
                let v = read_signed_le(&value_bytes[value_offset..end]);
                value_offset = end;
                v
            }
            CODE_HALF => {
                let end = value_offset + half_size;
                if end > value_bytes.len() {
                    return Err(UsdcError::IntegerArrayDecode {
                        context: "value bytes truncated (half)",
                    });
                }
                let v = read_signed_le(&value_bytes[value_offset..end]);
                value_offset = end;
                v
            }
            _ => {
                let end = value_offset + int_size;
                if end > value_bytes.len() {
                    return Err(UsdcError::IntegerArrayDecode {
                        context: "value bytes truncated (full)",
                    });
                }
                let v = read_signed_le(&value_bytes[value_offset..end]);
                value_offset = end;
                v
            }
        };

        prev = prev.wrapping_add(delta);
        if int_size == 4 {
            // 32-bit arrays are summed in `int32` arithmetic, so a delta may
            // wrap (`integerCoding.cpp`, `_DecodeNHelper`).
            #[allow(clippy::cast_possible_truncation, reason = "int32 wrap")]
            let wrapped = prev as i32;
            prev = i64::from(wrapped);
        }
        elements.push(prev);
    }

    Ok(elements)
}

/// Reads a compressed integer array from `data`.
///
/// Reads `compressed_size: u64`, then that many bytes of LZ4-compressed
/// data, decompresses, and integer-array decodes.
///
/// Returns `(decoded_elements, bytes_consumed)`.
///
/// Spec: AOUSD Core §16.3.4.
pub fn read_compressed_ints(
    data: &[u8],
    count: usize,
    int_size: usize,
) -> Result<(Vec<i64>, usize), UsdcError> {
    if data.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "compressed int array",
            offset: 0,
            expected: 8,
        });
    }

    let (size, rest) = data.split_at(8);
    let compressed_size = u64::from_le_bytes([
        size[0], size[1], size[2], size[3], size[4], size[5], size[6], size[7],
    ]);
    let Some(compressed) = usize::try_from(compressed_size)
        .ok()
        .and_then(|csz| rest.get(..csz))
    else {
        return Err(UsdcError::UnexpectedEof {
            section: "compressed int array data",
            offset: 8,
            expected: compressed_size,
        });
    };
    let total = 8 + compressed.len();

    let encoded_size =
        encoded_int_array_size(count, int_size).ok_or(UsdcError::IntegerArrayDecode {
            context: "element count exceeds the address space",
        })?;
    let decompressed = lz4_decompress(compressed, encoded_size)?;
    let elements = decode_integer_array(&decompressed, count, int_size)?;
    Ok((elements, total))
}

/// Computes the largest encoded size of an integer array before LZ4, or
/// `None` when it overflows.
///
/// This equals `int_size + num_code_bytes + count * int_size`.
fn encoded_int_array_size(count: usize, int_size: usize) -> Option<usize> {
    if count == 0 {
        return Some(0);
    }
    count
        .checked_mul(int_size)?
        .checked_add(count.div_ceil(4))?
        .checked_add(int_size)
}

/// Reads a signed little-endian integer of 1–8 bytes, sign-extending to i64.
fn read_signed_le(bytes: &[u8]) -> i64 {
    let len = bytes.len();
    debug_assert!(len <= 8, "read_signed_le: max 8 bytes");
    // Copy into an 8-byte buffer with sign extension.
    let sign_bit = if len > 0 && bytes[len - 1] & 0x80 != 0 {
        0xFF
    } else {
        0x00
    };
    let mut buf = [sign_bit; 8];
    buf[..len].copy_from_slice(bytes);
    i64::from_le_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_signed_le_positive() {
        // 1 as i32 LE = [1, 0, 0, 0]
        assert_eq!(read_signed_le(&[1, 0, 0, 0]), 1);
    }

    #[test]
    fn read_signed_le_negative() {
        // -1 as i32 LE = [0xFF, 0xFF, 0xFF, 0xFF]
        assert_eq!(read_signed_le(&[0xFF, 0xFF, 0xFF, 0xFF]), -1);
    }

    #[test]
    fn read_signed_le_single_byte() {
        // -128 as i8 = 0x80
        assert_eq!(read_signed_le(&[0x80]), -128);
        assert_eq!(read_signed_le(&[127]), 127);
    }

    #[test]
    fn decode_all_common() {
        // 3 elements, int_size=4, common_value=5
        // Codes: all 0 → code byte = 0b00_00_00_00 = 0x00
        // Expected: [5, 10, 15] (cumulative sum of delta=5)
        let mut data = Vec::new();
        // common_value = 5 (i32 LE)
        data.extend_from_slice(&5_i32.to_le_bytes());
        // 1 code byte (3 elements × 2 bits = 6 bits, fits in 1 byte)
        data.push(0x00);
        // No value bytes needed (all common).

        let result = decode_integer_array(&data, 3, 4).unwrap();
        assert_eq!(result, vec![5, 10, 15]);
    }

    #[test]
    fn decode_mixed_codes() {
        // 2 elements, int_size=4
        // Element 0: CODE_COMMON (delta = common_value = 10) → value = 10
        // Element 1: CODE_FULL (delta from value bytes = 3) → value = 13
        let mut data = Vec::new();
        // common_value = 10
        data.extend_from_slice(&10_i32.to_le_bytes());
        // code byte: element 0 = CODE_COMMON(0), element 1 = CODE_FULL(3)
        // bits: 0b00_11_00_00 → but element ordering is low bits first:
        //   element 0 at bits [1:0] = 00
        //   element 1 at bits [3:2] = 11
        // = 0b0000_1100 = 0x0C
        data.push(0x0C);
        // value bytes: delta for element 1 = 3 (i32 LE)
        data.extend_from_slice(&3_i32.to_le_bytes());

        let result = decode_integer_array(&data, 2, 4).unwrap();
        assert_eq!(result, vec![10, 13]);
    }

    #[test]
    fn decode_wraps_32_bit_deltas() {
        // i32::MIN then i32::MAX: the writer stores the wrapped delta -1.
        let mut data = Vec::new();
        data.extend_from_slice(&(-1_i32).to_le_bytes()); // common delta
        data.push(0x03); // element 0: full; element 1: common
        data.extend_from_slice(&i32::MIN.to_le_bytes());
        let result = decode_integer_array(&data, 2, 4).unwrap();
        assert_eq!(result, vec![i64::from(i32::MIN), i64::from(i32::MAX)]);
    }

    #[test]
    fn decode_empty() {
        let result = decode_integer_array(&[], 0, 4).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn lz4_allocation_is_bounded_by_the_input() {
        let compressed = lz4_flex::compress(&[7_u8; 4096]);
        let mut framed = vec![0_u8];
        framed.extend_from_slice(&compressed);
        // A size the input cannot expand to allocates only what it can.
        for size in [4096, usize::MAX, compressed.len() * 256 + 64] {
            assert_eq!(lz4_decompress(&framed, size).unwrap(), [7_u8; 4096]);
        }
    }

    #[test]
    fn lz4_chunks_have_i32_sizes() {
        let original = b"hello world hello world hello world";
        let compressed = lz4_flex::compress(original);
        let mut framed = vec![2_u8];
        for _ in 0..2 {
            framed.extend_from_slice(&i32::try_from(compressed.len()).unwrap().to_le_bytes());
            framed.extend_from_slice(&compressed);
        }
        let decompressed = lz4_decompress(&framed, 2 * original.len()).unwrap();
        assert_eq!(decompressed, [&original[..], &original[..]].concat());
        // A negative or truncated chunk fails.
        framed[1] = 0xFF;
        framed[4] = 0xFF;
        assert!(lz4_decompress(&framed, 2 * original.len()).is_err());
        assert!(lz4_decompress(&framed[..10], 2 * original.len()).is_err());
    }

    #[test]
    fn compressed_int_counts_are_checked() {
        let mut data = 5_u64.to_le_bytes().to_vec();
        data.extend_from_slice(&[0, 1, 2, 3, 4]);
        for count in [usize::MAX, usize::MAX / 4, u32::MAX as usize] {
            assert!(read_compressed_ints(&data, count, 8).is_err());
        }
        let mut huge = u64::MAX.to_le_bytes().to_vec();
        huge.push(0);
        assert!(read_compressed_ints(&huge, 16, 4).is_err());
    }

    #[test]
    fn lz4_single_block_roundtrip() {
        // Compress some data with lz4_flex, then decompress through our wrapper.
        let original = b"hello world hello world hello world";
        let compressed = lz4_flex::compress(original);

        // Prepend num_chunks = 0.
        let mut framed = vec![0_u8];
        framed.extend_from_slice(&compressed);

        let decompressed = lz4_decompress(&framed, original.len()).unwrap();
        assert_eq!(decompressed, original);
    }
}
