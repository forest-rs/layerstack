// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Diagnostics in words, as `layerstack` writes its own: each
//! diagnostic's `display(store)` returns an `impl Display` that writes its
//! ids as text with the store they belong to. An error type's `Display`,
//! having no store, writes the same sentence with the ids themselves.

use alloc::{format, string::String};
use core::fmt;
use layerstack::{EditError, LayerId, LayerStore, PathId, PropertyPath, TargetPath, TokenId};

/// A `Display` that writes with a closure.
#[cfg(feature = "usd")]
pub(crate) struct Shown<F>(pub(crate) F);

#[cfg(feature = "usd")]
impl<F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result> fmt::Display for Shown<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(f)
    }
}

/// What writes a diagnostic's ids: the store they belong to, or, without
/// one, the ids themselves.
#[derive(Clone, Copy)]
pub(crate) struct Names<'a>(pub(crate) Option<&'a dyn LayerStore>);

#[allow(dead_code, reason = "each method is used by some feature set")]
impl Names<'_> {
    /// A prim path (`/World/Rock`).
    pub(crate) fn path(self, id: PathId) -> String {
        match self.0 {
            Some(store) => store.paths().display(id, store.tokens()),
            None => format!("{id:?}"),
        }
    }

    /// A name (`primvars:height`).
    pub(crate) fn token(self, id: TokenId) -> String {
        match self.0 {
            Some(store) => store.tokens().resolve(id).into(),
            None => format!("{id:?}"),
        }
    }

    /// A property path (`/World/Rock.primvars:height`).
    pub(crate) fn property(self, path: PropertyPath) -> String {
        let mut out = self.path(path.prim_path());
        out.push('.');
        out.push_str(&self.token(path.property()));
        out
    }

    /// A target path, of a prim or a property.
    pub(crate) fn target(self, path: TargetPath) -> String {
        match path {
            TargetPath::Prim(prim) => self.path(prim),
            TargetPath::Property(property) => self.property(property),
        }
    }

    /// A layer: its identifier as OpenUSD writes a layer (`@./set.usda@`)
    /// when the store names it, else its id (`layer 3`).
    pub(crate) fn layer(self, id: LayerId) -> String {
        match self.0.and_then(|store| store.layer_identifier(id)) {
            Some(identifier) => format!("@{identifier}@"),
            None => format!("layer {}", id.0),
        }
    }

    /// An edit error, through [`EditError::display`] with a store.
    pub(crate) fn edit(self, f: &mut fmt::Formatter<'_>, error: &EditError) -> fmt::Result {
        match self.0 {
            Some(store) => write!(f, "{}", error.display(store)),
            None => write!(f, "{error}"),
        }
    }
}

/// A diagnostic that writes itself in words with [`Names`].
pub(crate) trait Describe {
    /// Writes the diagnostic, its ids written by `names`.
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result;
}

/// A diagnostic and the names it is written with.
pub(crate) struct Described<'a, T: ?Sized>(pub(crate) &'a T, pub(crate) Names<'a>);

impl<T: Describe + ?Sized> fmt::Display for Described<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.describe(f, self.1)
    }
}

/// `item` written with the store its ids belong to.
pub(crate) fn with_store<'a, T: Describe + ?Sized>(
    item: &'a T,
    store: &'a dyn LayerStore,
) -> impl fmt::Display + 'a {
    Described(item, Names(Some(store)))
}

#[cfg(all(test, feature = "all"))]
mod tests;
