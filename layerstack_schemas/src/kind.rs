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
//! [`KindRegistry::new`] holds OpenUSD's built-in kinds. OpenUSD reads
//! further kinds from each plugin's `plugInfo.json` (`Info.Kinds`, each
//! with an optional `baseKind`); `layerstack_schemagen` reads the same
//! declarations at build time (a schema library's `rust_module` has
//! `KINDS` and `register_kinds`, and `--kind-plugin` makes a module of
//! kinds alone), and [`KindRegistry::register_all`] registers them as
//! OpenUSD does: one by one, skipping what it rejects. What OpenUSD accepts
//! but cannot answer for, a base nobody declares or a cycle of bases, is
//! reported by [`KindRegistry::issues`].
//!
//! Spec: AOUSD Core §7.6.2.4.4 (`kind`), §11.4 (the model hierarchy).
//! OpenUSD: `KindRegistry` (`pxr/usd/kind/registry.cpp`).
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
/// use layerstack_schemas::kind::{KindError, KindRegistry};
///
/// let mut kinds = KindRegistry::new();
/// assert!(kinds.is_a("assembly", "model"));
/// kinds.register("set", Some("assembly")).unwrap();
/// assert!(kinds.is_a("set", "group") && !kinds.is_component("set"));
/// // As OpenUSD reads a plugin's `Kinds`: each registers or is skipped.
/// let rejected = kinds.register_all([("shot", Some("set")), ("model", None)]);
/// assert_eq!(rejected, [KindError::AlreadyRegistered("model".into())]);
/// assert!(kinds.is_group("shot"));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindRegistry {
    /// Registered kinds beyond the built-in ones, with their base kinds.
    registered: Vec<(Arc<str>, Option<Arc<str>>)>,
}

/// Why a kind was not registered. OpenUSD rejects the same declarations,
/// with a coding error, and goes on with the next.
///
/// OpenUSD: `KindRegistry::_Register`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindError {
    /// The name is not an identifier (OpenUSD: "Invalid kind").
    InvalidName(String),
    /// A kind of that name is registered already: a built-in kind, or one
    /// registered before (OpenUSD: "Kind '…' has already been
    /// registered"). The first registration stands.
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

impl core::error::Error for KindError {}

/// A registered hierarchy OpenUSD accepts but cannot fully answer for
/// ([`KindRegistry::issues`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KindIssue {
    /// `kind`'s base is no known kind. OpenUSD registers it anyway, and
    /// `IsA` follows the name: `kind` is a kind of `base` and of nothing
    /// above it.
    UnknownBase {
        /// The registered kind.
        kind: String,
        /// Its base, which no kind is.
        base: String,
    },
    /// Following base kinds from `kinds[0]` comes back to it through the
    /// rest, in order. OpenUSD registers the kinds, and `IsA` from a kind
    /// that reaches the cycle, for a kind it never meets on the way, does
    /// not return; [`KindRegistry::is_a`] answers `false`.
    Cycle {
        /// The kinds of the cycle, the first registered first, each the
        /// base of the one before.
        kinds: Vec<String>,
    },
}

impl fmt::Display for KindIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBase { kind, base } => {
                write!(f, "kind `{kind}` has base `{base}`, which is no kind")
            }
            Self::Cycle { kinds } => {
                f.write_str("kinds form a cycle of bases:")?;
                for kind in kinds {
                    write!(f, " `{kind}` ->")?;
                }
                match kinds.first() {
                    Some(first) => write!(f, " `{first}`"),
                    None => Ok(()),
                }
            }
        }
    }
}

/// Whether `name` is an identifier, as OpenUSD's `TfIsValidIdentifier`
/// checks a kind name.
fn is_identifier(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
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

    /// Registers `kind`, a kind of `base` (or of none; an empty base is
    /// none, as OpenUSD reads `"baseKind": ""`). The base need not be known:
    /// OpenUSD registers such a kind too ([`KindIssue::UnknownBase`]).
    ///
    /// # Errors
    ///
    /// [`KindError`] when `kind` is not an ASCII identifier or is already
    /// registered, as OpenUSD refuses them; the registry is unchanged.
    ///
    /// OpenUSD: `KindRegistry::_Register`.
    pub fn register(&mut self, kind: &str, base: Option<&str>) -> Result<(), KindError> {
        if !is_identifier(kind) {
            return Err(KindError::InvalidName(kind.into()));
        }
        if self.has_kind(kind) {
            return Err(KindError::AlreadyRegistered(kind.into()));
        }
        let base = base.filter(|base| !base.is_empty()).map(Arc::from);
        self.registered.push((Arc::from(kind), base));
        Ok(())
    }

    /// Registers each declaration in order, as OpenUSD registers a
    /// plugin's `Kinds`: a declaration it rejects is skipped, and the rest
    /// still register. Returns the rejections, in order; check
    /// [`KindRegistry::issues`] for the hierarchy the accepted ones make.
    ///
    /// AOUSD Core §7.6.2.4.4, §11.4. OpenUSD: `KindRegistry::_RegisterDefaults`.
    #[must_use = "the declarations that were not registered"]
    pub fn register_all<'a>(
        &mut self,
        declarations: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    ) -> Vec<KindError> {
        declarations
            .into_iter()
            .filter_map(|(kind, base)| self.register(kind, base).err())
            .collect()
    }

    /// What the registered hierarchy holds that OpenUSD accepts but cannot
    /// fully answer for: bases that are no kind, and cycles of bases, in
    /// registration order. Empty for a well-formed hierarchy.
    #[must_use]
    pub fn issues(&self) -> Vec<KindIssue> {
        let mut issues = Vec::new();
        let mut in_cycle: Vec<&str> = Vec::new();
        for (kind, base) in &self.registered {
            if let Some(base) = base
                && !self.has_kind(base)
            {
                issues.push(KindIssue::UnknownBase {
                    kind: String::from(&**kind),
                    base: String::from(&**base),
                });
            }
            if in_cycle.contains(&&**kind) {
                continue;
            }
            // Walk the bases; a cycle through `kind` comes back to it.
            let mut path: Vec<&str> = alloc::vec![kind];
            while let Some(next) = self.base_kind(path[path.len() - 1]) {
                if next == &**kind {
                    in_cycle.extend(&path);
                    issues.push(KindIssue::Cycle {
                        kinds: path.iter().map(|kind| String::from(*kind)).collect(),
                    });
                    break;
                }
                if path.contains(&next) || !self.has_kind(next) {
                    break;
                }
                path.push(next);
            }
        }
        issues
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
    /// OpenUSD: `KindRegistry::GetBaseKind`, which reports a coding error
    /// for an unknown kind and returns the empty token.
    #[must_use]
    pub fn base_kind(&self, kind: &str) -> Option<&str> {
        self.entry(kind).flatten()
    }

    /// Whether `kind` is `base` or, through its base kinds, a kind of it.
    /// An unknown kind is only itself; a kind whose base is no kind is that
    /// base and nothing above it. On a cycle of bases, `base` is found if
    /// the walk meets it; otherwise this is `false`, where OpenUSD's walk
    /// does not return ([`KindIssue::Cycle`]).
    ///
    /// OpenUSD: `KindRegistry::IsA`.
    #[must_use]
    pub fn is_a(&self, kind: &str, base: &str) -> bool {
        let mut current = kind;
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

    /// Whether `kind` is a kind of `model`. OpenUSD: `KindRegistry::IsModel`.
    #[must_use]
    pub fn is_model(&self, kind: &str) -> bool {
        self.is_a(kind, MODEL)
    }

    /// Whether `kind` is a kind of `group`. OpenUSD: `KindRegistry::IsGroup`.
    #[must_use]
    pub fn is_group(&self, kind: &str) -> bool {
        self.is_a(kind, GROUP)
    }

    /// Whether `kind` is a kind of `assembly`. OpenUSD:
    /// `KindRegistry::IsAssembly`.
    #[must_use]
    pub fn is_assembly(&self, kind: &str) -> bool {
        self.is_a(kind, ASSEMBLY)
    }

    /// Whether `kind` is a kind of `component`. OpenUSD:
    /// `KindRegistry::IsComponent`.
    #[must_use]
    pub fn is_component(&self, kind: &str) -> bool {
        self.is_a(kind, COMPONENT)
    }

    /// Whether `kind` is a kind of `subcomponent`. OpenUSD:
    /// `KindRegistry::IsSubComponent`.
    #[must_use]
    pub fn is_subcomponent(&self, kind: &str) -> bool {
        self.is_a(kind, SUBCOMPONENT)
    }

    /// Every known kind, built-in ones first, then in registration order.
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
        assert!(kinds.is_model("assembly") && kinds.is_group("assembly"));
        assert!(kinds.is_assembly("assembly") && !kinds.is_component("assembly"));
        assert!(kinds.is_subcomponent("subcomponent") && !kinds.is_model("subcomponent"));
        assert!(kinds.issues().is_empty());
    }

    /// OpenUSD's `_Register`: invalid names and redefinitions, built-in
    /// ones included, are refused and change nothing.
    #[test]
    fn invalid_and_repeated_names_are_refused() {
        let mut kinds = KindRegistry::new();
        kinds.register("prop", Some("component")).unwrap();
        assert!(kinds.is_a("prop", "model"));
        let before = kinds.clone();
        assert_eq!(
            kinds.register("prop", Some("group")),
            Err(KindError::AlreadyRegistered("prop".into()))
        );
        assert_eq!(
            kinds.register("model", Some("group")),
            Err(KindError::AlreadyRegistered("model".into()))
        );
        for name in ["1x", "bad name", "", "a:b"] {
            assert_eq!(
                kinds.register(name, None),
                Err(KindError::InvalidName(name.into()))
            );
        }
        assert_eq!(kinds, before);
        assert!(!kinds.is_group("model"), "the built-in kind stands");
    }

    /// As OpenUSD reads a plugin's `Kinds`: each declaration registers or is
    /// skipped; unknown bases and forward bases are kept.
    #[test]
    fn register_all_skips_only_what_it_rejects() {
        let mut kinds = KindRegistry::new();
        let rejected = kinds.register_all([
            ("leaf", Some("branch")),
            ("bad name", None),
            ("branch", Some("component")),
            ("orphan", Some("nowhere")),
            ("blank", Some("")),
            ("leaf", Some("group")),
        ]);
        assert_eq!(
            rejected,
            [
                KindError::InvalidName("bad name".into()),
                KindError::AlreadyRegistered("leaf".into()),
            ]
        );
        assert!(kinds.is_component("leaf") && !kinds.is_group("leaf"));
        assert!(kinds.is_a("orphan", "nowhere") && !kinds.is_model("orphan"));
        assert_eq!(kinds.base_kind("blank"), None);
        assert_eq!(
            kinds.issues(),
            [KindIssue::UnknownBase {
                kind: "orphan".into(),
                base: "nowhere".into(),
            }]
        );
    }

    /// OpenUSD registers a cycle of bases and its `IsA` walks it forever
    /// unless it meets the kind asked for; here such a walk ends `false`.
    #[test]
    fn cycles_register_and_are_reported() {
        let mut kinds = KindRegistry::new();
        let rejected = kinds.register_all([
            ("ring_a", Some("ring_b")),
            ("ring_b", Some("ring_a")),
            ("loop", Some("loop")),
            ("tail", Some("ring_a")),
        ]);
        assert!(rejected.is_empty());
        assert!(kinds.is_a("ring_a", "ring_b") && kinds.is_a("tail", "ring_b"));
        assert!(!kinds.is_model("ring_a") && !kinds.is_model("tail"));
        assert!(kinds.is_a("loop", "loop") && !kinds.is_a("loop", "model"));
        assert_eq!(
            kinds.issues(),
            [
                KindIssue::Cycle {
                    kinds: vec!["ring_a".into(), "ring_b".into()],
                },
                KindIssue::Cycle {
                    kinds: vec!["loop".into()],
                },
            ]
        );
    }
}
