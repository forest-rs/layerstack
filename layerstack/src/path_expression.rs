// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition of `pathExpression` values.
//!
//! A path expression (OpenUSD's `SdfPathExpression`) combines path patterns
//! and references to other expressions with set operators. Composition
//! handles it in two steps:
//!
//! - Each opinion is anchored and mapped into the stage namespace: relative
//!   patterns are made absolute at the prim that authors them, and every
//!   pattern prefix and reference path is mapped through the arcs to the
//!   composed prim. A pattern outside an arc's domain matches nothing,
//!   except one with a leading stretch (`//`), which matches anywhere.
//!   OpenUSD: `PcpMapFunction::MapSourceToTarget(SdfPathExpression)` in
//!   `pxr/usd/pcp/mapFunction.cpp`, applied by `UsdStage` value resolution.
//! - `%_` names the next weaker opinion's expression, spliced in by
//!   [`fold`] at the default time and at numeric times alike; once no
//!   weaker opinion is left, it matches nothing. OpenUSD:
//!   `SdfPathExpression::ComposeOver`.
//!
//! Text is written the way `SdfPathExpression::GetText` writes it:
//! operators spaced, parentheses only where precedence needs them.
//! Predicates (`{...}`) are kept as authored.
//!
//! Spec: AOUSD Core §10 (composition arcs map namespace), §12.3 (attribute
//! value resolution).

use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    string::String,
    sync::Arc,
    vec::Vec,
};

use hashbrown::HashMap;

use crate::{
    doc::{FieldValue, InterpolationType, LayerStore, Value},
    path::PathId,
    prim_index::{ArcKind, Opinion, OpinionValue, PrimIndex},
    prim_index_graph::{NodeId, PrimIndexGraph},
    spec_path::{SpecComponent, SpecPath},
    stage::value_at_time,
};

/// A set operator, tightest binding first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Op {
    /// Juxtaposition: `a b`.
    ImpliedUnion,
    /// `a + b`.
    Union,
    /// `a & b`.
    Intersection,
    /// `a - b`.
    Difference,
}

impl Op {
    fn text(self) -> &'static str {
        match self {
            Self::ImpliedUnion => " ",
            Self::Union => " + ",
            Self::Intersection => " & ",
            Self::Difference => " - ",
        }
    }
}

/// A parsed path expression.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Expr {
    /// The empty expression, which matches nothing.
    Nothing,
    Pattern(Pattern),
    Reference(Reference),
    Complement(Box<Self>),
    Op(Op, Box<Self>, Box<Self>),
}

/// A path pattern: a literal path prefix followed by the rest of the
/// pattern (wildcards, `//`, predicates), kept verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pattern {
    absolute: bool,
    /// Literal prim names of the prefix; a relative prefix may also hold
    /// `.` and `..`.
    prims: Vec<String>,
    /// A literal property name ending the prefix.
    property: Option<String>,
    /// The rest of the pattern text: when `prims` is not empty, either empty
    /// or starting with `/` or `.`.
    rest: String,
}

/// A reference to another expression: `%_`, `%:name`, `%/Path:name` or a
/// relative `%../Path:name`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reference {
    /// The prim path, `None` when no path is authored.
    path: Option<RefPath>,
    name: String,
}

/// The prim path of a [`Reference`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct RefPath {
    absolute: bool,
    /// Prim names; a relative path starts with one or more `..`.
    prims: Vec<String>,
}

impl Reference {
    fn is_weaker(&self) -> bool {
        self.path.is_none() && self.name == "_"
    }
}

impl Pattern {
    /// `//`: every path.
    fn is_everything(&self) -> bool {
        self.absolute && self.prims.is_empty() && self.property.is_none() && self.rest == "/"
    }
}

impl Expr {
    fn everything() -> Self {
        Self::Pattern(Pattern {
            absolute: true,
            prims: Vec::new(),
            property: None,
            rest: "/".to_owned(),
        })
    }

    fn is_everything(&self) -> bool {
        matches!(self, Self::Pattern(pattern) if pattern.is_everything())
    }

    /// `~operand`, simplified as `SdfPathExpression::MakeComplement` does.
    fn complement(operand: Self) -> Self {
        match operand {
            Self::Nothing => Self::everything(),
            operand if operand.is_everything() => Self::Nothing,
            Self::Complement(inner) => *inner,
            operand => Self::Complement(Box::new(operand)),
        }
    }

    /// `left op right`, simplified as `SdfPathExpression::MakeOp` does.
    fn op(op: Op, left: Self, right: Self) -> Self {
        let (l_nothing, r_nothing) = (left == Self::Nothing, right == Self::Nothing);
        let (l_all, r_all) = (left.is_everything(), right.is_everything());
        match op {
            Op::ImpliedUnion | Op::Union => {
                if l_all || r_all {
                    Self::everything()
                } else if l_nothing {
                    right
                } else if r_nothing {
                    left
                } else {
                    Self::Op(op, Box::new(left), Box::new(right))
                }
            }
            Op::Intersection => {
                if l_nothing || r_nothing {
                    Self::Nothing
                } else if l_all {
                    right
                } else if r_all {
                    left
                } else {
                    Self::Op(op, Box::new(left), Box::new(right))
                }
            }
            Op::Difference => {
                if l_nothing || r_all {
                    Self::Nothing
                } else if r_nothing {
                    left
                } else if l_all {
                    Self::complement(right)
                } else {
                    Self::Op(op, Box::new(left), Box::new(right))
                }
            }
        }
    }

    /// Whether the expression names the next weaker expression, `%_`.
    fn has_weaker_reference(&self) -> bool {
        match self {
            Self::Nothing | Self::Pattern(_) => false,
            Self::Reference(reference) => reference.is_weaker(),
            Self::Complement(operand) => operand.has_weaker_reference(),
            Self::Op(_, left, right) => left.has_weaker_reference() || right.has_weaker_reference(),
        }
    }

    /// Rebuilds the expression bottom-up, replacing each pattern and
    /// reference through `atom`.
    fn rebuild(self, atom: &mut impl FnMut(Self) -> Self) -> Self {
        match self {
            Self::Nothing => Self::Nothing,
            Self::Pattern(_) | Self::Reference(_) => atom(self),
            Self::Complement(operand) => Self::complement(operand.rebuild(atom)),
            Self::Op(op, left, right) => Self::op(op, left.rebuild(atom), right.rebuild(atom)),
        }
    }

    /// The operator precedence of the expression's outermost node; atoms and
    /// complements bind tightest.
    fn precedence(&self) -> Option<Op> {
        match self {
            Self::Op(op, ..) => Some(*op),
            _ => None,
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Nothing => {}
            Self::Pattern(pattern) => pattern.write(out),
            Self::Reference(reference) => {
                out.push('%');
                if let Some(path) = &reference.path {
                    if path.absolute {
                        write_absolute(out, &path.prims);
                    } else {
                        out.push_str(&path.prims.join("/"));
                    }
                }
                if !reference.is_weaker() {
                    out.push(':');
                }
                out.push_str(&reference.name);
            }
            Self::Complement(operand) => {
                out.push('~');
                operand.write_grouped(out, operand.precedence().is_some());
            }
            Self::Op(op, left, right) => {
                // Left-associative: a right operand of equal precedence
                // needs parentheses.
                left.write_grouped(out, left.precedence().is_some_and(|p| p > *op));
                out.push_str(op.text());
                right.write_grouped(out, right.precedence().is_some_and(|p| p >= *op));
            }
        }
    }

    fn write_grouped(&self, out: &mut String, grouped: bool) {
        if grouped {
            out.push('(');
        }
        self.write(out);
        if grouped {
            out.push(')');
        }
    }

    /// The expression's text, as `SdfPathExpression::GetText` writes it.
    fn text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
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

impl Pattern {
    fn write(&self, out: &mut String) {
        if self.absolute {
            write_absolute(out, &self.prims);
        } else {
            out.push_str(&self.prims.join("/"));
        }
        if let Some(property) = &self.property {
            out.push('.');
            out.push_str(property);
        }
        out.push_str(&self.rest);
    }

    /// Makes a relative pattern absolute at the prim `anchor`; `None` when
    /// `..` climbs above the root.
    fn anchored(mut self, anchor: &[String]) -> Option<Self> {
        if self.absolute {
            return Some(self);
        }
        let relative_names = !self.prims.is_empty();
        let prims = anchor_names(anchor, core::mem::take(&mut self.prims))?;
        // The rest without the separator that joins it to the prefix: a
        // relative prefix's rest holds its separator (`/`, or `.` before a
        // property), a pattern with no literal prefix (`Name*`) has none.
        let (separated, body) = match self.rest.strip_prefix('/') {
            Some(body) if relative_names => (true, body),
            _ => (!relative_names, self.rest.as_str()),
        };
        self.rest = if prims.is_empty() || body.is_empty() || !separated {
            body.to_owned()
        } else {
            alloc::format!("/{body}")
        };
        self.absolute = true;
        self.prims = prims;
        Some(self)
    }
}

/// Makes relative prim names (`.`, `..` and names) absolute at `anchor`;
/// `None` when `..` climbs above the root.
fn anchor_names(anchor: &[String], relative: Vec<String>) -> Option<Vec<String>> {
    let mut prims: Vec<String> = anchor.to_vec();
    for name in relative {
        match name.as_str() {
            "." => {}
            ".." => {
                prims.pop()?;
            }
            _ => prims.push(name),
        }
    }
    Some(prims)
}

/// Whether `c` may appear in a literal prim or property name.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The length of the literal name at the start of `text`, when a
/// separator or the end follows it; `0` otherwise.
fn literal_name_len(text: &str) -> usize {
    let len = text.find(|c: char| !is_name_char(c)).unwrap_or(text.len());
    let after = &text[len..];
    if after.is_empty() || after.starts_with(['/', '.']) {
        len
    } else {
        0
    }
}

/// Parses one path pattern, splitting its literal prefix from the rest.
fn parse_pattern(text: &str) -> Option<Pattern> {
    let absolute = text.starts_with('/');
    let mut prims: Vec<String> = Vec::new();
    let mut pos = usize::from(absolute);
    if !absolute {
        // `.` (only before `//` or the end) and `..` components.
        loop {
            let tail = &text[pos..];
            let skip = if prims.is_empty() {
                0
            } else if tail.starts_with('/') && !tail.starts_with("//") {
                1
            } else {
                break;
            };
            let component = &tail[skip..];
            let len = if component.starts_with("..") {
                2
            } else if prims.is_empty() && component.starts_with('.') {
                1
            } else {
                break;
            };
            let after = &component[len..];
            if !(after.is_empty() || after.starts_with('/'))
                || (len == 1 && after.starts_with('/') && !after.starts_with("//"))
            {
                // `.name` and `./Name` are not patterns.
                return None;
            }
            prims.push(component[..len].to_owned());
            pos += skip + len;
        }
    }
    let mut property = None;
    let mut first = prims.is_empty();
    loop {
        let tail = &text[pos..];
        // Past the first name, a single `/` separates the next one.
        let (skip, name) = if first {
            (0, tail)
        } else if tail.starts_with('/') && !tail.starts_with("//") {
            (1, &tail[1..])
        } else {
            break;
        };
        first = false;
        let len = literal_name_len(name);
        if len == 0 {
            break;
        }
        prims.push(name[..len].to_owned());
        pos += skip + len;
        if let Some(prop) = name[len..].strip_prefix('.') {
            // A property ends the prefix when it is literal and ends the
            // pattern.
            if !prop.is_empty() && prop.chars().all(|c| is_name_char(c) || c == ':') {
                property = Some(prop.to_owned());
                pos = text.len();
            }
            break;
        }
    }
    let rest = text[pos..].to_owned();
    if !absolute && prims.is_empty() && rest.is_empty() {
        return None;
    }
    Some(Pattern {
        absolute,
        prims,
        property,
        rest,
    })
}

fn parse_reference(text: &str) -> Option<Reference> {
    let body = text.strip_prefix('%')?;
    if body == "_" {
        return Some(Reference {
            path: None,
            name: "_".to_owned(),
        });
    }
    let (path, name) = body.rsplit_once(':')?;
    if name.is_empty() || !name.chars().all(is_name_char) {
        return None;
    }
    let path = if path.is_empty() {
        None
    } else {
        // An absolute path, or a relative one that starts with `..`.
        let (absolute, names) = match path.strip_prefix('/') {
            Some(names) => (true, names),
            None if path.starts_with("..") => (false, path),
            None => return None,
        };
        let prims: Vec<String> = if names.is_empty() {
            Vec::new()
        } else {
            names.split('/').map(ToOwned::to_owned).collect()
        };
        let leading_parents = if absolute {
            0
        } else {
            prims.iter().take_while(|n| *n == "..").count()
        };
        if prims[leading_parents..]
            .iter()
            .any(|n| n.is_empty() || !n.chars().all(is_name_char))
        {
            return None;
        }
        Some(RefPath { absolute, prims })
    };
    Some(Reference {
        path,
        name: name.to_owned(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token<'a> {
    Open,
    Close,
    Tilde,
    Binary(Op),
    Atom(&'a str),
}

fn tokenize(text: &str) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        let c = bytes[pos];
        match c {
            b' ' | b'\t' => pos += 1,
            b'(' => {
                tokens.push(Token::Open);
                pos += 1;
            }
            b')' => {
                tokens.push(Token::Close);
                pos += 1;
            }
            b'~' => {
                tokens.push(Token::Tilde);
                pos += 1;
            }
            b'+' => {
                tokens.push(Token::Binary(Op::Union));
                pos += 1;
            }
            b'&' => {
                tokens.push(Token::Binary(Op::Intersection));
                pos += 1;
            }
            b'-' => {
                tokens.push(Token::Binary(Op::Difference));
                pos += 1;
            }
            b'\n' | b'\r' => return None,
            _ => {
                let start = pos;
                let mut depth = 0_usize;
                let mut quote: Option<u8> = None;
                while pos < bytes.len() {
                    let c = bytes[pos];
                    if let Some(q) = quote {
                        if c == b'\\' {
                            pos += 1;
                        } else if c == q {
                            quote = None;
                        }
                    } else if depth > 0 {
                        match c {
                            b'{' => depth += 1,
                            b'}' => depth -= 1,
                            b'"' | b'\'' => quote = Some(c),
                            _ => {}
                        }
                    } else if c == b'[' {
                        // A glob character class, `[a-z]` or `[!a-z]`, kept
                        // whole: `-` inside it is a range, not an operator.
                        pos += 1;
                        while bytes.get(pos).is_some_and(|c| *c != b']') {
                            pos += 1;
                        }
                        if pos == bytes.len() {
                            return None;
                        }
                    } else {
                        match c {
                            b'{' => depth = 1,
                            b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')' | b'~' | b'+' | b'&'
                            | b'-' => break,
                            _ => {}
                        }
                    }
                    pos += 1;
                }
                if depth > 0 || quote.is_some() {
                    return None;
                }
                tokens.push(Token::Atom(&text[start..pos]));
            }
        }
    }
    Some(tokens)
}

struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<Token<'a>> {
        self.tokens.get(self.pos).copied()
    }

    /// Operators from loosest to tightest: `-`, `&`, `+`, juxtaposition.
    fn binary(&mut self, level: Op) -> Option<Expr> {
        let tighter = match level {
            Op::Difference => Some(Op::Intersection),
            Op::Intersection => Some(Op::Union),
            Op::Union => Some(Op::ImpliedUnion),
            Op::ImpliedUnion => None,
        };
        let operand = |parser: &mut Self| match tighter {
            Some(tighter) => parser.binary(tighter),
            None => parser.unary(),
        };
        let mut left = operand(self)?;
        loop {
            match (level, self.peek()) {
                (Op::ImpliedUnion, Some(Token::Open | Token::Tilde | Token::Atom(_))) => {}
                (_, Some(Token::Binary(op))) if op == level => self.pos += 1,
                _ => return Some(left),
            }
            let right = operand(self)?;
            left = Expr::op(level, left, right);
        }
    }

    fn unary(&mut self) -> Option<Expr> {
        let token = self.peek()?;
        self.pos += 1;
        match token {
            Token::Tilde => Some(Expr::complement(self.unary()?)),
            Token::Open => {
                let inner = self.binary(Op::Difference)?;
                (self.peek() == Some(Token::Close)).then(|| {
                    self.pos += 1;
                    inner
                })
            }
            Token::Atom(text) if text.starts_with('%') => {
                parse_reference(text).map(Expr::Reference)
            }
            Token::Atom(text) => parse_pattern(text).map(Expr::Pattern),
            Token::Close | Token::Binary(_) => None,
        }
    }
}

/// Parses path expression text; `None` for text this parser does not
/// understand, which composition then leaves as authored.
fn parse(text: &str) -> Option<Expr> {
    let tokens = tokenize(text)?;
    if tokens.is_empty() {
        return Some(Expr::Nothing);
    }
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.binary(Op::Difference)?;
    (parser.pos == parser.tokens.len()).then_some(expr)
}

/// Splices `weaker` into every `%_` of `stronger`.
///
/// OpenUSD: `SdfPathExpression::ComposeOver`.
fn compose_over(stronger: Expr, weaker: &Expr) -> Expr {
    stronger.rebuild(&mut |atom| match atom {
        Expr::Reference(reference) if reference.is_weaker() => weaker.clone(),
        atom => atom,
    })
}

/// One arc's map from its target's namespace to the namespace it is
/// authored in: `source` maps to `target`, and with `root_identity`, every
/// other path maps to itself.
///
/// OpenUSD: `PcpMapFunction`.
#[derive(Clone, Debug)]
struct ArcMap {
    source: Vec<String>,
    target: Vec<String>,
    root_identity: bool,
}

impl ArcMap {
    /// Maps an absolute prim path; `None` outside the map's domain.
    ///
    /// The most specific pair applies; a result that falls under a more
    /// specific pair's target is blocked, since that pair's inverse would
    /// not map it back. OpenUSD: `_Map` in `pxr/usd/pcp/mapFunction.cpp`.
    fn map(&self, path: &[String]) -> Option<Vec<String>> {
        if path.starts_with(&self.source) {
            let mut mapped = self.target.clone();
            mapped.extend_from_slice(&path[self.source.len()..]);
            return Some(mapped);
        }
        if !self.root_identity {
            return None;
        }
        // The root identity matched, with no elements: the pair's target is
        // more specific.
        (!path.starts_with(&self.target) || self.target.is_empty()).then(|| path.to_vec())
    }
}

/// Maps an absolute prim path through `maps`, innermost arc first.
fn map_path(maps: &[ArcMap], path: &[String]) -> Option<Vec<String>> {
    let mut path = path.to_vec();
    for map in maps {
        path = map.map(&path)?;
    }
    Some(path)
}

/// Anchors `expr` at the prim `anchor` and maps it into the stage
/// namespace through `maps`, innermost arc first.
///
/// A pattern whose prefix is the absolute root and has more after it (`//`,
/// `//Name`) is kept as is; any other pattern, and any reference path,
/// outside the maps' domain matches nothing.
///
/// OpenUSD: `SdfPathExpression::MakeAbsolute` and
/// `PcpMapFunction::MapSourceToTarget` (`_MapPathExpressionImpl` in
/// `pxr/usd/pcp/mapFunction.cpp`).
fn anchor_and_map(expr: Expr, anchor: &[String], maps: &[ArcMap]) -> Expr {
    expr.rebuild(&mut |atom| match atom {
        Expr::Pattern(pattern) => {
            let Some(mut pattern) = pattern.anchored(anchor) else {
                return Expr::Nothing;
            };
            // A leading stretch (`//...`) matches anywhere and maps to
            // itself; any other prefix, the root included, maps through
            // the arcs. OpenUSD: `HasLeadingStretch`.
            if pattern.prims.is_empty()
                && pattern.property.is_none()
                && pattern.rest.starts_with('/')
            {
                return Expr::Pattern(pattern);
            }
            match map_path(maps, &pattern.prims) {
                Some(prims) => {
                    pattern.prims = prims;
                    Expr::Pattern(pattern)
                }
                None => Expr::Nothing,
            }
        }
        Expr::Reference(mut reference) => {
            let Some(path) = reference.path.take() else {
                return Expr::Reference(reference);
            };
            let prims = if path.absolute {
                Some(path.prims)
            } else {
                anchor_names(anchor, path.prims)
            };
            match prims.and_then(|prims| map_path(maps, &prims)) {
                Some(prims) => {
                    reference.path = Some(RefPath {
                        absolute: true,
                        prims,
                    });
                    Expr::Reference(reference)
                }
                None => Expr::Nothing,
            }
        }
        atom => atom,
    })
}

/// A node's site prim, which anchors its relative expressions, and the
/// maps from its namespace to the stage namespace.
type NodeNamespace = (Vec<String>, Vec<ArcMap>);

/// The prim names of `site`, without its variant selections.
fn prim_names(store: &dyn LayerStore, site: &SpecPath) -> Vec<String> {
    site.components()
        .iter()
        .filter_map(|component| match component {
            SpecComponent::Prim(name) => Some(store.tokens().resolve(*name).to_owned()),
            SpecComponent::VariantSelection { .. } => None,
        })
        .collect()
}

/// The maps of the arcs from `node` up to the root of `graph`, innermost
/// first, for a composed prim at namespace depth `prim_depth`; `None` when
/// a site does not line up with the arc that reaches it.
///
/// Each arc maps the site it targets to the site it is authored at, at the
/// namespace depth it is authored on; a variant arc maps every path to
/// itself. Class arcs (inherits, specializes) and references or payloads
/// within one layer stack also map every other path to itself. OpenUSD:
/// `_CreateMapExpressionForArc` in `pxr/usd/pcp/primIndex.cpp`.
fn node_maps(
    store: &dyn LayerStore,
    graph: &PrimIndexGraph,
    node: NodeId,
    prim_depth: usize,
) -> Option<Vec<ArcMap>> {
    let mut maps = Vec::new();
    let mut cursor = graph.node(node)?;
    while let Some(parent_id) = cursor.parent() {
        let parent = graph.node(parent_id)?;
        if cursor.arc_kind() != ArcKind::Variants {
            let below = prim_depth.checked_sub(usize::from(cursor.namespace_depth()))?;
            let mut source = prim_names(store, cursor.site());
            let mut target = prim_names(store, parent.site());
            source.truncate(source.len().checked_sub(below)?);
            target.truncate(target.len().checked_sub(below)?);
            let root_identity =
                matches!(cursor.arc_kind(), ArcKind::Inherits | ArcKind::Specializes)
                    || cursor.layer_stack() == parent.layer_stack();
            maps.push(ArcMap {
                source,
                target,
                root_identity,
            });
        }
        cursor = parent;
    }
    Some(maps)
}

/// Anchors and maps one authored value, or each element of an array of
/// path expressions; `None` when nothing changes.
fn anchor_value(value: &Value, anchor: &[String], maps: &[ArcMap]) -> Option<Value> {
    match value {
        Value::PathExpression(text) => {
            let text = anchor_and_map(parse(text)?, anchor, maps).text();
            Some(Value::PathExpression(Arc::from(text)))
        }
        Value::Array(items) if items.iter().any(|v| matches!(v, Value::PathExpression(_))) => {
            let items = items
                .iter()
                .map(|item| anchor_value(item, anchor, maps).unwrap_or_else(|| item.clone()))
                .collect();
            Some(Value::Array(items))
        }
        _ => None,
    }
}

/// Anchors every path expression an opinion of `prims` authors at the prim
/// that authors it, and maps it through the arcs into the stage namespace,
/// so value resolution composes them in one namespace.
///
/// Spec: AOUSD Core §10 (composition arcs map namespace). OpenUSD:
/// `PcpMapFunction::MapSourceToTarget(SdfPathExpression)`, applied to each
/// node's opinions by `UsdStage` value resolution.
pub(crate) fn anchor_opinions(store: &dyn LayerStore, prims: &mut HashMap<PathId, PrimIndex>) {
    for (path, index) in prims.iter_mut() {
        let prim_depth = store.paths().resolve(*path).depth();
        let graph = &index.graph;
        let mut maps: HashMap<NodeId, Option<NodeNamespace>> = HashMap::new();
        for opinions in index.opinions_by_field.values_mut() {
            for opinion in opinions.iter_mut() {
                // The default, and each time sample, of a path expression.
                let authored: Vec<&mut Value> = match &mut opinion.value {
                    OpinionValue::Field(FieldValue::Value(value)) => Vec::from([value]),
                    OpinionValue::Property(spec) => spec
                        .default
                        .iter_mut()
                        .chain(spec.time_samples.iter_mut().flatten().map(|(_, v)| v))
                        .collect(),
                    OpinionValue::Field(_) => Vec::new(),
                };
                let mut authored = authored
                    .into_iter()
                    .filter(|value| matches!(value, Value::PathExpression(_) | Value::Array(_)))
                    .peekable();
                if authored.peek().is_none() {
                    continue;
                }
                let node = opinion.key.node;
                let Some((anchor, node_maps)) = maps
                    .entry(node)
                    .or_insert_with(|| {
                        let anchor = prim_names(store, graph.node(node)?.site());
                        Some((anchor, node_maps(store, graph, node, prim_depth)?))
                    })
                    .as_ref()
                else {
                    continue;
                };
                for value in authored {
                    if let Some(anchored) = anchor_value(value, anchor, node_maps) {
                        *value = anchored;
                    }
                }
            }
        }
    }
}

/// The values a path expression query reads, strongest first: each
/// answering opinion's position (`None` for a schema fallback) and its
/// value, `None` for a block in effect.
pub(crate) type ChainValues<'v> = Vec<(Option<usize>, Option<Cow<'v, Value>>)>;

/// How a chain of path expressions composed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fold {
    /// The composed expression; `None` when a block resolved the query to
    /// no value.
    pub(crate) value: Option<Value>,
    /// The positions of the values that contributed, strongest first:
    /// the strongest one and each one a `%_` spliced in.
    pub(crate) contributors: Vec<Option<usize>>,
    /// Where the fold ended before its `%_` were all filled, when it did:
    /// at a block, or at a value that is not a path expression.
    pub(crate) stopped_at: Option<(Option<usize>, Stop)>,
}

/// Why a [`Fold`] ended early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Block,
    Incompatible,
}

/// Composes a path expression query over `values` (see [`ChainValues`]):
/// each `%_` of the strongest expression splices in the next weaker one,
/// until none is left, and a `%_` that nothing fills matches nothing.
///
/// A `%_` that reaches a block ends the chain. At a numeric time (`at_time`)
/// the `%_` then matches nothing; at the default time the query resolves to
/// no value, as in OpenUSD 26.08.
///
/// Returns `None` when the strongest value is not a path expression: the
/// query is not a path expression query.
///
/// Spec: AOUSD Core §12.3 (attribute value resolution). OpenUSD:
/// `SdfPathExpression::ComposeOver`, applied by `UsdStage` value
/// resolution.
pub(crate) fn fold(values: ChainValues<'_>, at_time: bool) -> Option<Fold> {
    let mut values = values.into_iter();
    let (position, strongest) = values.next()?;
    let Some(Value::PathExpression(text)) = strongest.as_deref() else {
        return None;
    };
    let mut fold = Fold {
        value: None,
        contributors: Vec::from([position]),
        stopped_at: None,
    };
    let Some(mut expr) = parse(text) else {
        fold.value = Some(Value::PathExpression(text.clone()));
        return Some(fold);
    };
    while expr.has_weaker_reference() {
        let Some((position, weaker)) = values.next() else {
            break;
        };
        let Some(weaker) = weaker else {
            fold.stopped_at = Some((position, Stop::Block));
            if !at_time {
                return Some(fold);
            }
            break;
        };
        let Some(weaker) = (match weaker.as_ref() {
            Value::PathExpression(text) => parse(text),
            _ => None,
        }) else {
            fold.stopped_at = Some((position, Stop::Incompatible));
            break;
        };
        fold.contributors.push(position);
        expr = compose_over(expr, &weaker);
    }
    let expr = compose_over(expr, &Expr::Nothing);
    fold.value = Some(Value::PathExpression(Arc::from(expr.text())));
    Some(fold)
}

/// Composes the default-time value of `opinions` over `fallback` when it
/// is a path expression (see [`fold`]); `None` otherwise.
pub(crate) fn fold_default(opinions: &[Opinion], fallback: Option<&Value>) -> Option<Fold> {
    let strongest = opinions
        .iter()
        .find_map(|opinion| opinion.value.default_value())
        .or(fallback)?;
    if !matches!(strongest, Value::PathExpression(_)) {
        return None;
    }
    let values = opinions
        .iter()
        .enumerate()
        .filter_map(|(position, opinion)| Some((position, opinion.value.default_value()?)))
        .map(|(position, value)| {
            let value = (*value != Value::Blocked).then_some(Cow::Borrowed(value));
            (Some(position), value)
        })
        .chain(fallback.map(|value| (None, Some(Cow::Borrowed(value)))))
        .collect();
    fold(values, false)
}

/// Composes the value of `opinions` at stage time `time` over `fallback`
/// when it is a path expression (see [`fold`]); `None` otherwise.
///
/// Each opinion answers with its time samples, else its spline, else its
/// default, as for any other value.
///
/// Spec: AOUSD Core §12.3.2.
pub(crate) fn fold_at_time(
    opinions: &[Opinion],
    time: f64,
    interp: InterpolationType,
    fallback: Option<&Value>,
) -> Option<Fold> {
    // Decide from the strongest answering opinion's authored kind, without
    // sampling or cloning anything, whether this is a path expression query.
    let strongest_is_expression = opinions
        .iter()
        .find_map(answers_with_expression)
        .unwrap_or(matches!(fallback, Some(Value::PathExpression(_))));
    if !strongest_is_expression {
        return None;
    }
    let values = opinions
        .iter()
        .enumerate()
        .filter_map(|(position, opinion)| {
            Some((
                Some(position),
                value_at_time(opinion, time, interp)?.map(Cow::Owned),
            ))
        })
        .chain(fallback.map(|value| (None, Some(Cow::Borrowed(value)))))
        .collect();
    fold(values, true)
}

/// Whether `opinion` answers a numeric-time query with a path expression:
/// `None` when it authors no time samples, spline or default, so a weaker
/// opinion answers. Looks at the authored values only; nothing is sampled.
fn answers_with_expression(opinion: &Opinion) -> Option<bool> {
    let is_expression = |value: &Value| matches!(value, Value::PathExpression(_));
    if let Some(samples) = opinion.value.time_samples() {
        return Some(samples.iter().any(|(_, value)| is_expression(value)));
    }
    if opinion.value.spline().is_some() {
        return Some(false);
    }
    opinion.value.default_value().map(is_expression)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(text: &str) -> String {
        parse(text).expect("parses").text()
    }

    fn names(path: &str) -> Vec<String> {
        path.split('/')
            .filter(|n| !n.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    /// Text is written as `SdfPathExpression::GetText` writes it (checked
    /// against OpenUSD 26.08).
    #[test]
    fn text_matches_openusd() {
        for (text, expected) in [
            ("/A /B", "/A /B"),
            ("/A    /B", "/A /B"),
            ("/A+/B", "/A + /B"),
            ("/A&/B", "/A & /B"),
            ("/A-/B", "/A - /B"),
            ("~ /A", "~/A"),
            ("(/A /B) & /C", "/A /B & /C"),
            ("/A (/B & /C)", "/A (/B & /C)"),
            ("((/A))", "/A"),
            ("(/A - /B) - /C", "/A - /B - /C"),
            ("/A - (/B - /C)", "/A - (/B - /C)"),
            ("~(/A /B)", "~(/A /B)"),
            ("~(~/A)", "/A"),
            ("(/A + /B) & /C", "/A + /B & /C"),
            ("(/A - /B) & /C", "(/A - /B) & /C"),
            ("/A - (/B + /C)", "/A - /B + /C"),
            ("(/A + /B) /C", "(/A + /B) /C"),
            ("/A & (/B /C)", "/A & /B /C"),
            ("/A + (/B + /C)", "/A + (/B + /C)"),
            ("%_ /A", "%_ /A"),
            ("%:heroes /B", "%:heroes /B"),
            ("%/Sets:heroes", "%/Sets:heroes"),
            (".//", ".//"),
            ("../B", "../B"),
            ("/World//*{isa:\"Mesh\"}", "/World//*{isa:\"Mesh\"}"),
            ("/A/B.x:y*", "/A/B.x:y*"),
            ("", ""),
        ] {
            assert_eq!(round_trip(text), expected, "{text:?}");
        }
    }

    /// `%_` splices in the weaker expression, and resolves to nothing once
    /// no weaker expression is left.
    #[test]
    fn weaker_references_compose_as_in_openusd() {
        let compose = |strong: &str, weak: &str| {
            compose_over(parse(strong).unwrap(), &parse(weak).unwrap()).text()
        };
        assert_eq!(compose("/A %_", "/B /C"), "/A (/B /C)");
        assert_eq!(compose("/A & %_", "/B /C"), "/A & /B /C");
        assert_eq!(compose("/A - %_", "/B - /C"), "/A - (/B - /C)");
        assert_eq!(compose("/A %_", ""), "/A");
        assert_eq!(compose("/A %_", "%_ /D"), "/A (%_ /D)");
        assert_eq!(compose("%_", "/D"), "/D");
        assert_eq!(compose("%_ - %_", ""), "");
        assert_eq!(compose("~%_", ""), "//");
        assert_eq!(compose("/X", "/B"), "/X");
    }

    /// Relative patterns anchor at the authoring prim; patterns and
    /// reference paths map through the arcs, and drop out of the domain of
    /// a map without a root identity (checked against OpenUSD 26.08).
    #[test]
    fn anchoring_and_mapping_match_openusd() {
        let reference = [ArcMap {
            source: names("/Gear"),
            target: names("/Part"),
            root_identity: false,
        }];
        let map = |text: &str, anchor: &str| {
            anchor_and_map(parse(text).unwrap(), &names(anchor), &reference).text()
        };
        assert_eq!(map(".//", "/Gear"), "/Part//");
        assert_eq!(map(".", "/Gear"), "/Part");
        assert_eq!(
            map("Tip .. ../Hub", "/Gear/Tooth"),
            "/Part/Tooth/Tip /Part /Part/Hub"
        );
        assert_eq!(map("/Gear/Tooth //Tooth", "/Gear"), "/Part/Tooth //Tooth");
        assert_eq!(
            map("/Gear/Tooth /Elsewhere ../Elsewhere", "/Gear"),
            "/Part/Tooth"
        );
        assert_eq!(map("/Elsewhere", "/Gear"), "");
        assert_eq!(map("/", "/Gear"), "");
        assert_eq!(map("//", "/Gear"), "//");
        assert_eq!(map("~/Elsewhere", "/Gear"), "//");
        assert_eq!(map("/Elsewhere - /Gear/T", "/Gear"), "");
        assert_eq!(map("/Gear/T - /Elsewhere", "/Gear"), "/Part/T");
        assert_eq!(map("~/Elsewhere - /Gear/T", "/Gear"), "~/Part/T");
        assert_eq!(
            map("/Gear/T.x:y /Gear.size", "/Gear"),
            "/Part/T.x:y /Part.size"
        );
        assert_eq!(map("/Gear/T* /Gear//T", "/Gear"), "/Part/T* /Part//T");
        assert_eq!(map("%/Gear:x %/Else:y %:z %_", "/Gear"), "%/Part:x %:z %_");

        let internal = [ArcMap {
            source: names("/Local"),
            target: names("/Copy"),
            root_identity: true,
        }];
        let map =
            |text: &str| anchor_and_map(parse(text).unwrap(), &names("/Local"), &internal).text();
        assert_eq!(
            map("/Local/Pin /Stray Pin ../Stray /Copy/X"),
            "/Copy/Pin /Stray /Copy/Pin /Stray"
        );
        assert_eq!(map("/"), "/");
    }

    /// Character classes, root globs and relative references authored on
    /// `/Model`, mapped across an external reference and an internal one
    /// to `/Copy` (checked against OpenUSD 26.08 `MakeAbsolute` and
    /// `PcpMapFunction::MapSourceToTarget`).
    #[test]
    fn globs_and_references_map_as_in_openusd() {
        let external = [ArcMap {
            source: names("/Model"),
            target: names("/Copy"),
            root_identity: false,
        }];
        let internal = [ArcMap {
            root_identity: true,
            ..external[0].clone()
        }];
        let map = |text: &str, maps: &[ArcMap]| {
            anchor_and_map(parse(text).unwrap(), &names("/Model"), maps).text()
        };
        for (text, over_external, over_internal) in [
            ("/Model/A[a-z]", "/Copy/A[a-z]", "/Copy/A[a-z]"),
            ("/Model/A[!a-z]", "/Copy/A[!a-z]", "/Copy/A[!a-z]"),
            (
                "/Model/A[a-z0-9_]*",
                "/Copy/A[a-z0-9_]*",
                "/Copy/A[a-z0-9_]*",
            ),
            ("A[a-z]", "/Copy/A[a-z]", "/Copy/A[a-z]"),
            (
                "A[-a] A[a-]",
                "/Copy/A[-a] /Copy/A[a-]",
                "/Copy/A[-a] /Copy/A[a-]",
            ),
            (
                "/Model/A[a-z] - /Model/B",
                "/Copy/A[a-z] - /Copy/B",
                "/Copy/A[a-z] - /Copy/B",
            ),
            (
                "/Model/A? /Model/B*",
                "/Copy/A? /Copy/B*",
                "/Copy/A? /Copy/B*",
            ),
            ("/A*", "", "/A*"),
            ("/*", "", "/*"),
            ("/A*/B", "", "/A*/B"),
            ("//A*", "//A*", "//A*"),
            ("//", "//", "//"),
            ("*", "/Copy/*", "/Copy/*"),
            ("../*", "", "/*"),
            ("..//A", "//A", "//A"),
            ("../Other//", "", "/Other//"),
            ("/Model/A*//B", "/Copy/A*//B", "/Copy/A*//B"),
            ("%../Model:foo", "%/Copy:foo", "%/Copy:foo"),
            ("%..:foo", "", "%/:foo"),
            ("%:foo", "%:foo", "%:foo"),
            ("%/Model/Sub:foo", "%/Copy/Sub:foo", "%/Copy/Sub:foo"),
            ("%/Other:foo", "", "%/Other:foo"),
        ] {
            assert_eq!(map(text, &external), over_external, "{text:?} external");
            assert_eq!(map(text, &internal), over_internal, "{text:?} internal");
        }
    }

    /// Only a chain whose strongest answering opinion authors a path
    /// expression takes the expression fold; any other chain, a dense array
    /// or a spline over a weaker expression included, is left to ordinary
    /// resolution.
    #[test]
    fn only_expression_chains_fold() {
        use crate::{
            doc::LayerOffset,
            interner::TokenInterner,
            path::PathInterner,
            property::PropertySpec,
            spline::{CurveType, Extrapolation, SplineData, SplineDataType},
        };
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let spec_path = SpecPath::parse("/A", &mut tokens, &mut paths).expect("spec path");
        let lookup_path = paths.intern(crate::path::Path::root());
        let field = tokens.intern("x");
        let opinion = |strength: u16, spec: PropertySpec| Opinion {
            key: crate::prim_index::OpinionKey {
                node: NodeId::ROOT,
                layer_strength: strength,
                layer_id: crate::doc::LayerId(1),
                lookup_path,
                spec_path: spec_path.clone(),
            },
            field,
            value: spec.into(),
            layer_offset: LayerOffset::IDENTITY,
        };
        let expression = |text: &str| Value::PathExpression(text.into());
        let weaker = opinion(1, PropertySpec::attribute().with_default(expression("/W")));
        let chain = |strongest: PropertySpec| [opinion(0, strongest), weaker.clone()];
        let at_time = |opinions: &[Opinion]| {
            fold_at_time(opinions, 1.0, InterpolationType::Held, None).and_then(|fold| fold.value)
        };

        let array =
            chain(PropertySpec::attribute().with_default(Value::Array(Vec::from([Value::Int(1)]))));
        assert_eq!(fold_default(&array, None), None);
        assert_eq!(at_time(&array), None);
        let spline = chain(PropertySpec::attribute().with_spline(SplineData {
            data_type: SplineDataType::Double,
            default_curve_type: CurveType::Bezier,
            pre_extrapolation: Extrapolation::Held,
            post_extrapolation: Extrapolation::Held,
            loop_params: None,
            knots: Vec::new(),
        }));
        assert_eq!(at_time(&spline), None);

        let sampled = chain(
            PropertySpec::attribute().with_time_samples(Vec::from([(0.0, expression("/S %_"))])),
        );
        assert_eq!(at_time(&sampled), Some(expression("/S /W")));
        // No opinion answers: the schema fallback decides.
        assert_eq!(
            fold_at_time(&[], 1.0, InterpolationType::Held, Some(&expression("/F")))
                .and_then(|fold| fold.value),
            Some(expression("/F"))
        );
    }

    #[test]
    fn unsupported_text_is_not_parsed() {
        assert_eq!(parse("/A\n/B"), None);
        assert_eq!(parse("(/A"), None);
        assert_eq!(parse("/A{isa:Mesh"), None);
        assert_eq!(parse("/A[a-z"), None);
        assert_eq!(parse("/A -"), None);
        assert_eq!(parse("%Sub:foo"), None);
        assert!(parse("/A /B").is_some());
    }
}
