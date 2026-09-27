// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The path expression grammar, ported rule for rule from OpenUSD's PEG
//! grammars (`pxr/usd/sdf/pathExpression.cpp`, `pathPatternParser.h`,
//! `predicateExpressionParser.h`), including where they commit (a rule
//! that fails after committing fails the whole parse) and where they
//! backtrack.
//!
//! Each rule returns `Ok(Some(_))` when it matches, `Ok(None)` when it does
//! not (and the caller backtracks), and `Err` when the parse fails.

use alloc::{borrow::ToOwned, boxed::Box, string::String, vec::Vec};
use core::fmt;

use super::{
    ArgValue, CallKind, ExpressionReference, PathExpression, PathPattern, PredicateArg,
    PredicateCall, PredicateExpression, PredicateOp, Prefix, RefPath, SetOp,
};

/// Why path expression text did not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The byte offset where parsing failed.
    pub offset: usize,
    /// What was expected there.
    pub message: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ill-formed path expression at byte {}: {}",
            self.offset, self.message
        )
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ParseError {}

type Rule<T> = Result<Option<T>, ParseError>;

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

fn is_blank(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// PEGTL's `identifier_other`: ASCII letters, digits and `_`.
fn is_ident_other(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

const RESERVED: [&str; 6] = ["not", "and", "or", "inf", "true", "false"];

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn error(&self, message: &'static str) -> ParseError {
        ParseError {
            offset: self.pos,
            message,
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, s: &str) -> bool {
        if self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    /// `star<blank>`; whether any were skipped.
    fn blanks(&mut self) -> bool {
        let start = self.pos;
        while self.peek().is_some_and(is_blank) {
            self.pos += 1;
        }
        self.pos > start
    }

    /// PEGTL's `keyword`: the word, not followed by `identifier_other`.
    fn keyword(&mut self, word: &str) -> bool {
        if self.rest().starts_with(word) && !self.peek_at(word.len()).is_some_and(is_ident_other) {
            self.pos += word.len();
            true
        } else {
            false
        }
    }

    /// PEGTL's `identifier`: an ASCII letter or `_`, then `identifier_other`.
    fn identifier(&mut self) -> Option<&'a str> {
        let start = self.pos;
        if !self
            .peek()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        {
            return None;
        }
        self.pos += 1;
        while self.peek().is_some_and(is_ident_other) {
            self.pos += 1;
        }
        Some(&self.text[start..self.pos])
    }

    /// An identifier that is not a reserved word (`PredFuncName`,
    /// `PredKWArgName`).
    fn name(&mut self) -> Option<&'a str> {
        let start = self.pos;
        match self.identifier() {
            Some(name) if !RESERVED.contains(&name) => Some(name),
            _ => {
                self.pos = start;
                None
            }
        }
    }

    // ── Path expressions ─────────────────────────────────────────────

    /// `PathExpr`: factors joined by explicit operators or blanks, reduced
    /// by precedence (`Sdf_PathExprBuilder`).
    fn path_expr(&mut self) -> Rule<PathExpression> {
        let Some(first) = self.path_factor()? else {
            return Ok(None);
        };
        let mut operands = Vec::from([first]);
        let mut ops: Vec<SetOp> = Vec::new();
        loop {
            let save = self.pos;
            // `PathExprStep`: an explicit operator commits to a factor.
            self.blanks();
            let op = match self.peek() {
                Some(b'+') => Some(SetOp::Union),
                Some(b'&') => Some(SetOp::Intersection),
                Some(b'-') => Some(SetOp::Difference),
                _ => None,
            };
            if let Some(op) = op {
                self.pos += 1;
                self.blanks();
                let Some(factor) = self.path_factor()? else {
                    return Err(self.error("expected path expression after operator"));
                };
                push_op(&mut operands, &mut ops, op);
                operands.push(factor);
                continue;
            }
            // `ImpliedUnionStep`: blanks, when a factor follows.
            self.pos = save;
            if self.blanks() {
                let after_blanks = self.pos;
                if let Some(factor) = self.lookahead(Self::path_factor)? {
                    let _ = factor;
                    self.pos = after_blanks;
                    let factor = self
                        .path_factor()?
                        .ok_or_else(|| self.error("expected path expression"))?;
                    push_op(&mut operands, &mut ops, SetOp::ImpliedUnion);
                    operands.push(factor);
                    continue;
                }
            }
            self.pos = save;
            break;
        }
        while let Some(op) = ops.pop() {
            reduce(&mut operands, op);
        }
        Ok(operands.pop())
    }

    /// Runs `rule` and rewinds, as PEGTL's `at` does.
    fn lookahead<T>(&mut self, rule: impl FnOnce(&mut Self) -> Rule<T>) -> Rule<T> {
        let save = self.pos;
        let result = rule(self);
        self.pos = save;
        result
    }

    /// `PathFactor`: an optional `~`, then an atom.
    fn path_factor(&mut self) -> Rule<PathExpression> {
        let save = self.pos;
        self.blanks();
        let complement = self.eat(b'~');
        if complement {
            self.blanks();
        } else {
            self.pos = save;
        }
        let Some(atom) = self.path_atom()? else {
            self.pos = save;
            return Ok(None);
        };
        Ok(Some(if complement {
            PathExpression::complement(atom)
        } else {
            atom
        }))
    }

    /// `PathExprAtom`: a reference, a pattern or a parenthesized group.
    fn path_atom(&mut self) -> Rule<PathExpression> {
        if let Some(reference) = self.expression_reference()? {
            return Ok(Some(PathExpression::reference(reference)));
        }
        if let Some(pattern) = self.path_pattern()? {
            return Ok(Some(PathExpression::pattern(pattern)));
        }
        if self.eat(b'(') {
            self.blanks();
            let Some(inner) = self.path_expr()? else {
                return Err(self.error("expected path expression after '('"));
            };
            self.blanks();
            if !self.eat(b')') {
                return Err(self.error("expected ')' to close expression group"));
            }
            return Ok(Some(inner));
        }
        Ok(None)
    }

    /// `ExpressionReference`: `%_`, or `%` and a path and name.
    fn expression_reference(&mut self) -> Rule<ExpressionReference> {
        if self.peek() != Some(b'%') {
            return Ok(None);
        }
        // `WeakerRef`: `%_` not followed by `identifier_other` or `:`.
        if self.rest().starts_with("%_")
            && !self
                .peek_at(2)
                .is_some_and(|b| is_ident_other(b) || b == b':')
        {
            self.pos += 2;
            return Ok(Some(ExpressionReference::weaker()));
        }
        self.pos += 1;
        let start = self.pos;
        if self.eat(b'/') {
            // `AbsExpressionRefPath`.
            self.ref_path_and_name()?;
        } else {
            // `RelExpressionRefPath`: `..` segments, then a path or a name.
            self.dot_dots(|_| {});
            if self.eat(b'/') {
                self.ref_path_and_name()?;
            } else {
                self.ref_name()?;
            }
        }
        let text = &self.text[start..self.pos];
        let (path, name) = text.rsplit_once(':').unwrap_or(("", text));
        let path = if path.is_empty() {
            None
        } else if let Some(names) = path.strip_prefix('/') {
            Some(RefPath {
                absolute: true,
                prims: names.split('/').map(ToOwned::to_owned).collect(),
            })
        } else {
            Some(RefPath {
                absolute: false,
                prims: path.split('/').map(ToOwned::to_owned).collect(),
            })
        };
        Ok(Some(ExpressionReference {
            path,
            name: name.to_owned(),
        }))
    }

    /// `ExpressionRefPathAndName`: identifiers separated by `/`, then
    /// `:name`.
    fn ref_path_and_name(&mut self) -> Result<(), ParseError> {
        if self.identifier().is_none() {
            return Err(self.error("expected expression reference path after '/'"));
        }
        while self.eat(b'/') {
            if self.identifier().is_none() {
                return Err(self.error("expected identifier"));
            }
        }
        self.ref_name()
    }

    /// `ExpressionRefName`: `:` and an identifier.
    fn ref_name(&mut self) -> Result<(), ParseError> {
        if !self.eat(b':') || self.identifier().is_none() {
            return Err(self.error("expected identifier"));
        }
        Ok(())
    }

    /// `DotDots`: `..` then more `/..`; calls `each` per `..`.
    fn dot_dots(&mut self, mut each: impl FnMut(&mut Self)) -> bool {
        if !self.eat_str("..") {
            return false;
        }
        each(self);
        loop {
            let save = self.pos;
            if self.eat(b'/') && self.eat_str("..") {
                each(self);
            } else {
                self.pos = save;
                return true;
            }
        }
    }

    // ── Path patterns ────────────────────────────────────────────────

    /// `PathPattern`.
    fn path_pattern(&mut self) -> Rule<PathPattern> {
        let mut pattern = PathPattern {
            prefix: Prefix::default(),
            components: Vec::new(),
            is_property: false,
        };
        if self.peek() == Some(b'/') {
            // `AbsPathPattern` commits: a separator, then elements.
            pattern.prefix.absolute = true;
            if self.eat_str("//") {
                pattern.append_child(String::new(), None);
            } else {
                self.pos += 1;
            }
            self.pattern_elems(&mut pattern)?;
            return Ok(Some(pattern));
        }
        if self.dot_dots(|_| pattern.append_child("..".into(), None)) {
            if self.eat_str("//") {
                // `DotDotsStretchTail`.
                pattern.append_child(String::new(), None);
                self.pattern_elems(&mut pattern)?;
            } else if self.peek() == Some(b'/') && self.peek_at(1) != Some(b'/') {
                // `DotDotsStep` commits to elements.
                self.pos += 1;
                if self.pattern_elems(&mut pattern)?.is_none() {
                    return Err(self.error("expected path pattern element after '/'"));
                }
            }
            return Ok(Some(pattern));
        }
        if self.pattern_elems(&mut pattern)?.is_some() {
            return Ok(Some(pattern));
        }
        if self.eat(b'.') {
            // `ReflexiveRelative`, optionally `//` and elements.
            if self.eat_str("//") {
                pattern.append_child(String::new(), None);
                self.pattern_elems(&mut pattern)?;
            }
            return Ok(Some(pattern));
        }
        Ok(None)
    }

    /// `PathPatternElems`: prim elements joined by `/` or `//`, then a
    /// property element or a trailing `//`.
    fn pattern_elems(&mut self, pattern: &mut PathPattern) -> Rule<()> {
        let Some((text, predicate)) = self.pattern_elem(false)? else {
            return Ok(None);
        };
        pattern.append_child(text, predicate);
        loop {
            // `StretchStep`: `//`, when an element follows.
            if self.rest().starts_with("//") {
                let found = self.lookahead(|p| {
                    p.pos += 2;
                    p.pattern_elem(false)
                })?;
                if found.is_some() {
                    self.pos += 2;
                    pattern.append_child(String::new(), None);
                    let (text, predicate) = self
                        .pattern_elem(false)?
                        .ok_or_else(|| self.error("expected path pattern element"))?;
                    pattern.append_child(text, predicate);
                    continue;
                }
            }
            // `PrimPatStep`: a single `/` commits to an element.
            if self.peek() == Some(b'/') && self.peek_at(1) != Some(b'/') {
                self.pos += 1;
                let Some((text, predicate)) = self.pattern_elem(false)? else {
                    return Err(self.error("expected path pattern element"));
                };
                pattern.append_child(text, predicate);
                continue;
            }
            break;
        }
        if self.eat(b'.') {
            let Some((text, predicate)) = self.pattern_elem(true)? else {
                return Err(self.error("expected property pattern element after '.'"));
            };
            pattern.append_property(text, predicate);
        } else if self.eat_str("//") {
            pattern.append_child(String::new(), None);
        }
        Ok(Some(()))
    }

    /// `PrimPathPatternElem` or `PropPathPatternElem`: glob text, then an
    /// optional predicate; or a predicate alone.
    fn pattern_elem(&mut self, property: bool) -> Rule<(String, Option<PredicateExpression>)> {
        let start = self.pos;
        loop {
            match self.peek() {
                Some(b'[') => {
                    // `BracketClass` commits to its content and `]`.
                    self.pos += 1;
                    let content = self.pos;
                    while self.peek().is_some_and(|b| {
                        is_ident_other(b) || matches!(b, b'!' | b'-' | b'?' | b'*')
                    }) {
                        self.pos += 1;
                    }
                    if self.pos == content || !self.eat(b']') {
                        return Err(self.error("expected ']' to close bracket class"));
                    }
                }
                Some(b)
                    if is_ident_other(b) || b == b'?' || b == b'*' || (property && b == b':') =>
                {
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let text = self.text[start..self.pos].to_owned();
        let predicate = self.braced_predicate()?;
        if text.is_empty() && predicate.is_none() {
            return Ok(None);
        }
        Ok(Some((text, predicate)))
    }

    /// `BracedPredExpr`: `{` commits to a predicate expression and `}`.
    fn braced_predicate(&mut self) -> Rule<PredicateExpression> {
        if !self.eat(b'{') {
            return Ok(None);
        }
        self.blanks();
        let Some(predicate) = self.pred_expr()? else {
            return Err(self.error("expected predicate expression"));
        };
        self.blanks();
        if !self.eat(b'}') {
            return Err(self.error("expected '}' to close predicate expression"));
        }
        Ok(Some(predicate))
    }

    // ── Predicate expressions ────────────────────────────────────────

    /// `PredExpr`: factors joined by `and`, `or` or blanks, reduced by
    /// precedence (`SdfPredicateExprBuilder`).
    fn pred_expr(&mut self) -> Rule<PredicateExpression> {
        let Some(first) = self.pred_factor()? else {
            return Ok(None);
        };
        let mut operands = Vec::from([first]);
        let mut ops: Vec<PredicateOp> = Vec::new();
        loop {
            let save = self.pos;
            // `PredExprStep`: `and` or `or` commits to a factor.
            self.blanks();
            let op = if self.keyword("and") {
                Some(PredicateOp::And)
            } else if self.keyword("or") {
                Some(PredicateOp::Or)
            } else {
                None
            };
            if let Some(op) = op {
                self.blanks();
                let Some(factor) = self.pred_factor()? else {
                    return Err(self.error("expected predicate expression after operator"));
                };
                push_pred_op(&mut operands, &mut ops, op);
                operands.push(factor);
                continue;
            }
            // `ImpliedAndStep`: blanks, when a factor follows.
            self.pos = save;
            if self.blanks() {
                let after_blanks = self.pos;
                if self.lookahead(Self::pred_factor)?.is_some() {
                    self.pos = after_blanks;
                    let factor = self
                        .pred_factor()?
                        .ok_or_else(|| self.error("expected predicate expression"))?;
                    push_pred_op(&mut operands, &mut ops, PredicateOp::ImpliedAnd);
                    operands.push(factor);
                    continue;
                }
            }
            self.pos = save;
            break;
        }
        while let Some(op) = ops.pop() {
            reduce_pred(&mut operands, op);
        }
        Ok(operands.pop())
    }

    /// `PredFactor`: any number of `not`, then an atom.
    fn pred_factor(&mut self) -> Rule<PredicateExpression> {
        let save = self.pos;
        let mut nots = 0;
        self.blanks();
        if self.keyword("not") {
            nots = 1;
            loop {
                let before = self.pos;
                if self.blanks() && self.keyword("not") {
                    nots += 1;
                } else {
                    self.pos = before;
                    break;
                }
            }
            self.blanks();
        } else {
            self.pos = save;
        }
        let Some(mut atom) = self.pred_atom()? else {
            self.pos = save;
            return Ok(None);
        };
        for _ in 0..nots {
            atom = PredicateExpression::Not(Box::new(atom));
        }
        Ok(Some(atom))
    }

    /// `PredAtom`: a colon, paren or bare call, or a parenthesized group.
    fn pred_atom(&mut self) -> Rule<PredicateExpression> {
        let start = self.pos;
        if let Some(name) = self.name() {
            if self.eat(b':') {
                // `PredColonCall` commits to its arguments.
                let args = self.colon_args()?;
                return Ok(Some(call(CallKind::Colon, name, args)));
            }
            let after_name = self.pos;
            self.blanks();
            if self.eat(b'(') {
                // `PredParenCall` commits to its arguments and `)`.
                self.blanks();
                let args = self.paren_args()?;
                self.blanks();
                if !self.eat(b')') {
                    return Err(self.error("expected ')' to close function call"));
                }
                return Ok(Some(call(CallKind::Paren, name, args)));
            }
            self.pos = after_name;
            return Ok(Some(call(CallKind::Bare, name, Vec::new())));
        }
        self.pos = start;
        if self.eat(b'(') {
            self.blanks();
            let Some(inner) = self.pred_expr()? else {
                return Err(self.error("expected predicate expression after '('"));
            };
            self.blanks();
            if !self.eat(b')') {
                return Err(self.error("expected ')' to close predicate expression group"));
            }
            return Ok(Some(inner));
        }
        Ok(None)
    }

    /// `PredColonArgs`: values separated by `,`.
    fn colon_args(&mut self) -> Result<Vec<PredicateArg>, ParseError> {
        let mut args = Vec::new();
        let Some(value) = self.arg_value()? else {
            return Err(self.error("expected argument list after ':'"));
        };
        args.push(positional(value));
        while self.eat(b',') {
            let Some(value) = self.arg_value()? else {
                return Err(self.error("expected argument value"));
            };
            args.push(positional(value));
        }
        Ok(args)
    }

    /// `OptSpacedComma`.
    fn comma(&mut self) -> bool {
        let save = self.pos;
        self.blanks();
        if self.eat(b',') {
            self.blanks();
            true
        } else {
            self.pos = save;
            false
        }
    }

    /// `PredKWArgPrefix`: a name and `=`.
    fn keyword_prefix(&mut self) -> Option<&'a str> {
        let save = self.pos;
        let name = self.name()?;
        self.blanks();
        if self.eat(b'=') {
            self.blanks();
            Some(name)
        } else {
            self.pos = save;
            None
        }
    }

    /// `PredParenArgs`: positional arguments, then keyword arguments.
    fn paren_args(&mut self) -> Result<Vec<PredicateArg>, ParseError> {
        let mut args = Vec::new();
        // `list<PredParenPosArg, OptSpacedComma>`.
        loop {
            let save = self.pos;
            if !args.is_empty() && !self.comma() {
                break;
            }
            let value = if self.lookahead(|p| Ok(p.keyword_prefix()))?.is_some() {
                None
            } else {
                self.arg_value()?
            };
            match value {
                Some(value) => args.push(positional(value)),
                None => {
                    self.pos = save;
                    break;
                }
            }
        }
        // Keyword arguments: after positional ones, a comma commits to
        // them.
        let committed = !args.is_empty() && self.comma();
        if !args.is_empty() && !committed {
            return Ok(args);
        }
        let save = self.pos;
        match self.keyword_arg()? {
            Some(arg) => args.push(arg),
            None if committed => return Err(self.error("expected keyword argument after ','")),
            None => {
                self.pos = save;
                return Ok(args);
            }
        }
        while self.comma() {
            let Some(arg) = self.keyword_arg()? else {
                return Err(self.error("expected keyword argument after ','"));
            };
            args.push(arg);
        }
        Ok(args)
    }

    /// `PredKWArg`: a name and `=` commit to a value.
    fn keyword_arg(&mut self) -> Rule<PredicateArg> {
        let Some(name) = self.keyword_prefix() else {
            return Ok(None);
        };
        let Some(value) = self.arg_value()? else {
            return Err(self.error("expected argument value after '='"));
        };
        Ok(Some(PredicateArg {
            name: Some(name.to_owned()),
            value,
        }))
    }

    /// `PredArgVal`: a float, an integer, a boolean or a string.
    fn arg_value(&mut self) -> Rule<ArgValue> {
        let start = self.pos;
        if let Some(value) = self.arg_float()? {
            return Ok(Some(ArgValue::Float(value)));
        }
        self.pos = start;
        if let Some(value) = self.arg_int() {
            return Ok(Some(ArgValue::Int(value)));
        }
        self.pos = start;
        if self.keyword("true") {
            return Ok(Some(ArgValue::Bool(true)));
        }
        if self.keyword("false") {
            return Ok(Some(ArgValue::Bool(false)));
        }
        self.arg_string().map(|s| s.map(ArgValue::String))
    }

    fn digits(&mut self) -> bool {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        self.pos > start
    }

    /// `PredArgFloat`.
    fn arg_float(&mut self) -> Rule<f64> {
        let start = self.pos;
        let negative = self.eat(b'-');
        if self.keyword("inf") {
            return Ok(Some(if negative {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }));
        }
        if !self.digits() {
            return Ok(None);
        }
        let exponent = |p: &mut Self| -> Result<bool, ParseError> {
            if !matches!(p.peek(), Some(b'e' | b'E')) {
                return Ok(false);
            }
            p.pos += 1;
            if matches!(p.peek(), Some(b'-' | b'+')) {
                p.pos += 1;
            }
            if !p.digits() {
                return Err(p.error("expected digits"));
            }
            Ok(true)
        };
        if self.eat(b'.') {
            // `Frac` commits to digits.
            if !self.digits() {
                return Err(self.error("expected digits"));
            }
            exponent(self)?;
        } else if !exponent(self)? {
            return Ok(None);
        }
        Ok(self.text[start..self.pos].parse().ok())
    }

    /// `PredArgInt`, when it fits an `i64`.
    fn arg_int(&mut self) -> Option<i64> {
        let start = self.pos;
        self.eat(b'-');
        if !self.digits() {
            return None;
        }
        self.text[start..self.pos].parse().ok()
    }

    /// `PredArgString`: quoted, or a run of unquoted characters (possibly
    /// empty).
    fn arg_string(&mut self) -> Rule<String> {
        if let Some(quote @ (b'"' | b'\'')) = self.peek() {
            self.pos += 1;
            let start = self.pos;
            loop {
                let Some(c) = self.rest().chars().next() else {
                    return Err(self.error("expected closing quote"));
                };
                if c == char::from(quote) {
                    break;
                }
                if c == '\\' {
                    self.pos += 1;
                    let escaped = self.peek();
                    if !escaped.is_some_and(|b| {
                        b == quote || matches!(b, b'\\' | b'b' | b'f' | b'n' | b'r' | b't')
                    }) {
                        return Err(self.error("expected escape character after '\\'"));
                    }
                    self.pos += 1;
                } else if u32::from(c) < 0x20 {
                    return Err(self.error("expected closing quote"));
                } else {
                    self.pos += c.len_utf8();
                }
            }
            let body = &self.text[start..self.pos];
            self.pos += 1;
            return Ok(Some(unescape(body)));
        }
        let start = self.pos;
        while self.peek().is_some_and(|b| {
            is_ident_other(b)
                || matches!(
                    b,
                    b'~' | b'!'
                        | b'@'
                        | b'#'
                        | b'$'
                        | b'%'
                        | b'^'
                        | b'&'
                        | b'*'
                        | b'-'
                        | b'+'
                        | b'='
                        | b'|'
                        | b'\\'
                        | b'.'
                        | b'?'
                        | b'/'
                )
        }) {
            self.pos += 1;
        }
        Ok(Some(unescape(&self.text[start..self.pos])))
    }
}

fn positional(value: ArgValue) -> PredicateArg {
    PredicateArg { name: None, value }
}

fn call(kind: CallKind, name: &str, args: Vec<PredicateArg>) -> PredicateExpression {
    PredicateExpression::Call(PredicateCall {
        kind,
        name: name.to_owned(),
        args,
    })
}

/// Processes backslash escapes as `TfEscapeStringReplaceChar` does.
fn unescape(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 == bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'x' => {
                let mut n: u8 = 0;
                let mut digits = 0;
                while digits < 2 && bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit) {
                    i += 1;
                    let digit =
                        u8::try_from(char::from(bytes[i]).to_digit(16).unwrap_or(0)).unwrap_or(0);
                    n = n.wrapping_mul(16).wrapping_add(digit);
                    digits += 1;
                }
                out.push(n);
            }
            b'0'..=b'7' => {
                let mut n: u8 = 0;
                let mut digits = 0;
                i -= 1;
                while digits < 3 && bytes.get(i + 1).is_some_and(|b| (b'0'..=b'7').contains(b)) {
                    i += 1;
                    n = n.wrapping_mul(8).wrapping_add(bytes[i] - b'0');
                    digits += 1;
                }
                out.push(n);
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Pushes `op`, first reducing the pending operators that bind at least as
/// tightly (left associativity).
fn push_op(operands: &mut Vec<PathExpression>, ops: &mut Vec<SetOp>, op: SetOp) {
    while ops.last().is_some_and(|top| *top <= op) {
        let top = ops.pop().unwrap_or(op);
        reduce(operands, top);
    }
    ops.push(op);
}

fn reduce(operands: &mut Vec<PathExpression>, op: SetOp) {
    let right = operands.pop().unwrap_or_default();
    let left = operands.pop().unwrap_or_default();
    operands.push(PathExpression::op(op, left, right));
}

fn push_pred_op(
    operands: &mut Vec<PredicateExpression>,
    ops: &mut Vec<PredicateOp>,
    op: PredicateOp,
) {
    while ops.last().is_some_and(|top| *top <= op) {
        let top = ops.pop().unwrap_or(op);
        reduce_pred(operands, top);
    }
    ops.push(op);
}

fn reduce_pred(operands: &mut Vec<PredicateExpression>, op: PredicateOp) {
    let (Some(right), Some(left)) = (operands.pop(), operands.pop()) else {
        return;
    };
    operands.push(PredicateExpression::Op(op, Box::new(left), Box::new(right)));
}

/// Parses path expression text (see [`PathExpression::parse`]).
pub(super) fn parse_expression(text: &str) -> Result<PathExpression, ParseError> {
    if text.is_empty() {
        return Ok(PathExpression::nothing());
    }
    let mut parser = Parser::new(text);
    parser.blanks();
    let Some(expression) = parser.path_expr()? else {
        return Err(parser.error("expected path expression"));
    };
    parser.blanks();
    // `eolf`: the end, or a line end, after which OpenUSD stops reading.
    if parser.peek().is_some() && !parser.eat(b'\n') && !parser.eat_str("\r\n") {
        return Err(parser.error("expected end of path expression"));
    }
    Ok(expression)
}
