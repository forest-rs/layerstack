// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! String and path interning for `layerstack`.
//!
//! This module keeps the public types small (`TokenId`, `PathId`): an id
//! stays the same for as long as its interner lives.
//!
//! Spec: AOUSD Core uses tokens pervasively; this crate uses interning to model
//! token identity efficiently (see AOUSD Core terminology around tokens).

use alloc::{boxed::Box, sync::Arc, vec::Vec};
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

    /// The token id whose numeric identifier is `id` ([`Self::get`]).
    pub(crate) const fn from_raw(id: u32) -> Self {
        Self(id)
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
    /// The domains this interner is a snapshot of, directly or through
    /// other snapshots, each with how many leading ids it shares with that
    /// domain: ids below the count name the same strings in both.
    ancestry: Vec<(Arc<()>, u32)>,
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
            ancestry: Vec::new(),
            strings: Interned::default(),
            expressions: AtomicBool::new(false),
        };
        let empty = interner.intern("");
        debug_assert_eq!(empty, TokenId::EMPTY, "the empty token is interned first");
        interner
    }
}

/// The text of `s` before its first NUL, a token's identity
/// ([`TokenInterner::intern`]).
fn before_nul(s: &str) -> &str {
    s.find('\0').map_or(s, |end| &s[..end])
}

#[cfg(test)]
impl TokenInterner {
    /// How many strings are interned.
    pub(crate) fn len(&self) -> u32 {
        self.strings.len()
    }
}

impl TokenInterner {
    /// Sorts `ids` by their text, stably: a sorted run already in place is
    /// merged rather than sorted again. Every sort of tokens by text goes
    /// through here, so the program carries one instance of the standard
    /// library's sort for it rather than one per call site.
    pub(crate) fn sort_by_text(&self, ids: &mut [TokenId]) {
        ids.sort_by(|a, b| self.resolve(*a).cmp(self.resolve(*b)));
    }

    /// Sorts `ids` in dictionary order ([`crate::ident::dictionary_cmp`],
    /// OpenUSD's `TfDictionaryLessThan`), stably. Like
    /// [`Self::sort_by_text`], one sort serves every caller.
    pub(crate) fn sort_in_dictionary_order(&self, ids: &mut [TokenId]) {
        ids.sort_by(|a, b| crate::ident::dictionary_cmp(self.resolve(*a), self.resolve(*b)));
    }

    pub(crate) fn snapshot(&self) -> Self {
        let strings = self.strings.snapshot();
        let shared = strings.len();
        // An ancestor only this interner still refers to can no longer be
        // asked about: no interner or registry of that domain is left.
        let ancestry = self
            .ancestry
            .iter()
            .filter(|(domain, _)| Arc::strong_count(domain) > 1)
            .map(|(domain, count)| (Arc::clone(domain), (*count).min(shared)))
            .chain([(Arc::clone(&self.affinity), shared)])
            .collect();
        Self {
            affinity: Arc::default(),
            ancestry,
            strings,
            expressions: AtomicBool::new(self.has_expressions()),
        }
    }

    /// Marks the ids interned so far, so a value holding them (a schema
    /// registry) can later check that another interner names them alike
    /// ([`Self::names_alike`]).
    pub(crate) fn mark(&self) -> TokenMark {
        TokenMark {
            domain: Arc::clone(&self.affinity),
            len: self.strings.len(),
        }
    }

    /// Whether every id `mark` covers names the same string here as in the
    /// interner that made it: this is that interner, or a snapshot of it
    /// taken after the mark.
    pub(crate) fn names_alike(&self, mark: &TokenMark) -> bool {
        Arc::ptr_eq(&self.affinity, &mark.domain)
            || self
                .ancestry
                .iter()
                .any(|(domain, count)| Arc::ptr_eq(domain, &mark.domain) && *count >= mark.len)
    }

    /// Interns a string, returning its stable [`TokenId`].
    ///
    /// A token is the text before any NUL in `s`: `"a\0b"` and `"a"` are one
    /// token, which resolves to `"a"`. OpenUSD 26.11 gives `TfToken` that
    /// identity however it is created (`pxr/base/tf/token.h`), so a USDA
    /// `token` value with an escaped NUL reads as its prefix.
    #[must_use]
    pub fn intern(&self, s: impl AsRef<str>) -> TokenId {
        let s = before_nul(s.as_ref());
        let (id, new) = self.strings.intern(s, || Box::from(s));
        if new && crate::variable_expression::is_expression(s) {
            self.expressions.store(true, Ordering::Release);
        }
        TokenId(id)
    }

    /// Returns the [`TokenId`] of `s` if it has been interned.
    #[must_use]
    pub fn lookup(&self, s: &str) -> Option<TokenId> {
        self.strings.find(before_nul(s)).map(TokenId)
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

/// The ids a [`TokenInterner`] had interned at one point
/// ([`TokenInterner::mark`]).
#[derive(Clone, Debug)]
pub(crate) struct TokenMark {
    domain: Arc<()>,
    len: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_ends_at_its_first_nul() {
        // OpenUSD 26.11: `TfToken(std::string("a\0b", 3))` is `TfToken("a")`.
        let tokens = TokenInterner::default();
        let a = tokens.intern("a");
        assert_eq!(tokens.intern("a\0b"), a);
        assert_eq!(tokens.resolve(tokens.intern("x\0")), "x");
        assert_eq!(tokens.lookup("a\0z"), Some(a));
        assert_eq!(tokens.intern("\0b"), TokenId::EMPTY);
    }

    #[test]
    fn every_interner_starts_with_the_empty_token() {
        let tokens = TokenInterner::default();
        assert_eq!(tokens.lookup(""), Some(TokenId::EMPTY));
        assert_eq!(tokens.intern(""), TokenId::EMPTY);
        assert_eq!(tokens.resolve(TokenId::EMPTY), "");
        assert_ne!(tokens.intern("rock"), TokenId::EMPTY);
    }
}
