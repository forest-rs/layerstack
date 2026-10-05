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
//! empty, its `membershipExpression` decides instead (unless its `mode` is
//! `relationship`): a path expression ([`PathExpression`]) whose
//! references to other collections' expressions (`%/Prim:name`) resolve
//! recursively, evaluated with OpenUSD's collection predicates
//! ([`CollectionPredicates`]) by an [`ExpressionEvaluator`].
//!
//! Spec: AOUSD Core §15 (collections). The `membershipExpression`, its
//! `mode` and path expressions are OpenUSD's, as of 26.08.

mod cache;
pub use cache::*;

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use layerstack::path_expression::{
    ExpressionReference, MatchResult, MatcherError, ParseError, PathExpression, PathMatcher,
    ReferenceProblem, ReferenceResolver, Searcher,
};
use layerstack::{HashMap, HashSet, Path, PathId, PropertyPath, TargetPath, Value};

use crate::predicate::{CollectionPredicate, CollectionPredicates, is_abstract, is_defined};
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
///
/// OpenUSD: `IsPathIncluded`, with the expansion rule it reports
/// (`includedByMembershipExpression` for an expression).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Membership {
    /// Included by its rule map, by the rule of the entry that decided it.
    Included(ExpansionRule),
    /// Included by its `membershipExpression`.
    IncludedByExpression,
    /// Not included.
    Excluded,
}

impl Membership {
    /// Whether the path is included.
    #[must_use]
    pub fn is_included(self) -> bool {
        !matches!(self, Self::Excluded)
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
    /// A `membershipExpression` does not parse; it matches nothing.
    InvalidExpression {
        /// The collection whose expression it is.
        collection: PropertyPath,
        /// The composed expression text.
        text: Arc<str>,
        /// Why it does not parse.
        error: ParseError,
    },
    /// An expression references a collection that does not exist (no prim,
    /// or no such `CollectionAPI` instance on it); the reference matches
    /// nothing.
    MissingReference {
        /// The collection whose expression has the reference.
        from: PropertyPath,
        /// The reference, as written.
        reference: String,
    },
    /// An expression references a collection whose expression is being
    /// resolved, which would recurse forever; this occurrence matches
    /// nothing.
    ReferenceCycle {
        /// The collection whose expression has the reference.
        from: PropertyPath,
        /// The reference, as written.
        reference: String,
        /// The collection it names.
        collection: PropertyPath,
    },
    /// The resolved expression cannot be evaluated (a predicate that does
    /// not bind, a malformed glob): as in OpenUSD, it matches nothing.
    UnevaluableExpression {
        /// Why.
        error: MatcherError,
    },
}

/// A complete path expression, linked with OpenUSD's collection predicates,
/// matching the objects of a scene.
///
/// OpenUSD: `UsdObjectCollectionExpressionEvaluator`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExpressionEvaluator {
    expression: PathExpression,
    matcher: PathMatcher<CollectionPredicate>,
}

impl ExpressionEvaluator {
    /// Links `expression`, a complete expression
    /// ([`PathExpression::is_complete`]), with the predicates of `scene`.
    ///
    /// # Errors
    ///
    /// [`MatcherError`] when it is not complete, or a predicate does not
    /// bind; OpenUSD's evaluator then matches nothing.
    pub fn new(scene: &Scene<'_>, expression: &PathExpression) -> Result<Self, MatcherError> {
        let matcher = expression.matcher(&CollectionPredicates::new(*scene))?;
        Ok(Self {
            expression: expression.clone(),
            matcher,
        })
    }

    /// The expression.
    #[must_use]
    pub fn expression(&self) -> &PathExpression {
        &self.expression
    }

    /// Whether it matches nothing, having no patterns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.matcher.is_empty()
    }

    /// Matches the prim or property at `path` of `scene`: false, constantly,
    /// when there is no such object.
    ///
    /// OpenUSD: `UsdObjectCollectionExpressionEvaluator::Match`.
    #[must_use]
    pub fn matches(&self, scene: &Scene<'_>, path: TargetPath) -> MatchResult {
        if !object_exists(scene, path) {
            return MatchResult::constant(false);
        }
        self.matcher
            .matches(&CollectionPredicates::new(*scene), scene.store(), path)
    }

    /// A depth-first incremental search over the objects of `scene`: see
    /// [`ExpressionSearch::next`].
    ///
    /// OpenUSD: `UsdObjectCollectionExpressionEvaluator::MakeIncrementalSearcher`.
    #[must_use]
    pub fn search<'e, 's>(&'e self, scene: &Scene<'s>) -> ExpressionSearch<'e, 's> {
        ExpressionSearch {
            predicates: CollectionPredicates::new(*scene),
            scene: *scene,
            searcher: self.matcher.searcher(),
        }
    }
}

/// A depth-first search with an [`ExpressionEvaluator`], reusing what it
/// learned about ancestors.
///
/// OpenUSD: `UsdObjectCollectionExpressionEvaluator::IncrementalSearcher`.
#[derive(Debug)]
pub struct ExpressionSearch<'e, 's> {
    predicates: CollectionPredicates<'s>,
    scene: Scene<'s>,
    searcher: Searcher<'e, CollectionPredicate>,
}

impl ExpressionSearch<'_, '_> {
    /// Matches `path`, the next object of the search: it must follow the
    /// previous path in some depth-first order (a child, a sibling, or a
    /// sibling of an ancestor; a prim's properties count as its children).
    /// A result constant over descendants holds for every object below
    /// `path`, which the search can skip.
    pub fn next(&mut self, path: TargetPath) -> MatchResult {
        self.searcher
            .next(&self.predicates, self.scene.store(), path)
    }
}

/// Whether the prim or property at `path` exists: a prim on the stage, or a
/// property an opinion declares or its prim's schemas define.
fn object_exists(scene: &Scene<'_>, path: TargetPath) -> bool {
    let stage = scene.stage();
    match path {
        TargetPath::Prim(prim) => stage.has_prim(prim),
        TargetPath::Property(property) => {
            let prim = property.prim_path();
            stage.has_prim(prim)
                && (stage
                    .resolve_property_declaration(prim, property.property())
                    .is_some()
                    || stage
                        .property_definition(prim, property.property(), scene.store())
                        .is_some())
        }
    }
}

/// A collection's membership, computed once and queried for any path.
///
/// OpenUSD: `UsdCollectionMembershipQuery`.
#[derive(Clone, Debug, PartialEq)]
#[doc(alias = "UsdCollectionMembershipQuery")]
pub struct MembershipQuery {
    rules: HashMap<TargetPath, MembershipRule>,
    included_collections: Vec<PropertyPath>,
    expansion_rule: ExpansionRule,
    expression: Option<PathExpression>,
    evaluator: Option<ExpressionEvaluator>,
    referenced_collections: Vec<PropertyPath>,
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
    #[doc(alias = "UsdCollectionAPI::ComputeMembershipQuery")]
    #[doc(alias = "ComputeMembershipQuery")]
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
            evaluator: None,
            referenced_collections: Vec::new(),
            problems: Vec::new(),
        };
        let own = prim.property_path(&format!("collection:{name}"));
        if mode != CollectionApiMode::Expression {
            let chain: Vec<PropertyPath> = own.into_iter().collect();
            query.gather(scene, path, name, &chain);
        }
        if mode != CollectionApiMode::Relationship
            && let Some(own) = own
        {
            query.resolve_expression(scene, own);
        }
        query
    }

    /// Resolves the collection's complete `membershipExpression` and links
    /// it.
    ///
    /// OpenUSD: `UsdCollectionAPI::ResolveCompleteMembershipExpression`,
    /// then `UsdObjectCollectionExpressionEvaluator`.
    fn resolve_expression(&mut self, scene: &Scene<'_>, own: PropertyPath) {
        let mut resolver = Resolver {
            scene,
            problems: Vec::new(),
        };
        let expression = resolver.expression(own);
        let resolved = expression.resolve_complete(own, &mut resolver);
        self.problems.extend(resolver.problems);
        for problem in resolved.problems {
            self.problems.push(match problem {
                ReferenceProblem::Missing { from, reference } => {
                    MembershipProblem::MissingReference {
                        from,
                        reference: reference.to_string(),
                    }
                }
                ReferenceProblem::Cycle {
                    from,
                    reference,
                    target,
                } => MembershipProblem::ReferenceCycle {
                    from,
                    reference: reference.to_string(),
                    collection: target,
                },
            });
        }
        self.referenced_collections = resolved.referenced;
        match ExpressionEvaluator::new(scene, &resolved.expression) {
            Ok(evaluator) => self.evaluator = Some(evaluator),
            Err(error) => self
                .problems
                .push(MembershipProblem::UnevaluableExpression { error }),
        }
        self.expression = Some(resolved.expression);
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
                evaluator: None,
                referenced_collections: Vec::new(),
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

    /// The complete `membershipExpression`, its references resolved, when
    /// it may decide membership (the collection's `mode` is not
    /// `relationship`); the empty expression when none is authored.
    ///
    /// OpenUSD: `UsdCollectionAPI::ResolveCompleteMembershipExpression`.
    #[must_use]
    pub fn expression(&self) -> Option<&PathExpression> {
        self.expression.as_ref()
    }

    /// The expression's evaluator, when there is an expression and it
    /// links.
    #[must_use]
    pub fn evaluator(&self) -> Option<&ExpressionEvaluator> {
        self.evaluator.as_ref()
    }

    /// The collections the expression references, directly or through
    /// them, in the order it read them.
    #[must_use]
    pub fn referenced_collections(&self) -> &[PropertyPath] {
        &self.referenced_collections
    }

    /// What was left out of the membership, and why.
    #[must_use]
    pub fn problems(&self) -> &[MembershipProblem] {
        &self.problems
    }

    /// Whether the membership reads the collection `collection`: it
    /// includes it, directly or through the collections it includes, its
    /// expression references it, directly or not, or either tried to (a
    /// circular inclusion or reference, or a missing one). An edit to that
    /// collection can change this membership. A missing reference could
    /// name any collection made later, so it depends on every collection.
    #[must_use]
    pub fn depends_on(&self, collection: PropertyPath) -> bool {
        self.included_collections.contains(&collection)
            || self.referenced_collections.contains(&collection)
            || self.problems.iter().any(|problem| match problem {
                MembershipProblem::CircularInclusion { collection: c }
                | MembershipProblem::MissingPrim { collection: c }
                | MembershipProblem::ReferenceCycle { collection: c, .. } => *c == collection,
                MembershipProblem::MissingReference { .. } => true,
                MembershipProblem::InvalidExpression { .. }
                | MembershipProblem::UnevaluableExpression { .. } => false,
            })
    }

    /// Whether the collection includes the prim or property `path`.
    ///
    /// With a rule map, the nearest entry on the path or an ancestor
    /// decides: an exclude excludes; an include includes a prim unless it
    /// is `explicitOnly` of an ancestor, and a property only when the entry
    /// is the property itself, `expandPrimsAndProperties`, or
    /// `explicitOnly` of the property. The pseudo-root is never a member
    /// of a rule map: `includeRoot` includes its descendants.
    ///
    /// Without a rule map, the expression decides
    /// ([`Membership::IncludedByExpression`]) whatever the expansion rule,
    /// for any object of the scene (the pseudo-root included, which `//`
    /// matches); without an expression, or one that does not evaluate,
    /// nothing is included.
    ///
    /// OpenUSD: `UsdCollectionMembershipQuery::IsPathIncluded`
    /// (`_IsPathIncludedByRuleMap`, which only includes prim and property
    /// paths, else `UsdObjectCollectionExpressionEvaluator::Match`).
    #[must_use]
    #[doc(alias = "UsdCollectionMembershipQuery::IsPathIncluded")]
    #[doc(alias = "IsPathIncluded")]
    pub fn is_included(&self, scene: &Scene<'_>, path: TargetPath) -> Membership {
        if !self.uses_rule_map() {
            let included = self
                .evaluator
                .as_ref()
                .is_some_and(|evaluator| evaluator.matches(scene, path).value);
            return if included {
                Membership::IncludedByExpression
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

/// Finds the collections expression references name.
struct Resolver<'s, 'a> {
    scene: &'s Scene<'a>,
    problems: Vec<MembershipProblem>,
}

impl Resolver<'_, '_> {
    /// The composed `membershipExpression` of `collection`, the empty
    /// expression when none is authored or it does not parse (reported).
    fn expression(&mut self, collection: PropertyPath) -> PathExpression {
        let tokens = self.scene.store().tokens();
        let Some(name) = collection_name(tokens.resolve(collection.property())) else {
            return PathExpression::nothing();
        };
        let prim = PrimView::new(*self.scene, collection.prim_path());
        let text = prim.read_value(
            &format!("collection:{name}:membershipExpression"),
            |v, _| match v {
                Value::PathExpression(text) => Some(text.clone()),
                _ => None,
            },
        );
        let Some(text) = text else {
            return PathExpression::nothing();
        };
        PathExpression::parse(&text).unwrap_or_else(|error| {
            self.problems.push(MembershipProblem::InvalidExpression {
                collection,
                text,
                error,
            });
            PathExpression::nothing()
        })
    }
}

impl ReferenceResolver for Resolver<'_, '_> {
    type Key = PropertyPath;

    /// `%:name` names the collection `name` on the prim of `from`,
    /// `%/Path:name` the one on `/Path`: a collection only when that prim
    /// has that `CollectionAPI` instance.
    fn resolve(
        &mut self,
        from: &PropertyPath,
        reference: &ExpressionReference,
    ) -> Option<(PropertyPath, PathExpression)> {
        let store = self.scene.store();
        let prim = if reference.has_path() {
            let names = reference.absolute_path()?;
            let tokens = store.tokens();
            let segments = names
                .iter()
                .map(|name| tokens.lookup(name))
                .collect::<Option<Vec<_>>>()?;
            store.paths().lookup(&Path::root().join(&segments))?
        } else {
            from.prim_path()
        };
        let name = reference.name();
        if !self.scene.stage().has_prim(prim)
            || !self.scene.has_api(prim, CollectionApi::SCHEMA, Some(name))
        {
            return None;
        }
        let collection =
            PrimView::new(*self.scene, prim).property_path(&format!("collection:{name}"))?;
        Some((collection, self.expression(collection)))
    }
}

impl MembershipQuery {
    /// Every prim and property the collection includes, among the prims a
    /// default traversal visits (defined and not abstract, below such
    /// prims), each once.
    ///
    /// With a rule map, each include expands by its rule, skipping what is
    /// excluded; an `explicitOnly` include adds the path alone. With an
    /// expression, a depth-first search matches every prim, and every
    /// property when the expansion rule is `expandPrimsAndProperties`
    /// (only for prims that did not match with a result that may vary
    /// below them, as in OpenUSD), and a subtree whose result is constant
    /// is added or skipped whole without matching it.
    ///
    /// OpenUSD: `UsdComputeIncludedPathsFromCollection` with
    /// `UsdPrimDefaultPredicate`.
    #[must_use]
    pub fn included_paths(&self, scene: &Scene<'_>) -> Vec<TargetPath> {
        let mut out = Included::default();
        if self.uses_rule_map() {
            self.included_by_rule_map(scene, &mut out);
        } else if let Some(evaluator) = self.evaluator.as_ref().filter(|e| !e.is_empty()) {
            let properties = self.expansion_rule == ExpansionRule::ExpandPrimsAndProperties;
            let mut search = evaluator.search(scene);
            if let Some(root) = scene.root() {
                for child in children(scene, root) {
                    search_from(scene, &mut search, child, properties, &mut out);
                }
            }
        }
        out.paths
    }

    fn included_by_rule_map(&self, scene: &Scene<'_>, out: &mut Included) {
        let stage = scene.stage();
        let has_excludes = self
            .rules
            .values()
            .any(|rule| *rule == MembershipRule::Exclude);
        for (path, rule) in &self.rules {
            let MembershipRule::Include(rule) = *rule else {
                continue;
            };
            let prim = match *path {
                TargetPath::Property(_) => {
                    if object_exists(scene, *path) {
                        out.add(*path);
                    }
                    continue;
                }
                TargetPath::Prim(prim) => prim,
            };
            if !stage.has_prim(prim) {
                continue;
            }
            let root = scene.parent(prim).is_none();
            if rule == ExpansionRule::ExplicitOnly {
                if !root && traversable(scene, prim) {
                    out.add(*path);
                }
                continue;
            }
            let mut stack: Vec<PathId> = if root {
                children(scene, prim)
                    .rev()
                    .filter(|child| traversable(scene, *child))
                    .collect()
            } else if traversable(scene, prim) {
                Vec::from([prim])
            } else {
                Vec::new()
            };
            while let Some(current) = stack.pop() {
                if has_excludes
                    && !self
                        .is_included(scene, TargetPath::Prim(current))
                        .is_included()
                {
                    continue;
                }
                out.add(TargetPath::Prim(current));
                if rule == ExpansionRule::ExpandPrimsAndProperties {
                    for name in stage.property_names(current, scene.store()) {
                        let property = TargetPath::Property(PropertyPath::new(current, name));
                        if self.rules.get(&property) != Some(&MembershipRule::Exclude) {
                            out.add(property);
                        }
                    }
                }
                stack.extend(
                    children(scene, current)
                        .rev()
                        .filter(|child| traversable(scene, *child)),
                );
            }
        }
    }
}

/// Paths in the order found, each once.
#[derive(Default)]
struct Included {
    paths: Vec<TargetPath>,
    seen: HashSet<TargetPath>,
}

impl Included {
    fn add(&mut self, path: TargetPath) {
        if self.seen.insert(path) {
            self.paths.push(path);
        }
    }
}

/// The children of `prim` on the stage.
fn children<'s>(scene: &Scene<'s>, prim: PathId) -> impl DoubleEndedIterator<Item = PathId> + 's {
    scene
        .stage()
        .children_of(prim)
        .unwrap_or_default()
        .iter()
        .copied()
}

/// Whether a default traversal visits the prim: defined and not abstract.
///
/// OpenUSD: `UsdPrimDefaultPredicate` (active and loaded as layerstack's
/// stages always are).
fn traversable(scene: &Scene<'_>, prim: PathId) -> bool {
    is_defined(scene, prim) && !is_abstract(scene, prim)
}

/// The depth-first expression search of `UsdComputeIncludedPathsFromCollection`
/// from `prim`.
fn search_from(
    scene: &Scene<'_>,
    search: &mut ExpressionSearch<'_, '_>,
    prim: PathId,
    properties: bool,
    out: &mut Included,
) {
    if !traversable(scene, prim) {
        return;
    }
    let result = search.next(TargetPath::Prim(prim));
    let mut did_properties = false;
    if result.value {
        if result.is_constant() {
            // Everything below matches: add the subtree without matching.
            let mut stack = Vec::from([prim]);
            while let Some(current) = stack.pop() {
                out.add(TargetPath::Prim(current));
                if properties {
                    did_properties = true;
                    for name in scene.stage().property_names(current, scene.store()) {
                        out.add(TargetPath::Property(PropertyPath::new(current, name)));
                    }
                }
                stack.extend(
                    children(scene, current)
                        .rev()
                        .filter(|child| traversable(scene, *child)),
                );
            }
        } else {
            out.add(TargetPath::Prim(prim));
        }
    }
    if properties && !did_properties && !(result.value && !result.is_constant()) {
        for name in scene.stage().property_names(prim, scene.store()) {
            let property = TargetPath::Property(PropertyPath::new(prim, name));
            if search.next(property).value {
                out.add(property);
            }
        }
    }
    if result.is_constant() {
        return;
    }
    for child in children(scene, prim) {
        search_from(scene, search, child, properties, out);
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
