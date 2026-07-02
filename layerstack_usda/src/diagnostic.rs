// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Parse diagnostics for error reporting and recovery.

use alloc::string::String;

use crate::Span;

/// Severity of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A fatal parse error. The resulting AST may be incomplete.
    Error,
    /// A non-fatal warning.
    Warning,
}

/// A diagnostic message with source location.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// The source span this diagnostic refers to.
    pub span: Span,
    /// Human-readable message.
    pub message: String,
    /// Severity level.
    pub severity: Severity,
}

impl Diagnostic {
    /// Creates a new error diagnostic.
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            severity: Severity::Error,
        }
    }

    /// Creates a new warning diagnostic.
    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            severity: Severity::Warning,
        }
    }
}
