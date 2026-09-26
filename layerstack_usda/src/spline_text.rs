// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDA spline values: `double height.spline = { ... }`.
//!
//! OpenUSD's text syntax for `TsSpline` (`SplineValue` in
//! `pxr/usd/sdf/textFileFormatParser.h`; written by
//! `Sdf_FileIOUtility::WriteSpline` in `pxr/usd/sdf/fileIO_Common.cpp`):
//!
//! ```text
//! double height.spline = {
//!     bezier,
//!     pre: linear,
//!     post: sloped(0.57),
//!     loop: (15, 25, 0, 2, 11.7),
//!     7: 5.5 & 7.21; post held,
//!     15: 8.18; post curve (2.49, 1.17),
//!     20: 14.72; pre (3.77, -1.4); post curve (1.1, -1.4),
//! }
//! ```
//!
//! Items are separated by commas: the curve type, the pre- and
//! post-extrapolation, the inner-loop parameters and one item per knot. A
//! knot is its time, optionally a pre-value and `&`, its value, then
//! `;`-separated parameters: the pre-tangent, the post-segment
//! interpolation with its tangent, and custom data. A Bézier tangent is
//! `(width, slope)`, a Hermite tangent `(slope)`, either optionally with a
//! tangent algorithm.
//!
//! [`SplineData`] holds no knot custom data and no `loopBoundaryTime`, so a
//! spline that authors them is rejected rather than read without them.
//! Tangent algorithms are read and dropped: the tangents they produced are
//! authored beside them and kept, as the USDC reader does.
//!
//! Spec: AOUSD Core §12.3.3 (spline opinions).

use alloc::{format, string::String, vec::Vec};

use layerstack::spline::{
    CurveType, Extrapolation, Knot, KnotInterp, LoopParams, SplineData, SplineDataType,
};

/// Parses the text of a spline value, from its `{` to its `}`, as a spline
/// of `data_type` values.
///
/// # Errors
///
/// A message naming what is malformed or unsupported.
pub(crate) fn parse(text: &str, data_type: SplineDataType) -> Result<SplineData, String> {
    let mut parser = Parser {
        tokens: tokenize(text)?,
        pos: 0,
    };
    parser.expect(&Token::Punct('{'))?;
    let mut spline = SplineData {
        data_type,
        default_curve_type: CurveType::Bezier,
        pre_extrapolation: Extrapolation::Held,
        post_extrapolation: Extrapolation::Held,
        loop_params: None,
        knots: Vec::new(),
    };
    loop {
        if parser.eat(&Token::Punct('}')) {
            break;
        }
        parser.item(&mut spline)?;
        if !parser.eat(&Token::Punct(',')) {
            parser.expect(&Token::Punct('}'))?;
            break;
        }
    }
    if parser.pos != parser.tokens.len() {
        return Err("unexpected text after the spline".into());
    }
    let curve_type = spline.default_curve_type;
    for knot in &mut spline.knots {
        knot.curve_type = curve_type;
        // Knot values and slopes are of the spline's value type, as
        // OpenUSD's parser stores them (`Ts_TypedKnotData<T>`); widths and
        // times stay doubles.
        for v in [
            &mut knot.value,
            &mut knot.pre_tan_slope,
            &mut knot.post_tan_slope,
        ] {
            *v = quantize(*v, data_type);
        }
        if let Some(pre) = &mut knot.pre_value {
            *pre = quantize(*pre, data_type);
        }
    }
    spline.knots.sort_by(|a, b| a.time.total_cmp(&b.time));
    if spline.knots.windows(2).any(|w| w[0].time == w[1].time) {
        return Err("two knots at the same time".into());
    }
    Ok(spline)
}

/// `v` rounded to the spline's value type.
#[allow(
    clippy::cast_possible_truncation,
    reason = "rounding to the value type"
)]
fn quantize(v: f64, data_type: SplineDataType) -> f64 {
    match data_type {
        SplineDataType::Float => f64::from(v as f32),
        SplineDataType::Half => f64::from(layerstack::half::to_f32(layerstack::half::from_f64(v))),
        SplineDataType::Double | SplineDataType::Unspecified => v,
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Word(String),
    Punct(char),
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(at, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '#' {
            while chars.next_if(|&(_, c)| c != '\n').is_some() {}
        } else if "{}(),:;&=[]<>@".contains(c) {
            // Beyond the spline's own punctuation, what a knot's custom
            // data dictionary holds, which is reported rather than read.
            tokens.push(Token::Punct(c));
            chars.next();
        } else if c == '"' || c == '\'' {
            chars.next();
            let mut text = String::new();
            for (_, d) in chars.by_ref() {
                if d == c {
                    break;
                }
                text.push(d);
            }
            tokens.push(Token::Word(text));
        } else if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' {
            let mut end = at;
            while let Some(&(i, c)) = chars.peek() {
                let exponent_sign =
                    (c == '-' || c == '+') && text[..i].ends_with(['e', 'E']) && i > at;
                if c.is_ascii_alphanumeric()
                    || c == '.'
                    || exponent_sign
                    || (i == at && (c == '-' || c == '+'))
                {
                    end = i + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let word = &text[at..end];
            let number = match word {
                "inf" | "+inf" => f64::INFINITY,
                "-inf" => f64::NEG_INFINITY,
                "nan" | "-nan" | "+nan" => f64::NAN,
                _ => word
                    .parse::<f64>()
                    .map_err(|_| format!("`{word}` is not a number"))?,
            };
            tokens.push(Token::Number(number));
        } else if c.is_alphabetic() || c == '_' {
            let mut end = at;
            while let Some(&(i, c)) = chars.peek() {
                if c.is_alphanumeric() || c == '_' {
                    end = i + c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let word = &text[at..end];
            match word {
                "inf" => tokens.push(Token::Number(f64::INFINITY)),
                "nan" => tokens.push(Token::Number(f64::NAN)),
                _ => tokens.push(Token::Word(word.into())),
            }
        } else {
            return Err(format!("unexpected `{c}` in a spline"));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &Token) -> Result<(), String> {
        if self.eat(token) {
            Ok(())
        } else {
            Err(format!("expected {}", describe(token)))
        }
    }

    fn word(&mut self) -> Option<String> {
        match self.peek() {
            Some(Token::Word(word)) => {
                let word = word.clone();
                self.pos += 1;
                Some(word)
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(Token::Number(n)) => {
                let n = *n;
                self.pos += 1;
                Ok(n)
            }
            _ => Err("expected a number".into()),
        }
    }

    fn item(&mut self, spline: &mut SplineData) -> Result<(), String> {
        if let Some(Token::Number(_)) = self.peek() {
            let knot = self.knot()?;
            spline.knots.push(knot);
            return Ok(());
        }
        let word = self.word().ok_or("expected a spline item")?;
        match word.as_str() {
            "bezier" => spline.default_curve_type = CurveType::Bezier,
            "hermite" => spline.default_curve_type = CurveType::Hermite,
            "pre" => {
                self.expect(&Token::Punct(':'))?;
                spline.pre_extrapolation = self.extrapolation()?;
            }
            "post" => {
                self.expect(&Token::Punct(':'))?;
                spline.post_extrapolation = self.extrapolation()?;
            }
            "loop" => {
                self.expect(&Token::Punct(':'))?;
                self.expect(&Token::Punct('('))?;
                let proto_start = self.number()?;
                self.expect(&Token::Punct(','))?;
                let proto_end = self.number()?;
                self.expect(&Token::Punct(','))?;
                let num_pre_loops = self.count()?;
                self.expect(&Token::Punct(','))?;
                let num_post_loops = self.count()?;
                self.expect(&Token::Punct(','))?;
                let value_offset = self.number()?;
                self.expect(&Token::Punct(')'))?;
                spline.loop_params = Some(LoopParams {
                    proto_start,
                    proto_end,
                    num_pre_loops,
                    num_post_loops,
                    value_offset,
                });
            }
            other => return Err(format!("unknown spline item `{other}`")),
        }
        Ok(())
    }

    fn count(&mut self) -> Result<i32, String> {
        let n = self.number()?;
        #[allow(clippy::cast_possible_truncation, reason = "checked integral below")]
        let count = n as i32;
        if f64::from(count) == n {
            Ok(count)
        } else {
            Err(format!("`{n}` is not a loop count"))
        }
    }

    fn extrapolation(&mut self) -> Result<Extrapolation, String> {
        let word = self.word().ok_or("expected an extrapolation")?;
        Ok(match word.as_str() {
            "none" => Extrapolation::Block,
            "held" => Extrapolation::Held,
            "linear" => Extrapolation::Linear,
            "sloped" => {
                self.expect(&Token::Punct('('))?;
                let slope = self.number()?;
                self.expect(&Token::Punct(')'))?;
                Extrapolation::Sloped(slope)
            }
            "loop" => {
                let mode = self.word().ok_or("expected a loop mode")?;
                let extrapolation = match mode.as_str() {
                    "repeat" => Extrapolation::LoopRepeat,
                    "reset" => Extrapolation::LoopReset,
                    "oscillate" => Extrapolation::LoopOscillate,
                    other => return Err(format!("unknown loop mode `{other}`")),
                };
                if self.peek() == Some(&Token::Punct('(')) {
                    return Err("unsupported: spline loopBoundaryTime".into());
                }
                extrapolation
            }
            other => return Err(format!("unknown extrapolation `{other}`")),
        })
    }

    fn knot(&mut self) -> Result<Knot, String> {
        let time = self.number()?;
        self.expect(&Token::Punct(':'))?;
        let first = self.number()?;
        let (pre_value, value) = if self.eat(&Token::Punct('&')) {
            (Some(first), self.number()?)
        } else {
            (None, first)
        };
        let mut knot = Knot {
            time,
            value,
            pre_value,
            next_interp: KnotInterp::Held,
            curve_type: CurveType::Bezier,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 0.0,
            post_tan_width: 0.0,
            pre_tan_slope: 0.0,
            post_tan_slope: 0.0,
        };
        while self.eat(&Token::Punct(';')) {
            if self.peek() == Some(&Token::Punct('{')) {
                return Err("unsupported: spline knot custom data".into());
            }
            let word = self.word().ok_or("expected a knot parameter")?;
            match word.as_str() {
                "pre" => {
                    let (width, slope) = self.tangent()?;
                    knot.pre_tan_width = width.unwrap_or(0.0);
                    knot.pre_tan_slope = slope;
                }
                "post" => {
                    let interp = self.word().ok_or("expected an interpolation")?;
                    knot.next_interp = match interp.as_str() {
                        "none" => KnotInterp::Block,
                        "held" => KnotInterp::Held,
                        "linear" => KnotInterp::Linear,
                        "curve" => KnotInterp::Curve,
                        other => return Err(format!("unknown interpolation `{other}`")),
                    };
                    if self.peek() == Some(&Token::Punct('(')) {
                        let (width, slope) = self.tangent()?;
                        knot.post_tan_width = width.unwrap_or(0.0);
                        knot.post_tan_slope = slope;
                    }
                }
                other => return Err(format!("unknown knot parameter `{other}`")),
            }
        }
        Ok(knot)
    }

    /// `(width, slope)` or `(slope)`, each optionally followed by a tangent
    /// algorithm, which is dropped.
    fn tangent(&mut self) -> Result<(Option<f64>, f64), String> {
        self.expect(&Token::Punct('('))?;
        let first = self.number()?;
        let mut second = None;
        if self.eat(&Token::Punct(',')) {
            if let Some(Token::Number(_)) = self.peek() {
                second = Some(self.number()?);
                if self.eat(&Token::Punct(',')) {
                    self.algorithm()?;
                }
            } else {
                self.algorithm()?;
            }
        }
        self.expect(&Token::Punct(')'))?;
        Ok(match second {
            Some(slope) => (Some(first), slope),
            None => (None, first),
        })
    }

    fn algorithm(&mut self) -> Result<(), String> {
        match self.word().as_deref() {
            Some("custom" | "autoEase") => Ok(()),
            _ => Err("expected a tangent algorithm".into()),
        }
    }
}

fn describe(token: &Token) -> String {
    match token {
        Token::Number(n) => format!("{n}"),
        Token::Word(w) => format!("`{w}`"),
        Token::Punct(c) => format!("`{c}`"),
    }
}

/// Writes the spline items of a `.spline = { ... }` value, each on its own
/// line at `indent`, as OpenUSD's `Sdf_FileIOUtility::WriteSpline` writes
/// them. `number` writes one number of the spline's value type.
pub(crate) fn write(
    out: &mut String,
    indent: &str,
    spline: &SplineData,
    time: &dyn Fn(&mut String, f64),
    value: &dyn Fn(&mut String, f64),
) {
    let curves = spline
        .knots
        .iter()
        .any(|knot| knot.next_interp == KnotInterp::Curve);
    if curves || spline.default_curve_type == CurveType::Hermite {
        out.push_str(indent);
        out.push_str(match spline.default_curve_type {
            CurveType::Bezier => "bezier,\n",
            CurveType::Hermite => "hermite,\n",
        });
    }
    for (label, extrapolation) in [
        ("pre", spline.pre_extrapolation),
        ("post", spline.post_extrapolation),
    ] {
        let mode = match extrapolation {
            Extrapolation::Held => continue,
            Extrapolation::Block => "none",
            Extrapolation::Linear => "linear",
            Extrapolation::Sloped(_) => "sloped",
            Extrapolation::LoopRepeat => "loop repeat",
            Extrapolation::LoopReset => "loop reset",
            Extrapolation::LoopOscillate => "loop oscillate",
        };
        out.push_str(indent);
        out.push_str(label);
        out.push_str(": ");
        out.push_str(mode);
        if let Extrapolation::Sloped(slope) = extrapolation {
            out.push('(');
            time(out, slope);
            out.push(')');
        }
        out.push_str(",\n");
    }
    if let Some(lp) = spline.loop_params.filter(|lp| {
        *lp != LoopParams {
            proto_start: 0.0,
            proto_end: 0.0,
            num_pre_loops: 0,
            num_post_loops: 0,
            value_offset: 0.0,
        }
    }) {
        out.push_str(indent);
        out.push_str("loop: (");
        time(out, lp.proto_start);
        out.push_str(", ");
        time(out, lp.proto_end);
        out.push_str(&format!(", {}, {}, ", lp.num_pre_loops, lp.num_post_loops));
        time(out, lp.value_offset);
        out.push_str("),\n");
    }
    let bezier = spline.default_curve_type == CurveType::Bezier;
    let tangent = |out: &mut String, label: &str, width: f64, slope: f64| {
        out.push_str("; ");
        out.push_str(label);
        out.push_str(" (");
        if bezier {
            time(out, width);
            out.push_str(", ");
        }
        value(out, slope);
        out.push(')');
    };
    // The first knot's pre-tangent is written as if a curve preceded it.
    let mut interp = KnotInterp::Curve;
    for knot in &spline.knots {
        out.push_str(indent);
        time(out, knot.time);
        out.push(':');
        if let Some(pre_value) = knot.pre_value {
            out.push(' ');
            value(out, pre_value);
            out.push_str(" &");
        }
        out.push(' ');
        value(out, knot.value);
        if interp == KnotInterp::Curve {
            tangent(out, "pre", knot.pre_tan_width, knot.pre_tan_slope);
        }
        interp = knot.next_interp;
        match interp {
            KnotInterp::Curve => {
                tangent(out, "post curve", knot.post_tan_width, knot.post_tan_slope);
            }
            KnotInterp::Block => out.push_str("; post none"),
            KnotInterp::Held => out.push_str("; post held"),
            KnotInterp::Linear => out.push_str("; post linear"),
        }
        out.push_str(",\n");
    }
}
