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

/// Decompresses an LZ4-framed block.
///
/// The first byte is a chunk count:
/// - `0` → single-block LZ4 decompress (rest of `data` is the block).
/// - `n > 0` → `n` chunked sub-blocks, each prefixed by a 1-byte size.
///
/// `output_size` is the expected decompressed size (known from the caller).
///
/// Spec: AOUSD Core §16.3.4.
pub fn lz4_decompress(data: &[u8], output_size: usize) -> Result<Vec<u8>, UsdcError> {
    if data.is_empty() {
        return Err(UsdcError::DecompressionFailed {
            context: "empty LZ4 input",
        });
    }

    let num_chunks = data[0];
    let payload = &data[1..];

    if num_chunks == 0 {
        // Single-block decompress.
        lz4_flex::decompress(payload, output_size).map_err(|_| UsdcError::DecompressionFailed {
            context: "LZ4 single-block decompress",
        })
    } else {
        // Chunked decompress (rarely encountered).
        let mut out = Vec::with_capacity(output_size);
        let mut cursor = payload;
        for _ in 0..num_chunks {
            if cursor.is_empty() {
                return Err(UsdcError::DecompressionFailed {
                    context: "LZ4 chunked: missing chunk size byte",
                });
            }
            let chunk_size = cursor[0] as usize;
            cursor = &cursor[1..];
            if cursor.len() < chunk_size {
                return Err(UsdcError::DecompressionFailed {
                    context: "LZ4 chunked: chunk data truncated",
                });
            }
            let chunk_data = &cursor[..chunk_size];
            cursor = &cursor[chunk_size..];
            let decompressed = lz4_flex::decompress(chunk_data, output_size).map_err(|_| {
                UsdcError::DecompressionFailed {
                    context: "LZ4 chunked: chunk decompress",
                }
            })?;
            out.extend_from_slice(&decompressed);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Integer array decoding (delta + 2-bit codes)
// ---------------------------------------------------------------------------

/// Two-bit code values for the integer array encoder.
const CODE_COMMON: u8 = 0;
const CODE_QUARTER: u8 = 1;
const CODE_HALF: u8 = 2;
const CODE_FULL: u8 = 3;

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

    let num_code_bytes = (count * 2).div_ceil(8);
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
            CODE_FULL => {
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
            _ => unreachable!(),
        };

        prev = prev.wrapping_add(delta);
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

    let compressed_size = u64::from_le_bytes(data[..8].try_into().unwrap());
    #[allow(
        clippy::cast_possible_truncation,
        reason = "compressed blocks are well under 4 GiB"
    )]
    let csz = compressed_size as usize;
    let total = 8 + csz;

    if data.len() < total {
        return Err(UsdcError::UnexpectedEof {
            section: "compressed int array data",
            offset: 8,
            expected: compressed_size,
        });
    }

    let encoded_size = encoded_int_array_size(count, int_size);
    let decompressed = lz4_decompress(&data[8..total], encoded_size)?;
    let elements = decode_integer_array(&decompressed, count, int_size)?;
    Ok((elements, total))
}

/// Computes the expected encoded size of an integer array before LZ4.
///
/// This equals `int_size + num_code_bytes + count * int_size`.
fn encoded_int_array_size(count: usize, int_size: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let num_code_bytes = (count * 2).div_ceil(8);
    int_size + num_code_bytes + count * int_size
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
    fn decode_empty() {
        let result = decode_integer_array(&[], 0, 4).unwrap();
        assert!(result.is_empty());
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
