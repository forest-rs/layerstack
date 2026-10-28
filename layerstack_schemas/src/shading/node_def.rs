// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How a shading node's `NodeDefAPI` attributes select its implementation.
//!
//! OpenUSD `UsdShadeNodeDefAPI` (`nodeDefAPI.cpp`): the generated getters
//! return what is authored; these read it the way C++ consults it.

use crate::usd_shade::{NodeDefApi, NodeDefApiImplementationSource};

impl NodeDefApiImplementationSource {
    /// The source OpenUSD consults for an authored `info:implementationSource`:
    /// `authored` itself when the schema lists it, otherwise `Id`.
    ///
    /// C++ `UsdShadeNodeDefAPI::GetImplementationSource` warns about a token
    /// it does not know (or an unauthored one without a fallback) and falls
    /// back to `id`, so a shader with a misspelled source still resolves by
    /// its `info:id`. The `AllowedTokens` validation rule still reports the
    /// authored `Other` value.
    #[must_use]
    pub fn effective(authored: Option<&Self>) -> Self {
        match authored {
            Some(Self::SourceAsset) => Self::SourceAsset,
            Some(Self::SourceCode) => Self::SourceCode,
            Some(Self::Id | Self::Other(_)) | None => Self::Id,
        }
    }
}

impl NodeDefApi<'_> {
    /// The implementation source OpenUSD consults for this node: the
    /// composed `info:implementationSource` when the schema lists it, and
    /// `Id` otherwise ([`NodeDefApiImplementationSource::effective`]).
    /// [`Self::implementation_source`] keeps the authored value.
    #[doc(alias = "UsdShadeNodeDefAPI::GetImplementationSource")]
    #[doc(alias = "GetImplementationSource")]
    #[must_use]
    pub fn effective_implementation_source(&self) -> NodeDefApiImplementationSource {
        NodeDefApiImplementationSource::effective(self.implementation_source().as_ref())
    }
}
