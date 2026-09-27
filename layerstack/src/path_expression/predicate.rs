// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Predicate expressions: the braced tests in a path pattern
//! (`{isa:Mesh}`, `{kind(component, strict=true) and not abstract}`).
//!
//! OpenUSD: `pxr/usd/sdf/predicateExpression.h`.

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;

/// A logical operator of a predicate expression, tightest binding first.
///
/// OpenUSD: `SdfPredicateExpression::Op`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PredicateOp {
    /// Juxtaposition, `a b`: an `and` binding tighter than the keyword.
    ImpliedAnd,
    /// `a and b`.
    And,
    /// `a or b`.
    Or,
}

/// A predicate expression: calls combined with `not`, `and` and `or`.
///
/// OpenUSD: `SdfPredicateExpression`.
#[derive(Clone, Debug, PartialEq)]
pub enum PredicateExpression {
    /// A call of a predicate function.
    Call(PredicateCall),
    /// `not operand`.
    Not(Box<Self>),
    /// A logical operation on two operands.
    Op(PredicateOp, Box<Self>, Box<Self>),
}

/// How a predicate call is written.
///
/// OpenUSD: `SdfPredicateExpression::FnCall::Kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallKind {
    /// Just the name: `defined`.
    Bare,
    /// Name, colon, comma-separated positional arguments: `isa:Mesh,Xform`.
    Colon,
    /// Name and parenthesized positional then keyword arguments:
    /// `kind(component, strict=true)`.
    Paren,
}

/// A call of a predicate function.
///
/// OpenUSD: `SdfPredicateExpression::FnCall`.
#[derive(Clone, Debug, PartialEq)]
pub struct PredicateCall {
    /// How it is written.
    pub kind: CallKind,
    /// The function's name.
    pub name: String,
    /// Its arguments, positional ones first.
    pub args: Vec<PredicateArg>,
}

/// A predicate call argument.
///
/// OpenUSD: `SdfPredicateExpression::FnArg`.
#[derive(Clone, Debug, PartialEq)]
pub struct PredicateArg {
    /// The keyword, for a keyword argument (`strict=true`).
    pub name: Option<String>,
    /// The value.
    pub value: ArgValue,
}

/// A predicate argument's value, typed as the parser reads it: `true`
/// and `false` are booleans, `12` an integer, `1.5`, `1e3` and `inf`
/// floats, anything else (quoted or not) a string.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    /// `true` or `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A floating-point number.
    Float(f64),
    /// A string.
    String(String),
}

impl ArgValue {
    /// The value as OpenUSD converts it to a `bool` parameter: booleans as
    /// they are, numbers by whether they are non-zero; strings do not
    /// convert.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Int(i) => Some(*i != 0),
            Self::Float(f) => Some(*f != 0.0),
            Self::String(_) => None,
        }
    }

    /// The string, for a string value.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The value as `Sdf_FileIOUtility::StringFromVtValue` writes it:
    /// booleans as `1` and `0`, floats in shortest form, strings quoted.
    /// `lossless` writes booleans as `true` and `false`, and integral floats
    /// with `.0`, so the text parses back to the same types.
    fn write(&self, out: &mut String, lossless: bool) {
        match self {
            Self::Bool(b) if lossless => out.push_str(if *b { "true" } else { "false" }),
            Self::Bool(b) => out.push(if *b { '1' } else { '0' }),
            Self::Int(i) => out.push_str(&i.to_string()),
            Self::Float(f) => {
                let text = shortest_double(*f);
                let integral = !text.contains(['.', 'e', 'n']);
                out.push_str(&text);
                if lossless && integral {
                    out.push_str(".0");
                }
            }
            Self::String(s) => quote(s, out),
        }
    }
}

/// Why a predicate library did not bind a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PredicateBindError {
    /// The function's name.
    pub name: String,
    /// Why, for a person.
    pub reason: String,
}

impl fmt::Display for PredicateBindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot bind predicate `{}`: {}", self.name, self.reason)
    }
}

impl PredicateBindError {
    /// A bind error for the function `name`.
    #[must_use]
    pub fn new(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reason: reason.into(),
        }
    }
}

impl PredicateExpression {
    fn precedence(&self) -> Option<PredicateOp> {
        match self {
            Self::Op(op, ..) => Some(*op),
            _ => None,
        }
    }

    /// Visits every call, left to right.
    pub fn calls(&self) -> Vec<&PredicateCall> {
        let mut out = Vec::new();
        self.for_each_call(&mut |call| out.push(call));
        out
    }

    fn for_each_call<'a>(&'a self, visit: &mut impl FnMut(&'a PredicateCall)) {
        match self {
            Self::Call(call) => visit(call),
            Self::Not(operand) => operand.for_each_call(visit),
            Self::Op(_, left, right) => {
                left.for_each_call(visit);
                right.for_each_call(visit);
            }
        }
    }

    /// The expression's text, as `SdfPredicateExpression::GetText` writes
    /// it.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, false);
        out
    }

    pub(crate) fn write(&self, out: &mut String, lossless: bool) {
        match self {
            Self::Call(call) => call.write(out, lossless),
            Self::Not(operand) => {
                // `not` is the tightest operator; only a binary operand
                // needs parentheses.
                out.push_str("not ");
                operand.write_grouped(out, operand.precedence().is_some(), lossless);
            }
            Self::Op(op, left, right) => {
                left.write_grouped(out, left.precedence().is_some_and(|p| p > *op), lossless);
                out.push_str(match op {
                    PredicateOp::ImpliedAnd => " ",
                    PredicateOp::And => " and ",
                    PredicateOp::Or => " or ",
                });
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
}

impl PredicateCall {
    fn write(&self, out: &mut String, lossless: bool) {
        out.push_str(&self.name);
        match self.kind {
            CallKind::Bare => {}
            CallKind::Colon => {
                for (i, arg) in self.args.iter().enumerate() {
                    out.push(if i == 0 { ':' } else { ',' });
                    arg.value.write(out, lossless);
                }
            }
            CallKind::Paren => {
                out.push('(');
                for (i, arg) in self.args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if let Some(name) = &arg.name {
                        out.push_str(name);
                        out.push('=');
                    }
                    arg.value.write(out, lossless);
                }
                out.push(')');
            }
        }
    }

    /// Its keyword argument `name`'s value, if given.
    #[must_use]
    pub fn keyword(&self, name: &str) -> Option<&ArgValue> {
        self.args
            .iter()
            .find(|arg| arg.name.as_deref() == Some(name))
            .map(|arg| &arg.value)
    }

    /// Its positional arguments' values.
    pub fn positional(&self) -> impl Iterator<Item = &ArgValue> {
        self.args
            .iter()
            .filter(|arg| arg.name.is_none())
            .map(|arg| &arg.value)
    }
}

/// Quotes `text` as `Sdf_FileIOUtility::Quote` does: double quotes unless
/// the text has `"` and no `'`, triple quotes around newlines, escapes
/// for the quote, `\`, `\r`, `\t` and other unprintable ASCII.
fn quote(text: &str, out: &mut String) {
    let quote = if text.contains('"') && !text.contains('\'') {
        '\''
    } else {
        '"'
    };
    let triple = text.contains('\n');
    let open = if triple { 3 } else { 1 };
    for _ in 0..open {
        out.push(quote);
    }
    for c in text.chars() {
        match c {
            '\n' if triple => out.push('\n'),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_ascii() && !(' '..='~').contains(&c) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    for _ in 0..open {
        out.push(quote);
    }
}

/// `value` in shortest round-trip form, as `TfStringify(double)` writes it
/// (double-conversion's `ToShortest`, decimal for decimal exponents from
/// -6 to 14, `inf`, `-inf` and `nan`).
fn shortest_double(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.into();
    }
    // The shortest digits and decimal exponent.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::new();
    if value < 0.0 {
        out.push('-');
    }
    if (-6..15).contains(&exponent) {
        let point = exponent + 1;
        let len = i32::try_from(digits.len()).unwrap_or(i32::MAX);
        if point <= 0 {
            out.push_str("0.");
            for _ in 0..-point {
                out.push('0');
            }
            out.push_str(&digits);
        } else if point >= len {
            out.push_str(&digits);
            for _ in 0..point - len {
                out.push('0');
            }
        } else {
            let (whole, fraction) = digits.split_at(point.unsigned_abs() as usize);
            out.push_str(whole);
            out.push('.');
            out.push_str(fraction);
        }
    } else {
        let (first, rest) = digits.split_at(1);
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        out.push('e');
        out.push_str(&exponent.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_print_as_openusd_prints_them() {
        for (value, text) in [
            (1.5, "1.5"),
            (1e5, "100000"),
            (1e14, "100000000000000"),
            (1e15, "1e15"),
            (1.25e20, "1.25e20"),
            (1e-7, "1e-7"),
            (0.000_001, "0.000001"),
            (2.0, "2"),
            (-3.25, "-3.25"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ] {
            assert_eq!(shortest_double(value), text, "{value}");
        }
    }

    #[test]
    fn strings_quote_as_openusd_quotes_them() {
        let quoted = |text: &str| {
            let mut out = String::new();
            quote(text, &mut out);
            out
        };
        assert_eq!(quoted("x y"), "\"x y\"");
        assert_eq!(quoted("q\""), "'q\"'");
        assert_eq!(quoted("x\"y'z"), "\"x\\\"y'z\"");
        assert_eq!(quoted("tab\there"), "\"tab\\there\"");
        assert_eq!(quoted("line\nnext"), "\"\"\"line\nnext\"\"\"");
        assert_eq!(quoted(""), "\"\"");
    }
}
