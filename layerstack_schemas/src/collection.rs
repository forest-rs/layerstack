// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Collection membership: which prims and properties a `CollectionAPI`
//! instance includes, as OpenUSD's `UsdCollectionAPI::ComputeMembershipQuery`
//! and `UsdCollectionMembershipQuery::IsPathIncluded` decide it.
//!
//! A collection's membership comes from its includes and excludes (its
//! rule map): each included path with the collection's expansion rule, each
//! excluded path, `includeRoot` for the pseudo-root, and the rule maps of
//! the collections it includes by their collection paths. When that map is
//! empty, OpenUSD matches its `membershipExpression` instead (unless its
//! `mode` is `relationship`); layerstack does not evaluate path expressions
//! yet, and says so ([`Membership::ExpressionUnsupported`]) instead of
//! guessing.

use alloc::{format, string::ToString, sync::Arc, vec::Vec};

use layerstack::{HashMap, PathId, PropertyPath, TargetPath, Value};

use crate::usd::{CollectionApi, CollectionApiExpansionRule, CollectionApiMode};
use crate::view::{PrimView, Scene};

/// How far an included path extends to its descendants.
///
/// OpenUSD: the `expansionRule` tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExpansionRule {
    /// The included path alone.
    ExplicitOnly,
    /// The included path and its descendant prims.
    ExpandPrims,
    /// The included path and its descendant prims and their properties.
    ExpandPrimsAndProperties,
}

impl ExpansionRule {
    /// Its token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitOnly => "explicitOnly",
            Self::ExpandPrims => "expandPrims",
            Self::ExpandPrimsAndProperties => "expandPrimsAndProperties",
        }
    }

    fn of(rule: &CollectionApiExpansionRule) -> Self {
        match rule {
            CollectionApiExpansionRule::ExplicitOnly => Self::ExplicitOnly,
            CollectionApiExpansionRule::ExpandPrimsAndProperties => Self::ExpandPrimsAndProperties,
            // `expandPrims` is the fallback, and OpenUSD's rule for an
            // empty token.
            _ => Self::ExpandPrims,
        }
    }
}

/// An entry of a collection's rule map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MembershipRule {
    /// The path is included, and its descendants as the rule says.
    Include(ExpansionRule),
    /// The path and its descendants are excluded (unless a descendant has
    /// its own entry).
    Exclude,
}

/// Whether a collection includes a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Membership {
    /// Included, by the rule of the entry that decided it.
    Included(ExpansionRule),
    /// Not included.
    Excluded,
    /// Decided by the collection's `membershipExpression`, which layerstack
    /// does not evaluate yet: the answer is unknown.
    ExpressionUnsupported,
}

impl Membership {
    /// Whether the path is included, if that is known.
    #[must_use]
    pub fn is_included(self) -> Option<bool> {
        match self {
            Self::Included(_) => Some(true),
            Self::Excluded => Some(false),
            Self::ExpressionUnsupported => None,
        }
    }
}

/// Why part of a collection's membership was left out, as OpenUSD warns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipProblem {
    /// The collection includes a collection that (transitively) includes
    /// it; the inner inclusion is skipped.
    CircularInclusion {
        /// The collection path included again.
        collection: PropertyPath,
    },
    /// The collection includes a collection on a prim the stage does not
    /// have; the inclusion is skipped.
    MissingPrim {
        /// The collection path.
        collection: PropertyPath,
    },
}

/// A collection's membership, computed once and queried for any path.
///
/// OpenUSD: `UsdCollectionMembershipQuery`.
#[derive(Clone, Debug, PartialEq)]
pub struct MembershipQuery {
    rules: HashMap<TargetPath, MembershipRule>,
    included_collections: Vec<PropertyPath>,
    expansion_rule: ExpansionRule,
    expression: Option<Arc<str>>,
    problems: Vec<MembershipProblem>,
}

/// The base names of `CollectionAPI`'s own properties, which name no
/// collection (`UsdCollectionAPI::IsSchemaPropertyBaseName`).
const SCHEMA_BASE_NAMES: [&str; 7] = [
    "includes",
    "excludes",
    "expansionRule",
    "includeRoot",
    "membershipExpression",
    "mode",
    "__INSTANCE_NAME__",
];

/// The collection name of the collection path `property` (`collection:a`
/// names `a`, `collection:a:b` names `a:b`), if it is one.
///
/// OpenUSD: `UsdCollectionAPI::IsCollectionAPIPath`.
pub(crate) fn collection_name(property: &str) -> Option<&str> {
    let name = property.strip_prefix("collection:")?;
    let base = property.rsplit(':').next().unwrap_or(property);
    (!name.is_empty() && !SCHEMA_BASE_NAMES.contains(&base)).then_some(name)
}

impl MembershipQuery {
    /// Computes the membership of the collection `name` on the prim at
    /// `path`.
    ///
    /// OpenUSD: `UsdCollectionAPI::ComputeMembershipQuery`.
    #[must_use]
    pub fn compute(scene: &Scene<'_>, path: PathId, name: &str) -> Self {
        let prim = PrimView::new(*scene, path);
        let property = |base: &str| format!("collection:{name}:{base}");
        let mode = prim
            .read_value(&property("mode"), CollectionApiMode::read)
            .unwrap_or(CollectionApiMode::Automatic);
        let expansion_rule = prim
            .read_value(&property("expansionRule"), CollectionApiExpansionRule::read)
            .map_or(ExpansionRule::ExpandPrims, |rule| ExpansionRule::of(&rule));
        let mut query = Self {
            rules: HashMap::new(),
            included_collections: Vec::new(),
            expansion_rule,
            expression: None,
            problems: Vec::new(),
        };
        if mode != CollectionApiMode::Expression {
            let mut chain = Vec::new();
            if let Some(own) = prim.property_path(&format!("collection:{name}")) {
                chain.push(own);
            }
            query.gather(scene, path, name, &chain);
        }
        if mode != CollectionApiMode::Relationship {
            query.expression = prim
                .read_value(&property("membershipExpression"), |v, _| match v {
                    Value::PathExpression(text) => Some(text.clone()),
                    _ => None,
                })
                .filter(|text| !text.trim().is_empty());
        }
        query
    }

    /// Adds the rule map of the collection `name` on `path`.
    ///
    /// OpenUSD: `UsdCollectionAPI::_ComputeMembershipQueryImpl`.
    fn gather(&mut self, scene: &Scene<'_>, path: PathId, name: &str, chain: &[PropertyPath]) {
        let prim = PrimView::new(*scene, path);
        let property = |base: &str| format!("collection:{name}:{base}");
        let rule = prim
            .read_value(&property("expansionRule"), CollectionApiExpansionRule::read)
            .map_or(ExpansionRule::ExpandPrims, |rule| ExpansionRule::of(&rule));
        let mut includes = prim.read_targets(&property("includes"));
        let excludes = prim.read_targets(&property("excludes"));
        if rule != ExpansionRule::ExplicitOnly
            && prim
                .read_value(&property("includeRoot"), crate::value::read_bool)
                .unwrap_or(false)
            && let Some(root) = scene.root()
        {
            includes.push(TargetPath::Prim(root));
        }
        let tokens = scene.store().tokens();
        for included in includes {
            let collection = match included {
                TargetPath::Property(collection) => {
                    collection_name(tokens.resolve(collection.property()))
                        .map(|name| (collection, name.to_string()))
                }
                TargetPath::Prim(_) => None,
            };
            let Some((collection, included_name)) = collection else {
                self.rules.insert(included, MembershipRule::Include(rule));
                continue;
            };
            if chain.contains(&collection) {
                self.problems
                    .push(MembershipProblem::CircularInclusion { collection });
                continue;
            }
            if !scene.stage().has_prim(collection.prim_path()) {
                self.problems
                    .push(MembershipProblem::MissingPrim { collection });
                continue;
            }
            let mut inner = Self {
                rules: HashMap::new(),
                included_collections: Vec::new(),
                expansion_rule: rule,
                expression: None,
                problems: Vec::new(),
            };
            let mut seen = chain.to_vec();
            seen.push(collection);
            inner.gather(scene, collection.prim_path(), &included_name, &seen);
            self.rules.extend(inner.rules);
            if !self.included_collections.contains(&collection) {
                self.included_collections.push(collection);
            }
            for nested in inner.included_collections {
                if !self.included_collections.contains(&nested) {
                    self.included_collections.push(nested);
                }
            }
            self.problems.extend(inner.problems);
        }
        for excluded in excludes {
            self.rules.insert(excluded, MembershipRule::Exclude);
        }
    }

    /// Whether the rule map decides membership: it is not empty. Otherwise
    /// the `membershipExpression` does, if there is one.
    #[must_use]
    pub fn uses_rule_map(&self) -> bool {
        !self.rules.is_empty()
    }

    /// The rule map: each included or excluded path.
    pub fn rules(&self) -> impl Iterator<Item = (&TargetPath, &MembershipRule)> {
        self.rules.iter()
    }

    /// The collections it includes, directly or through them, by collection
    /// path.
    #[must_use]
    pub fn included_collections(&self) -> &[PropertyPath] {
        &self.included_collections
    }

    /// The collection's own expansion rule.
    #[must_use]
    pub fn expansion_rule(&self) -> ExpansionRule {
        self.expansion_rule
    }

    /// The `membershipExpression` text, when it may decide membership (the
    /// collection's `mode` is not `relationship` and the text is not empty).
    #[must_use]
    pub fn expression(&self) -> Option<&str> {
        self.expression.as_deref()
    }

    /// What was left out of the membership, and why.
    #[must_use]
    pub fn problems(&self) -> &[MembershipProblem] {
        &self.problems
    }

    /// Whether the membership reads the collection `collection`: it
    /// includes it, directly or through the collections it includes, or
    /// tried to (a circular inclusion, or one on a missing prim). An edit
    /// to that collection can change this membership.
    #[must_use]
    pub fn depends_on(&self, collection: PropertyPath) -> bool {
        self.included_collections.contains(&collection)
            || self.problems.iter().any(|problem| match problem {
                MembershipProblem::CircularInclusion { collection: c }
                | MembershipProblem::MissingPrim { collection: c } => *c == collection,
            })
    }

    /// Whether the collection includes the prim or property `path`.
    ///
    /// With a rule map, the nearest entry on the path or an ancestor
    /// decides: an exclude excludes; an include includes a prim unless it
    /// is `explicitOnly` of an ancestor, and a property only when the entry
    /// is the property itself, `expandPrimsAndProperties`, or
    /// `explicitOnly` of the property. The pseudo-root is never a member:
    /// `includeRoot` includes its descendants. Without a rule map, an
    /// expression decides ([`Membership::ExpressionUnsupported`]), and no
    /// expression includes nothing.
    ///
    /// OpenUSD: `UsdCollectionMembershipQuery::IsPathIncluded`
    /// (`_IsPathIncludedByRuleMap`, which only includes prim and property
    /// paths).
    #[must_use]
    pub fn is_included(&self, scene: &Scene<'_>, path: TargetPath) -> Membership {
        if !self.uses_rule_map() {
            return if self.expression.is_some() {
                Membership::ExpressionUnsupported
            } else {
                Membership::Excluded
            };
        }
        if let TargetPath::Prim(prim) = path
            && scene.parent(prim).is_none()
        {
            return Membership::Excluded;
        }
        let property = matches!(path, TargetPath::Property(_));
        let mut at = Some(path);
        while let Some(current) = at {
            if let Some(entry) = self.rules.get(&current) {
                match *entry {
                    MembershipRule::Exclude => return Membership::Excluded,
                    MembershipRule::Include(rule) => {
                        let decides = if property {
                            matches!(current, TargetPath::Property(_))
                                || rule == ExpansionRule::ExpandPrimsAndProperties
                                || (rule == ExpansionRule::ExplicitOnly && current == path)
                        } else {
                            rule != ExpansionRule::ExplicitOnly || current == path
                        };
                        if decides {
                            return Membership::Included(rule);
                        }
                    }
                }
            }
            at = match current {
                TargetPath::Property(p) => Some(TargetPath::Prim(p.prim_path())),
                TargetPath::Prim(prim) => scene.parent(prim).map(TargetPath::Prim),
            };
        }
        Membership::Excluded
    }
}

impl<'a> CollectionApi<'a> {
    /// The collection's membership ([`MembershipQuery::compute`]).
    ///
    /// OpenUSD: `UsdCollectionAPI::ComputeMembershipQuery`.
    #[must_use]
    pub fn membership_query(&self) -> MembershipQuery {
        MembershipQuery::compute(&self.scene(), self.path(), self.instance())
    }
}
