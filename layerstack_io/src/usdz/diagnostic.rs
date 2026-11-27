// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Evidence retained when a package member is read with recovery.

use alloc::sync::Arc;
use core::fmt;

use crate::usda::diagnostic::{Diagnostic, Severity};
use crate::usdc::AssembleDiagnostic;
use layerstack::{AssetResolveError, LayerId};

/// A diagnostic from a loaded package member.
///
/// The format diagnostic retains its original source span or authored spec
/// path. Spans are byte offsets in `member`, not in the ZIP archive.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MemberDiagnostic {
    /// Path inside the package, as listed in its directory.
    pub member: Arc<str>,
    /// The layer assigned to this member for this read.
    pub layer: LayerId,
    /// The original diagnostic and the phase that reported it.
    pub diagnostic: ImportDiagnostic,
}

impl MemberDiagnostic {
    /// The diagnostic `diagnostic` of the member at `member`, read as the
    /// layer `layer`: for a reader of a source outside a package, such as
    /// a loose file, to report in the same shape.
    #[must_use]
    pub fn new(member: Arc<str>, layer: LayerId, diagnostic: ImportDiagnostic) -> Self {
        Self {
            member,
            layer,
            diagnostic,
        }
    }
}

/// Writes `member: diagnostic`.
impl fmt::Display for MemberDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.member, self.diagnostic)
    }
}

/// A recoverable problem reported while reading a package layer.
///
/// Format-specific evidence is preserved rather than converted to display
/// text. Parse and lowering diagnostics can describe incomplete source;
/// emission and assembly diagnostics can describe omitted authored content.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ImportDiagnostic {
    /// An unresolved package-relative composition dependency.
    AssetResolve {
        /// Authored asset path anchored to this diagnostic's member.
        asset_path: Arc<str>,
        /// Original resolver error.
        error: AssetResolveError,
    },
    /// USDA syntax parsing, with a source span and severity.
    UsdaParse(Diagnostic),
    /// USDA CST-to-AST lowering, with a source span and severity.
    UsdaLower(Diagnostic),
    /// USDA AST-to-layer emission, with a source span and severity.
    UsdaEmit(Diagnostic),
    /// USDC content not represented in the layer, with spec and field context.
    UsdcAssemble(AssembleDiagnostic),
}

impl ImportDiagnostic {
    /// Whether this reports malformed or unrepresented content.
    ///
    /// USDA keeps its reported severity. Every USDC assembly diagnostic
    /// reports content the layer model did not represent and counts as an
    /// error here. Warnings remain available to callers with stricter policy.
    #[must_use]
    pub fn is_error(&self) -> bool {
        match self {
            Self::UsdaParse(d) | Self::UsdaLower(d) | Self::UsdaEmit(d) => {
                matches!(d.severity, Severity::Error)
            }
            Self::UsdcAssemble(_) | Self::AssetResolve { .. } => true,
        }
    }
}

/// Writes the phase that reported it, then the diagnostic: `USDA parse
/// error at bytes 10..14: ...`, `USDC assembly /A.b: ...`.
impl fmt::Display for ImportDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AssetResolve { asset_path, error } => write!(f, "asset `{asset_path}` {error}"),
            Self::UsdaParse(d) => write!(f, "USDA parse {d}"),
            Self::UsdaLower(d) => write!(f, "USDA lowering {d}"),
            Self::UsdaEmit(d) => write!(f, "USDA emission {d}"),
            Self::UsdcAssemble(d) => write!(f, "USDC assembly {d}"),
        }
    }
}

#[cfg(test)]
mod tests;
