// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Byte order marks, which a USDA layer must not begin with.

use core::fmt;

/// A Unicode byte order mark at the start of a would-be USDA layer.
///
/// USDA is UTF-8 without a byte order mark (AOUSD Core §16.2). OpenUSD
/// 26.11 refuses to read a `.usda` asset that begins with one, before
/// parsing it (`_CheckBOM`, `pxr/usd/sdf/usdaFileFormat.cpp`), and
/// [`Display`](fmt::Display) gives its reason word for word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrderMark {
    /// `EF BB BF`.
    Utf8,
    /// `FE FF` or `FF FE` (which also begins the UTF-32 little-endian
    /// mark, so that mark is reported as this one, as OpenUSD does).
    Utf16,
    /// `00 00 FE FF`.
    Utf32,
}

impl ByteOrderMark {
    /// The byte order mark `bytes` begins with, checked in OpenUSD's order:
    /// UTF-8, then UTF-16, then UTF-32.
    #[must_use]
    pub fn find(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [0xEF, 0xBB, 0xBF, ..] => Some(Self::Utf8),
            [0xFE, 0xFF, ..] | [0xFF, 0xFE, ..] => Some(Self::Utf16),
            [0x00, 0x00, 0xFE, 0xFF, ..] => Some(Self::Utf32),
            _ => None,
        }
    }
}

impl fmt::Display for ByteOrderMark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Utf8 => "Asset starts with UTF-8 BOM which is not supported",
            Self::Utf16 => "Asset starts with UTF-16 BOM marker which is not supported",
            Self::Utf32 => "Asset starts with UTF-32 BOM marker which is not supported",
        })?;
        f.write_str(", please convert the file to UTF-8 without the BOM.")
    }
}

impl core::error::Error for ByteOrderMark {}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::ByteOrderMark;

    #[test]
    fn marks_are_found_in_openusd_order() {
        let cases: [(&[u8], Option<ByteOrderMark>); 7] = [
            (b"\xEF\xBB\xBF#usda 1.0", Some(ByteOrderMark::Utf8)),
            (b"\xFE\xFF\0#", Some(ByteOrderMark::Utf16)),
            (b"\xFF\xFE#\0", Some(ByteOrderMark::Utf16)),
            (b"\xFF\xFE\0\0#\0\0\0", Some(ByteOrderMark::Utf16)),
            (b"\0\0\xFE\xFF\0\0\0#", Some(ByteOrderMark::Utf32)),
            (b"#usda 1.0\n", None),
            (b"\xEF\xBB", None),
        ];
        for (bytes, mark) in cases {
            assert_eq!(ByteOrderMark::find(bytes), mark, "{bytes:?}");
        }
        assert_eq!(
            ByteOrderMark::Utf8.to_string(),
            "Asset starts with UTF-8 BOM which is not supported, \
             please convert the file to UTF-8 without the BOM."
        );
    }
}
