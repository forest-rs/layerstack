// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Variable expressions.
//!
//! A variable expression is a string enclosed in backticks, such as
//! `` `"./${ASSET}.usd"` `` or `` `if(${HIGH}, "high", "low")` ``, that is
//! evaluated against a set of [`ExpressionVariables`]. Asset paths of
//! sublayers, references and payloads, and variant selections, may be
//! expressions; composition evaluates them with the `expressionVariables`
//! of the layer stack that authors them.
//!
//! The language has:
//!
//! - literals: strings in double or single quotes, 64-bit integers, `True`
//!   and `False` (or `true`, `false`), and `None` (or `none`);
//! - variables: `${NAME}` on its own, and substituted into a quoted string
//!   (`"${NAME}_suffix"`), where a variable whose value is a string
//!   expression is evaluated in turn;
//! - lists of scalars of one type: `["a", "b"]`, `[1, 2]`;
//! - the functions `if`, `eq`, `neq`, `lt`, `leq`, `gt`, `geq`, `and`, `or`,
//!   `not`, `contains`, `at`, `len` and `defined`.
//!
//! `matches_regex` parses but does not evaluate: it reports an error.
//!
//! Spec: AOUSD Core §7.6.1.7 reserves the `expressionVariables` layer field
//! as out of scope. This follows OpenUSD's `SdfVariableExpression`
//! (`pxr/usd/sdf/variableExpression.h`, its grammar in
//! `variableExpressionParser.cpp` and its evaluation in
//! `variableExpressionImpl.cpp`), including its error messages.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::cmp::Ordering;

/// Returns `true` when `s` is a variable expression: longer than two
/// characters, starting and ending with a backtick.
///
/// A `true` result does not mean the expression is valid.
///
/// OpenUSD: `SdfVariableExpression::IsExpression`.
#[must_use]
pub fn is_expression(s: &str) -> bool {
    s.len() > 2 && s.starts_with('`') && s.ends_with('`')
}

/// A value an expression evaluates to, or a variable holds.
///
/// A list holds scalars of one type; an empty list has no element type
/// ([`ExpressionValue::EmptyList`]). "No value" (`None`) is the absence of
/// an `ExpressionValue`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ExpressionValue {
    /// A string.
    String(String),
    /// A 64-bit integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// A non-empty list of strings.
    StringList(Vec<String>),
    /// A non-empty list of integers.
    IntList(Vec<i64>),
    /// A non-empty list of booleans.
    BoolList(Vec<bool>),
    /// An empty list.
    EmptyList,
}

impl ExpressionValue {
    /// The type name used in error messages: `string`, `int`, `bool` or
    /// `list`.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::String(_) => "string",
            Self::Int(_) => "int",
            Self::Bool(_) => "bool",
            Self::StringList(_) | Self::IntList(_) | Self::BoolList(_) | Self::EmptyList => "list",
        }
    }

    /// The value as a string, if it is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Whether `self` and `other` have the same type, lists of different
    /// element types (and the empty list) being different types.
    fn same_type(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }
}

/// The type name of an optional value, `None` for no value.
fn type_name(value: Option<&ExpressionValue>) -> &'static str {
    value.map_or("None", ExpressionValue::type_name)
}

/// The value of one expression variable.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum VariableValue {
    /// No value (`None`).
    None,
    /// A value of a supported type. A string that is itself an expression is
    /// evaluated where the variable is used.
    Value(ExpressionValue),
    /// A value of a type expressions do not support, by type name. Using the
    /// variable is an error.
    Unsupported(String),
}

/// A set of expression variables: values by name.
///
/// OpenUSD passes a `VtDictionary` to `SdfVariableExpression::Evaluate`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ExpressionVariables {
    values: BTreeMap<String, VariableValue>,
}

impl ExpressionVariables {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the value of `name`, replacing any previous value.
    pub fn insert(&mut self, name: impl Into<String>, value: VariableValue) {
        self.values.insert(name.into(), value);
    }

    /// Returns the value of `name`, if it is set.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&VariableValue> {
        self.values.get(name)
    }

    /// Returns `true` when no variable is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns the variables, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &VariableValue)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Adds every variable of `weaker` that `self` does not set: `self`
    /// composed over `weaker`.
    ///
    /// OpenUSD: `VtDictionaryOver`.
    pub fn compose_over(&mut self, weaker: &Self) {
        for (name, value) in &weaker.values {
            self.values
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
    }
}

/// The result of evaluating a [`VariableExpression`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evaluation {
    /// The value, or `None` when the expression yields no value or an
    /// error occurred.
    pub value: Option<ExpressionValue>,
    /// The errors found while parsing or evaluating, in order; empty on
    /// success.
    pub errors: Vec<String>,
    /// Every variable the evaluation looked up, including those of
    /// variables whose values are expressions and those `defined` tests.
    pub used_variables: BTreeSet<String>,
}

impl Evaluation {
    /// The value as a string: `Ok(None)` for no value, an error for a value
    /// of another type or a failed evaluation (the errors, joined).
    ///
    /// Composition takes asset paths and variant selections this way.
    /// OpenUSD: `SdfVariableExpression::EvaluateTyped<std::string>` and
    /// `Pcp_EvaluateVariableExpression`, which joins the errors with `"; "`.
    pub fn into_string(self) -> Result<Option<String>, String> {
        match self.value {
            _ if !self.errors.is_empty() => Err(self.errors.join("; ")),
            None => Ok(None),
            Some(ExpressionValue::String(s)) => Ok(Some(s)),
            Some(other) => Err(format!(
                "Expression evaluated to '{}' but expected 'string'",
                other.type_name()
            )),
        }
    }
}

/// A parsed variable expression.
///
/// Parsing never fails outright: an expression that does not parse is kept
/// with its errors ([`VariableExpression::errors`]) and evaluates to them.
///
/// OpenUSD: `SdfVariableExpression`.
#[derive(Clone, Debug)]
pub struct VariableExpression {
    source: String,
    root: Result<Node, Vec<String>>,
}

impl VariableExpression {
    /// Parses `expression`, including its enclosing backticks.
    #[must_use]
    pub fn parse(expression: &str) -> Self {
        Self {
            source: expression.to_string(),
            root: Parser::new(expression).expression(),
        }
    }

    /// The expression as given to [`VariableExpression::parse`].
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Returns `true` when the expression parsed. A valid expression may
    /// still fail to evaluate.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.root.is_ok()
    }

    /// The errors found while parsing; empty for a valid expression.
    #[must_use]
    pub fn errors(&self) -> &[String] {
        match &self.root {
            Ok(_) => &[],
            Err(errors) => errors,
        }
    }

    /// Evaluates the expression with `variables`.
    #[must_use]
    pub fn evaluate(&self, variables: &ExpressionVariables) -> Evaluation {
        let root = match &self.root {
            Ok(root) => root,
            Err(errors) => {
                return Evaluation {
                    errors: errors.clone(),
                    ..Evaluation::default()
                };
            }
        };
        let mut context = Context {
            variables,
            stack: Vec::new(),
            used: BTreeSet::new(),
        };
        let (value, errors) = match root.evaluate(&mut context) {
            Ok(value) => (value, Vec::new()),
            Err(errors) => (None, errors),
        };
        Evaluation {
            value,
            errors,
            used_variables: context.used,
        }
    }
}

// ── Syntax tree ─────────────────────────────────────────────────────────

/// One literal or variable run of a quoted string.
#[derive(Clone, Debug)]
enum Part {
    Text(String),
    Variable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comparison {
    Eq,
    Neq,
    Lt,
    Leq,
    Gt,
    Geq,
}

impl Comparison {
    fn name(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Neq => "neq",
            Self::Lt => "lt",
            Self::Leq => "leq",
            Self::Gt => "gt",
            Self::Geq => "geq",
        }
    }

    fn holds(self, ordering: Ordering) -> bool {
        match self {
            Self::Eq => ordering == Ordering::Equal,
            Self::Neq => ordering != Ordering::Equal,
            Self::Lt => ordering == Ordering::Less,
            Self::Leq => ordering != Ordering::Greater,
            Self::Gt => ordering == Ordering::Greater,
            Self::Geq => ordering != Ordering::Less,
        }
    }
}

#[derive(Clone, Debug)]
enum Node {
    String(Vec<Part>),
    Variable(String),
    Int(i64),
    Bool(bool),
    None,
    List(Vec<Self>),
    If(Box<Self>, Box<Self>, Option<Box<Self>>),
    Compare(Comparison, Box<Self>, Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
    Contains(Box<Self>, Box<Self>),
    MatchesRegex(Box<Self>, Box<Self>),
    At(Box<Self>, Box<Self>),
    Len(Box<Self>),
    Defined(Vec<Self>),
}

// ── Parser ──────────────────────────────────────────────────────────────

/// A parse error: its message and the byte offset it was found at.
type ParseError = (String, usize);

/// A function call as parsed, before its name and arity are checked.
#[derive(Clone, Debug)]
enum Syntax {
    Node(Node),
    List(Vec<Self>),
    Call(String, Vec<Self>),
}

/// A recursive-descent port of OpenUSD's PEG grammar
/// (`variableExpressionParser.cpp`). A rule that does not match returns
/// `Ok(None)` and consumes nothing; a rule that commits (`if_must`) returns
/// an error instead.
struct Parser<'a> {
    input: &'a [u8],
    source: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            input: source.as_bytes(),
            source,
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn at(&self, s: &str) -> bool {
        self.input[self.pos..].starts_with(s.as_bytes())
    }

    fn eat(&mut self, s: &str) -> bool {
        let found = self.at(s);
        if found {
            self.pos += s.len();
        }
        found
    }

    fn spaces(&mut self) {
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
    }

    /// `` '`' body '`' ``; anything after the closing backtick is ignored,
    /// as OpenUSD's grammar does not require the end of input.
    fn expression(mut self) -> Result<Node, Vec<String>> {
        let parsed = (|| {
            if !self.eat("`") {
                return Err(("Expressions must begin with '`'".into(), self.pos));
            }
            let body = self
                .body()?
                .ok_or_else(|| ("Unexpected expression".into(), self.pos))?;
            if !self.eat("`") {
                return Err(("Missing ending '`'".into(), self.pos));
            }
            Ok(body)
        })();
        let syntax =
            parsed.map_err(|(message, at)| vec![format!("{message} at character {at}")])?;
        build(syntax).map_err(|error| vec![error])
    }

    /// A scalar or a list.
    fn body(&mut self) -> Result<Option<Syntax>, ParseError> {
        if let Some(scalar) = self.scalar()? {
            return Ok(Some(scalar));
        }
        self.list()
    }

    fn scalar(&mut self) -> Result<Option<Syntax>, ParseError> {
        if let Some(name) = self.variable()? {
            return Ok(Some(Syntax::Node(Node::Variable(name))));
        }
        for quote in *b"\"'" {
            if let Some(parts) = self.quoted(quote)? {
                return Ok(Some(Syntax::Node(Node::String(parts))));
            }
        }
        if let Some(value) = self.integer()? {
            return Ok(Some(Syntax::Node(Node::Int(value))));
        }
        for (word, value) in [
            ("True", true),
            ("true", true),
            ("False", false),
            ("false", false),
        ] {
            if self.keyword(word) {
                return Ok(Some(Syntax::Node(Node::Bool(value))));
            }
        }
        if self.keyword("None") || self.keyword("none") {
            return Ok(Some(Syntax::Node(Node::None)));
        }
        self.function()
    }

    /// `${NAME}`: once `${` matches, the name and `}` must follow.
    fn variable(&mut self) -> Result<Option<String>, ParseError> {
        if !self.eat("${") {
            return Ok(None);
        }
        let name = self
            .identifier()
            .ok_or_else(|| ("Variables must be a C identifier".into(), self.pos))?;
        if !self.eat("}") {
            return Err(("Missing ending '}'".into(), self.pos));
        }
        Ok(Some(name))
    }

    /// A C identifier: `[A-Za-z_][A-Za-z0-9_]*`.
    fn identifier(&mut self) -> Option<String> {
        let start = self.pos;
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => self.pos += 1,
            _ => return None,
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.pos += 1;
        }
        Some(self.source[start..self.pos].to_string())
    }

    /// `word`, not followed by an identifier character.
    fn keyword(&mut self, word: &str) -> bool {
        let follows = self.input.get(self.pos + word.len()).copied();
        if self.at(word) && !follows.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
            self.pos += word.len();
            true
        } else {
            false
        }
    }

    /// `-?[0-9]+`, as a 64-bit integer.
    fn integer(&mut self) -> Result<Option<i64>, ParseError> {
        let start = self.pos;
        let digits = start + usize::from(self.peek() == Some(b'-'));
        let end = digits
            + self.input[digits..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
        if end == digits {
            return Ok(None);
        }
        self.pos = end;
        self.source[start..end]
            .parse::<i64>()
            .map(Some)
            .map_err(|_| {
                let literal = &self.source[start..end];
                (format!("Integer {literal} out of range."), start)
            })
    }

    /// A string in `quote`s: runs of text, with `\` escaping a backtick,
    /// `$`, `\` or the quote, and `${NAME}` substitutions. Once the opening
    /// quote matches, the closing one must follow.
    fn quoted(&mut self, quote: u8) -> Result<Option<Vec<Part>>, ParseError> {
        if self.peek() != Some(quote) {
            return Ok(None);
        }
        self.pos += 1;
        let mut parts = Vec::new();
        loop {
            if let Some(name) = self.variable()? {
                parts.push(Part::Variable(name));
                continue;
            }
            let start = self.pos;
            while let Some(c) = self.peek() {
                let escaped = self.input.get(self.pos + 1).copied();
                if c == b'\\'
                    && matches!(escaped, Some(e) if e == b'`' || e == b'$' || e == b'\\' || e == quote)
                {
                    self.pos += 2;
                } else if c == quote || self.at("${") {
                    break;
                } else {
                    self.pos += 1;
                }
            }
            if self.pos == start {
                break;
            }
            parts.push(Part::Text(unescape(&self.input[start..self.pos])));
        }
        if self.peek() != Some(quote) {
            return Err((
                if quote == b'"' {
                    "Missing ending '\"'".into()
                } else {
                    "Missing ending \"'\"".into()
                },
                self.pos,
            ));
        }
        self.pos += 1;
        Ok(Some(parts))
    }

    /// `name(` arguments `)`, spaces allowed around `(`, `,` and `)`. Once
    /// `name(` matches, the closing `)` must follow.
    fn function(&mut self) -> Result<Option<Syntax>, ParseError> {
        let start = self.pos;
        let Some(name) = self.identifier() else {
            return Ok(None);
        };
        self.spaces();
        if !self.eat("(") {
            self.pos = start;
            return Ok(None);
        }
        self.spaces();
        let args = self.separated(Self::body)?;
        self.spaces();
        if !self.eat(")") {
            return Err(("Missing ending ')'".into(), self.pos));
        }
        self.spaces();
        Ok(Some(Syntax::Call(name, args)))
    }

    /// `[` scalars `]`. Once `[` matches, the closing `]` must follow.
    fn list(&mut self) -> Result<Option<Syntax>, ParseError> {
        if !self.eat("[") {
            return Ok(None);
        }
        let elements = self.separated(Self::scalar)?;
        if elements.is_empty() {
            self.spaces();
        }
        if !self.eat("]") {
            return Err(("Missing ending ']'".into(), self.pos));
        }
        Ok(Some(Syntax::List(elements)))
    }

    /// `item (' '* ',' ' '* item)*`, or nothing.
    fn separated(
        &mut self,
        mut item: impl FnMut(&mut Self) -> Result<Option<Syntax>, ParseError>,
    ) -> Result<Vec<Syntax>, ParseError> {
        let mut items = Vec::new();
        let Some(first) = item(self)? else {
            return Ok(items);
        };
        items.push(first);
        loop {
            let before = self.pos;
            self.spaces();
            if !self.eat(",") {
                self.pos = before;
                break;
            }
            self.spaces();
            match item(self)? {
                Some(next) => items.push(next),
                None => {
                    self.pos = before;
                    break;
                }
            }
        }
        Ok(items)
    }
}

/// Replaces the C escapes of `text` (`\n`, `\t`, `\x41`, `\101`, ...) and
/// drops the backslash of any other escaped character.
///
/// OpenUSD: `TfEscapeString`.
fn unescape(text: &[u8]) -> String {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        i += 1;
        if c != b'\\' || i == text.len() {
            out.push(c);
            continue;
        }
        let e = text[i];
        i += 1;
        match e {
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
                while digits < 2 && i < text.len() && text[i].is_ascii_hexdigit() {
                    n = n.wrapping_mul(16).wrapping_add(hex_value(text[i]));
                    i += 1;
                    digits += 1;
                }
                out.push(n);
            }
            b'0'..=b'7' => {
                let mut n: u8 = e - b'0';
                let mut digits = 1;
                while digits < 3 && i < text.len() && (b'0'..=b'7').contains(&text[i]) {
                    n = n.wrapping_mul(8).wrapping_add(text[i] - b'0');
                    i += 1;
                    digits += 1;
                }
                out.push(n);
            }
            other => out.push(other),
        }
    }
    String::from_utf8(out)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

fn hex_value(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => c - b'A' + 10,
    }
}

/// Builds the tree, checking each call's function name and arity.
///
/// A fixed-arity function checks its arity before its arguments, a
/// variadic one its minimum; OpenUSD reports the first such error
/// (`FunctionNodeCreator` in `variableExpressionParser.cpp`).
fn build(syntax: Syntax) -> Result<Node, String> {
    let (name, args) = match syntax {
        Syntax::Node(node) => return Ok(node),
        Syntax::List(elements) => {
            let built: Vec<Result<Node, String>> = elements.into_iter().map(build).collect();
            if let Some(error) = built
                .iter()
                .rev()
                .find_map(|element| element.as_ref().err())
            {
                return Err(error.clone());
            }
            return Ok(Node::List(built.into_iter().flatten().collect()));
        }
        Syntax::Call(name, args) => (name, args),
    };
    let fixed: &[usize] = match name.as_str() {
        "if" => &[2, 3],
        "eq" | "neq" | "lt" | "leq" | "gt" | "geq" | "contains" | "matches_regex" | "at" => &[2],
        "not" | "len" => &[1],
        "and" | "or" => &[],
        "defined" => &[],
        _ => return Err(format!("Unknown function {name}")),
    };
    let minimum = match name.as_str() {
        "and" | "or" => Some(2),
        "defined" => Some(1),
        _ => None,
    };
    match minimum {
        Some(min) if args.len() < min => {
            return Err(format!(
                "Function '{name}' requires at least {min} arguments."
            ));
        }
        None if !fixed.contains(&args.len()) => {
            return Err(format!(
                "Function '{name}' does not take {} arguments.",
                args.len()
            ));
        }
        _ => {}
    }
    // Every argument is built; the last error wins.
    let built: Vec<Result<Node, String>> = args.into_iter().map(build).collect();
    if let Some(error) = built.iter().rev().find_map(|arg| arg.as_ref().err()) {
        return Err(error.clone());
    }
    let mut args: Vec<Node> = built.into_iter().flatten().collect();
    let mut take = || Box::new(args.remove(0));
    Ok(match name.as_str() {
        "if" => {
            let (condition, value) = (take(), take());
            let other = (!args.is_empty()).then(|| Box::new(args.remove(0)));
            Node::If(condition, value, other)
        }
        "eq" => Node::Compare(Comparison::Eq, take(), take()),
        "neq" => Node::Compare(Comparison::Neq, take(), take()),
        "lt" => Node::Compare(Comparison::Lt, take(), take()),
        "leq" => Node::Compare(Comparison::Leq, take(), take()),
        "gt" => Node::Compare(Comparison::Gt, take(), take()),
        "geq" => Node::Compare(Comparison::Geq, take(), take()),
        "contains" => Node::Contains(take(), take()),
        "matches_regex" => Node::MatchesRegex(take(), take()),
        "at" => Node::At(take(), take()),
        "not" => Node::Not(take()),
        "len" => Node::Len(take()),
        "and" => Node::And(args),
        "or" => Node::Or(args),
        _ => Node::Defined(args),
    })
}

// ── Evaluation ──────────────────────────────────────────────────────────

/// The result of evaluating a node: a value or no value, or errors.
type Eval = Result<Option<ExpressionValue>, Vec<String>>;

struct Context<'v> {
    variables: &'v ExpressionVariables,
    /// The variables whose expression values are being evaluated.
    stack: Vec<String>,
    used: BTreeSet<String>,
}

impl Context<'_> {
    /// The value of `name`: `None` when it is not set, otherwise its value,
    /// evaluating a string that is an expression.
    ///
    /// OpenUSD: `EvalContext::GetVariable`.
    fn variable(&mut self, name: &str) -> Option<Eval> {
        if self.stack.iter().any(|entry| entry == name) {
            let chain: Vec<String> = self.stack.iter().map(|s| format!("'{s}'")).collect();
            return Some(Err(vec![format!(
                "Encountered circular variable substitutions: [{}, '{name}']",
                chain.join(", ")
            )]));
        }
        self.used.insert(name.to_string());
        let value = self.variables.get(name)?;
        Some(match value {
            VariableValue::None => Ok(None),
            VariableValue::Unsupported(type_name) => Err(vec![format!(
                "Variable '{name}' has unsupported type {type_name}"
            )]),
            VariableValue::Value(ExpressionValue::String(s)) if is_expression(s) => {
                let expression = VariableExpression::parse(s);
                match &expression.root {
                    Ok(root) => {
                        self.stack.push(name.to_string());
                        let result = root.evaluate(self);
                        self.stack.pop();
                        result
                    }
                    Err(errors) => Err(errors
                        .iter()
                        .map(|error| format!("{error} (in variable '{name}')"))
                        .collect()),
                }
            }
            VariableValue::Value(value) => Ok(Some(value.clone())),
        })
    }
}

/// Collects the errors of `results`, in order.
fn errors_of<'r>(results: impl IntoIterator<Item = &'r Eval>) -> Vec<String> {
    results
        .into_iter()
        .filter_map(|result| result.as_ref().err())
        .flatten()
        .cloned()
        .collect()
}

/// Evaluates two arguments, failing with the errors of both.
fn both(
    ctx: &mut Context<'_>,
    a: &Node,
    b: &Node,
) -> Result<[Option<ExpressionValue>; 2], Vec<String>> {
    let (a, b) = (a.evaluate(ctx), b.evaluate(ctx));
    match (a, b) {
        (Ok(a), Ok(b)) => Ok([a, b]),
        (a, b) => Err(errors_of([&a, &b])),
    }
}

fn function_error(name: &str, message: &str) -> Vec<String> {
    vec![format!("{name}: {message}")]
}

impl Node {
    fn evaluate(&self, ctx: &mut Context<'_>) -> Eval {
        match self {
            Self::Int(value) => Ok(Some(ExpressionValue::Int(*value))),
            Self::Bool(value) => Ok(Some(ExpressionValue::Bool(*value))),
            Self::None => Ok(None),
            Self::String(parts) => evaluate_string(ctx, parts),
            Self::Variable(name) => ctx
                .variable(name)
                .unwrap_or_else(|| Err(vec![format!("No value for variable '{name}'")])),
            Self::List(elements) => evaluate_list(ctx, elements),
            Self::If(condition, value, other) => {
                evaluate_if(ctx, condition, value, other.as_deref())
            }
            Self::Compare(op, a, b) => {
                let [a, b] = both(ctx, a, b)?;
                compare(*op, a.as_ref(), b.as_ref())
            }
            Self::And(args) => evaluate_logical(ctx, "and", args, false),
            Self::Or(args) => evaluate_logical(ctx, "or", args, true),
            Self::Not(arg) => match arg.evaluate(ctx)? {
                Some(ExpressionValue::Bool(value)) => Ok(Some(ExpressionValue::Bool(!value))),
                other => Err(function_error(
                    "not",
                    &format!("Invalid type {} for argument", type_name(other.as_ref())),
                )),
            },
            Self::Contains(search_in, search_for) => {
                let [search_in, search_for] = both(ctx, search_in, search_for)?;
                contains(search_in.as_ref(), search_for.as_ref())
            }
            Self::MatchesRegex(search_in, pattern) => {
                both(ctx, search_in, pattern)?;
                Err(function_error(
                    "matches_regex",
                    "regular expressions are not supported",
                ))
            }
            Self::At(source, index) => {
                let [source, index] = both(ctx, source, index)?;
                let Some(ExpressionValue::Int(index)) = index else {
                    return Err(function_error("at", "Index must be an integer"));
                };
                at(source.as_ref(), index)
            }
            Self::Len(source) => {
                let len = match source.evaluate(ctx)? {
                    Some(ExpressionValue::String(s)) => s.len(),
                    Some(ExpressionValue::StringList(l)) => l.len(),
                    Some(ExpressionValue::IntList(l)) => l.len(),
                    Some(ExpressionValue::BoolList(l)) => l.len(),
                    Some(ExpressionValue::EmptyList) => 0,
                    _ => return Err(function_error("len", "Unsupported type")),
                };
                Ok(Some(ExpressionValue::Int(
                    i64::try_from(len).unwrap_or(i64::MAX),
                )))
            }
            Self::Defined(args) => {
                let mut defined = true;
                let mut errors = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    match arg.evaluate(ctx) {
                        Err(e) => errors.extend(e),
                        Ok(Some(ExpressionValue::String(name))) => {
                            ctx.used.insert(name.clone());
                            defined &= ctx.variables.get(&name).is_some();
                        }
                        Ok(other) => errors.extend(function_error(
                            "defined",
                            &format!(
                                "Invalid type {} for argument {i}",
                                type_name(other.as_ref())
                            ),
                        )),
                    }
                }
                if errors.is_empty() {
                    Ok(Some(ExpressionValue::Bool(defined)))
                } else {
                    Err(errors)
                }
            }
        }
    }
}

/// Concatenates the parts of a quoted string. A variable that is not set is
/// replaced by its name; one with no value by nothing; one of another type
/// than string is an error.
///
/// OpenUSD: `StringNode::Evaluate`.
fn evaluate_string(ctx: &mut Context<'_>, parts: &[Part]) -> Eval {
    let mut out = String::new();
    for part in parts {
        match part {
            Part::Text(text) => out.push_str(text),
            Part::Variable(name) => match ctx.variable(name) {
                None => out.push_str(name),
                Some(Err(errors)) => return Err(errors),
                Some(Ok(None)) => {}
                Some(Ok(Some(ExpressionValue::String(s)))) => out.push_str(&s),
                Some(Ok(Some(other))) => {
                    return Err(vec![format!(
                        "String value required for substituting variable '{name}', got {}.",
                        other.type_name()
                    )]);
                }
            },
        }
    }
    Ok(Some(ExpressionValue::String(out)))
}

/// Evaluates a list; its elements must be scalars of one type.
///
/// OpenUSD: `ListNode::Evaluate`.
fn evaluate_list(ctx: &mut Context<'_>, elements: &[Node]) -> Eval {
    let mut list = ExpressionValue::EmptyList;
    let mut errors = Vec::new();
    for (i, element) in elements.iter().enumerate() {
        let value = match element.evaluate(ctx) {
            Ok(value) => value,
            Err(e) => {
                errors.extend(e);
                continue;
            }
        };
        let pushed = match (&mut list, value) {
            (ExpressionValue::EmptyList, Some(ExpressionValue::String(s))) => {
                list = ExpressionValue::StringList(vec![s]);
                Ok(())
            }
            (ExpressionValue::EmptyList, Some(ExpressionValue::Int(n))) => {
                list = ExpressionValue::IntList(vec![n]);
                Ok(())
            }
            (ExpressionValue::EmptyList, Some(ExpressionValue::Bool(b))) => {
                list = ExpressionValue::BoolList(vec![b]);
                Ok(())
            }
            (ExpressionValue::StringList(l), Some(ExpressionValue::String(s))) => {
                l.push(s);
                Ok(())
            }
            (ExpressionValue::IntList(l), Some(ExpressionValue::Int(n))) => {
                l.push(n);
                Ok(())
            }
            (ExpressionValue::BoolList(l), Some(ExpressionValue::Bool(b))) => {
                l.push(b);
                Ok(())
            }
            (_, other) => Err(type_name(other.as_ref())),
        };
        if let Err(found) = pushed {
            errors.push(format!(
                "Unexpected value of type {found} in list at element {i}"
            ));
        }
    }
    if errors.is_empty() {
        Ok(Some(list))
    } else {
        Err(errors)
    }
}

/// `if(condition, value)` and `if(condition, value, other)`: both values
/// are evaluated, and must have the same type unless one is `None`; the
/// chosen one's errors are the result's.
///
/// OpenUSD: `IfNode::_Evaluate`.
fn evaluate_if(
    ctx: &mut Context<'_>,
    condition: &Node,
    value: &Node,
    other: Option<&Node>,
) -> Eval {
    let Some(ExpressionValue::Bool(condition)) = condition.evaluate(ctx)? else {
        return Err(function_error("if", "Condition must be a boolean value"));
    };
    let value = value.evaluate(ctx);
    let other = other.map_or(Ok(None), |other| other.evaluate(ctx));
    if let (Ok(Some(a)), Ok(Some(b))) = (&value, &other)
        && !a.same_type(b)
    {
        return Err(function_error(
            "if",
            "if-value and else-value must evaluate to the same type or None.",
        ));
    }
    if condition { value } else { other }
}

/// `eq` and the other comparisons of two scalars of one type, or of two
/// `None`s for `eq` and `neq`.
///
/// OpenUSD: `ComparisonNode::Evaluate`.
fn compare(op: Comparison, a: Option<&ExpressionValue>, b: Option<&ExpressionValue>) -> Eval {
    let name = op.name();
    let ordering = match (a, b) {
        (None, None) => match op {
            Comparison::Eq => return Ok(Some(ExpressionValue::Bool(true))),
            Comparison::Neq => return Ok(Some(ExpressionValue::Bool(false))),
            _ => {
                return Err(function_error(
                    name,
                    "Comparison operation not supported for None",
                ));
            }
        },
        (Some(a), Some(b)) if a.same_type(b) => match (a, b) {
            (ExpressionValue::String(a), ExpressionValue::String(b)) => {
                a.as_bytes().cmp(b.as_bytes())
            }
            (ExpressionValue::Int(a), ExpressionValue::Int(b)) => a.cmp(b),
            (ExpressionValue::Bool(a), ExpressionValue::Bool(b)) => a.cmp(b),
            _ => return Err(function_error(name, "Unsupported type for comparison")),
        },
        (a, b) => {
            return Err(function_error(
                name,
                &format!(
                    "Cannot compare values of type {} and {}",
                    type_name(a),
                    type_name(b)
                ),
            ));
        }
    };
    Ok(Some(ExpressionValue::Bool(op.holds(ordering))))
}

/// `and` (`stop_at` false) and `or` (`stop_at` true): the arguments must
/// be booleans, and evaluation stops at the first equal to `stop_at`.
///
/// OpenUSD: `LogicalNode::Evaluate`.
fn evaluate_logical(ctx: &mut Context<'_>, name: &str, args: &[Node], stop_at: bool) -> Eval {
    let mut result = !stop_at;
    let mut errors = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        match arg.evaluate(ctx) {
            Err(e) => errors.extend(e),
            Ok(Some(ExpressionValue::Bool(value))) => {
                if value == stop_at {
                    result = value;
                    break;
                }
            }
            Ok(other) => errors.extend(function_error(
                name,
                &format!(
                    "Invalid type {} for argument {i}",
                    type_name(other.as_ref())
                ),
            )),
        }
    }
    if errors.is_empty() {
        Ok(Some(ExpressionValue::Bool(result)))
    } else {
        Err(errors)
    }
}

/// `contains(search_in, search_for)`: a substring of a string, or an
/// element of a list.
///
/// OpenUSD: `ContainsNode::Evaluate`.
fn contains(search_in: Option<&ExpressionValue>, search_for: Option<&ExpressionValue>) -> Eval {
    let invalid = || Err(function_error("contains", "Invalid search value"));
    let found = match (search_in, search_for) {
        (Some(ExpressionValue::EmptyList), _) => false,
        (Some(ExpressionValue::String(s)), Some(ExpressionValue::String(t))) => {
            s.contains(t.as_str())
        }
        (Some(ExpressionValue::StringList(l)), Some(ExpressionValue::String(t))) => l.contains(t),
        (Some(ExpressionValue::IntList(l)), Some(ExpressionValue::Int(n))) => l.contains(n),
        (Some(ExpressionValue::BoolList(l)), Some(ExpressionValue::Bool(b))) => l.contains(b),
        (
            Some(
                ExpressionValue::String(_)
                | ExpressionValue::StringList(_)
                | ExpressionValue::IntList(_)
                | ExpressionValue::BoolList(_),
            ),
            _,
        ) => return invalid(),
        _ => {
            return Err(function_error(
                "contains",
                "Value to search must be a list or string",
            ));
        }
    };
    Ok(Some(ExpressionValue::Bool(found)))
}

/// `at(source, index)`: the element of a list, or the byte of a string, at
/// `index`, counted from the end when negative.
///
/// OpenUSD: `AtNode::Evaluate`.
fn at(source: Option<&ExpressionValue>, index: i64) -> Eval {
    let out_of_range = || Err(function_error("at", "Index out of range"));
    let normalize = |len: usize| -> Option<usize> {
        let len = i64::try_from(len).ok()?;
        let index = if index < 0 { index + len } else { index };
        (0..len)
            .contains(&index)
            .then(|| usize::try_from(index).ok())?
    };
    let value = match source {
        Some(ExpressionValue::String(s)) => {
            let Some(i) = normalize(s.len()) else {
                return out_of_range();
            };
            ExpressionValue::String(String::from_utf8_lossy(&s.as_bytes()[i..=i]).into_owned())
        }
        Some(ExpressionValue::StringList(l)) => match normalize(l.len()) {
            Some(i) => ExpressionValue::String(l[i].clone()),
            None => return out_of_range(),
        },
        Some(ExpressionValue::IntList(l)) => match normalize(l.len()) {
            Some(i) => ExpressionValue::Int(l[i]),
            None => return out_of_range(),
        },
        Some(ExpressionValue::BoolList(l)) => match normalize(l.len()) {
            Some(i) => ExpressionValue::Bool(l[i]),
            None => return out_of_range(),
        },
        Some(ExpressionValue::EmptyList) => return out_of_range(),
        _ => return Err(function_error("at", "Only supported for lists or strings")),
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec::Vec};

    use super::{ExpressionValue, ExpressionVariables, VariableExpression, VariableValue};

    /// An expected value.
    enum Want {
        N,
        S(&'static str),
        I(i64),
        B(bool),
        Ss(&'static [&'static str]),
        Is(&'static [i64]),
        Bs(&'static [bool]),
        Empty,
    }
    use Want::*;

    impl Want {
        fn value(&self) -> Option<ExpressionValue> {
            Some(match self {
                N => return None,
                S(s) => ExpressionValue::String((*s).into()),
                I(n) => ExpressionValue::Int(*n),
                B(b) => ExpressionValue::Bool(*b),
                Ss(l) => ExpressionValue::StringList(l.iter().map(|s| String::from(*s)).collect()),
                Is(l) => ExpressionValue::IntList(l.to_vec()),
                Bs(l) => ExpressionValue::BoolList(l.to_vec()),
                Empty => ExpressionValue::EmptyList,
            })
        }
    }

    /// The variables the table is evaluated with, as `probe.py` gives
    /// them to OpenUSD.
    fn variables() -> ExpressionVariables {
        let mut vars = ExpressionVariables::new();
        let s = |s: &str| VariableValue::Value(ExpressionValue::String(s.into()));
        for (name, value) in [
            ("STR", s("abc")),
            ("EMPTY", s("")),
            ("SUB", s("`'sub_${STR}'`")),
            ("BADSUB", s("`'${'`")),
            ("LOOP", s("`${LOOP2}`")),
            ("LOOP2", s("`${LOOP}`")),
            ("NONEEXPR", s("`None`")),
            ("INTEXPR", s("`42`")),
            ("INT", VariableValue::Value(ExpressionValue::Int(7))),
            ("NEG", VariableValue::Value(ExpressionValue::Int(-3))),
            ("YES", VariableValue::Value(ExpressionValue::Bool(true))),
            ("NO", VariableValue::Value(ExpressionValue::Bool(false))),
            (
                "NAMES",
                VariableValue::Value(ExpressionValue::StringList(
                    ["x", "y"].map(String::from).to_vec(),
                )),
            ),
            (
                "NUMS",
                VariableValue::Value(ExpressionValue::IntList([1, 2, 3].to_vec())),
            ),
            ("ASSET", VariableValue::Unsupported("SdfAssetPath".into())),
            ("FLOATV", VariableValue::Unsupported("double".into())),
        ] {
            vars.insert(name, value);
        }
        vars
    }

    /// Each expression evaluates as OpenUSD 26.8's `SdfVariableExpression`
    /// does with the same variables: value, errors (verbatim) and used
    /// variables (not compared where `defined` looks names up, which
    /// OpenUSD does not report). Generated with usd-core 26.8.
    #[test]
    fn evaluation_matches_openusd() {
        /// An expression, its value, its errors and its used variables.
        type Case = (
            &'static str,
            Want,
            &'static [&'static str],
            Option<&'static [&'static str]>,
        );
        #[rustfmt::skip]
        let table: &[Case] = &[
        ("`\"./${STR}.usd\"`", S("./abc.usd"), &[], Some(&["STR"])),
        ("`'${STR}_sel'`", S("abc_sel"), &[], Some(&["STR"])),
        ("`\"${UNDEF}\"`", S("UNDEF"), &[], Some(&["UNDEF"])),
        ("`\"a${UNDEF}b\"`", S("aUNDEFb"), &[], Some(&["UNDEF"])),
        ("`${STR}`", S("abc"), &[], Some(&["STR"])),
        ("`${UNDEF}`", N, &["No value for variable 'UNDEF'"], Some(&["UNDEF"])),
        ("`${INT}`", I(7), &[], Some(&["INT"])),
        ("`\"x${INT}\"`", N, &["String value required for substituting variable 'INT', got int."], Some(&["INT"])),
        ("`${SUB}`", S("sub_abc"), &[], Some(&["STR", "SUB"])),
        ("`\"p_${SUB}\"`", S("p_sub_abc"), &[], Some(&["STR", "SUB"])),
        ("`${BADSUB}`", N, &["Variables must be a C identifier at character 4 (in variable 'BADSUB')"], Some(&["BADSUB"])),
        ("`${LOOP}`", N, &["Encountered circular variable substitutions: ['LOOP', 'LOOP2', 'LOOP']"], Some(&["LOOP", "LOOP2"])),
        ("`${ASSET}`", N, &["Variable 'ASSET' has unsupported type SdfAssetPath"], Some(&["ASSET"])),
        ("`\"${NONEEXPR}x\"`", S("x"), &[], Some(&["NONEEXPR"])),
        ("`${NONEEXPR}`", N, &[], Some(&["NONEEXPR"])),
        ("`${INTEXPR}`", I(42), &[], Some(&["INTEXPR"])),
        ("`\"\"`", S(""), &[], Some(&[])),
        ("`''`", S(""), &[], Some(&[])),
        ("`\"a\\\"b\"`", S("a\"b"), &[], Some(&[])),
        ("`'a\\'b'`", S("a'b"), &[], Some(&[])),
        ("`\"a\\`b\"`", S("a`b"), &[], Some(&[])),
        ("`\"a`b\"`", S("a`b"), &[], Some(&[])),
        ("`\"\\${STR}\"`", S("${STR}"), &[], Some(&[])),
        ("`\"a\\nb\"`", S("a\nb"), &[], Some(&[])),
        ("`\"a\\\\b\"`", S("a\\b"), &[], Some(&[])),
        ("`\"a\\qb\"`", S("aqb"), &[], Some(&[])),
        ("`\"tab\\tx\"`", S("tab\tx"), &[], Some(&[])),
        ("`\"\\x41\\102\"`", S("AB"), &[], Some(&[])),
        ("`42`", I(42), &[], Some(&[])),
        ("`-7`", I(-7), &[], Some(&[])),
        ("`007`", I(7), &[], Some(&[])),
        ("`99999999999999999999`", N, &["Integer 99999999999999999999 out of range. at character 1"], Some(&[])),
        ("`True`", B(true), &[], Some(&[])),
        ("`true`", B(true), &[], Some(&[])),
        ("`False`", B(false), &[], Some(&[])),
        ("`false`", B(false), &[], Some(&[])),
        ("`None`", N, &[], Some(&[])),
        ("`none`", N, &[], Some(&[])),
        ("`Truex`", N, &["Unexpected expression at character 1"], Some(&[])),
        ("`[]`", Empty, &[], Some(&[])),
        ("`[ ]`", Empty, &[], Some(&[])),
        ("`[\"a\", \"b\"]`", Ss(&["a", "b"]), &[], Some(&[])),
        ("`[\"a\" , \"b\"]`", Ss(&["a", "b"]), &[], Some(&[])),
        ("`[1, 2]`", Is(&[1, 2]), &[], Some(&[])),
        ("`[True, False]`", Bs(&[true, false]), &[], Some(&[])),
        ("`[1, \"a\"]`", N, &["Unexpected value of type string in list at element 1"], Some(&[])),
        ("`[None]`", N, &["Unexpected value of type None in list at element 0"], Some(&[])),
        ("`[[1]]`", N, &["Missing ending ']' at character 2"], Some(&[])),
        ("`[ 1]`", N, &["Missing ending ']' at character 3"], Some(&[])),
        ("`[1 ]`", N, &["Missing ending ']' at character 3"], Some(&[])),
        ("`[${STR}, \"d\"]`", Ss(&["abc", "d"]), &[], Some(&["STR"])),
        ("`if(${YES}, \"a\", \"b\")`", S("a"), &[], Some(&["YES"])),
        ("`if(${NO}, \"a\", \"b\")`", S("b"), &[], Some(&["NO"])),
        ("`if(${NO}, \"a\")`", N, &[], Some(&["NO"])),
        ("`if(${YES}, \"a\")`", S("a"), &[], Some(&["YES"])),
        ("`if(${INT}, \"a\", \"b\")`", N, &["if: Condition must be a boolean value"], Some(&["INT"])),
        ("`if(${YES}, \"a\", 1)`", N, &["if: if-value and else-value must evaluate to the same type or None."], Some(&["YES"])),
        ("`if(${YES}, \"a\", None)`", S("a"), &[], Some(&["YES"])),
        ("`if(${YES}, None, 1)`", N, &[], Some(&["YES"])),
        ("`if(${NO}, ${UNDEF}, \"b\")`", S("b"), &[], Some(&["NO", "UNDEF"])),
        ("`if(${YES}, ${UNDEF}, \"b\")`", N, &["No value for variable 'UNDEF'"], Some(&["UNDEF", "YES"])),
        ("`if(True, [1], [])`", N, &["if: if-value and else-value must evaluate to the same type or None."], Some(&[])),
        ("`if(True, [1], [2])`", Is(&[1]), &[], Some(&[])),
        ("`if ( True , \"a\" , \"b\" )`", S("a"), &[], Some(&[])),
        ("`if(True,\"a\",\"b\")`", S("a"), &[], Some(&[])),
        ("`if(True, \"a\", \"b\", \"c\")`", N, &["Function 'if' does not take 4 arguments."], Some(&[])),
        ("`if(True)`", N, &["Function 'if' does not take 1 arguments."], Some(&[])),
        ("`eq(1, 1)`", B(true), &[], Some(&[])),
        ("`eq(1, 2)`", B(false), &[], Some(&[])),
        ("`eq(\"a\", \"a\")`", B(true), &[], Some(&[])),
        ("`eq(\"a\", 1)`", N, &["eq: Cannot compare values of type string and int"], Some(&[])),
        ("`eq(None, None)`", B(true), &[], Some(&[])),
        ("`neq(None, None)`", B(false), &[], Some(&[])),
        ("`lt(None, None)`", N, &["lt: Comparison operation not supported for None"], Some(&[])),
        ("`eq([1], [1])`", N, &["eq: Unsupported type for comparison"], Some(&[])),
        ("`neq(1, 2)`", B(true), &[], Some(&[])),
        ("`lt(1, 2)`", B(true), &[], Some(&[])),
        ("`lt(\"b\", \"a\")`", B(false), &[], Some(&[])),
        ("`leq(2, 2)`", B(true), &[], Some(&[])),
        ("`gt(True, False)`", B(true), &[], Some(&[])),
        ("`geq(\"a\", \"b\")`", B(false), &[], Some(&[])),
        ("`and(True, True)`", B(true), &[], Some(&[])),
        ("`and(True, False)`", B(false), &[], Some(&[])),
        ("`and(False, ${UNDEF})`", B(false), &[], Some(&[])),
        ("`and(True, ${UNDEF})`", N, &["No value for variable 'UNDEF'"], Some(&["UNDEF"])),
        ("`and(True, 1)`", N, &["and: Invalid type int for argument 1"], Some(&[])),
        ("`and(True)`", N, &["Function 'and' requires at least 2 arguments."], Some(&[])),
        ("`or(False, False, True)`", B(true), &[], Some(&[])),
        ("`or(True, ${UNDEF})`", B(true), &[], Some(&[])),
        ("`or(False, 1, ${UNDEF})`", N, &["or: Invalid type int for argument 1", "No value for variable 'UNDEF'"], Some(&["UNDEF"])),
        ("`not(True)`", B(false), &[], Some(&[])),
        ("`not(1)`", N, &["not: Invalid type int for argument"], Some(&[])),
        ("`not(${UNDEF})`", N, &["No value for variable 'UNDEF'"], Some(&["UNDEF"])),
        ("`contains(\"abc\", \"b\")`", B(true), &[], Some(&[])),
        ("`contains(\"abc\", \"z\")`", B(false), &[], Some(&[])),
        ("`contains([\"a\", \"b\"], \"b\")`", B(true), &[], Some(&[])),
        ("`contains([1, 2], 3)`", B(false), &[], Some(&[])),
        ("`contains([1, 2], \"a\")`", N, &["contains: Invalid search value"], Some(&[])),
        ("`contains([], 1)`", B(false), &[], Some(&[])),
        ("`contains(1, 1)`", N, &["contains: Value to search must be a list or string"], Some(&[])),
        ("`contains(${NAMES}, \"y\")`", B(true), &[], Some(&["NAMES"])),
        ("`contains(${NUMS}, 2)`", B(true), &[], Some(&["NUMS"])),
        ("`contains(\"abc\", 1)`", N, &["contains: Invalid search value"], Some(&[])),
        ("`at(\"abc\", 0)`", S("a"), &[], Some(&[])),
        ("`at(\"abc\", -1)`", S("c"), &[], Some(&[])),
        ("`at(\"abc\", 3)`", N, &["at: Index out of range"], Some(&[])),
        ("`at([1, 2], 1)`", I(2), &[], Some(&[])),
        ("`at([], 0)`", N, &["at: Index out of range"], Some(&[])),
        ("`at(1, 0)`", N, &["at: Only supported for lists or strings"], Some(&[])),
        ("`at(\"abc\", \"0\")`", N, &["at: Index must be an integer"], Some(&[])),
        ("`at(${NAMES}, -2)`", S("x"), &[], Some(&["NAMES"])),
        ("`len(\"abc\")`", I(3), &[], Some(&[])),
        ("`len([1, 2, 3])`", I(3), &[], Some(&[])),
        ("`len([])`", I(0), &[], Some(&[])),
        ("`len(1)`", N, &["len: Unsupported type"], Some(&[])),
        ("`len(None)`", N, &["len: Unsupported type"], Some(&[])),
        ("`len(${NUMS})`", I(3), &[], Some(&["NUMS"])),
        ("`defined(\"STR\")`", B(true), &[], None),
        ("`defined(\"STR\", \"UNDEF\")`", B(false), &[], None),
        ("`defined(\"UNDEF\")`", B(false), &[], None),
        ("`defined(1)`", N, &["defined: Invalid type int for argument 0"], None),
        ("`defined()`", N, &["Function 'defined' requires at least 1 arguments."], None),
        ("`nosuch(1)`", N, &["Unknown function nosuch"], Some(&[])),
        ("`eq(1)`", N, &["Function 'eq' does not take 1 arguments."], Some(&[])),
        ("`eq(1, 2, 3)`", N, &["Function 'eq' does not take 3 arguments."], Some(&[])),
        ("`${`", N, &["Variables must be a C identifier at character 3"], Some(&[])),
        ("`${STR`", N, &["Missing ending '}' at character 6"], Some(&[])),
        ("`${1A}`", N, &["Variables must be a C identifier at character 3"], Some(&[])),
        ("`\"abc`", N, &["Missing ending '\"' at character 6"], Some(&[])),
        ("`'abc`", N, &["Missing ending \"'\" at character 6"], Some(&[])),
        ("`abc`", N, &["Unexpected expression at character 1"], Some(&[])),
        ("`\"a\" \"b\"`", N, &["Missing ending '`' at character 4"], Some(&[])),
        ("`\"a\"x`", N, &["Missing ending '`' at character 4"], Some(&[])),
        ("` \"a\"`", N, &["Unexpected expression at character 1"], Some(&[])),
        ("`\"a\" `", N, &["Missing ending '`' at character 4"], Some(&[])),
        ("`if(True, \"a\"`", N, &["Missing ending ')' at character 13"], Some(&[])),
        ("`[1, 2`", N, &["Missing ending ']' at character 6"], Some(&[])),
        ("`${FLOATV}`", N, &["Variable 'FLOATV' has unsupported type double"], Some(&["FLOATV"])),
        ("`\"${FLOATV}\"`", N, &["Variable 'FLOATV' has unsupported type double"], Some(&["FLOATV"])),
        ("`\"${INT}\"`", N, &["String value required for substituting variable 'INT', got int."], Some(&["INT"])),
        ("`${YES}`", B(true), &[], Some(&["YES"])),
        ("`${NAMES}`", Ss(&["x", "y"]), &[], Some(&["NAMES"])),
        ("`if(${UNDEF}, 1, 2)`", N, &["No value for variable 'UNDEF'"], Some(&["UNDEF"])),
        ("`eq(${UNDEF}, ${UNDEF2})`", N, &["No value for variable 'UNDEF'", "No value for variable 'UNDEF2'"], Some(&["UNDEF", "UNDEF2"])),
        ("`[${UNDEF}, ${UNDEF2}]`", N, &["No value for variable 'UNDEF'", "No value for variable 'UNDEF2'"], Some(&["UNDEF", "UNDEF2"])),
        ("`\"a\"` trailing", S("a"), &[], Some(&[])),
        ("`if(eq(${STR}, \"abc\"), \"yes\", \"no\")`", S("yes"), &[], Some(&["STR"])),
        ("`${NEG}`", I(-3), &[], Some(&["NEG"])),
        ("`at(${NUMS}, ${NEG})`", I(1), &[], Some(&["NEG", "NUMS"])),
        ("`if(True, nosuch(), 1)`", N, &["Unknown function nosuch"], Some(&[])),
        ("`if(nosuch(1), 1)`", N, &["Unknown function nosuch"], Some(&[])),
        ];
        let vars = variables();
        let mut failures = Vec::new();
        for (source, want, errors, used) in table {
            let result = VariableExpression::parse(source).evaluate(&vars);
            let used_ok = used.is_none_or(|used| {
                result
                    .used_variables
                    .iter()
                    .map(String::as_str)
                    .eq(used.iter().copied())
            });
            if result.value != want.value() || result.errors != *errors || !used_ok {
                failures.push(alloc::format!("{source}: {result:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn parse_errors_are_kept() {
        let expression = VariableExpression::parse("`${`");
        assert!(!expression.is_valid());
        assert_eq!(
            expression.errors(),
            ["Variables must be a C identifier at character 3"]
        );
        assert!(VariableExpression::parse("`'a'`").is_valid());
        assert!(super::is_expression("`a`"));
        assert!(!super::is_expression("``"));
        assert!(!super::is_expression("a`"));
    }

    #[test]
    fn strings_are_taken_as_asset_paths() {
        let vars = variables();
        let eval = |s: &str| VariableExpression::parse(s).evaluate(&vars).into_string();
        assert_eq!(eval("`\"${STR}.usd\"`"), Ok(Some("abc.usd".into())));
        assert_eq!(eval("`None`"), Ok(None));
        assert_eq!(
            eval("`${INT}`"),
            Err("Expression evaluated to 'int' but expected 'string'".into())
        );
        assert_eq!(
            eval("`eq(${UNDEF}, ${UNDEF2})`"),
            Err("No value for variable 'UNDEF'; No value for variable 'UNDEF2'".into())
        );
    }

    #[test]
    fn matches_regex_is_not_evaluated() {
        let result = VariableExpression::parse("`matches_regex(\"abc\", \"a.c\")`")
            .evaluate(&ExpressionVariables::new());
        assert_eq!(result.value, None);
        assert_eq!(
            result.errors,
            ["matches_regex: regular expressions are not supported"]
        );
    }

    #[test]
    fn variables_compose_stronger_first() {
        let s = |s: &str| VariableValue::Value(ExpressionValue::String(s.into()));
        let mut strong = ExpressionVariables::new();
        strong.insert("A", s("strong"));
        let mut weak = ExpressionVariables::new();
        weak.insert("A", s("weak"));
        weak.insert("B", s("weak"));
        strong.compose_over(&weak);
        assert_eq!(strong.get("A"), Some(&s("strong")));
        assert_eq!(strong.get("B"), Some(&s("weak")));
    }
}
