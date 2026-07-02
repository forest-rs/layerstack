// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Error types for USDC reading.
//!
//! Spec: AOUSD Core §16.3 (crate binary format).

use alloc::string::String;
use core::fmt;

/// Errors that can occur while reading a USDC file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsdcError {
    /// Invalid magic bytes (expected `PXR-USDC`).
    InvalidMagic,
    /// Unsupported crate format version.
    UnsupportedVersion {
        /// Major version byte.
        major: u8,
        /// Minor version byte.
        minor: u8,
        /// Patch version byte.
        patch: u8,
    },
    /// Data too short for the expected structure.
    UnexpectedEof {
        /// Which section or structure was being read.
        section: &'static str,
        /// Byte offset where the read was attempted.
        offset: u64,
        /// How many bytes were expected.
        expected: u64,
    },
    /// A section referenced by the TOC exceeds the file bounds.
    SectionOutOfBounds {
        /// Section name (from TOC).
        name: String,
        /// Section start offset.
        offset: u64,
        /// Section size.
        size: u64,
    },
    /// LZ4 decompression failure.
    DecompressionFailed {
        /// Context describing what was being decompressed.
        context: &'static str,
    },
    /// Integer array decoding failure.
    IntegerArrayDecode {
        /// Context describing what was being decoded.
        context: &'static str,
    },
    /// Unrecognized value type in a `ValueRep`.
    UnknownValueType {
        /// The raw type byte.
        type_byte: u8,
    },
    /// Unrecognized spec form.
    UnknownSpecForm {
        /// The raw form value.
        form: u32,
    },
    /// Path reconstruction failure.
    PathReconstruction,
    /// Inconsistent section data.
    Inconsistent {
        /// Description of the inconsistency.
        message: &'static str,
    },
}

impl fmt::Display for UsdcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => write!(f, "invalid magic bytes (expected PXR-USDC)"),
            Self::UnsupportedVersion {
                major,
                minor,
                patch,
            } => write!(f, "unsupported USDC version {major}.{minor}.{patch}"),
            Self::UnexpectedEof {
                section,
                offset,
                expected,
            } => write!(
                f,
                "unexpected EOF in {section} at offset {offset} (expected {expected} bytes)"
            ),
            Self::SectionOutOfBounds { name, offset, size } => write!(
                f,
                "section {name:?} out of bounds (offset={offset}, size={size})"
            ),
            Self::DecompressionFailed { context } => {
                write!(f, "LZ4 decompression failed: {context}")
            }
            Self::IntegerArrayDecode { context } => {
                write!(f, "integer array decode failed: {context}")
            }
            Self::UnknownValueType { type_byte } => {
                write!(f, "unknown value type: {type_byte}")
            }
            Self::UnknownSpecForm { form } => write!(f, "unknown spec form: {form}"),
            Self::PathReconstruction => write!(f, "path reconstruction failed"),
            Self::Inconsistent { message } => write!(f, "inconsistent data: {message}"),
        }
    }
}
