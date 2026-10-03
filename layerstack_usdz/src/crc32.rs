// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure-Rust CRC-32 implementation.
//!
//! Uses the standard IEEE polynomial (0xEDB88320, bit-reflected) and a
//! const-computed slicing-by-eight lookup tables to process eight bytes
//! per checksum step.

/// Const-computed CRC-32 lookup table (IEEE polynomial, reflected).
const TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let mut i = 0_u32;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
};

// Table n advances one input byte through n additional zero bytes.
// Together these apply the same reflected IEEE polynomial to eight bytes
// at once. Chunk decoding is explicit little-endian, independent of the host.
const SLICES: [[u32; 256]; 8] = {
    let mut slices = [TABLE; 8];
    let mut n = 1;
    while n < slices.len() {
        let mut i = 0;
        while i < 256 {
            let previous = slices[n - 1][i];
            slices[n][i] = (previous >> 8) ^ TABLE[(previous & 0xff) as usize];
            i += 1;
        }
        n += 1;
    }
    slices
};

/// Computes the CRC-32 checksum of `data`.
///
/// ```
/// use layerstack_usdz::crc32::crc32;
///
/// assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
/// assert_eq!(crc32(b""), 0);
/// ```
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    let (chunks, remainder) = data.as_chunks::<8>();
    for chunk in chunks {
        let first =
            (crc ^ u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])).to_le_bytes();
        crc = SLICES[7][usize::from(first[0])]
            ^ SLICES[6][usize::from(first[1])]
            ^ SLICES[5][usize::from(first[2])]
            ^ SLICES[4][usize::from(first[3])]
            ^ SLICES[3][usize::from(chunk[4])]
            ^ SLICES[2][usize::from(chunk[5])]
            ^ SLICES[1][usize::from(chunk[6])]
            ^ SLICES[0][usize::from(chunk[7])];
    }
    for &byte in remainder {
        let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[idx];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn chunk_boundaries_and_unaligned_buffers_match_the_polynomial() {
        let bytes: alloc::vec::Vec<_> = (0_u32..4128)
            .map(|n| n.wrapping_mul(0x9e37_79b9).rotate_left(13).to_le_bytes()[0])
            .collect();
        // A bit-at-a-time reference is independent of the slicing tables.
        let reference = |input: &[u8]| {
            let mut crc = u32::MAX;
            for &byte in input {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ if crc & 1 == 0 { 0 } else { 0xedb8_8320 };
                }
            }
            !crc
        };
        for offset in 0..32 {
            for len in 0..=256 {
                let input = &bytes[offset..offset + len];
                assert_eq!(crc32(input), reference(input), "offset={offset}, len={len}");
            }
            for len in [1023, 1024, 1025, 4095, 4096, 4097] {
                let input = &bytes[offset..offset + len];
                assert_eq!(crc32(input), reference(input), "offset={offset}, len={len}");
            }
        }
    }

    #[test]
    fn known_vectors() {
        // "123456789" has CRC-32 = 0xCBF43926 (IEEE)
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn empty_input() {
        assert_eq!(crc32(b""), 0x0000_0000);
    }
}
