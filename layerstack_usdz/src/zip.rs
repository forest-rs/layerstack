// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal ZIP reader for USDZ packages.
//!
//! USDZ files are constrained ZIP archives (§16.4.1):
//! - All entries use compression method 0 (Stored / uncompressed)
//! - No encryption (general purpose bit flag bits 0 and 6 clear)
//! - 32-bit ZIP only (no Zip64 extensions)
//! - No End of Central Directory comment
//! - Local file header offsets are 64-byte aligned
//! - No data descriptors (bit 3 of general purpose flag clear)
//!
//! These constraints make the format simple enough that a full ZIP library
//! is unnecessary.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::error::UsdzError;

// ── ZIP signatures ──────────────────────────────────────────────────────

/// End of Central Directory signature.
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
/// Central Directory File Header signature.
const CDFH_SIGNATURE: u32 = 0x0201_4b50;
/// Local File Header signature.
const LFH_SIGNATURE: u32 = 0x0403_4b50;

/// Minimum EOCD size (22 bytes, no comment).
const EOCD_MIN_SIZE: usize = 22;
/// Central Directory File Header fixed size (46 bytes).
const CDFH_FIXED_SIZE: usize = 46;
/// Local File Header fixed size (30 bytes).
const LFH_FIXED_SIZE: usize = 30;

// ── Public types ────────────────────────────────────────────────────────

/// A parsed ZIP archive backed by a byte slice.
///
/// Only supports the constrained subset required by USDZ (§16.4.1).
#[derive(Debug)]
pub struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
}

/// A single entry in the ZIP archive.
#[derive(Clone, Debug)]
pub struct ZipEntry {
    /// File name (path within the archive).
    pub name: Arc<str>,
    /// Byte offset of the uncompressed data within the archive.
    pub data_offset: usize,
    /// Uncompressed (and compressed, since Stored) size in bytes.
    pub size: usize,
    /// Expected CRC-32 checksum from the central directory.
    pub crc32: u32,
}

// ── Little-endian helpers ───────────────────────────────────────────────

fn read_u16(data: &[u8], off: usize) -> Result<u16, UsdzError> {
    data.get(off..off + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(UsdzError::UnexpectedEof)
}

fn read_u32(data: &[u8], off: usize) -> Result<u32, UsdzError> {
    data.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(UsdzError::UnexpectedEof)
}

// ── Implementation ──────────────────────────────────────────────────────

impl<'a> ZipArchive<'a> {
    /// Parses a USDZ-constrained ZIP archive from `data`.
    ///
    /// Validates all USDZ constraints (§16.4.1) and populates the entry
    /// list from the Central Directory.
    pub fn parse(data: &'a [u8]) -> Result<Self, UsdzError> {
        // 1. Locate EOCD (End of Central Directory).
        let eocd_offset = find_eocd(data)?;

        // 2. Read EOCD fields.
        let comment_len = read_u16(data, eocd_offset + 20)?;
        if comment_len != 0 {
            return Err(UsdzError::ConstraintViolation {
                reason: "EOCD comment must be empty in USDZ",
            });
        }

        let total_entries = read_u16(data, eocd_offset + 10)? as usize;
        let cd_offset = read_u32(data, eocd_offset + 16)? as usize;

        // 3. Parse Central Directory entries.
        let mut entries = Vec::with_capacity(total_entries);
        let mut pos = cd_offset;

        for _ in 0..total_entries {
            let entry = parse_cd_entry(data, pos)?;
            pos = entry.next_offset;
            entries.push(entry.zip_entry);
        }

        Ok(Self { data, entries })
    }

    /// Returns a slice of all entries in the archive.
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Returns the raw data for an entry.
    pub fn entry_data(&self, entry: &ZipEntry) -> &'a [u8] {
        &self.data[entry.data_offset..entry.data_offset + entry.size]
    }

    /// Finds an entry by name.
    pub fn find(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|e| &*e.name == name)
    }
}

// ── EOCD search ─────────────────────────────────────────────────────────

/// Locates the End of Central Directory record by scanning backwards.
///
/// USDZ requires no comment, so EOCD is always the last 22 bytes — but
/// we still scan backwards for robustness.
fn find_eocd(data: &[u8]) -> Result<usize, UsdzError> {
    if data.len() < EOCD_MIN_SIZE {
        return Err(UsdzError::InvalidZip {
            reason: "file too small for EOCD",
        });
    }

    // USDZ forbids comments, so check the fixed position first.
    let fast_offset = data.len() - EOCD_MIN_SIZE;
    if read_u32(data, fast_offset)? == EOCD_SIGNATURE {
        return Ok(fast_offset);
    }

    // Fallback: scan backwards (max 65557 bytes for comment + EOCD).
    let search_start = data.len().saturating_sub(EOCD_MIN_SIZE + 0xFFFF);
    for off in (search_start..=fast_offset).rev() {
        if read_u32(data, off).ok() == Some(EOCD_SIGNATURE) {
            return Ok(off);
        }
    }

    Err(UsdzError::InvalidZip {
        reason: "EOCD signature not found",
    })
}

// ── Central Directory entry parsing ─────────────────────────────────────

/// Temporary struct for CD parsing that includes next offset.
struct CdParsed {
    zip_entry: ZipEntry,
    next_offset: usize,
}

/// Parses a single Central Directory File Header at `offset`.
fn parse_cd_entry(data: &[u8], offset: usize) -> Result<CdParsed, UsdzError> {
    if offset + CDFH_FIXED_SIZE > data.len() {
        return Err(UsdzError::UnexpectedEof);
    }

    let sig = read_u32(data, offset)?;
    if sig != CDFH_SIGNATURE {
        return Err(UsdzError::InvalidZip {
            reason: "bad Central Directory entry signature",
        });
    }

    let flags = read_u16(data, offset + 8)?;
    let compression = read_u16(data, offset + 10)?;
    let crc32_val = read_u32(data, offset + 16)?;
    let compressed_size = read_u32(data, offset + 20)? as usize;
    let uncompressed_size = read_u32(data, offset + 24)? as usize;
    let name_len = read_u16(data, offset + 28)? as usize;
    let extra_len = read_u16(data, offset + 30)? as usize;
    let comment_len = read_u16(data, offset + 32)? as usize;
    let local_header_offset = read_u32(data, offset + 42)? as usize;

    // USDZ constraints.
    if compression != 0 {
        return Err(UsdzError::ConstraintViolation {
            reason: "compression method must be 0 (Stored)",
        });
    }
    if flags & 0x01 != 0 {
        return Err(UsdzError::ConstraintViolation {
            reason: "encryption is not allowed",
        });
    }
    if flags & 0x40 != 0 {
        return Err(UsdzError::ConstraintViolation {
            reason: "strong encryption is not allowed",
        });
    }
    if flags & 0x08 != 0 {
        return Err(UsdzError::ConstraintViolation {
            reason: "data descriptors are not allowed",
        });
    }
    if compressed_size != uncompressed_size {
        return Err(UsdzError::ConstraintViolation {
            reason: "compressed size must equal uncompressed size (Stored)",
        });
    }
    if !local_header_offset.is_multiple_of(64) {
        return Err(UsdzError::ConstraintViolation {
            reason: "local file header must be 64-byte aligned",
        });
    }

    // Read file name.
    let name_start = offset + CDFH_FIXED_SIZE;
    let name_end = name_start + name_len;
    if name_end > data.len() {
        return Err(UsdzError::UnexpectedEof);
    }
    let name =
        core::str::from_utf8(&data[name_start..name_end]).map_err(|_| UsdzError::InvalidZip {
            reason: "file name is not valid UTF-8",
        })?;

    // Validate the Local File Header and compute data offset.
    let data_offset = validate_local_header(data, local_header_offset)?;

    let next = name_end + extra_len + comment_len;

    Ok(CdParsed {
        zip_entry: ZipEntry {
            name: Arc::from(name),
            data_offset,
            size: uncompressed_size,
            crc32: crc32_val,
        },
        next_offset: next,
    })
}

// ── Local File Header validation ────────────────────────────────────────

/// Validates a Local File Header and returns the data offset.
fn validate_local_header(data: &[u8], offset: usize) -> Result<usize, UsdzError> {
    if offset + LFH_FIXED_SIZE > data.len() {
        return Err(UsdzError::UnexpectedEof);
    }

    let sig = read_u32(data, offset)?;
    if sig != LFH_SIGNATURE {
        return Err(UsdzError::InvalidZip {
            reason: "bad Local File Header signature",
        });
    }

    let name_len = read_u16(data, offset + 26)? as usize;
    let extra_len = read_u16(data, offset + 28)? as usize;

    let data_offset = offset + LFH_FIXED_SIZE + name_len + extra_len;
    if data_offset > data.len() {
        return Err(UsdzError::UnexpectedEof);
    }

    Ok(data_offset)
}
