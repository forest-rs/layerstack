// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure-Rust CRC-32 implementation.
//!
//! Uses the standard IEEE polynomial (0xEDB88320, bit-reflected) and
//! slicing-by-eight lookup tables to process eight bytes per checksum
//! step: computed while compiling, or on WebAssembly for each checksum.

/// The slicing-by-eight lookup tables of the IEEE polynomial (reflected).
///
/// Table 0 is the byte-at-a-time table; table n advances one input byte
/// through n additional zero bytes. Together they apply the polynomial to
/// eight bytes at once.
const fn slicing_tables() -> [[u32; 256]; 8] {
    let mut slices = [[0_u32; 256]; 8];
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
        slices[0][i as usize] = crc;
        i += 1;
    }
    let mut n = 1;
    while n < slices.len() {
        let mut i = 0;
        while i < 256 {
            let previous = slices[n - 1][i];
            slices[n][i] = (previous >> 8) ^ slices[0][(previous & 0xff) as usize];
            i += 1;
        }
        n += 1;
    }
    slices
}

/// The tables, computed while compiling. WebAssembly computes them for
/// each checksum instead: as data they are 8 KiB of the module that does
/// not compress, and computing them is a few thousand integer steps, next
/// to checksumming a package member.
#[cfg(not(target_family = "wasm"))]
const SLICES: [[u32; 256]; 8] = slicing_tables();

/// Computes the CRC-32 checksum of `data`.
///
/// ```
/// use layerstack_usdz::crc32::crc32;
///
/// assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
/// assert_eq!(crc32(b""), 0);
/// ```
pub fn crc32(data: &[u8]) -> u32 {
    #[cfg(not(target_family = "wasm"))]
    let slices = &SLICES;
    #[cfg(target_family = "wasm")]
    let slices = &slicing_tables();
    let mut crc = 0xFFFF_FFFF_u32;
    // Chunk decoding is explicit little-endian, independent of the host.
    let (chunks, remainder) = data.as_chunks::<8>();
    for chunk in chunks {
        let first =
            (crc ^ u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])).to_le_bytes();
        crc = slices[7][usize::from(first[0])]
            ^ slices[6][usize::from(first[1])]
            ^ slices[5][usize::from(first[2])]
            ^ slices[4][usize::from(first[3])]
            ^ slices[3][usize::from(chunk[4])]
            ^ slices[2][usize::from(chunk[5])]
            ^ slices[1][usize::from(chunk[6])]
            ^ slices[0][usize::from(chunk[7])];
    }
    for &byte in remainder {
        let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ slices[0][idx];
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
