// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Diagnostics in words.
//!
//! Every diagnostic that names paths, names or specs by id renders with
//! the store those ids belong to: `display(store)` returns an
//! `impl Display` that writes them as text (`/World/Sun.inputs:intensity`).
//! A diagnostic that is also an error type implements `Display` too,
//! which, having no store, writes the ids themselves.

use alloc::{format, string::String};
use core::fmt;

use crate::{
    doc::{LayerId, LayerStore},
    interner::TokenId,
    path::{PathId, PropertyPath, TargetPath},
    spec_path::SpecPath,
};

/// What writes a diagnostic's ids: the store they belong to, or, without
/// one, the ids themselves.
#[derive(Clone, Copy)]
pub(crate) struct Names<'a>(pub(crate) Option<&'a dyn LayerStore>);

impl Names<'_> {
    /// A prim path (`/World/Sun`).
    pub(crate) fn path(self, id: PathId) -> String {
        match self.0 {
            Some(store) => store.paths().display(id, store.tokens()),
            None => format!("{id:?}"),
        }
    }

    /// A name (`inputs:intensity`).
    pub(crate) fn token(self, id: TokenId) -> String {
        match self.0 {
            Some(store) => store.tokens().resolve(id).into(),
            None => format!("{id:?}"),
        }
    }

    /// A spec path (`/World/Fern{season=autumn}.color`); without a store,
    /// its prim path's id, `{..}` for its variant selections, and its
    /// property's id.
    pub(crate) fn spec(self, path: &SpecPath) -> String {
        if let Some(store) = self.0 {
            return path.display(store.paths(), store.tokens());
        }
        let mut out = self.path(path.prim_path());
        if path.has_variant_selections() {
            out.push_str("{..}");
        }
        if let Some(name) = path.property() {
            out.push('.');
            out.push_str(&self.token(name));
        }
        out
    }

    /// A property path (`/World/Sun.inputs:intensity`).
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

    /// A layer. A store does not name its layers, so this is the id
    /// (`layer 3`); a host maps it to an identifier.
    pub(crate) fn layer(self, id: LayerId) -> String {
        format!("layer {}", id.0)
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
