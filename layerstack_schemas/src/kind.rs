// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Kinds: the values of a prim's `kind` metadata, and which kinds are
//! kinds of which (`component` is a kind of `model`).
//!
//! Kinds are the vocabulary of the model hierarchy ([`Scene::is_model`],
//! [`Scene::is_group`]) and of the collection predicates `model`, `group`
//! and `kind:`. Composition never consults them, so they live beside the
//! schemas rather than in `layerstack`.
//!
//! Spec: AOUSD Core §7.6.2.4.4 (`kind`), §11.4 (the model hierarchy).
//! OpenUSD: `KindRegistry` (`pxr/usd/kind/registry.h`), whose built-in
//! kinds [`KindRegistry::new`] holds; OpenUSD reads further kinds from
//! plugins, which [`KindRegistry::register`] stands in for.
//!
//! [`Scene::is_model`]: crate::Scene::is_model
//! [`Scene::is_group`]: crate::Scene::is_group

use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt;

/// `model`: the base of every kind in the model hierarchy.
pub const MODEL: &str = "model";
/// `group`: a model that groups models.
pub const GROUP: &str = "group";
/// `assembly`: a group that is a point of interest.
pub const ASSEMBLY: &str = "assembly";
/// `component`: a leaf model.
pub const COMPONENT: &str = "component";
/// `subcomponent`: a point of interest inside a component, outside the
/// model hierarchy.
pub const SUBCOMPONENT: &str = "subcomponent";

/// OpenUSD's built-in kinds and their base kinds
/// (`KindRegistry::_RegisterDefaults`).
const BUILT_IN: [(&str, Option<&str>); 5] = [
    (SUBCOMPONENT, None),
    (MODEL, None),
    (COMPONENT, Some(MODEL)),
    (GROUP, Some(MODEL)),
    (ASSEMBLY, Some(GROUP)),
];

/// The kinds a scene knows: OpenUSD's built-in kinds, and any the caller
/// registers.
///
/// A [`Scene`](crate::Scene) reads kinds with OpenUSD's
/// ([`KindRegistry::openusd`]) unless given another
/// ([`Scene::with_kinds`](crate::Scene::with_kinds)).
///
/// ```
/// use layerstack_schemas::kind::KindRegistry;
///
/// let mut kinds = KindRegistry::new();
/// assert!(kinds.is_a("assembly", "model"));
/// kinds.register("set", Some("assembly")).unwrap();
/// assert!(kinds.is_a("set", "group") && !kinds.is_a("set", "component"));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindRegistry {
    /// Registered kinds beyond the built-in ones, with their base kinds.
    registered: Vec<(Arc<str>, Option<Arc<str>>)>,
}

/// Why a kind was not registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindError {
    /// The name is not an identifier.
    InvalidName(String),
    /// A kind of that name is already registered.
    AlreadyRegistered(String),
}

impl fmt::Display for KindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(f, "invalid kind `{name}`"),
            Self::AlreadyRegistered(name) => write!(f, "kind `{name}` is already registered"),
        }
    }
}

impl KindRegistry {
    /// OpenUSD's built-in kinds, shared by every scene that is not given
    /// others.
    #[must_use]
    pub fn openusd() -> &'static Self {
        static OPENUSD: KindRegistry = KindRegistry::new();
        &OPENUSD
    }

    /// OpenUSD's built-in kinds: `model`, with `component` and `group`
    /// (with `assembly`) below it, and `subcomponent`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            registered: Vec::new(),
        }
    }

    /// Registers `kind`, a kind of `base` (or of none).
    ///
    /// # Errors
    ///
    /// [`KindError`] when `kind` is not an ASCII identifier or is already
    /// registered, as OpenUSD refuses them.
    pub fn register(&mut self, kind: &str, base: Option<&str>) -> Result<(), KindError> {
        let valid = kind
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid {
            return Err(KindError::InvalidName(kind.into()));
        }
        if self.has_kind(kind) {
            return Err(KindError::AlreadyRegistered(kind.into()));
        }
        self.registered.push((Arc::from(kind), base.map(Arc::from)));
        Ok(())
    }

    fn entry(&self, kind: &str) -> Option<Option<&str>> {
        BUILT_IN
            .iter()
            .find(|(name, _)| *name == kind)
            .map(|(_, base)| *base)
            .or_else(|| {
                self.registered
                    .iter()
                    .find(|(name, _)| &**name == kind)
                    .map(|(_, base)| base.as_deref())
            })
    }

    /// Whether `kind` is a known kind.
    ///
    /// OpenUSD: `KindRegistry::HasKind`.
    #[must_use]
    pub fn has_kind(&self, kind: &str) -> bool {
        self.entry(kind).is_some()
    }

    /// The kind `kind` is a kind of, if it is known and has one.
    ///
    /// OpenUSD: `KindRegistry::GetBaseKind`.
    #[must_use]
    pub fn base_kind(&self, kind: &str) -> Option<&str> {
        self.entry(kind).flatten()
    }

    /// Whether `kind` is `base` or, through its base kinds, a kind of it.
    /// An unknown kind is only itself.
    ///
    /// OpenUSD: `KindRegistry::IsA`.
    #[must_use]
    pub fn is_a(&self, kind: &str, base: &str) -> bool {
        let mut current = kind;
        // A registered kind may name itself as a base, directly or not.
        for _ in 0..=BUILT_IN.len() + self.registered.len() {
            if current == base {
                return true;
            }
            match self.base_kind(current) {
                Some(next) => current = next,
                None => return false,
            }
        }
        false
    }

    /// Every known kind, built-in ones first.
    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        BUILT_IN
            .iter()
            .map(|(name, _)| *name)
            .chain(self.registered.iter().map(|(name, _)| &**name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpenUSD 26.08's built-in hierarchy.
    #[test]
    fn built_in_kinds_are_openusd_s() {
        let kinds = KindRegistry::openusd();
        assert!(kinds.is_a("component", "model") && kinds.is_a("assembly", "group"));
        assert!(kinds.is_a("assembly", "model") && kinds.is_a("group", "group"));
        assert!(!kinds.is_a("subcomponent", "model") && !kinds.is_a("model", "group"));
        assert!(kinds.is_a("bogus", "bogus") && !kinds.is_a("bogus", "model"));
        assert!(!kinds.has_kind("bogus") && kinds.has_kind("subcomponent"));
        assert_eq!(kinds.base_kind("assembly"), Some("group"));
        assert_eq!(kinds.kinds().count(), 5);
    }

    #[test]
    fn registered_kinds_extend_the_hierarchy() {
        let mut kinds = KindRegistry::new();
        kinds.register("prop", Some("component")).unwrap();
        kinds.register("loop", Some("loop")).unwrap();
        assert!(kinds.is_a("prop", "model") && !kinds.is_a("loop", "model"));
        assert_eq!(
            kinds.register("prop", None),
            Err(KindError::AlreadyRegistered("prop".into()))
        );
        assert_eq!(
            kinds.register("model", None),
            Err(KindError::AlreadyRegistered("model".into()))
        );
        assert!(matches!(
            kinds.register("1x", None),
            Err(KindError::InvalidName(_))
        ));
    }
}
