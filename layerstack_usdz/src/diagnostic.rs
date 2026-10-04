// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Evidence retained when a package member is read with recovery.

use alloc::sync::Arc;

use layerstack::{AssetResolveError, LayerId};
use layerstack_usda::diagnostic::{Diagnostic, Severity};
use layerstack_usdc::assemble::AssembleDiagnostic;

/// A diagnostic from a loaded package member.
///
/// The format diagnostic retains its original source span or authored spec
/// path. Spans are byte offsets in `member`, not in the ZIP archive.
#[derive(Clone, Debug)]
pub struct MemberDiagnostic {
    /// Path inside the package, as listed in its directory.
    pub member: Arc<str>,
    /// The layer assigned to this member for this read.
    pub layer_id: LayerId,
    /// The original diagnostic and the phase that reported it.
    pub diagnostic: ImportDiagnostic,
}

/// A recoverable problem reported while reading a package layer.
///
/// Format-specific evidence is preserved rather than converted to display
/// text. Parse and lowering diagnostics can describe incomplete source;
/// emission and assembly diagnostics can describe omitted authored content.
#[derive(Clone, Debug)]
pub enum ImportDiagnostic {
    /// An unresolved package-relative composition dependency.
    AssetResolve {
        /// Authored asset path anchored to this diagnostic's member.
        asset: Arc<str>,
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

#[cfg(test)]
mod tests;
