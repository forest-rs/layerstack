// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Path expressions: sets of prim and property paths, as OpenUSD's
//! `SdfPathExpression` describes them (a collection's
//! `membershipExpression`, for one).
//!
//! A [`PathExpression`] combines [`PathPattern`]s and
//! [`ExpressionReference`]s with set operators. From tightest to loosest:
//! complement (`~`), implied union (juxtaposition, `a b`), union (`+`),
//! intersection (`&`) and difference (`-`); binary operators associate to
//! the left, and parentheses group.
//!
//! A pattern is a literal path prefix followed by components: prim name
//! globs (`*`, `?`, `[a-z]`, `[!a-z]`), `//` for any number of prims
//! between, a trailing property glob (`.name*`), and predicates in braces
//! (`{isa:Mesh}`, see [`PredicateExpression`]), which a caller-supplied
//! [`Predicates`] library evaluates. `/World//*.points` names every
//! `points` property below `/World`; `//{kind:component}` every prim whose
//! predicate holds.
//!
//! A reference names another expression: `%_` the next weaker opinion's
//! ([`PathExpression::compose_over`]), `%/Path:name` and `%:name` a named
//! one, resolved by a caller-supplied [`ReferenceResolver`]
//! ([`PathExpression::resolve_complete`]).
//!
//! To match paths, make a [`PathMatcher`] from a complete expression (all
//! prefixes absolute, no references) and a predicate library
//! ([`PathExpression::matcher`]). A match is a [`MatchResult`]: whether the
//! path matches, and whether every descendant of the path matches the same
//! way ([`Constancy`]), which lets a traversal skip subtrees. A
//! [`Searcher`] matches paths in depth-first order incrementally.
//!
//! ```
//! use layerstack::path_expression::{NoPredicates, PathExpression};
//! use layerstack::{InMemoryStore, TargetPath};
//!
//! let mut store = InMemoryStore::default();
//! let pebble = store.path("/World/Rocks/Pebble");
//! let points = store.tokens.intern("points");
//!
//! let expression = PathExpression::parse("/World//Peb* - /World/Rocks/Pebble.points").unwrap();
//! assert_eq!(expression.text(), "/World//Peb* - /World/Rocks/Pebble.points");
//! let matcher = expression.matcher(&NoPredicates).unwrap();
//! assert!(matcher.matches(&NoPredicates, &store, TargetPath::Prim(pebble)).value);
//! let property = TargetPath::Property(layerstack::PropertyPath::new(pebble, points));
//! assert!(!matcher.matches(&NoPredicates, &store, property).value);
//! ```
//!
//! Text is written as `SdfPathExpression::GetText` writes it: operators
//! spaced, parentheses only where precedence needs them, predicate
//! arguments normalized.
//!
//! OpenUSD: `pxr/usd/sdf/pathExpression.h`, `pathPattern.h`,
//! `predicateExpression.h` and `pathExpressionEval.h`. AOUSD Core defines
//! the path expression value type (§16.3.10.14) but not its grammar or
//! evaluation; this module follows OpenUSD 26.08.

mod eval;
mod glob;
mod parse;
mod predicate;
pub(crate) mod value;

pub(crate) use value::{
    Fold, Stop, anchor_fresh_values, anchor_opinions, fold_at_time, fold_default,
};

use alloc::{boxed::Box, string::String, vec::Vec};
use core::fmt;
use core::hash::Hash;

use hashbrown::HashSet;

pub use eval::{
    Constancy, MatchResult, MatcherError, NoPredicates, PathMatcher, Predicates, Searcher,
};
pub use parse::ParseError;
pub use predicate::{
    ArgValue, CallKind, PredicateArg, PredicateBindError, PredicateCall, PredicateExpression,
    PredicateOp,
};

/// A binary set operator, tightest binding first.
///
/// OpenUSD: `SdfPathExpression::Op`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SetOp {
    /// Juxtaposition, `a b`: union binding tighter than `+`.
    ImpliedUnion,
    /// `a + b`.
    Union,
    /// `a & b`.
    Intersection,
    /// `a - b`.
    Difference,
}

impl SetOp {
    fn text(self) -> &'static str {
        match self {
            Self::ImpliedUnion => " ",
            Self::Union => " + ",
            Self::Intersection => " & ",
            Self::Difference => " - ",
        }
    }
}

/// A node of a non-empty [`PathExpression`].
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// A path pattern.
    Pattern(PathPattern),
    /// A reference to another expression.
    Reference(ExpressionReference),
    /// Every path the operand does not match.
    Complement(Box<Self>),
    /// A set operation on two operands.
    Op(SetOp, Box<Self>, Box<Self>),
}

/// A path expression: a set of prim and property paths.
///
/// The empty expression matches nothing ([`PathExpression::nothing`]);
/// `//` matches everything ([`PathExpression::everything`]). Combining
/// expressions simplifies them as OpenUSD does: `~~a` is `a`, `a + //` is
/// `//`, `a & //` is `a`, and an empty operand drops out.
///
/// OpenUSD: `SdfPathExpression`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathExpression {
    root: Option<Expr>,
}

/// A path pattern: a literal path prefix, then pattern components.
///
/// OpenUSD: `SdfPathPattern`.
#[derive(Clone, Debug, PartialEq)]
pub struct PathPattern {
    prefix: Prefix,
    components: Vec<PatternComponent>,
    is_property: bool,
}

/// A pattern's literal prefix: a prim path, absolute or relative (`.`,
/// `..`, `Name/Other`), optionally ending in a property.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Prefix {
    pub(crate) absolute: bool,
    /// Prim names; a relative prefix may start with `..` entries.
    pub(crate) prims: Vec<String>,
    pub(crate) property: Option<String>,
}

/// A component of a [`PathPattern`] after its literal prefix.
///
/// OpenUSD: `SdfPathPattern::Component`.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternComponent {
    /// The name or glob it matches; empty for a stretch (`//`) or a bare
    /// predicate (`{pred}`, matching any name).
    pub text: String,
    /// The predicate the matched object must satisfy.
    pub predicate: Option<PredicateExpression>,
    /// Whether `text` is a literal name rather than a glob.
    pub literal: bool,
}

impl PatternComponent {
    /// Whether it is a stretch, `//`: any number of prims.
    #[must_use]
    pub fn is_stretch(&self) -> bool {
        self.text.is_empty() && self.predicate.is_none()
    }

    fn stretch() -> Self {
        Self {
            text: String::new(),
            predicate: None,
            literal: false,
        }
    }
}

/// A reference to another path expression: `%_`, `%:name`, `%/Path:name`,
/// or a relative `%../Path:name`.
///
/// OpenUSD: `SdfPathExpression::ExpressionReference`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExpressionReference {
    path: Option<RefPath>,
    name: String,
}

/// The prim path of an [`ExpressionReference`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RefPath {
    pub(crate) absolute: bool,
    /// Prim names; a relative path starts with one or more `..`.
    pub(crate) prims: Vec<String>,
}

impl ExpressionReference {
    /// `%_`: the next weaker expression.
    #[must_use]
    pub fn weaker() -> Self {
        Self {
            path: None,
            name: "_".into(),
        }
    }

    /// Whether it is `%_`.
    #[must_use]
    pub fn is_weaker(&self) -> bool {
        self.path.is_none() && self.name == "_"
    }

    /// The name of the expression it references (`_` for `%_`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The prim names of an absolute reference path, root first; `None`
    /// when no path is authored (`%:name`, `%_`) or the path is relative.
    #[must_use]
    pub fn absolute_path(&self) -> Option<&[String]> {
        self.path
            .as_ref()
            .filter(|path| path.absolute)
            .map(|path| path.prims.as_slice())
    }

    /// Whether a path is authored (`%/Path:name`, `%../Path:name`).
    #[must_use]
    pub fn has_path(&self) -> bool {
        self.path.is_some()
    }

    fn write(&self, out: &mut String) {
        out.push('%');
        if let Some(path) = &self.path {
            if path.absolute {
                write_absolute(out, &path.prims);
            } else {
                out.push_str(&path.prims.join("/"));
            }
        }
        if !self.is_weaker() {
            out.push(':');
        }
        out.push_str(&self.name);
    }
}

impl fmt::Display for ExpressionReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

fn write_absolute(out: &mut String, prims: &[String]) {
    if prims.is_empty() {
        out.push('/');
    }
    for name in prims {
        out.push('/');
        out.push_str(name);
    }
}

impl Prefix {
    fn is_reflexive(&self) -> bool {
        !self.absolute && self.prims.is_empty() && self.property.is_none()
    }

    fn is_absolute_root(&self) -> bool {
        self.absolute && self.prims.is_empty() && self.property.is_none()
    }

    /// The prefix as `SdfPath::GetAsString` writes it.
    fn write(&self, out: &mut String) {
        if self.absolute {
            write_absolute(out, &self.prims);
        } else if self.prims.is_empty() {
            out.push('.');
        } else {
            out.push_str(&self.prims.join("/"));
        }
        if let Some(property) = &self.property {
            out.push('.');
            out.push_str(property);
        }
    }
}

impl PathPattern {
    /// `//`: every path.
    ///
    /// OpenUSD: `SdfPathPattern::Everything`.
    #[must_use]
    pub fn everything() -> Self {
        Self {
            prefix: Prefix {
                absolute: true,
                ..Prefix::default()
            },
            components: Vec::from([PatternComponent::stretch()]),
            is_property: false,
        }
    }

    /// Whether its prefix is absolute.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        self.prefix.absolute
    }

    /// The prim names of its literal prefix, root first; a relative prefix
    /// may start with `..` entries.
    #[must_use]
    pub fn prefix_prims(&self) -> &[String] {
        &self.prefix.prims
    }

    /// The property its literal prefix ends in, if any.
    #[must_use]
    pub fn prefix_property(&self) -> Option<&str> {
        self.prefix.property.as_deref()
    }

    /// Its components after the literal prefix.
    #[must_use]
    pub fn components(&self) -> &[PatternComponent] {
        &self.components
    }

    /// Whether it matches only properties.
    #[must_use]
    pub fn is_property(&self) -> bool {
        self.is_property
    }

    /// Whether it starts with `//` at the absolute root, so it matches
    /// anywhere.
    ///
    /// OpenUSD: `SdfPathPattern::HasLeadingStretch`.
    #[must_use]
    pub fn has_leading_stretch(&self) -> bool {
        self.prefix.is_absolute_root() && self.components.first().is_some_and(|c| c.is_stretch())
    }

    fn has_trailing_stretch(&self) -> bool {
        !self.is_property
            && self
                .components
                .last()
                .is_some_and(PatternComponent::is_stretch)
    }

    /// Appends a prim component, into the prefix while it is still literal.
    ///
    /// OpenUSD: `SdfPathPattern::AppendChild`.
    fn append_child(&mut self, text: String, predicate: Option<PredicateExpression>) {
        if self.is_property
            || (text.is_empty() && predicate.is_none() && self.has_trailing_stretch())
        {
            return;
        }
        let literal = crate::ident::is_identifier(&text);
        if (literal || text == "..") && predicate.is_none() && self.components.is_empty() {
            self.prefix.prims.push(text);
        } else {
            self.components.push(PatternComponent {
                text,
                predicate,
                literal,
            });
        }
    }

    /// Appends the property component, into the prefix while it is still
    /// literal.
    ///
    /// OpenUSD: `SdfPathPattern::AppendProperty`.
    fn append_property(&mut self, text: String, predicate: Option<PredicateExpression>) {
        if self.is_property || (text.is_empty() && predicate.is_none()) {
            return;
        }
        let literal = !text.is_empty() && text.split(':').all(crate::ident::is_identifier);
        if literal && predicate.is_none() && self.components.is_empty() {
            self.prefix.property = Some(text);
        } else {
            if self.has_trailing_stretch() {
                self.append_child("*".into(), None);
            }
            self.components.push(PatternComponent {
                text,
                predicate,
                literal,
            });
        }
        self.is_property = true;
    }

    /// The pattern's text, as `SdfPathPattern::GetText` writes it.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, false);
        out
    }

    fn write(&self, out: &mut String, lossless: bool) {
        if self.prefix.is_reflexive() {
            if self
                .components
                .first()
                .is_none_or(PatternComponent::is_stretch)
            {
                out.push('.');
            }
        } else {
            self.prefix.write(out);
        }
        let absolute_root = self.prefix.is_absolute_root();
        let count = self.components.len();
        for (i, component) in self.components.iter().enumerate() {
            if component.is_stretch() {
                out.push_str(if i == 0 && absolute_root { "/" } else { "//" });
                continue;
            }
            if i + 1 == count && self.is_property {
                out.push('.');
            } else if !out.is_empty() && !out.ends_with('/') {
                out.push('/');
            }
            out.push_str(&component.text);
            if let Some(predicate) = &component.predicate {
                out.push('{');
                predicate.write(out, lossless);
                out.push('}');
            }
        }
    }
}

impl Expr {
    fn is_everything(&self) -> bool {
        matches!(self, Self::Pattern(pattern) if *pattern == PathPattern::everything())
    }

    /// The operator precedence of the outermost node; atoms and complements
    /// bind tightest.
    fn precedence(&self) -> Option<SetOp> {
        match self {
            Self::Op(op, ..) => Some(*op),
            _ => None,
        }
    }

    fn write(&self, out: &mut String, lossless: bool) {
        match self {
            Self::Pattern(pattern) => pattern.write(out, lossless),
            Self::Reference(reference) => reference.write(out),
            Self::Complement(operand) => {
                out.push('~');
                operand.write_grouped(out, operand.precedence().is_some(), lossless);
            }
            Self::Op(op, left, right) => {
                // Left-associative: a right operand of equal precedence
                // needs parentheses.
                left.write_grouped(out, left.precedence().is_some_and(|p| p > *op), lossless);
                out.push_str(op.text());
                right.write_grouped(out, right.precedence().is_some_and(|p| p >= *op), lossless);
            }
        }
    }

    fn write_grouped(&self, out: &mut String, grouped: bool, lossless: bool) {
        if grouped {
            out.push('(');
        }
        self.write(out, lossless);
        if grouped {
            out.push(')');
        }
    }

    /// Visits patterns and references left to right.
    fn for_each_atom<'a>(&'a self, visit: &mut impl FnMut(&'a Self)) {
        match self {
            Self::Pattern(_) | Self::Reference(_) => visit(self),
            Self::Complement(operand) => operand.for_each_atom(visit),
            Self::Op(_, left, right) => {
                left.for_each_atom(visit);
                right.for_each_atom(visit);
            }
        }
    }
}

impl PathExpression {
    /// Parses expression text; empty or blank text is the empty expression.
    ///
    /// OpenUSD: `SdfPathExpression(std::string)`, whose grammar this follows
    /// exactly. As there, names in patterns and references are ASCII.
    ///
    /// # Errors
    ///
    /// [`ParseError`] with the byte offset where parsing failed.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        parse::parse_expression(text)
    }

    /// The empty expression, which matches nothing.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// `//`, which matches every path.
    #[must_use]
    pub fn everything() -> Self {
        Self::pattern(PathPattern::everything())
    }

    /// `%_`, the next weaker expression.
    #[must_use]
    pub fn weaker() -> Self {
        Self::reference(ExpressionReference::weaker())
    }

    /// The expression of one pattern.
    #[must_use]
    pub fn pattern(pattern: PathPattern) -> Self {
        Self {
            root: Some(Expr::Pattern(pattern)),
        }
    }

    /// The expression of one reference.
    #[must_use]
    pub fn reference(reference: ExpressionReference) -> Self {
        Self {
            root: Some(Expr::Reference(reference)),
        }
    }

    /// The root node; `None` for the empty expression.
    #[must_use]
    pub fn root(&self) -> Option<&Expr> {
        self.root.as_ref()
    }

    /// Whether it is the empty expression, which matches nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    fn is_everything(&self) -> bool {
        self.root.as_ref().is_some_and(Expr::is_everything)
    }

    /// The set complement of `operand`, simplified: `~~a` is `a`, and
    /// nothing and everything swap.
    ///
    /// OpenUSD: `SdfPathExpression::MakeComplement`.
    #[must_use]
    pub fn complement(operand: Self) -> Self {
        match operand.root {
            None => Self::everything(),
            Some(root) if root.is_everything() => Self::nothing(),
            Some(Expr::Complement(inner)) => Self { root: Some(*inner) },
            Some(root) => Self {
                root: Some(Expr::Complement(Box::new(root))),
            },
        }
    }

    /// `left op right`, simplified when an operand is nothing or
    /// everything.
    ///
    /// OpenUSD: `SdfPathExpression::MakeOp`.
    #[must_use]
    pub fn op(op: SetOp, left: Self, right: Self) -> Self {
        let (mut op, mut right) = (op, right);
        let trivial = |e: &Self| e.is_empty() || e.is_everything();
        if op == SetOp::Difference && (trivial(&left) || trivial(&right)) {
            op = SetOp::Intersection;
            right = Self::complement(right);
        }
        let intersection = op == SetOp::Intersection;
        if left.is_empty() {
            if intersection { Self::nothing() } else { right }
        } else if right.is_empty() {
            if intersection { Self::nothing() } else { left }
        } else if left.is_everything() {
            if intersection {
                right
            } else {
                Self::everything()
            }
        } else if right.is_everything() {
            if intersection {
                left
            } else {
                Self::everything()
            }
        } else {
            let (Some(l), Some(r)) = (left.root, right.root) else {
                unreachable!("both operands are non-empty");
            };
            Self {
                root: Some(Expr::Op(op, Box::new(l), Box::new(r))),
            }
        }
    }

    /// Whether every pattern prefix and reference path is absolute.
    ///
    /// OpenUSD: `SdfPathExpression::IsAbsolute`.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        let mut absolute = true;
        if let Some(root) = &self.root {
            root.for_each_atom(&mut |atom| match atom {
                Expr::Pattern(pattern) => absolute &= pattern.prefix.absolute,
                Expr::Reference(reference) => {
                    absolute &= reference.path.as_ref().is_none_or(|path| path.absolute);
                }
                _ => {}
            });
        }
        absolute
    }

    /// Whether it contains references to other expressions.
    ///
    /// OpenUSD: `SdfPathExpression::ContainsExpressionReferences`.
    #[must_use]
    pub fn contains_references(&self) -> bool {
        let mut found = false;
        if let Some(root) = &self.root {
            root.for_each_atom(&mut |atom| found |= matches!(atom, Expr::Reference(_)));
        }
        found
    }

    /// Whether it contains `%_`.
    ///
    /// OpenUSD: `SdfPathExpression::ContainsWeakerExpressionReference`.
    #[must_use]
    pub fn contains_weaker_reference(&self) -> bool {
        let mut found = false;
        if let Some(root) = &self.root {
            root.for_each_atom(&mut |atom| {
                found |= matches!(atom, Expr::Reference(r) if r.is_weaker());
            });
        }
        found
    }

    /// Whether it can be matched: absolute, and without references.
    ///
    /// OpenUSD: `SdfPathExpression::IsComplete`.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.contains_references() && self.is_absolute()
    }

    /// Its references, left to right.
    #[must_use]
    pub fn references(&self) -> Vec<&ExpressionReference> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.for_each_atom(&mut |atom| {
                if let Expr::Reference(reference) = atom {
                    out.push(reference);
                }
            });
        }
        out
    }

    /// Rebuilds the expression, replacing each pattern and reference by
    /// `atom`'s expression and simplifying as [`PathExpression::op`] does.
    pub(crate) fn rebuild(self, atom: &mut impl FnMut(Expr) -> Self) -> Self {
        fn walk(node: Expr, atom: &mut impl FnMut(Expr) -> PathExpression) -> PathExpression {
            match node {
                Expr::Pattern(_) | Expr::Reference(_) => atom(node),
                Expr::Complement(operand) => PathExpression::complement(walk(*operand, atom)),
                Expr::Op(op, left, right) => {
                    let left = walk(*left, atom);
                    PathExpression::op(op, left, walk(*right, atom))
                }
            }
        }
        match self.root {
            None => Self::nothing(),
            Some(root) => walk(root, atom),
        }
    }

    /// Replaces each reference with the expression `resolve` gives for it,
    /// left to right; patterns stay.
    ///
    /// OpenUSD: `SdfPathExpression::ResolveReferences`.
    #[must_use]
    pub fn resolve_references(
        &self,
        resolve: &mut impl FnMut(&ExpressionReference) -> Self,
    ) -> Self {
        self.clone().rebuild(&mut |atom| match atom {
            Expr::Reference(reference) => resolve(&reference),
            atom => Self { root: Some(atom) },
        })
    }

    /// Replaces every `%_` with `weaker`.
    ///
    /// OpenUSD: `SdfPathExpression::ComposeOver`.
    #[must_use]
    pub fn compose_over(&self, weaker: &Self) -> Self {
        self.resolve_references(&mut |reference| {
            if reference.is_weaker() {
                weaker.clone()
            } else {
                Self::reference(reference.clone())
            }
        })
    }

    /// Resolves every named reference, recursively, into a complete
    /// expression: each reference becomes the expression `resolver` finds
    /// for it, with that expression's own references resolved in turn.
    ///
    /// `%_` becomes nothing, as does a reference `resolver` finds nothing
    /// for and a reference back to an expression being resolved (a cycle);
    /// both are reported. `origin` identifies this expression to the
    /// resolver, which resolves `%:name` against it.
    ///
    /// OpenUSD: `UsdCollectionAPI::ResolveCompleteMembershipExpression`,
    /// which substitutes nothing for a missing collection and for the
    /// second visit of a collection on a reference cycle.
    pub fn resolve_complete<R: ReferenceResolver>(
        &self,
        origin: R::Key,
        resolver: &mut R,
    ) -> ResolvedExpression<R::Key> {
        let mut out = ResolvedExpression {
            expression: Self::nothing(),
            referenced: Vec::new(),
            problems: Vec::new(),
        };
        let mut visiting = HashSet::new();
        visiting.insert(origin.clone());
        out.expression = resolve_in(self, &origin, resolver, &mut visiting, &mut out);
        out
    }

    /// Makes relative pattern prefixes and reference paths absolute at the
    /// prim whose names are `anchor`; one that climbs above the root with
    /// `..` matches nothing.
    ///
    /// OpenUSD: `SdfPathExpression::MakeAbsolute`.
    #[must_use]
    pub fn make_absolute(&self, anchor: &[String]) -> Self {
        value::anchor_and_map(self.clone(), anchor, &[])
    }

    /// The expression's text, as `SdfPathExpression::GetText` writes it.
    ///
    /// That text does not always parse back to the same expression:
    /// OpenUSD writes the predicate argument `true` as `1` (an integer when
    /// read back) and `2.0` as `2`.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        if let Some(root) = &self.root {
            root.write(&mut out, false);
        }
        out
    }

    /// Text that parses back to this expression: [`PathExpression::text`],
    /// except that boolean predicate arguments are written `true` and
    /// `false`, and integral floats with `.0`. Composed `pathExpression`
    /// values are stored this way.
    #[must_use]
    pub fn lossless_text(&self) -> String {
        let mut out = String::new();
        if let Some(root) = &self.root {
            root.write(&mut out, true);
        }
        out
    }

    /// Links its predicates with `predicates` into a matcher.
    ///
    /// OpenUSD: `SdfMakePathExpressionEval`.
    ///
    /// # Errors
    ///
    /// [`MatcherError`] when the expression is not complete
    /// ([`PathExpression::is_complete`]), a predicate call does not bind,
    /// or a glob is malformed. OpenUSD makes an empty evaluator then, which
    /// matches nothing.
    pub fn matcher<P: Predicates>(
        &self,
        predicates: &P,
    ) -> Result<PathMatcher<P::Call>, MatcherError> {
        PathMatcher::new(self, predicates)
    }
}

impl fmt::Display for PathExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

impl core::str::FromStr for PathExpression {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

/// Finds the expressions that references name, for
/// [`PathExpression::resolve_complete`].
pub trait ReferenceResolver {
    /// What identifies an expression (for collections, the prim and the
    /// collection name).
    type Key: Clone + Eq + Hash + fmt::Debug;

    /// The expression `reference`, found in the expression `from`, names,
    /// and its key; `None` when there is none.
    fn resolve(
        &mut self,
        from: &Self::Key,
        reference: &ExpressionReference,
    ) -> Option<(Self::Key, PathExpression)>;
}

/// A complete expression [`PathExpression::resolve_complete`] resolved,
/// with what it read.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedExpression<K> {
    /// The expression with every reference resolved.
    pub expression: PathExpression,
    /// The keys of the expressions it read, in the order it read them.
    pub referenced: Vec<K>,
    /// References resolved to nothing, and why.
    pub problems: Vec<ReferenceProblem<K>>,
}

/// A reference [`PathExpression::resolve_complete`] resolved to nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReferenceProblem<K> {
    /// The resolver found no expression for it.
    Missing {
        /// The expression the reference is in.
        from: K,
        /// The reference.
        reference: ExpressionReference,
    },
    /// It names an expression being resolved, which would recurse forever.
    Cycle {
        /// The expression the reference is in.
        from: K,
        /// The reference.
        reference: ExpressionReference,
        /// The expression it names.
        target: K,
    },
}

fn resolve_in<R: ReferenceResolver>(
    expression: &PathExpression,
    from: &R::Key,
    resolver: &mut R,
    visiting: &mut HashSet<R::Key>,
    out: &mut ResolvedExpression<R::Key>,
) -> PathExpression {
    expression.resolve_references(&mut |reference| {
        if reference.is_weaker() || reference.name.is_empty() {
            return PathExpression::nothing();
        }
        let Some((key, target)) = resolver.resolve(from, reference) else {
            out.problems.push(ReferenceProblem::Missing {
                from: from.clone(),
                reference: reference.clone(),
            });
            return PathExpression::nothing();
        };
        if !visiting.insert(key.clone()) {
            out.problems.push(ReferenceProblem::Cycle {
                from: from.clone(),
                reference: reference.clone(),
                target: key,
            });
            return PathExpression::nothing();
        }
        if !out.referenced.contains(&key) {
            out.referenced.push(key.clone());
        }
        let resolved = resolve_in(&target, &key, resolver, visiting, out);
        // Another branch may reach the same expression again.
        visiting.remove(&key);
        resolved
    })
}

#[cfg(test)]
mod tests;
