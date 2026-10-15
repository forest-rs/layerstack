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
//! plugins. Use [`KindRegistry::register_all`] for validated complete plugin
//! hierarchies, or [`KindRegistry::register`] for incremental declarations.
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

/// Why a complete kind hierarchy could not be registered.
/// Failed batches leave the registry unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KindValidationError {
    /// A declaration has an invalid or already registered name.
    Declaration(KindError),
    /// A base is absent from both the registry and the complete batch.
    UnknownBase {
        /// The derived kind.
        kind: String,
        /// Its missing base.
        base: String,
    },
    /// Following base kinds revisits a kind.
    Cycle {
        /// A kind whose ancestry reaches the cycle.
        kind: String,
    },
}
impl fmt::Display for KindValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Declaration(error) => error.fmt(f),
            Self::UnknownBase { kind, base } => {
                write!(f, "kind `{kind}` has unknown base `{base}`")
            }
            Self::Cycle { kind } => write!(f, "kind `{kind}` reaches an inheritance cycle"),
        }
    }
}
impl core::error::Error for KindValidationError {}

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

    /// Registers a complete batch atomically, accepting bases declared later
    /// in the batch. Names must be unique, bases known and ancestry acyclic.
    /// Validates the resulting hierarchy, including prior `register` calls.
    ///
    /// Existing incremental [`Self::register`] accepts unresolved bases; use
    /// this method at a publication boundary for a checked complete hierarchy.
    /// AOUSD Core §7.6.2.4.4, §11.4; OpenUSD plugin `Kinds` declarations.
    ///
    /// # Errors
    /// Returns [`KindValidationError`] without changing this registry.
    pub fn register_all<'a>(
        &mut self,
        declarations: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    ) -> Result<(), KindValidationError> {
        let mut candidate = self.clone();
        for (kind, base) in declarations {
            candidate
                .register(kind, base)
                .map_err(KindValidationError::Declaration)?;
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Checks that every base is registered and no ancestry contains a cycle.
    /// Does not modify this registry. AOUSD Core §11.4 (model hierarchy).
    ///
    /// # Errors
    /// Returns the first unknown base or cyclic ancestry in registration order.
    pub fn validate(&self) -> Result<(), KindValidationError> {
        let count = BUILT_IN.len() + self.registered.len();
        for kind in self.kinds() {
            let mut current = kind;
            for step in 0..=count {
                let Some(base) = self.base_kind(current) else {
                    break;
                };
                if !self.has_kind(base) {
                    return Err(KindValidationError::UnknownBase {
                        kind: current.into(),
                        base: base.into(),
                    });
                }
                if step == count {
                    return Err(KindValidationError::Cycle { kind: kind.into() });
                }
                current = base;
            }
        }
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
    use alloc::vec;

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
    #[test]
    fn complete_batches_accept_forward_bases_and_reject_without_mutation() {
        let mut kinds = KindRegistry::new();
        kinds
            .register_all([("leaf", Some("branch")), ("branch", Some("component"))])
            .unwrap();
        assert!(
            kinds.is_a("leaf", "model"),
            "forward declarations form a complete hierarchy"
        );
        let before = kinds.clone();
        for batch in [
            vec![("new", Some("missing"))],
            vec![("a", Some("b")), ("b", Some("a"))],
            vec![("self_loop", Some("self_loop"))],
            vec![("fresh", None), ("leaf", None)],
            vec![("fresh", None), ("bad name", None)],
        ] {
            assert!(
                kinds.register_all(batch).is_err(),
                "invalid hierarchy rejects"
            );
            assert_eq!(kinds, before, "a failed batch publishes no declarations");
        }
    }

    #[test]
    fn batch_validation_includes_earlier_incremental_declarations() {
        let mut kinds = KindRegistry::new();
        kinds.register("leaf", Some("branch")).unwrap();
        assert!(
            matches!(
                kinds.validate(),
                Err(KindValidationError::UnknownBase { .. })
            ),
            "unresolved incremental bases are visible"
        );
        kinds.register_all([("branch", Some("model"))]).unwrap();
        assert!(
            kinds.validate().is_ok(),
            "a later batch can complete a staged hierarchy"
        );
        kinds.register("loop", Some("loop")).unwrap();
        let before = kinds.clone();
        assert!(
            matches!(
                kinds.register_all([]),
                Err(KindValidationError::Cycle { .. })
            ),
            "existing cycles reject publication"
        );
        assert_eq!(
            kinds, before,
            "validation leaves incremental declarations untouched"
        );
    }
}
