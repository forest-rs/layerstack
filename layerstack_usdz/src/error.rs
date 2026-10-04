// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Error types for USDZ reading.
//!
//! Spec: AOUSD Core §16.4 (USDZ package format).

use alloc::sync::Arc;
use core::fmt;

use layerstack::doc::LayerId;

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
    /// A host explicitly selected a resident member absent from the archive.
    MissingMember {
        /// Normalized path of the selected member.
        member: Arc<str>,
    },
    /// CRC-32 checksum mismatch on an archive entry.
    CrcMismatch {
        /// Name of the entry with the bad checksum.
        entry: Arc<str>,
        /// Expected CRC-32 value from the central directory.
        expected: u32,
        /// Actual CRC-32 computed from the entry data.
        actual: u32,
    },
    /// A found package member could not be decoded. Recovery diagnostics
    /// from readable members instead appear in [`crate::UsdzResult`].
    LayerRead {
        /// Path of the offending member inside the package.
        member: Arc<str>,
        /// The layer ID assigned to the member for this read.
        layer_id: LayerId,
        /// The original typed failure.
        cause: LayerReadError,
    },
    /// Data too short for the expected structure.
    UnexpectedEof,
    /// A member the package loads needs a layer ID, and the outer
    /// resolver allocates none ([`AssetResolver::allocate_layer_id`]
    /// returned `None`).
    ///
    /// [`AssetResolver::allocate_layer_id`]: layerstack::AssetResolver::allocate_layer_id
    LayerIdUnavailable {
        /// The member's path in the package.
        member: Arc<str>,
    },
    /// Two of the layers read share a layer ID, so installing both would
    /// lose one: the outer resolver returned or allocated an ID twice, or
    /// one equal to the root layer's.
    DuplicateLayerId {
        /// The shared ID.
        id: LayerId,
    },
}

impl fmt::Display for UsdzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidZip { reason } => write!(f, "invalid ZIP: {reason}"),
            Self::ConstraintViolation { reason } => {
                write!(f, "USDZ constraint violated: {reason}")
            }
            Self::MissingMember { member } => {
                write!(f, "selected package member {member:?} is absent")
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
            Self::LayerRead { member, cause, .. } => write!(f, "layer {member:?}: {cause}"),
            Self::UnexpectedEof => write!(f, "unexpected end of data"),
            Self::LayerIdUnavailable { member } => write!(
                f,
                "the outer resolver allocates no layer ID for package member {member:?}"
            ),
            Self::DuplicateLayerId { id } => {
                write!(f, "two layers read from the package share {id:?}")
            }
        }
    }
}

/// A hard failure while decoding a package layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerReadError {
    /// The binary decoder's original error, including its version or offset.
    Usdc(layerstack_usdc::UsdcError),
    /// USDA text was not UTF-8; the error locates the invalid bytes.
    InvalidUtf8(core::str::Utf8Error),
    /// The member's extension does not identify a supported USD layer format.
    UnsupportedFormat,
}

impl fmt::Display for LayerReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usdc(error) => write!(f, "USDC: {error}"),
            Self::InvalidUtf8(error) => write!(f, "USDA is not UTF-8: {error}"),
            Self::UnsupportedFormat => f.write_str("unsupported layer format"),
        }
    }
}

impl core::error::Error for LayerReadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Usdc(error) => Some(error),
            Self::InvalidUtf8(error) => Some(error),
            Self::UnsupportedFormat => None,
        }
    }
}

impl core::error::Error for UsdzError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::LayerRead { cause, .. } => Some(cause),
            _ => None,
        }
    }
}
