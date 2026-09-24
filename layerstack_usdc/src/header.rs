// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDC file header parsing.
//!
//! The header occupies the first 32 bytes of a USDC file and contains the
//! magic identifier, format version, and TOC offset.
//!
//! Spec: AOUSD Core §16.3.2.

use crate::error::UsdcError;
use crate::version::CrateVersion;

/// Magic bytes at the start of every USDC file.
const MAGIC: &[u8; 8] = b"PXR-USDC";

/// Parsed USDC file header.
///
/// Spec: AOUSD Core §16.3.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Format version as `[major, minor, patch]`.
    pub version: [u8; 3],
    /// Absolute byte offset of the Table of Contents.
    pub toc_offset: u64,
}

/// Parses the 32-byte USDC header from the start of `data`.
///
/// Validates the magic bytes and the version range. Versions outside
/// [`CrateVersion::OLDEST_READABLE`]..=[`CrateVersion::NEWEST_READABLE`]
/// fail with [`UsdcError::UnsupportedVersion`]; see [`crate::version`].
///
/// Spec: AOUSD Core §16.3.2.
///
/// ```
/// use layerstack_usdc::header::parse_header;
///
/// let mut buf = [0u8; 32];
/// buf[..8].copy_from_slice(b"PXR-USDC");
/// buf[9] = 9; // version 0.9.0
/// buf[16..24].copy_from_slice(&1024u64.to_le_bytes());
///
/// let hdr = parse_header(&buf).unwrap();
/// assert_eq!(hdr.version, [0, 9, 0]);
/// assert_eq!(hdr.toc_offset, 1024);
/// ```
pub fn parse_header(data: &[u8]) -> Result<Header, UsdcError> {
    if data.len() < 32 {
        return Err(UsdcError::UnexpectedEof {
            section: "header",
            offset: 0,
            expected: 32,
        });
    }

    // Bytes 0–7: magic "PXR-USDC".
    if &data[..8] != MAGIC {
        return Err(UsdcError::InvalidMagic);
    }

    // Bytes 8–10: version (major, minor, patch).
    let major = data[8];
    let minor = data[9];
    let patch = data[10];

    // Bytes 11–15: reserved (5 bytes, must be zero).
    // We warn-and-continue in the Python reference; here we just ignore.

    if !CrateVersion::new(major, minor, patch).is_readable() {
        return Err(UsdcError::UnsupportedVersion {
            major,
            minor,
            patch,
        });
    }

    // Bytes 16–23: TOC offset (u64 LE).
    let toc_offset = u64::from_le_bytes(data[16..24].try_into().unwrap());

    // Bytes 24–31: reserved (u64, should be zero).
    // Ignored per Python reference.

    Ok(Header {
        version: [major, minor, patch],
        toc_offset,
    })
}

impl Header {
    /// The header's format version as a [`CrateVersion`].
    #[must_use]
    pub fn crate_version(&self) -> CrateVersion {
        let [major, minor, patch] = self.version;
        CrateVersion::new(major, minor, patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_header() {
        let mut buf = [0_u8; 32];
        buf[..8].copy_from_slice(b"PXR-USDC");
        buf[8] = 0; // major
        buf[9] = 9; // minor
        buf[10] = 0; // patch
        // toc_offset = 1024
        buf[16..24].copy_from_slice(&1024_u64.to_le_bytes());

        let h = parse_header(&buf).unwrap();
        assert_eq!(h.version, [0, 9, 0]);
        assert_eq!(h.toc_offset, 1024);
    }

    #[test]
    fn invalid_magic() {
        let buf = [0_u8; 32];
        assert_eq!(parse_header(&buf), Err(UsdcError::InvalidMagic));
    }

    #[test]
    fn unsupported_version() {
        let mut buf = [0_u8; 32];
        buf[..8].copy_from_slice(b"PXR-USDC");
        buf[8] = 1; // major = 1
        buf[9] = 0;
        buf[10] = 0;
        assert!(matches!(
            parse_header(&buf),
            Err(UsdcError::UnsupportedVersion { .. })
        ));
    }

    fn header_with_version(major: u8, minor: u8, patch: u8) -> [u8; 32] {
        let mut buf = [0_u8; 32];
        buf[..8].copy_from_slice(b"PXR-USDC");
        buf[8] = major;
        buf[9] = minor;
        buf[10] = patch;
        buf
    }

    #[test]
    fn readable_version_range() {
        for minor in 7..=12 {
            let h = parse_header(&header_with_version(0, minor, 0)).unwrap();
            assert_eq!(h.crate_version(), CrateVersion::new(0, minor, 0));
        }
        // Patch levels are forward compatible.
        assert!(parse_header(&header_with_version(0, 12, 3)).is_ok());
        for (major, minor) in [(0, 6), (0, 13), (0, 255), (1, 0)] {
            assert_eq!(
                parse_header(&header_with_version(major, minor, 0)),
                Err(UsdcError::UnsupportedVersion {
                    major,
                    minor,
                    patch: 0
                })
            );
        }
    }

    #[test]
    fn too_short() {
        let buf = [0_u8; 16];
        assert!(matches!(
            parse_header(&buf),
            Err(UsdcError::UnexpectedEof { .. })
        ));
    }
}
