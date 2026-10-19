// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Diagnostics in words, as `layerstack` writes its own: each
//! diagnostic's `display(store)` returns an `impl Display` that writes its
//! ids as text with the store they belong to.

use core::fmt;

/// A `Display` that writes with a closure.
pub(crate) struct Shown<F>(pub(crate) F);

impl<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result> fmt::Display for Shown<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(f)
    }
}
