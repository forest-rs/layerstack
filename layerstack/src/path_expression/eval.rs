// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Matching paths against complete path expressions.
//!
//! OpenUSD: `SdfPathExpressionEval` (`pxr/usd/sdf/pathExpressionEval.cpp`),
//! `SdfPredicateProgram` (`predicateProgram.h`) and
//! `SdfPredicateFunctionResult` (`predicateLibrary.h`), ported with their
//! results' constancy.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;

use crate::doc::LayerStore;
use crate::path::{Path, TargetPath};

use super::glob::Glob;
use super::{
    Expr, PathExpression, PathPattern, PredicateBindError, PredicateCall, PredicateExpression,
    SetOp,
};

/// Whether a match result holds for every descendant of the matched path.
///
/// OpenUSD: `SdfPredicateFunctionResult::Constancy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Constancy {
    /// Every descendant (prim or property) of the path matches the same
    /// way: a traversal can skip them.
    ConstantOverDescendants,
    /// Descendants may match differently.
    MayVaryOverDescendants,
}

/// Whether a path matches, and whether its descendants all match the same
/// way; also what a predicate function returns.
///
/// OpenUSD: `SdfPredicateFunctionResult`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MatchResult {
    /// Whether it matches.
    pub value: bool,
    /// Whether descendants match the same way.
    pub constancy: Constancy,
}

impl MatchResult {
    /// `value`, for the path and every descendant.
    #[must_use]
    pub const fn constant(value: bool) -> Self {
        Self {
            value,
            constancy: Constancy::ConstantOverDescendants,
        }
    }

    /// `value`, for the path alone.
    #[must_use]
    pub const fn varying(value: bool) -> Self {
        Self {
            value,
            constancy: Constancy::MayVaryOverDescendants,
        }
    }

    /// Whether descendants match the same way.
    #[must_use]
    pub fn is_constant(self) -> bool {
        self.constancy == Constancy::ConstantOverDescendants
    }

    /// Logical and, constant when both are, or when a constant side is
    /// false.
    ///
    /// OpenUSD: `SdfPredicateFunctionResult::And`.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        let (lc, rc) = (self.is_constant(), other.is_constant());
        let value = self.value && other.value;
        if (lc && rc) || (!self.value && lc) || (!other.value && rc) {
            Self::constant(value)
        } else {
            Self::varying(value)
        }
    }

    /// Logical or, constant when both are, or when a constant side is
    /// true.
    ///
    /// OpenUSD: `SdfPredicateFunctionResult::Or`.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        let (lc, rc) = (self.is_constant(), other.is_constant());
        let value = self.value || other.value;
        if (lc && rc) || (self.value && lc) || (other.value && rc) {
            Self::constant(value)
        } else {
            Self::varying(value)
        }
    }
}

impl core::ops::Not for MatchResult {
    type Output = Self;

    /// The opposite value, with the same constancy.
    fn not(self) -> Self {
        Self {
            value: !self.value,
            constancy: self.constancy,
        }
    }
}

/// A library of predicate functions, which evaluates the predicates of
/// path patterns (`{isa:Mesh}`) on the objects paths name.
///
/// Linking binds each call once ([`Predicates::bind`]); matching then
/// evaluates bound calls ([`Predicates::evaluate`]).
///
/// OpenUSD: `SdfPredicateLibrary`, as `SdfLinkPredicateExpression` links
/// against it.
pub trait Predicates {
    /// A call bound to its function and arguments.
    type Call;

    /// Binds `call`.
    ///
    /// # Errors
    ///
    /// [`PredicateBindError`] when no function has the call's name, or its
    /// arguments do not fit.
    fn bind(&self, call: &PredicateCall) -> Result<Self::Call, PredicateBindError>;

    /// Evaluates a bound call on the object at `object`: a prim, or a
    /// property.
    fn evaluate(&self, call: &Self::Call, object: TargetPath) -> MatchResult;
}

/// The empty predicate library: every predicate fails to bind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoPredicates;

impl Predicates for NoPredicates {
    type Call = core::convert::Infallible;

    fn bind(&self, call: &PredicateCall) -> Result<Self::Call, PredicateBindError> {
        Err(PredicateBindError::new(
            &call.name,
            "no predicates are defined",
        ))
    }

    fn evaluate(&self, call: &Self::Call, _: TargetPath) -> MatchResult {
        match *call {}
    }
}

/// Why a [`PathMatcher`] could not be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatcherError {
    /// The expression has relative prefixes or references
    /// ([`PathExpression::is_complete`]).
    Incomplete {
        /// The expression's text.
        expression: String,
    },
    /// A predicate call of a pattern did not bind.
    Predicate {
        /// The pattern's text.
        pattern: String,
        /// Why.
        error: PredicateBindError,
    },
    /// A glob does not compile (an unclosed `[`).
    Glob {
        /// The pattern's text.
        pattern: String,
        /// The glob.
        glob: String,
    },
}

impl fmt::Display for MatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete { expression } => write!(
                f,
                "cannot match `{expression}`: it has relative paths or references"
            ),
            Self::Predicate { pattern, error } => write!(f, "in `{pattern}`: {error}"),
            Self::Glob { pattern, glob } => write!(f, "in `{pattern}`: malformed glob `{glob}`"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for MatcherError {}

/// A predicate expression linked to bound calls.
#[derive(Clone, Debug, PartialEq)]
enum Program<C> {
    Call(C),
    Not(Box<Self>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
}

impl<C> Program<C> {
    fn link<P: Predicates<Call = C>>(
        expression: &PredicateExpression,
        predicates: &P,
    ) -> Result<Self, PredicateBindError> {
        Ok(match expression {
            PredicateExpression::Call(call) => Self::Call(predicates.bind(call)?),
            PredicateExpression::Not(operand) => {
                Self::Not(Box::new(Self::link(operand, predicates)?))
            }
            PredicateExpression::Op(op, left, right) => {
                let left = Box::new(Self::link(left, predicates)?);
                let right = Box::new(Self::link(right, predicates)?);
                match op {
                    super::PredicateOp::Or => Self::Or(left, right),
                    _ => Self::And(left, right),
                }
            }
        })
    }

    /// Runs the program as `SdfPredicateProgram::operator()` does: `and` and
    /// `or` short-circuit on the running value, and the result stays
    /// constant only while every call run was.
    fn run(&self, result: &mut MatchResult, call: &mut impl FnMut(&C) -> MatchResult) {
        match self {
            Self::Call(c) => {
                let other = call(c);
                result.value = other.value;
                if !other.is_constant() {
                    result.constancy = Constancy::MayVaryOverDescendants;
                }
            }
            Self::Not(operand) => {
                operand.run(result, call);
                result.value = !result.value;
            }
            Self::And(left, right) => {
                left.run(result, call);
                if result.value {
                    right.run(result, call);
                }
            }
            Self::Or(left, right) => {
                left.run(result, call);
                if !result.value {
                    right.run(result, call);
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Name {
    /// A literal name; empty for a bare predicate, which matches any name.
    Exact(String),
    Glob(Glob),
}

#[derive(Clone, Debug, PartialEq)]
struct Component<C> {
    name: Name,
    predicate: Option<Program<C>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchType {
    PrimOrProperty,
    PrimOnly,
    PropertyOnly,
}

/// One pattern, ready to match: its prefix, then segments of components
/// between stretches.
#[derive(Clone, Debug, PartialEq)]
struct PatternMatcher<C> {
    prefix_prims: Vec<String>,
    prefix_property: Option<String>,
    components: Vec<Component<C>>,
    /// Half-open ranges of `components` between stretches.
    segments: Vec<(usize, usize)>,
    stretch_begin: bool,
    stretch_end: bool,
    match_type: MatchType,
}

/// A path's elements (prim names, then its property's), with a way to name
/// each prefix's object for predicates.
struct Elements<'s> {
    names: Vec<&'s str>,
    prims: &'s [crate::interner::TokenId],
    property: Option<crate::interner::TokenId>,
    path: TargetPath,
    store: &'s dyn LayerStore,
}

impl<'s> Elements<'s> {
    fn new(store: &'s dyn LayerStore, path: TargetPath) -> Self {
        let (prim, property) = match path {
            TargetPath::Prim(prim) => (prim, None),
            TargetPath::Property(p) => (p.prim_path(), Some(p.property())),
        };
        let prims = store.paths().resolve(prim).segments();
        let tokens = store.tokens();
        let mut names: Vec<&'s str> = prims.iter().map(|t| tokens.resolve(*t)).collect();
        if let Some(property) = property {
            names.push(tokens.resolve(property));
        }
        Self {
            names,
            prims,
            property,
            path,
            store,
        }
    }

    fn len(&self) -> usize {
        self.names.len()
    }

    fn is_property(&self) -> bool {
        self.property.is_some()
    }

    /// The object at the prefix of `count` elements.
    fn object(&self, count: usize) -> Option<TargetPath> {
        if count == self.len() {
            return Some(self.path);
        }
        let paths = self.store.paths();
        paths
            .lookup(&Path::root().join(&self.prims[..count]))
            .map(TargetPath::Prim)
    }

    /// Whether the path starts with `prims` (and `property`).
    fn has_prefix(&self, prims: &[String], property: Option<&str>) -> bool {
        let prim_count = self.prims.len();
        prims.len() <= prim_count
            && prims.iter().zip(&self.names).all(|(a, b)| a == b)
            && property.is_none_or(|property| {
                prims.len() == prim_count
                    && self.property.is_some()
                    && self.names[prim_count] == property
            })
    }

    /// Whether the path is a prefix of `prims` (and `property`): a prim
    /// path of an ancestor or the same prim, or the same property path.
    fn is_prefix_of(&self, prims: &[String], property: Option<&str>) -> bool {
        let own = &self.names[..self.prims.len()];
        let same_prims = |len_ok: bool| len_ok && own.iter().zip(prims).all(|(a, b)| *a == b);
        match self.property {
            Some(_) => property.is_some_and(|property| {
                same_prims(own.len() == prims.len()) && self.names[own.len()] == property
            }),
            None => same_prims(own.len() <= prims.len()),
        }
    }
}

impl<C> PatternMatcher<C> {
    fn link<P: Predicates<Call = C>>(
        pattern: &PathPattern,
        predicates: &P,
    ) -> Result<Self, MatcherError> {
        let mut matcher = Self {
            prefix_prims: pattern.prefix.prims.clone(),
            prefix_property: pattern.prefix.property.clone(),
            components: Vec::new(),
            segments: Vec::new(),
            stretch_begin: false,
            stretch_end: false,
            match_type: MatchType::PrimOnly,
        };
        let count = pattern.components.len();
        for (i, component) in pattern.components.iter().enumerate() {
            if component.is_stretch() {
                if i + 1 == count {
                    matcher.stretch_end = true;
                }
                if matcher.components.is_empty() {
                    matcher.stretch_begin = true;
                } else {
                    matcher.close_segment();
                }
                continue;
            }
            let name = if component.literal || component.text.is_empty() {
                Name::Exact(component.text.clone())
            } else {
                Name::Glob(
                    Glob::compile(&component.text).ok_or_else(|| MatcherError::Glob {
                        pattern: pattern.text(),
                        glob: component.text.clone(),
                    })?,
                )
            };
            let predicate = component
                .predicate
                .as_ref()
                .map(|predicate| Program::link(predicate, predicates))
                .transpose()
                .map_err(|error| MatcherError::Predicate {
                    pattern: pattern.text(),
                    error,
                })?;
            matcher.components.push(Component { name, predicate });
        }
        if !matcher.stretch_end && !matcher.components.is_empty() {
            matcher.close_segment();
        }
        matcher.match_type = if pattern.is_property {
            MatchType::PropertyOnly
        } else if matcher.stretch_end
            || matcher
                .components
                .last()
                .is_some_and(|c| c.name == Name::Exact(String::new()))
        {
            MatchType::PrimOrProperty
        } else {
            MatchType::PrimOnly
        };
        Ok(matcher)
    }

    fn close_segment(&mut self) {
        let begin = self.segments.last().map_or(0, |s| s.1);
        self.segments.push((begin, self.components.len()));
    }

    fn is_bare_predicate(&self, index: usize) -> bool {
        let component = &self.components[index];
        component.name == Name::Exact(String::new()) && component.predicate.is_some()
    }

    fn segment_min_elements(&self, segment: (usize, usize)) -> usize {
        (segment.1 - segment.0) - usize::from(self.is_bare_predicate(segment.0))
    }

    fn prefix_len(&self) -> usize {
        self.prefix_prims.len() + usize::from(self.prefix_property.is_some())
    }

    /// `_CheckExactMatch`: whether `segment` matches the prefixes (element
    /// counts) `prefixes[*at..end]` exactly from `*at`; on a match, moves
    /// `*at` past it.
    fn check_exact(
        &self,
        segment: (usize, usize),
        prefixes: &[usize],
        end: usize,
        at: &mut usize,
        elements: &Elements<'_>,
        run: &mut impl FnMut(&Program<C>, TargetPath) -> MatchResult,
    ) -> MatchResult {
        let size = segment.1 - segment.0;
        if size > end - *at {
            return MatchResult::varying(false);
        }
        for (offset, component) in self.components[segment.0..segment.1].iter().enumerate() {
            let count = prefixes[*at + offset];
            let name = elements.names[count - 1];
            let named = match &component.name {
                Name::Exact(exact) => exact.is_empty() || exact == name,
                Name::Glob(glob) => glob.matches(name),
            };
            if !named {
                return MatchResult::varying(false);
            }
            if let Some(predicate) = &component.predicate {
                let result = match elements.object(count) {
                    Some(object) => run(predicate, object),
                    None => MatchResult::varying(false),
                };
                if !result.value {
                    return result;
                }
            }
        }
        *at += size;
        MatchResult::varying(true)
    }

    /// `_CheckMatch`: [`Self::check_exact`], also one element earlier when
    /// the segment starts with a bare predicate.
    fn check(
        &self,
        segment: (usize, usize),
        prefixes: &[usize],
        end: usize,
        at: &mut usize,
        elements: &Elements<'_>,
        run: &mut impl FnMut(&Program<C>, TargetPath) -> MatchResult,
    ) -> MatchResult {
        if *at != 0 && self.is_bare_predicate(segment.0) {
            *at -= 1;
            let result = self.check_exact(segment, prefixes, end, at, elements, run);
            if result.value {
                return result;
            }
            *at += 1;
        }
        self.check_exact(segment, prefixes, end, at, elements, run)
    }

    /// `_Match`.
    fn matches(
        &self,
        elements: &Elements<'_>,
        run: &mut impl FnMut(&Program<C>, TargetPath) -> MatchResult,
    ) -> MatchResult {
        let property = self.prefix_property.as_deref();
        if !elements.has_prefix(&self.prefix_prims, property) {
            return if elements.is_prefix_of(&self.prefix_prims, property) {
                MatchResult::varying(false)
            } else {
                MatchResult::constant(false)
            };
        }
        let is_property = elements.is_property();
        if self.match_type == MatchType::PropertyOnly && !is_property {
            return MatchResult::varying(false);
        }
        if self.match_type == MatchType::PrimOnly && is_property {
            return MatchResult::constant(false);
        }
        if self.components.is_empty() {
            if self.stretch_begin || self.stretch_end {
                return MatchResult::constant(true);
            }
            return if elements.len() == self.prefix_len() {
                MatchResult::varying(true)
            } else {
                MatchResult::constant(false)
            };
        }
        let extra = usize::from(
            self.stretch_begin && self.is_bare_predicate(0) && !self.prefix_prims.is_empty(),
        );
        let wanted = elements.len() - self.prefix_len() + extra;
        let prefixes = trailing_prefixes(elements.len(), wanted);
        if prefixes.is_empty() {
            return MatchResult::varying(false);
        }
        let end = prefixes.len();
        let mut at = 0;
        let count = self.components.len();
        for &segment in &self.segments {
            if !self.stretch_begin && segment.0 == 0 {
                let result = self.check(segment, &prefixes, end, &mut at, elements, run);
                if !result.value {
                    return result;
                }
                if !self.stretch_end && segment.1 == count && at != end {
                    return MatchResult::varying(false);
                }
            } else if !self.stretch_end && segment.1 == count {
                if end - at < self.segment_min_elements(segment) {
                    return MatchResult::varying(false);
                }
                let size = segment.1 - segment.0;
                // A segment starting with a bare predicate can be one
                // element longer than the path's prefixes (`/A//{p}/*`
                // against `/A`); it cannot match there. OpenUSD 26.08 reads
                // before the first prefix instead (undefined behavior).
                let Some(start) = end.checked_sub(size) else {
                    return MatchResult::varying(false);
                };
                at = start;
                let result = self.check_exact(segment, &prefixes, end, &mut at, elements, run);
                if !result.value {
                    return result;
                }
            } else {
                // An interior segment: the first place it matches.
                let size = segment.1 - segment.0;
                if size > end - at {
                    return MatchResult::varying(false);
                }
                let mut result = MatchResult::varying(false);
                let mut found = None;
                for start in at..=end - size {
                    let mut location = start;
                    result = self.check(segment, &prefixes, end, &mut location, elements, run);
                    if result.value {
                        found = Some(location);
                        break;
                    }
                }
                let Some(location) = found else {
                    return result;
                };
                at = location;
            }
        }
        if self.stretch_end {
            MatchResult::constant(true)
        } else {
            MatchResult::varying(true)
        }
    }

    /// `_Next`: the incremental step of a depth-first search.
    fn next(
        &self,
        state: &mut SearchState,
        elements: &Elements<'_>,
        run: &mut impl FnMut(&Program<C>, TargetPath) -> MatchResult,
    ) -> MatchResult {
        if state.constant_depth.is_some() {
            return MatchResult::constant(state.constant_value);
        }
        let path_count = elements.len();
        let prefix_count = self.prefix_len();
        let property = self.prefix_property.as_deref();
        if state.segment_depths.is_empty() && !elements.has_prefix(&self.prefix_prims, property) {
            if !elements.is_prefix_of(&self.prefix_prims, property) {
                state.constant_depth = Some(prefix_count);
                state.constant_value = false;
                return MatchResult::constant(false);
            }
            return MatchResult::varying(false);
        }
        let is_property = elements.is_property();
        if self.match_type == MatchType::PropertyOnly && !is_property {
            return MatchResult::varying(false);
        }
        if self.match_type == MatchType::PrimOnly && is_property {
            return MatchResult::constant(false);
        }
        if self.components.is_empty() {
            if self.stretch_begin || self.stretch_end {
                state.constant_depth = Some(prefix_count);
                state.constant_value = true;
                return MatchResult::constant(true);
            }
            if path_count > prefix_count {
                state.constant_depth = Some(prefix_count);
                state.constant_value = false;
                return MatchResult::constant(false);
            }
            return MatchResult::varying(true);
        }
        if state.segment_depths.len() == self.segments.len() {
            // Look for another match of the final segment.
            state.segment_depths.pop();
        }
        loop {
            let index = state.segment_depths.len();
            let segment = self.segments[index];
            let has_previous = index > 0;
            let is_final = index == self.segments.len() - 1;
            let base = state.segment_depths.last().copied().unwrap_or(prefix_count);
            let Some(available) = path_count.checked_sub(base) else {
                return MatchResult::varying(false);
            };
            if available < self.segment_min_elements(segment) {
                return MatchResult::varying(false);
            }
            let has_stretch = has_previous || self.stretch_begin;
            let size = segment.1 - segment.0;
            if !has_stretch && available > size {
                state.constant_depth = Some(path_count);
                state.constant_value = false;
                return MatchResult::constant(false);
            }
            let extra =
                usize::from(has_stretch && self.is_bare_predicate(segment.0) && path_count != 0);
            let prefixes = trailing_prefixes(path_count, available + extra);
            if prefixes.is_empty() {
                return MatchResult::varying(false);
            }
            let end = prefixes.len();
            let Some(start) = end.checked_sub(size) else {
                return MatchResult::varying(false);
            };
            let mut at = start;
            let result = if is_final {
                self.check_exact(segment, &prefixes, end, &mut at, elements, run)
            } else {
                self.check(segment, &prefixes, end, &mut at, elements, run)
            };
            if result.value {
                state.segment_depths.push(if at == end {
                    path_count
                } else {
                    path_count - 1
                });
            }
            if !result.value || is_final {
                break;
            }
        }
        if state.segment_depths.len() == self.segments.len() {
            let last = state.segment_depths.last().copied().unwrap_or(0);
            if self.stretch_end {
                state.constant_depth = Some(last);
                state.constant_value = true;
                return MatchResult::constant(true);
            }
            if last == path_count {
                return MatchResult::varying(true);
            }
            state.constant_depth = Some(last);
            state.constant_value = false;
            return MatchResult::constant(false);
        }
        MatchResult::varying(false)
    }
}

/// The element counts of the last `wanted` prefixes of a path of `len`
/// elements, shortest first (`SdfPath::GetPrefixes(n)`); all of them when
/// `wanted` exceeds `len`, none when it is zero.
fn trailing_prefixes(len: usize, wanted: usize) -> Vec<usize> {
    if wanted == 0 {
        return Vec::new();
    }
    let wanted = wanted.min(len);
    (len - wanted + 1..=len).collect()
}

/// The state of one pattern in a depth-first search.
#[derive(Clone, Debug, Default)]
struct SearchState {
    segment_depths: Vec<usize>,
    constant_depth: Option<usize>,
    constant_value: bool,
}

impl SearchState {
    /// `_PatternIncrSearchState::Pop`.
    fn pop(&mut self, depth: usize) {
        while self.segment_depths.last().is_some_and(|d| *d >= depth) {
            self.segment_depths.pop();
        }
        if self.constant_depth.is_some_and(|d| depth <= d) {
            self.constant_depth = None;
        }
    }
}

/// The set logic of an expression over its patterns, numbered left to
/// right.
#[derive(Clone, Debug, PartialEq)]
enum Logic {
    Pattern(usize),
    Not(Box<Self>),
    Or(Box<Self>, Box<Self>),
    And(Box<Self>, Box<Self>),
    AndNot(Box<Self>, Box<Self>),
}

impl Logic {
    /// Evaluates as `Sdf_PathExpressionEvalBase::_EvalExpr` does: a
    /// constant deciding left operand skips the right, otherwise both
    /// combine with their constancy.
    fn eval(&self, pattern: &mut impl FnMut(usize) -> MatchResult) -> MatchResult {
        match self {
            Self::Pattern(index) => pattern(*index),
            Self::Not(operand) => !operand.eval(pattern),
            Self::Or(left, right) => {
                let l = left.eval(pattern);
                if l.value && l.is_constant() {
                    l
                } else {
                    l.or(right.eval(pattern))
                }
            }
            Self::And(left, right) | Self::AndNot(left, right) => {
                let l = left.eval(pattern);
                if !l.value && l.is_constant() {
                    return l;
                }
                let r = right.eval(pattern);
                l.and(if matches!(self, Self::AndNot(..)) {
                    !r
                } else {
                    r
                })
            }
        }
    }
}

/// A complete path expression linked with a predicate library: it matches
/// prim and property paths.
///
/// A matcher of the empty expression matches nothing.
///
/// OpenUSD: `SdfPathExpressionEval`.
#[derive(Clone, Debug, PartialEq)]
pub struct PathMatcher<C> {
    patterns: Vec<PatternMatcher<C>>,
    logic: Option<Logic>,
}

impl<C> PathMatcher<C> {
    pub(super) fn new<P: Predicates<Call = C>>(
        expression: &PathExpression,
        predicates: &P,
    ) -> Result<Self, MatcherError> {
        if !expression.is_complete() {
            return Err(MatcherError::Incomplete {
                expression: expression.text(),
            });
        }
        let mut patterns = Vec::new();
        fn compile<C, P: Predicates<Call = C>>(
            node: &Expr,
            predicates: &P,
            patterns: &mut Vec<PatternMatcher<C>>,
        ) -> Result<Logic, MatcherError> {
            Ok(match node {
                Expr::Pattern(pattern) => {
                    patterns.push(PatternMatcher::link(pattern, predicates)?);
                    Logic::Pattern(patterns.len() - 1)
                }
                Expr::Reference(reference) => {
                    return Err(MatcherError::Incomplete {
                        expression: reference.to_string(),
                    });
                }
                Expr::Complement(operand) => {
                    Logic::Not(Box::new(compile(operand, predicates, patterns)?))
                }
                Expr::Op(op, left, right) => {
                    let left = Box::new(compile(left, predicates, patterns)?);
                    let right = Box::new(compile(right, predicates, patterns)?);
                    match op {
                        SetOp::ImpliedUnion | SetOp::Union => Logic::Or(left, right),
                        SetOp::Intersection => Logic::And(left, right),
                        SetOp::Difference => Logic::AndNot(left, right),
                    }
                }
            })
        }
        let logic = expression
            .root()
            .map(|root| compile(root, predicates, &mut patterns))
            .transpose()?;
        Ok(Self { patterns, logic })
    }

    /// Whether it matches nothing, having no patterns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Matches the prim or property `path`, evaluating predicates with
    /// `predicates` (the library it was linked with). `store` names the
    /// path's elements.
    ///
    /// OpenUSD: `SdfPathExpressionEval::Match`.
    pub fn matches<P: Predicates<Call = C>>(
        &self,
        predicates: &P,
        store: &dyn LayerStore,
        path: TargetPath,
    ) -> MatchResult {
        let Some(logic) = &self.logic else {
            return MatchResult::constant(false);
        };
        let elements = Elements::new(store, path);
        let mut run = run_with(predicates);
        logic.eval(&mut |index| self.patterns[index].matches(&elements, &mut run))
    }

    /// A depth-first incremental search.
    #[must_use]
    pub fn searcher(&self) -> Searcher<'_, C> {
        Searcher {
            matcher: self,
            states: alloc::vec![SearchState::default(); self.patterns.len()],
            last_depth: 0,
        }
    }
}

/// A predicate program runner over `predicates`.
fn run_with<'p, P: Predicates>(
    predicates: &'p P,
) -> impl FnMut(&Program<P::Call>, TargetPath) -> MatchResult + 'p {
    move |program, object| {
        let mut result = MatchResult::constant(false);
        program.run(&mut result, &mut |call| predicates.evaluate(call, object));
        result
    }
}

/// Matches paths in depth-first order, reusing what it learned about
/// ancestors: once a pattern's result is constant over a path's
/// descendants, it is not evaluated again below it.
///
/// Each path passed must follow the previous one in some depth-first
/// order: a child, a sibling, or a sibling of an ancestor (a prim's
/// properties count as its children).
///
/// OpenUSD: `SdfPathExpressionEval::IncrementalSearcher`.
#[derive(Clone, Debug)]
pub struct Searcher<'m, C> {
    matcher: &'m PathMatcher<C>,
    states: Vec<SearchState>,
    last_depth: usize,
}

impl<C> Searcher<'_, C> {
    /// Matches `path`, the next path of the search.
    pub fn next<P: Predicates<Call = C>>(
        &mut self,
        predicates: &P,
        store: &dyn LayerStore,
        path: TargetPath,
    ) -> MatchResult {
        let Some(logic) = &self.matcher.logic else {
            return MatchResult::constant(false);
        };
        let elements = Elements::new(store, path);
        let depth = elements.len();
        if depth <= self.last_depth {
            for state in &mut self.states {
                state.pop(depth);
            }
        }
        self.last_depth = depth;
        let mut run = run_with(predicates);
        let patterns = &self.matcher.patterns;
        let states = &mut self.states;
        logic.eval(&mut |index| patterns[index].next(&mut states[index], &elements, &mut run))
    }
}
