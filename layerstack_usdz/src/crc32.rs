// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure-Rust CRC-32 implementation.
//!
//! Uses the standard IEEE polynomial (0xEDB88320, bit-reflected) and a
//! const-computed 256-entry lookup table.

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
    for &byte in data {
        let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[idx];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32;

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
