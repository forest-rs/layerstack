// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Errors from writing a USDC file.

use alloc::string::String;
use core::fmt;

use crate::value_type::SpecForm;

/// Why a crate file could not be written.
///
/// Every check runs before any output is produced, so an error never leaves a
/// partial file behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsdcWriteError {
    /// A path is malformed or uses syntax the writer does not support
    /// (variant selections, relationship targets, relative paths).
    InvalidPath {
        /// The rejected path text.
        path: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A spec's form does not fit its path: the pseudo-root must be `/`,
    /// prims need a prim path, attributes and relationships a property path.
    SpecPathMismatch {
        /// The spec path.
        path: String,
        /// The spec form.
        form: SpecForm,
    },
    /// A spec form the writer does not produce (connection, relationship
    /// target, variant, mapper and expression specs; OpenUSD derives
    /// connection and target specs from `connectionPaths` / `targetPaths`
    /// instead of storing them).
    UnsupportedSpecForm {
        /// The spec path.
        path: String,
        /// The spec form.
        form: SpecForm,
    },
    /// Two specs share a path.
    DuplicateSpec {
        /// The repeated path.
        path: String,
    },
    /// There is no pseudo-root spec at `/`.
    MissingPseudoRoot,
    /// A spec's parent prim (or pseudo-root) has no spec.
    MissingParent {
        /// The orphaned spec's path.
        path: String,
    },
    /// A spec has two fields with the same name.
    DuplicateField {
        /// The spec path.
        path: String,
        /// The repeated field name.
        field: String,
    },
    /// A field name is empty.
    InvalidFieldName {
        /// The spec path.
        path: String,
    },
    /// A field name, string, token, asset path or dictionary key contains
    /// NUL. The token table is NUL-delimited and OpenUSD strings end at
    /// NUL, so such text cannot be stored.
    NulInText {
        /// The spec path.
        path: String,
        /// The field holding the text.
        field: String,
    },
    /// A dictionary has two entries with the same key.
    DuplicateDictionaryKey {
        /// The spec path.
        path: String,
        /// The field holding the dictionary.
        field: String,
        /// The repeated key.
        key: String,
    },
    /// A list op is not one OpenUSD can hold: an explicit list combined
    /// with prepended, appended or deleted items, or an item repeated within
    /// one list.
    InvalidListOp {
        /// The spec path.
        path: String,
        /// The field holding the list op.
        field: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The layer exceeds a limit of the format: more than `u32::MAX - 1`
    /// tokens, strings, paths, fields or field set entries, or a file larger
    /// than the 48-bit offsets of value representations can address.
    TooLarge,
}

impl fmt::Display for UsdcWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, reason } => write!(f, "invalid path {path:?}: {reason}"),
            Self::SpecPathMismatch { path, form } => {
                write!(f, "{path}: a {form:?} spec cannot have this path")
            }
            Self::UnsupportedSpecForm { path, form } => {
                write!(f, "{path}: {form:?} specs are not written")
            }
            Self::DuplicateSpec { path } => write!(f, "{path}: duplicate spec"),
            Self::MissingPseudoRoot => write!(f, "no pseudo-root spec at /"),
            Self::MissingParent { path } => write!(f, "{path}: parent has no spec"),
            Self::DuplicateField { path, field } => write!(f, "{path}: duplicate field {field:?}"),
            Self::InvalidFieldName { path } => write!(f, "{path}: empty field name"),
            Self::NulInText { path, field } => write!(f, "{path}: {field} contains NUL"),
            Self::DuplicateDictionaryKey { path, field, key } => {
                write!(f, "{path}: {field} repeats dictionary key {key:?}")
            }
            Self::InvalidListOp {
                path,
                field,
                reason,
            } => write!(f, "{path}: {field}: {reason}"),
            Self::TooLarge => write!(f, "layer exceeds the crate format's limits"),
        }
    }
}

impl core::error::Error for UsdcWriteError {}
