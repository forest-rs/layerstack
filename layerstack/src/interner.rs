// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! String and path interning for `layerstack`.
//!
//! This module keeps the public types small (`TokenId`, `PathId`): an id
//! stays the same for as long as its interner lives.
//!
//! Spec: AOUSD Core uses tokens pervasively; this crate uses interning to model
//! token identity efficiently (see AOUSD Core terminology around tokens).

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::interning::Interned;

/// A stable identifier for an interned token.
///
/// Ids compare in interning order, which threads interning at once make
/// vary from run to run: an order for maps and sets, not for reporting
/// names, which compare by their strings. OpenUSD:
/// `TfTokenFastArbitraryLessThan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenId(u32);

impl TokenId {
    /// The empty token, `""`, which every [`TokenInterner`] interns first.
    /// A value that needs a token before any is interned, such as the zero
    /// value of the `token` value type, uses it.
    ///
    /// OpenUSD: a default-constructed `TfToken`, which is empty.
    pub const EMPTY: Self = Self(crate::interning::id_of(0));

    /// Returns the underlying numeric identifier.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Interns strings into [`TokenId`] values.
///
/// Interning takes a shared reference, so threads can intern into one
/// interner at once (with the `std` feature, which makes it [`Sync`]). Ids
/// number strings in the order they are first interned; strings interned by
/// several threads at once take ids in whichever order the threads get
/// there, so an id's value says nothing about its string. Every interner
/// starts out holding the empty token, [`TokenId::EMPTY`].
///
/// ```
/// use layerstack::TokenInterner;
///
/// let interner = TokenInterner::default();
/// let id_a = interner.intern("hello");
/// let id_b = interner.intern("hello");  // same string → same id
/// assert_eq!(id_a, id_b);
/// assert_eq!(interner.resolve(id_a), "hello");
/// ```
#[derive(Debug)]
pub struct TokenInterner {
    pub(crate) affinity: Arc<()>,
    strings: Interned<Box<str>>,
    /// Whether some interned string is a variable expression, so a variant
    /// selection may be one ([`crate::variable_expression::is_expression`]).
    expressions: AtomicBool,
}

impl Default for TokenInterner {
    /// An interner holding the empty token ([`TokenId::EMPTY`]).
    fn default() -> Self {
        let interner = Self {
            affinity: Arc::default(),
            strings: Interned::default(),
            expressions: AtomicBool::new(false),
        };
        let empty = interner.intern("");
        debug_assert_eq!(empty, TokenId::EMPTY, "the empty token is interned first");
        interner
    }
}

#[cfg(test)]
impl TokenInterner {
    /// How many strings are interned.
    pub(crate) fn len(&self) -> u32 {
        self.strings.len()
    }
}

impl TokenInterner {
    pub(crate) fn snapshot(&self) -> Self {
        Self {
            affinity: Arc::default(),
            strings: self.strings.snapshot(),
            expressions: AtomicBool::new(self.has_expressions()),
        }
    }

    /// Interns a string, returning its stable [`TokenId`].
    #[must_use]
    pub fn intern(&self, s: impl AsRef<str>) -> TokenId {
        let s = s.as_ref();
        let (id, new) = self.strings.intern(s, || Box::from(s));
        if new && crate::variable_expression::is_expression(s) {
            self.expressions.store(true, Ordering::Release);
        }
        TokenId(id)
    }

    /// Returns the [`TokenId`] of `s` if it has been interned.
    #[must_use]
    pub fn lookup(&self, s: &str) -> Option<TokenId> {
        self.strings.find(s).map(TokenId)
    }

    /// Resolves a [`TokenId`] back to its string.
    #[must_use]
    pub fn resolve(&self, id: TokenId) -> &str {
        self.strings.get(id.0).expect("token id out of range")
    }

    /// Returns `true` when some interned string is a variable expression
    /// ([`crate::variable_expression::is_expression`]), which a variant
    /// selection token may then be.
    pub(crate) fn has_expressions(&self) -> bool {
        self.expressions.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_interner_starts_with_the_empty_token() {
        let tokens = TokenInterner::default();
        assert_eq!(tokens.lookup(""), Some(TokenId::EMPTY));
        assert_eq!(tokens.intern(""), TokenId::EMPTY);
        assert_eq!(tokens.resolve(TokenId::EMPTY), "");
        assert_ne!(tokens.intern("rock"), TokenId::EMPTY);
    }
}
