// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Error types for USDZ reading.
//!
//! Spec: AOUSD Core §16.4 (USDZ package format).

use alloc::sync::Arc;
use core::fmt;

/// Errors that can occur while reading a USDZ package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsdzError {
    /// Not a valid ZIP file (bad magic or structure).
    InvalidZip {
        /// Description of what went wrong.
        reason: &'static str,
    },
    /// A USDZ constraint was violated (§16.4.1).
    ConstraintViolation {
        /// Description of which constraint was violated.
        reason: &'static str,
    },
    /// No root layer found (empty archive or first file is not a USD layer).
    NoRootLayer,
    /// CRC-32 checksum mismatch on an archive entry.
    CrcMismatch {
        /// Name of the entry with the bad checksum.
        entry: Arc<str>,
        /// Expected CRC-32 value from the central directory.
        expected: u32,
        /// Actual CRC-32 computed from the entry data.
        actual: u32,
    },
    /// The contained USD layer could not be parsed.
    LayerParseError {
        /// Description of the parse error.
        message: Arc<str>,
    },
    /// Data too short for the expected structure.
    UnexpectedEof,
}

impl fmt::Display for UsdzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidZip { reason } => write!(f, "invalid ZIP: {reason}"),
            Self::ConstraintViolation { reason } => {
                write!(f, "USDZ constraint violated: {reason}")
            }
            Self::NoRootLayer => write!(f, "no root USD layer found in package"),
            Self::CrcMismatch {
                entry,
                expected,
                actual,
            } => write!(
                f,
                "CRC-32 mismatch for {entry:?}: expected {expected:#010x}, got {actual:#010x}"
            ),
            Self::LayerParseError { message } => write!(f, "layer parse error: {message}"),
            Self::UnexpectedEof => write!(f, "unexpected end of data"),
        }
    }
}
