// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The error the bindings throw.

use alloc::string::{String, ToString};
use core::fmt;

use layerstack_io::IoError;
use wasm_bindgen::JsValue;

use crate::wire::Wire;

/// Why a call into the bindings failed: a stable code, the failure in
/// words, and the source and path it is about.
///
/// The TypeScript wrapper turns it into a `LayerstackError` with the same
/// fields. Codes are dotted lowercase, a family and a case:
///
/// - `io.*`: [`layerstack_io::IoErrorKind::code`], such as `io.unavailable`
///   or `io.rejected`.
/// - `path.invalid`: a path argument is not an absolute prim path (or a
///   property path, where one is asked for).
/// - `prim.not_found`: no composed prim is at the path.
/// - `layer.unknown`: no layer of the stage has the id.
/// - `value.decode`: a retained array failed to decode when read.
///
/// Codes are part of the public contract: a released code keeps its
/// spelling and meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// The stable code naming the kind of failure.
    pub code: &'static str,
    /// The failure in words.
    pub message: String,
    /// The source identifier the failure is about, if any.
    pub identifier: Option<String>,
    /// The prim or property path the failure is about, if any.
    pub path: Option<String>,
}

impl Error {
    /// An error with `code` and `message`, naming nothing.
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            identifier: None,
            path: None,
        }
    }

    /// This error, naming the path it is about.
    #[must_use]
    pub fn at_path(mut self, path: &str) -> Self {
        self.path = Some(path.to_string());
        self
    }

    /// The positional record the wrapper decodes:
    /// `[code, message, identifier, path]`.
    pub(crate) fn to_wire(&self) -> Wire {
        Wire::List(alloc::vec![
            Wire::text(self.code),
            Wire::text(self.message.clone()),
            Wire::text_or_null(self.identifier.clone()),
            Wire::text_or_null(self.path.clone()),
        ])
    }
}

impl From<IoError> for Error {
    fn from(error: IoError) -> Self {
        Self {
            code: error.code(),
            message: error.message,
            identifier: error.identifier,
            path: None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl core::error::Error for Error {}

impl From<Error> for JsValue {
    fn from(error: Error) -> Self {
        error.to_wire().into()
    }
}
