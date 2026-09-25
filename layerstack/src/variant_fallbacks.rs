// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Variant fallback selections.
//!
//! A variant set without an authored selection contributes nothing (AOUSD
//! Core §10.3.2.5). A stage may name fallbacks instead: for each variant set
//! name, an ordered list of variant names to select where no opinion
//! selects one.
//!
//! OpenUSD calls these variant fallbacks (`PcpCache::SetVariantFallbacks`,
//! `UsdStage::SetGlobalVariantFallbacks`).

use alloc::vec::Vec;

use hashbrown::HashMap;

use crate::interner::TokenId;

/// Variant fallback selections: for each variant set name, the variant
/// names to select, in order of preference, where no selection is
/// authored.
pub type VariantFallbacks = HashMap<TokenId, Vec<TokenId>>;
