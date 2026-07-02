// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Table of Contents parsing.
//!
//! The TOC sits at the end of a USDC file (at the offset stored in the
//! header) and lists each section by name, offset, and size.
//!
//! Spec: AOUSD Core §16.3.3.

use alloc::string::String;

use crate::error::UsdcError;

/// A single TOC section entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionEntry {
    /// Absolute byte offset of the section data.
    pub offset: u64,
    /// Size of the section data in bytes.
    pub size: u64,
}

/// Known section names.
pub const TOKENS: &str = "TOKENS";
/// Known section name for string indices.
pub const STRINGS: &str = "STRINGS";
/// Known section name for field definitions.
pub const FIELDS: &str = "FIELDS";
/// Known section name for field set groups.
pub const FIELDSETS: &str = "FIELDSETS";
/// Known section name for path hierarchy.
pub const PATHS: &str = "PATHS";
/// Known section name for spec definitions.
pub const SPECS: &str = "SPECS";

/// Maximum number of sections we expect (guard against corrupt data).
const MAX_SECTIONS: u64 = 64;

/// Parsed Table of Contents.
///
/// Stores up to 6 known sections. Unknown section names are silently skipped.
#[derive(Clone, Debug, Default)]
pub struct Toc {
    /// TOKENS section entry.
    pub tokens: Option<SectionEntry>,
    /// STRINGS section entry.
    pub strings: Option<SectionEntry>,
    /// FIELDS section entry.
    pub fields: Option<SectionEntry>,
    /// FIELDSETS section entry.
    pub fieldsets: Option<SectionEntry>,
    /// PATHS section entry.
    pub paths: Option<SectionEntry>,
    /// SPECS section entry.
    pub specs: Option<SectionEntry>,
}

/// Parses the Table of Contents from `data` at the given `toc_offset`.
///
/// Spec: AOUSD Core §16.3.3.
pub fn parse_toc(data: &[u8], toc_offset: u64) -> Result<Toc, UsdcError> {
    #[allow(clippy::cast_possible_truncation, reason = "files >4 GiB unsupported")]
    let off = toc_offset as usize;
    if off + 8 > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "TOC",
            offset: toc_offset,
            expected: 8,
        });
    }

    let num_sections = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
    if num_sections > MAX_SECTIONS {
        return Err(UsdcError::Inconsistent {
            message: "TOC section count exceeds maximum",
        });
    }

    // Each entry: 16-byte name + 8-byte offset + 8-byte size = 32 bytes.
    let entries_start = off + 8;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded by MAX_SECTIONS (64)"
    )]
    let entries_size = num_sections as usize * 32;
    if entries_start + entries_size > data.len() {
        return Err(UsdcError::UnexpectedEof {
            section: "TOC entries",
            offset: entries_start as u64,
            expected: entries_size as u64,
        });
    }

    let mut toc = Toc::default();

    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded by MAX_SECTIONS (64)"
    )]
    for i in 0..num_sections as usize {
        let base = entries_start + i * 32;

        // 16-byte null-terminated section name.
        let name_bytes = &data[base..base + 16];
        let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(16);
        let name = core::str::from_utf8(&name_bytes[..name_end]).unwrap_or("");

        let section_offset = u64::from_le_bytes(data[base + 16..base + 24].try_into().unwrap());
        let section_size = u64::from_le_bytes(data[base + 24..base + 32].try_into().unwrap());

        let entry = SectionEntry {
            offset: section_offset,
            size: section_size,
        };

        // Validate bounds.
        if section_offset + section_size > data.len() as u64 {
            return Err(UsdcError::SectionOutOfBounds {
                name: String::from(name),
                offset: section_offset,
                size: section_size,
            });
        }

        match name {
            TOKENS => toc.tokens = Some(entry),
            STRINGS => toc.strings = Some(entry),
            FIELDS => toc.fields = Some(entry),
            FIELDSETS => toc.fieldsets = Some(entry),
            PATHS => toc.paths = Some(entry),
            SPECS => toc.specs = Some(entry),
            _ => {} // Unknown sections are silently ignored.
        }
    }

    Ok(toc)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    fn make_toc_entry(name: &str, offset: u64, size: u64) -> [u8; 32] {
        let mut buf = [0_u8; 32];
        let name_bytes = name.as_bytes();
        buf[..name_bytes.len()].copy_from_slice(name_bytes);
        buf[16..24].copy_from_slice(&offset.to_le_bytes());
        buf[24..32].copy_from_slice(&size.to_le_bytes());
        buf
    }

    #[test]
    fn parse_two_sections() {
        // File data: 256 bytes of padding, then TOC at offset 256.
        let mut data = vec![0_u8; 256];

        // TOC header: 2 sections.
        data.extend_from_slice(&2_u64.to_le_bytes());

        // Entry 1: TOKENS at offset 32, size 64.
        data.extend_from_slice(&make_toc_entry("TOKENS", 32, 64));

        // Entry 2: PATHS at offset 100, size 50.
        data.extend_from_slice(&make_toc_entry("PATHS", 100, 50));

        let toc = parse_toc(&data, 256).unwrap();
        assert_eq!(
            toc.tokens,
            Some(SectionEntry {
                offset: 32,
                size: 64
            })
        );
        assert_eq!(
            toc.paths,
            Some(SectionEntry {
                offset: 100,
                size: 50
            })
        );
        assert_eq!(toc.strings, None);
        assert_eq!(toc.fields, None);
    }
}
