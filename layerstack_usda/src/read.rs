// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene import without materializing numeric property arrays as syntax trees.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use layerstack::{AssetResolver, LayerId, PathInterner, SchemaRegistry, TokenInterner};

use crate::{
    Span, ast,
    diagnostic::Diagnostic,
    emit::EmitResult,
    lexer::{Lexer, Token, TokenKind},
    numeric::Number,
};

/// Work counters for one USDA import, excluding recursively resolved layers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadStats {
    /// The most tokens held at once for grammar navigation (lookahead and
    /// the rollback of a speculative parse), including fallback values.
    pub tokens: usize,
    /// The most syntax nodes held at once for structural lowering, which
    /// drops a prim's nodes once it has lowered the prim.
    pub syntax_nodes: usize,
    /// Nonempty numeric property arrays recognized for direct conversion.
    pub numeric_arrays: usize,
    /// Elements recognized for direct conversion (a point counts as one).
    pub numeric_elements: usize,
}

/// A USDA import, preserving diagnostics from each phase.
#[derive(Debug)]
pub struct ReadResult {
    /// Authored layer, resolved assets, emission diagnostics and rejection flag.
    pub emitted: EmitResult,
    /// Syntax and recovery diagnostics from the shared USDA grammar.
    pub parse_diagnostics: Vec<Diagnostic>,
    /// Semantic diagnostics from structural AST lowering.
    pub lower_diagnostics: Vec<Diagnostic>,
    /// Work counters for this layer.
    pub stats: ReadStats,
}

/// Imports USDA directly into an authored layer using the shared grammar.
///
/// Numeric scalar, vector and quaternion property arrays (including time
/// samples and variant branches) are consumed as events into native buffers
/// during parsing, without retaining their element tokens or scanning them again. This avoids per-element CST nodes, generic AST values and tuple
/// allocations. Matrices, metadata, array edits and unsupported or malformed
/// shapes use the ordinary lowering path, preserving its diagnostics and
/// recovery. Numeric range checking and value conversion are shared with
/// [`crate::emit::emit`]. Authored values and declared USD types are preserved.
///
/// Use [`crate::parser::parse`] for an inspectable AST or
/// [`crate::parser::parse_cst`] for lossless editing. The compact structural AST
/// used here is private and never returned to callers. Asset resolution and
/// file I/O remain caller owned; this API works with `no_std` and `alloc`.
///
/// Spec: AOUSD Core §6.2–6.5 (typed values), §12.3 (samples), §16.2 (USDA).
pub fn read_usda(
    source: &str,
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
) -> ReadResult {
    read_with_registry(source, layer_id, tokens, paths, resolver, None)
}

/// Imports USDA using caller-registered metadata types and spec targets.
///
/// Use the registry built with `tokens` to interpret custom metadata literals,
/// as [`crate::emit::emit_with_registry`] does: a field registered for the
/// statement's kind of spec takes its declared type, and a literal it cannot
/// hold rejects the layer. Defaults remain un-authored. Numeric property arrays retain the compact
/// import path and [`ReadStats`] accounting of [`read_usda`].
///
/// For storage-backed stages, `layerstack_io::StageDocument::open_in` supplies
/// `StageOptions::schemas` to this path for every imported USDA layer.
/// Spec: AOUSD Core §7.4 (metadata), §6.2–6.6 (types), §16.2 (USDA).
pub fn read_usda_with_registry(
    source: &str,
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    registry: &SchemaRegistry,
) -> ReadResult {
    read_with_registry(source, layer_id, tokens, paths, resolver, Some(registry))
}

fn read_with_registry(
    source: &str,
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    registry: Option<&SchemaRegistry>,
) -> ReadResult {
    let parsed = crate::parser::parse_for_read(source);
    let stats = ReadStats {
        tokens: parsed.tokens,
        syntax_nodes: parsed.syntax_nodes,
        numeric_arrays: parsed.arrays.arrays(),
        numeric_elements: parsed.arrays.elements(),
    };
    let emitted = crate::emit::emit_with_arrays(
        &parsed.lowered.layer,
        layer_id,
        tokens,
        paths,
        resolver,
        parsed.arrays,
        registry,
    );
    ReadResult {
        emitted,
        parse_diagnostics: parsed.diagnostics,
        lower_diagnostics: parsed.lowered.diagnostics,
        stats,
    }
}

/// Numeric values read as the parser met them, by attribute statement
/// and time sample, for emission to take in place of the placeholders it
/// lowers; the rare conversion errors are kept apart.
#[derive(Default)]
pub(crate) struct PreparedValues {
    values: BTreeMap<PreparedKey, layerstack::Value>,
    /// Why elements of the arrays at these keys did not convert.
    errors: BTreeMap<PreparedKey, Vec<alloc::string::String>>,
    /// The arrays read (tuples are not), and their elements.
    arrays: usize,
    elements: usize,
}

/// Where a prepared value is: the offset of its attribute statement, and
/// its time sample's ordinal (`None` for the default).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PreparedKey {
    statement: u32,
    /// 0 for the default, else the sample's ordinal plus one (a layer of
    /// fewer than 4 GiB has fewer samples).
    sample: u32,
}

impl PreparedKey {
    pub(crate) fn new(statement: u32, sample: Option<usize>) -> Self {
        Self {
            statement,
            sample: sample.map_or(0, |sample| {
                u32::try_from(sample + 1).expect("a sample ordinal of a USDA layer")
            }),
        }
    }
}

impl PreparedValues {
    pub(crate) fn insert(&mut self, key: PreparedKey, prepared: PreparedValue) {
        if let Some(elements) = prepared.elements {
            self.arrays += 1;
            self.elements += elements;
        }
        if !prepared.errors.is_empty() {
            self.errors.insert(key, prepared.errors);
        }
        self.values.insert(key, prepared.value);
    }

    /// Takes the value read at `key` and why its elements did not convert.
    pub(crate) fn take(
        &mut self,
        key: PreparedKey,
    ) -> Option<(layerstack::Value, Vec<alloc::string::String>)> {
        let value = self.values.remove(&key)?;
        Some((value, self.errors.remove(&key).unwrap_or_default()))
    }

    /// Numeric arrays read.
    pub(crate) fn arrays(&self) -> usize {
        self.arrays
    }

    /// Elements of the numeric arrays read.
    pub(crate) fn elements(&self) -> usize {
        self.elements
    }
}

/// A numeric array or tuple read straight into its value.
pub(crate) struct PreparedValue {
    /// The array's or tuple's source range.
    pub span: Span,
    /// An array's elements; `None` for a tuple.
    pub elements: Option<usize>,
    pub value: layerstack::Value,
    /// Why elements did not convert, which emission reports.
    pub errors: Vec<alloc::string::String>,
}

pub(crate) fn numeric_width(name: &str) -> Option<usize> {
    match crate::numeric::storage_name(name) {
        "bool" | "uchar" | "int" | "uint" | "int64" | "uint64" | "half" | "float" | "double"
        | "timecode" | "duration" => Some(0),
        "int2" | "half2" | "float2" | "double2" => Some(2),
        "int3" | "half3" | "float3" | "double3" => Some(3),
        "int4" | "half4" | "float4" | "double4" | "quath" | "quatf" | "quatd" => Some(4),
        _ => None,
    }
}

fn trivia(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Whitespace
            | TokenKind::Newline
            | TokenKind::PythonComment
            | TokenKind::CppComment
            | TokenKind::BlockComment
    )
}

#[derive(Clone)]
struct LexicalCursor<'a> {
    source: &'a str,
    lexer: Lexer<'a>,
    lookahead: Option<Token>,
    /// Whether a line break precedes [`Self::lookahead`].
    broken: bool,
}

/// A literal an element component holds: a number, `true` or `false`.
#[derive(Clone, Copy)]
enum Literal {
    Number(Number),
    Bool(bool),
}

impl Literal {
    /// The literal as structural lowering holds it.
    fn value(self) -> ast::Value<'static> {
        match self {
            Self::Number(Number::Int(n)) => ast::Value::Int(n),
            Self::Number(Number::UInt(n)) => ast::Value::UInt(n),
            Self::Number(Number::Float(f)) => ast::Value::Number(f),
            Self::Bool(b) => ast::Value::Bool(b),
        }
    }

    fn number(self) -> Option<Number> {
        match self {
            Self::Number(number) => Some(number),
            Self::Bool(_) => None,
        }
    }
}

impl<'a> LexicalCursor<'a> {
    fn peek(&mut self) -> Option<Token> {
        if self.lookahead.is_none() {
            let mut broken = false;
            self.lookahead = self.lexer.find(|token| {
                broken |= token.kind == TokenKind::Newline;
                !trivia(token.kind)
            });
            self.broken = broken;
        }
        self.lookahead
    }
    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        self.peek().filter(|token| token.kind == kind)?;
        self.lookahead.take()
    }
    #[inline(always)]
    fn literal(&mut self) -> Option<Literal> {
        let negative = self.eat(TokenKind::Minus).is_some();
        let token = self.peek()?;
        let text = token.text(self.source);
        let literal = match token.kind {
            TokenKind::Number => {
                let value = Number::of(&crate::lower::parse_number_value(text))
                    .expect("a number literal parses to a number");
                Literal::Number(if !negative {
                    value
                } else {
                    match value {
                        Number::Int(0) if text == "0" => Number::Float(-0.0),
                        Number::Int(n) => Number::Int(-n),
                        // Match structural lowering: the signed minimum is an
                        // exact integer despite its unsigned magnitude. AOUSD
                        // Core §6.2; OpenUSD numeric literal classification.
                        Number::UInt(n) if n == i64::MAX as u64 + 1 => Number::Int(i64::MIN),
                        Number::UInt(n) => Number::Float(-(n as f64)),
                        Number::Float(n) => Number::Float(-n),
                    }
                })
            }
            TokenKind::Ident => match (text, negative) {
                ("inf", false) => Literal::Number(Number::Float(f64::INFINITY)),
                ("inf", true) => Literal::Number(Number::Float(f64::NEG_INFINITY)),
                ("nan", false) => Literal::Number(Number::Float(f64::NAN)),
                ("true" | "True", false) => Literal::Bool(true),
                ("false" | "False", false) => Literal::Bool(false),
                _ => return None,
            },
            _ => return None,
        };
        self.lookahead = None;
        Some(literal)
    }
}

/// The elements of a numeric array after its `[`, read one at a time: a
/// scalar, or a parenthesized tuple of `width` components.
#[derive(Clone)]
struct Elements<'a> {
    cursor: LexicalCursor<'a>,
    width: usize,
}

/// What [`Elements::next_element`] read.
enum Element {
    /// An element's components (the first `width.max(1)` of them).
    Components([Literal; 4]),
    /// The closing `]`, which ends at this offset.
    End(u32),
    /// Something no element or `]` starts with.
    Malformed,
}

impl Elements<'_> {
    fn next_element(&mut self) -> Element {
        if let Some(end) = self.cursor.eat(TokenKind::RightBracket) {
            return Element::End(end.span.end);
        }
        let Some((components, _)) = self.components() else {
            return Element::Malformed;
        };
        // A `,` on the next line is refused; the structural parser reports it.
        if self
            .cursor
            .peek()
            .is_some_and(|token| token.kind == TokenKind::Comma)
            && self.cursor.broken
        {
            return Element::Malformed;
        }
        self.cursor.eat(TokenKind::Comma);
        Element::Components(components)
    }

    /// One element's components, and where its tuple's `)` ends (0 for a
    /// scalar); `None` when they are not there. Inlined into the element
    /// loop, where it runs once per element of the largest arrays.
    #[inline(always)]
    fn components(&mut self) -> Option<([Literal; 4], u32)> {
        let mut components = [Literal::Bool(false); 4];
        if self.width == 0 {
            components[0] = self.cursor.literal()?;
            return Some((components, 0));
        }
        self.cursor.eat(TokenKind::LeftParen)?;
        for component in &mut components[..self.width] {
            *component = self.cursor.literal()?;
            self.cursor.eat(TokenKind::Comma);
        }
        let close = self.cursor.eat(TokenKind::RightParen)?;
        Some((components, close.span.end))
    }
}

/// Consume typed numeric events once, keeping a checkpoint for generic
/// grammar recovery. Conversion errors stay deferred until attribute emission
/// so phase boundaries, diagnostic ordering and ignored declarations match.
///
/// An array whose every element converts to the declared type is read into
/// its native buffer directly ([`native_array`]); one that does not is read
/// again element by element, as values, to keep its errors and recovery.
pub(crate) fn numeric_array<'a>(
    source: &'a str,
    start: u32,
    type_hint: &str,
    width: usize,
) -> Option<(PreparedValue, Lexer<'a>)> {
    let mut cursor = LexicalCursor {
        source,
        lexer: Lexer::at_offset(source, start),
        lookahead: None,
        broken: false,
    };
    cursor.eat(TokenKind::LeftBracket)?;
    let elements = Elements { cursor, width };
    if let Some(layout) = Layout::of(type_hint, width) {
        let mut native = elements.clone();
        if let Some(read) = native_array(&mut native, layout) {
            return Some((
                PreparedValue {
                    span: Span::new(start, read.end),
                    elements: Some(read.count),
                    value: layerstack::Value::TypedArray(read.array),
                    errors: Vec::new(),
                },
                native.cursor.lexer,
            ));
        }
    }
    let mut events = NumericEvents {
        elements,
        type_hint,
        count: 0,
        end: None,
        malformed: false,
        errors: Vec::new(),
    };
    let mut value = layerstack::Value::array_from_iter(&mut events, None);
    let end = events.end?;
    if events.malformed || events.count == 0 {
        return None;
    }
    // Growing vectors are compacted before ownership reaches the layer.
    // Only this sink owns the fresh buffer, so no copy-on-write clone occurs.
    compact_buffer(&mut value);
    Some((
        PreparedValue {
            span: Span::new(start, end),
            elements: Some(events.count),
            value,
            errors: events.errors,
        },
        events.elements.cursor.lexer,
    ))
}

struct NumericEvents<'a, 't> {
    elements: Elements<'a>,
    type_hint: &'t str,
    count: usize,
    end: Option<u32>,
    malformed: bool,
    errors: Vec<alloc::string::String>,
}

impl Iterator for NumericEvents<'_, '_> {
    type Item = layerstack::Value;
    fn next(&mut self) -> Option<Self::Item> {
        if self.malformed || self.end.is_some() {
            return None;
        }
        let components = match self.elements.next_element() {
            Element::Components(components) => components,
            Element::End(end) => {
                self.end = Some(end);
                return None;
            }
            Element::Malformed => {
                self.malformed = true;
                return None;
            }
        };
        self.count += 1;
        let width = self.elements.width;
        let components = components.map(Literal::value);
        match crate::numeric::element(&components[..width.max(1)], self.type_hint, width) {
            Ok(value) => Some(value),
            Err(message) => {
                self.errors.push(message);
                Some(layerstack::Value::Blocked)
            }
        }
    }
}

/// The native element of a numeric array type, resolved once per array:
/// what [`crate::numeric::element`] converts each element to.
#[derive(Clone, Copy)]
enum Layout {
    Bool,
    UChar,
    Int,
    UInt,
    Int64,
    UInt64,
    Half,
    Float,
    Double,
    TimeCode,
    Duration,
    Vec2i,
    Vec3i,
    Vec4i,
    Vec2h,
    Vec3h,
    Vec4h,
    Vec2f,
    Vec3f,
    Vec4f,
    Vec2d,
    Vec3d,
    Vec4d,
    Quath,
    Quatf,
    Quatd,
}

impl Layout {
    /// The layout of `type_hint`'s elements, `width` components each.
    fn of(type_hint: &str, width: usize) -> Option<Self> {
        Some(if width == 0 {
            // A scalar converts by its declared name (`convert_scalar`).
            match type_hint {
                "bool" => Self::Bool,
                "uchar" => Self::UChar,
                "int" => Self::Int,
                "uint" => Self::UInt,
                "int64" => Self::Int64,
                "uint64" => Self::UInt64,
                "half" => Self::Half,
                "float" => Self::Float,
                "double" => Self::Double,
                "timecode" => Self::TimeCode,
                "duration" => Self::Duration,
                _ => return None,
            }
        } else {
            // A tuple converts by its storage type (`dimensioned_value`).
            match crate::numeric::storage_name(type_hint.strip_suffix("[]").unwrap_or(type_hint)) {
                "int2" => Self::Vec2i,
                "int3" => Self::Vec3i,
                "int4" => Self::Vec4i,
                "half2" => Self::Vec2h,
                "half3" => Self::Vec3h,
                "half4" => Self::Vec4h,
                "float2" => Self::Vec2f,
                "float3" => Self::Vec3f,
                "float4" => Self::Vec4f,
                "double2" => Self::Vec2d,
                "double3" => Self::Vec3d,
                "double4" => Self::Vec4d,
                "quath" => Self::Quath,
                "quatf" => Self::Quatf,
                "quatd" => Self::Quatd,
                _ => return None,
            }
        })
    }
}

/// Reads the tuple value of a `type_hint` attribute (a vector or
/// quaternion of `width` components) at `start` into its value, converting
/// it as [`crate::numeric::element`] does; `None` when it is not one whose
/// components all convert, which the value path then reads and reports.
pub(crate) fn numeric_tuple<'a>(
    source: &'a str,
    start: u32,
    type_hint: &str,
    width: usize,
) -> Option<(PreparedValue, Lexer<'a>)> {
    let layout = Layout::of(type_hint, width).filter(|_| width != 0)?;
    let cursor = LexicalCursor {
        source,
        lexer: Lexer::at_offset(source, start),
        lookahead: None,
        broken: false,
    };
    let mut elements = Elements { cursor, width };
    let (components, end) = elements.components()?;
    let value = native_value(&components, layout)?;
    Some((
        PreparedValue {
            span: Span::new(start, end),
            elements: None,
            value,
            errors: Vec::new(),
        },
        elements.cursor.lexer,
    ))
}

/// The value of one vector or quaternion element of `layout`, as
/// [`native_array`] converts each element of an array.
fn native_value(components: &[Literal; 4], layout: Layout) -> Option<layerstack::Value> {
    use crate::numeric::component;
    use layerstack::Value;
    Some(match layout {
        Layout::Vec2i => Value::Vec2i(tuple(components, component::int)?),
        Layout::Vec3i => Value::Vec3i(tuple(components, component::int)?),
        Layout::Vec4i => Value::Vec4i(tuple(components, component::int)?),
        Layout::Vec2h => Value::Vec2h(tuple(components, component::half)?),
        Layout::Vec3h => Value::Vec3h(tuple(components, component::half)?),
        Layout::Vec4h => Value::Vec4h(tuple(components, component::half)?),
        Layout::Vec2f => Value::Vec2f(tuple(components, component::float)?),
        Layout::Vec3f => Value::Vec3f(tuple(components, component::float)?),
        Layout::Vec4f => Value::Vec4f(tuple(components, component::float)?),
        Layout::Vec2d => Value::Vec2d(tuple(components, component::double)?),
        Layout::Vec3d => Value::Vec3d(tuple(components, component::double)?),
        Layout::Vec4d => Value::Vec4d(tuple(components, component::double)?),
        Layout::Quath => Value::Quath(quaternion(tuple(components, component::half)?)),
        Layout::Quatf => Value::Quatf(quaternion(tuple(components, component::float)?)),
        Layout::Quatd => Value::Quatd(quaternion(tuple(components, component::double)?)),
        _ => return None,
    })
}

/// The first `N` components, each converted by `convert`.
fn tuple<C: Copy + Default, const N: usize>(
    components: &[Literal; 4],
    convert: impl Fn(Option<Number>) -> Option<C>,
) -> Option<[C; N]> {
    let mut out = [C::default(); N];
    for (out, literal) in out.iter_mut().zip(components) {
        *out = convert(literal.number())?;
    }
    Some(out)
}

/// Quaternions are stored as (i, j, k, r) but authored as (r, i, j, k) in
/// USDA text per AOUSD Core §16.3.10.22.
fn quaternion<C: Copy>([r, i, j, k]: [C; 4]) -> [C; 4] {
    [i, j, k, r]
}

/// A numeric array read into its native buffer.
struct NativeArray {
    array: layerstack::TypedArray,
    count: usize,
    end: u32,
}

/// Reads the elements into the native buffer of `layout`, converting each
/// as [`crate::numeric::element`] does; `None` when the array is empty or
/// malformed, or an element does not convert, which the value path reports.
fn native_array(elements: &mut Elements<'_>, layout: Layout) -> Option<NativeArray> {
    use crate::numeric::component;
    use layerstack::TypedArray;

    fn read<E>(
        elements: &mut Elements<'_>,
        convert: impl Fn(&[Literal; 4]) -> Option<E>,
    ) -> Option<(Arc<Vec<E>>, u32)> {
        let mut items = Vec::new();
        loop {
            match elements.next_element() {
                Element::Components(components) => items.push(convert(&components)?),
                Element::End(end) if !items.is_empty() => {
                    items.shrink_to_fit();
                    return Some((Arc::new(items), end));
                }
                Element::End(_) | Element::Malformed => return None,
            }
        }
    }
    /// An integer element of type `ty`, `min..=max`.
    fn integer<T: Into<i128> + TryFrom<i128>>(
        literal: Literal,
        min: T,
        max: T,
        ty: &str,
    ) -> Option<T> {
        let n = literal
            .number()?
            .to_integer(min.into(), max.into(), ty)
            .ok()?;
        T::try_from(n).ok()
    }

    macro_rules! native {
        ($kind:ident, $convert:expr) => {{
            let (items, end) = read(elements, $convert)?;
            let count = items.len();
            (TypedArray::$kind(items), count, end)
        }};
    }
    let (array, count, end) = match layout {
        Layout::Bool => native!(Bool, |c| match c[0] {
            Literal::Bool(b) => Some(b),
            Literal::Number(number) => Some(number.to_bool()),
        }),
        Layout::UChar => native!(UChar, |c| integer(c[0], u8::MIN, u8::MAX, "uchar")),
        Layout::Int => native!(Int, |c| integer(c[0], i32::MIN, i32::MAX, "int")),
        Layout::UInt => native!(UInt, |c| integer(c[0], u32::MIN, u32::MAX, "uint")),
        Layout::Int64 => native!(Int64, |c| integer(c[0], i64::MIN, i64::MAX, "int64")),
        Layout::UInt64 => native!(UInt64, |c| integer(c[0], u64::MIN, u64::MAX, "uint64")),
        Layout::Half => native!(Half, |c| Some(c[0].number()?.to_half())),
        Layout::Float => native!(Float, |c| Some(c[0].number()?.to_float())),
        Layout::Double => native!(Double, |c| Some(c[0].number()?.to_double())),
        Layout::TimeCode => native!(TimeCode, |c| Some(c[0].number()?.to_double())),
        Layout::Duration => native!(Duration, |c| Some(c[0].number()?.to_double())),
        Layout::Vec2i => native!(Vec2i, |c| tuple::<_, 2>(c, component::int)),
        Layout::Vec3i => native!(Vec3i, |c| tuple::<_, 3>(c, component::int)),
        Layout::Vec4i => native!(Vec4i, |c| tuple::<_, 4>(c, component::int)),
        Layout::Vec2h => native!(Vec2h, |c| tuple::<_, 2>(c, component::half)),
        Layout::Vec3h => native!(Vec3h, |c| tuple::<_, 3>(c, component::half)),
        Layout::Vec4h => native!(Vec4h, |c| tuple::<_, 4>(c, component::half)),
        Layout::Vec2f => native!(Vec2f, |c| tuple::<_, 2>(c, component::float)),
        Layout::Vec3f => native!(Vec3f, |c| tuple::<_, 3>(c, component::float)),
        Layout::Vec4f => native!(Vec4f, |c| tuple::<_, 4>(c, component::float)),
        Layout::Vec2d => native!(Vec2d, |c| tuple::<_, 2>(c, component::double)),
        Layout::Vec3d => native!(Vec3d, |c| tuple::<_, 3>(c, component::double)),
        Layout::Vec4d => native!(Vec4d, |c| tuple::<_, 4>(c, component::double)),
        Layout::Quath => native!(Quath, |c| Some(quaternion(tuple(c, component::half)?))),
        Layout::Quatf => native!(Quatf, |c| Some(quaternion(tuple(c, component::float)?))),
        Layout::Quatd => native!(Quatd, |c| Some(quaternion(tuple(c, component::double)?))),
    };
    Some(NativeArray { array, count, end })
}

fn compact_buffer(value: &mut layerstack::Value) {
    use alloc::sync::Arc;
    use layerstack::{TypedArray, Value};
    let Value::TypedArray(array) = value else {
        return;
    };
    macro_rules! compact {
        ($($kind:ident),*) => { match array {
            $(TypedArray::$kind(items) => Arc::get_mut(items).expect("fresh numeric buffer").shrink_to_fit(),)*
            _ => unreachable!("supported numeric event types only"),
        }};
    }
    compact!(
        Bool, UChar, Int, UInt, Int64, UInt64, Half, Float, Double, TimeCode, Duration, Vec2d,
        Vec3d, Vec4d, Vec2f, Vec3f, Vec4f, Vec2h, Vec3h, Vec4h, Vec2i, Vec3i, Vec4i, Quatd, Quatf,
        Quath
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{format, string::String};
    use layerstack::{AssetResolveError, InMemoryStore, ResolvedAsset};

    struct NoAssets;
    impl AssetResolver for NoAssets {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &TokenInterner,
            _: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            Err(AssetResolveError::NotFound)
        }
        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
    }

    /// Minimized fuzz inputs: a prim statement whose name recovery lowers
    /// as a lone quote (`def"`) or as an empty name (`def def`) reads with
    /// error diagnostics; both once panicked.
    #[test]
    fn malformed_prim_names_read_with_errors() {
        for source in [
            "def\"",
            "def def",
            "#usda 1.0\ndef \"A\"\n{\n    def \"\"\n}\n",
        ] {
            let store = InMemoryStore::default();
            let read = read_usda(
                source,
                LayerId(1),
                &store.tokens,
                &store.paths,
                &mut NoAssets,
            );
            let errors = read
                .parse_diagnostics
                .iter()
                .chain(&read.lower_diagnostics)
                .chain(&read.emitted.diagnostics)
                .filter(|d| d.severity == crate::diagnostic::Severity::Error)
                .count();
            assert!(errors > 0, "{source:?}");
        }
    }

    fn compare(source: &str) -> ReadStats {
        let old = InMemoryStore::default();
        let parsed = crate::parser::parse(source);
        let emitted = crate::emit::emit(
            &parsed.layer,
            LayerId(1),
            &old.tokens,
            &old.paths,
            &mut NoAssets,
        );
        let new = InMemoryStore::default();
        let imported = read_usda(source, LayerId(1), &new.tokens, &new.paths, &mut NoAssets);
        let diagnostics: Vec<_> = imported
            .parse_diagnostics
            .into_iter()
            .chain(imported.lower_diagnostics)
            .collect();
        assert_eq!(
            format!("{:?}", parsed.diagnostics),
            format!("{diagnostics:?}"),
            "parse/lower: {source}"
        );
        assert_eq!(
            format!("{:?}", emitted.diagnostics),
            format!("{:?}", imported.emitted.diagnostics),
            "emit: {source}"
        );
        assert_eq!(emitted.rejected, imported.emitted.rejected);
        // Writer comparison retains declared types, empty array kinds, float
        // precision, signed zero, quaternion order, samples and variant sites.
        let old_saved = crate::save::save_usda(&emitted.layer, &old.tokens, &old.paths);
        let new_saved = crate::save::save_usda(&imported.emitted.layer, &new.tokens, &new.paths);
        assert_eq!(old_saved, new_saved, "saved layer: {source}");
        if old_saved.is_err() {
            assert_eq!(
                emitted.layer, imported.emitted.layer,
                "unsupported authored values: {source}"
            );
        }
        imported.stats
    }

    #[test]
    fn direct_numeric_arrays_match_the_ast_emitter() {
        for ty in [
            "bool", "uchar", "int", "uint", "int64", "uint64", "half", "float", "double",
            "timecode", "duration",
        ] {
            assert_eq!(
                compare(&format!(
                    "#usda 1.0\ndef \"A\" {{ {ty}[] values = [0, 1, 2,]; }}"
                ))
                .numeric_arrays,
                1
            );
            assert_eq!(
                compare(&format!("#usda 1.0\ndef \"A\" {{ {ty}[] values = []; }}")).numeric_arrays,
                0
            );
        }
        for ty in [
            "float2",
            "double2",
            "int2",
            "half2",
            "texCoord2f",
            "texCoord2d",
            "texCoord2h",
            "float3",
            "int3",
            "double3",
            "half3",
            "color3f",
            "point3d",
            "normal3h",
            "vector3f",
            "texCoord3f",
            "float4",
            "int4",
            "double4",
            "half4",
            "color4d",
            "quath",
            "quatf",
            "quatd",
        ] {
            let width = numeric_width(ty).unwrap();
            let tuple = (0..width)
                .map(|i| format!("{}", i + 1))
                .collect::<Vec<_>>()
                .join(", ");
            let stats = compare(&format!(
                "#usda 1.0\ndef \"A\" {{ {ty}[] values = [({tuple}), ({tuple}),]; }}"
            ));
            assert_eq!(stats.numeric_elements, 2);
        }
        compare(
            "#usda 1.0\ndef \"A\" { double[] values = [-0, -0.0, inf, -inf, nan, 18446744073709551615, -9223372036854775808]; }",
        );
        compare("#usda 1.0\ndef \"A\" { float3[] values = [(1/*a*/2, 3,), (4, - 0, nan)]; }");
    }

    #[test]
    fn native_buffers_hold_what_value_conversion_holds() {
        // Every element converts, so the array is read straight into its
        // native buffer; the values (bits included) are those of the value
        // path, for scalars by their declared type and tuples by storage.
        let arrays = [
            ("bool", "[0, 1, -0.0, nan, true, False, 2.5]"),
            ("uchar", "[0, 255, 1.9, -0.5]"),
            ("int", "[-2147483648, 2147483647, 2.7, -2.7, -0]"),
            ("uint", "[0, 4294967295, 7.5]"),
            ("int64", "[-9223372036854775808, 9223372036854775807, 1e3]"),
            ("uint64", "[18446744073709551615, 0, 3]"),
            ("half", "[1, 65504, 0.1, -0, inf, -inf, 16777217, 1e-8]"),
            (
                "float",
                "[16777217, 9007199254740993, 0.1, -0, nan, 1e39, -1e-46]",
            ),
            (
                "double",
                "[0.1, -0, 9223372036854775808, -9223372036854775809, 1e308]",
            ),
            ("timecode", "[1, 2.5, -inf]"),
            ("duration", "[1, -0, 2.5, -inf, nan]"),
            ("int2", "[(1, -2), (2.9, -2.9)]"),
            ("int4", "[(1, 2, 3, 4)]"),
            ("half3", "[(0.1, 1, 65519), (-0, 2, 3)]"),
            ("float3", "[(16777217, 0.1, -0), (9007199254740993, 1, 2)]"),
            ("double2", "[(0.1, 1e-300)]"),
            ("color3f", "[(1, 0.5, 0)]"),
            ("point3d", "[(1, 2, 3)]"),
            ("normal3h", "[(0, 0, 1)]"),
            ("texCoord2f", "[(0.25, 0.75)]"),
            ("vector3f", "[(1, 0, 0),]"),
            ("quath", "[(1, 0.5, 0.25, 0)]"),
            ("quatf", "[(1, 2, 3, 4)]"),
            ("quatd", "[(0.1, 0.2, 0.3, 0.4)]"),
        ];
        for (ty, value) in arrays {
            let source = format!("#usda 1.0\ndef \"A\" {{ {ty}[] values = {value}; }}");
            let stats = compare(&source);
            assert_eq!(stats.numeric_arrays, 1, "{source}");
            let parsed = crate::parser::parse(&source);
            let old = InMemoryStore::default();
            let emitted = crate::emit::emit(
                &parsed.layer,
                LayerId(1),
                &old.tokens,
                &old.paths,
                &mut NoAssets,
            );
            let new = InMemoryStore::default();
            let imported = read_usda(&source, LayerId(1), &new.tokens, &new.paths, &mut NoAssets);
            let value = |layer: &layerstack::Layer| {
                let prim = layer
                    .prims()
                    .values()
                    .find(|prim| !prim.properties.is_empty())
                    .expect("the prim");
                prim.properties[0].spec.default.clone()
            };
            let (expected, read) = (value(&emitted.layer), value(&imported.emitted.layer));
            assert!(
                matches!(read, Some(layerstack::Value::TypedArray(_))),
                "{source}: {read:?}"
            );
            // `Debug` tells `-0.0` from `0.0` and compares NaN, which `==`
            // never finds equal.
            assert_eq!(format!("{expected:?}"), format!("{read:?}"), "{source}");
        }
    }

    #[test]
    fn tuples_read_as_values_hold_what_value_conversion_holds() {
        // Vector and quaternion attribute values (defaults and samples) are
        // read straight into their values; ones that do not convert, or are
        // not one tuple of the declared width, take the value path, which
        // reports them as before.
        let source = r#"#usda 1.0
def "A" {
    double3 xformOp:translate = (1, -2.5, 1e300)
    double3 xformOp:translate.timeSamples = { 0: (1, 2, 3), 1: None, 2: (4, 5, 6) }
    float3 xformOp:scale = (16777217, 0.1, -0)
    color3f primvars:displayColor = (1, 0.5, 0) ( interpolation = "constant" )
    point3d p = (1, 2, 3)
    half3 h = (0.1, 65519, -0)
    int2 i = (2.9, -2.9)
    int4 j = (1, 2, 3, 4)
    texCoord2f t = (0.25, 0.75)
    quatf q = (1, 2, 3, 4)
    quath qh = (1, 0.5, 0.25, 0)
    quatd qd = (0.1, 0.2, 0.3, 0.4)
    float3 short = (1, 2)
    float3 long = (1, 2, 3, 4)
    float3 flag = (1, true, 3)
    int3 range = (2147483648, 0, 0)
    int2 fraction = (1e10, 0)
    double3 spaced = ( 1 , 2 /* two */ , 3 , )
    float3 after = (1, 2, 3) junk
}
"#;
        let stats = compare(source);
        assert_eq!(stats.numeric_arrays, 0, "tuples are no arrays");
    }

    #[test]
    fn samples_variants_repeated_declarations_and_fallback_keep_authored_content() {
        let stats = compare(
            r#"#usda 1.0
( customLayerData = { int[] numbers = [1, 2] } )
def "A" {
    int[] ids = [1, 2]
    int[] ids.timeSamples = { 3: [3], -1: [1, 2], 4: None, 5: [] }
    int[] ids = [4, 5]
    variantSet "look" = {
        "x" { point3f[] points = [(1, 2, 3)]; }
        "y" { point3f[] points = [(4, 5, 6)]; }
    }
    string[] names = ["a", "b"]
    matrix2d[] matrices = [((1, 0), (0, 1))]
    int[] edits = edit [append [1, 2]]
}
"#,
        );
        assert_eq!(stats.numeric_arrays, 6);
        compare(
            r#"#usda 1.0
def "A" {
    float[] primvars:weights = [1, 2]
    string user:label = "preserve the exact name"
    token subsetFamily:materialBind:familyType = "partition"
    rel collection:lights:includes = </A>
}
"#,
        );
        compare("#usda 1.0\r\ndef \"é\" { float[] x = [1, 2] # comment\r\n}");
    }

    #[test]
    fn malformed_shapes_and_numeric_range_failures_match_recovery_and_diagnostics() {
        for value in [
            "[1, 2147483648]",
            "[-1, 18446744073709551615]",
            "[1, \"bad\"]",
            "[1,,2]",
            "[1, -nan]",
            "[1, - ]",
            "[1,",
            "[None, 2]",
            "[edit [clear]]",
        ] {
            compare(&format!(
                "#usda 1.0\ndef \"A\" {{ int[] x = {value}\n int y = 7\n }}"
            ));
        }
        for value in [
            "[(1, 2)]",
            "[(1, 2, 3, 4)]",
            "[(1, 2, \"x\")]",
            "[(1,2,3), (4,5)]",
            "[(1,2,3),",
            "[[1, 2, 3]]",
        ] {
            compare(&format!(
                "#usda 1.0\ndef \"A\" {{ float3[] x = {value}\n int y = 7\n }}"
            ));
        }
    }

    #[test]
    fn events_preserve_continuations_fallback_and_deferred_error_order() {
        compare(
            r#"#usda 1.0
( customLayerData = { string marker = "[123]" } )
def "A" {
    float3[] points = [(1, 2, 3)] ( customData = { int[] ids = [4, 5] } )
    int[] overflow = [2147483648, -2147483649]
    uint[] samples.timeSamples = { 1: [0, -1], 2: [2, 3], 3: [4294967296] }
    # A continuation must keep exact source offsets and names.
    bool[] user:flags = [True, false, 1]
    double[] infinities = [-inf, inf, nan, -0] /* after the closing bracket */
    int[] ignored.connect = </A.overflow>
    def "Child" { float[] weights = [0.5, 1.0]; }
}
"#,
        );
        // Speculative conversion must return to the shared grammar even when
        // the unsupported element occurs after a large valid prefix.
        let prefix = "(1, 2, 3),".repeat(1_000);
        compare(&format!(
            "#usda 1.0\ndef \"A\" {{ float3[] points = [{prefix}(4, 5)]\n int after = 7\n }}"
        ));
        compare(&format!(
            "#usda 1.0\ndef \"A\" {{ float3[] points = [{prefix}(4, 5, 6)]\n string after = \"[not an array]\"\n }}"
        ));
    }

    #[test]
    fn reads_hold_a_window_of_tokens_across_rollback() {
        // Tokens are released as the parser consumes them, except where a
        // variant selection entry may roll back; recovery and diagnostics
        // match the eager parse, here with an entry that rolls back (`bad`)
        // after many released tokens.
        let mut source = String::from("#usda 1.0\n");
        for i in 0..200 {
            source.push_str(&format!(
                "def \"P{i}\" (\n    variants = {{ string look = \"a\"\n shape = \"b\"\n \"q\" = \"c\" }}\n) {{\n    int a = {i}\n    token t = \"x\"\n}}\n"
            ));
            if i == 150 {
                source.push_str("over \"Bad\" ( variants = { bad } ) {}\n");
            }
        }
        let stats = compare(&source);
        let window = 2 * crate::token_source::RELEASE_AFTER;
        assert!(stats.tokens <= window, "{stats:?}");
    }

    #[test]
    fn prims_lowered_as_they_close_keep_diagnostics_in_source_order() {
        // Lowering reports escapes in names and strings and malformed
        // splines; reading lowers each prim as it closes, yet reports what
        // lowering the whole tree reports, in the same order.
        let source = r#"#usda 1.0
(
    doc = "layer \xff"
)
def "A\1" (
    doc = "prim \xfe"
    references = @@@a\@@@b@@@
) {
    int i.spline = { 1: 2 }
    def "B\2" {
        string s = "\xfd"
        double d.spline = { bad }
    }
    variantSet "v" = {
        "x" ( doc = "branch \xfc" ) {
            def "C\3" { custom string t = "\xfb"; }
        }
    }
    rel r = </A>
    string after = "\xfa"
}
def "D\4" {}
"#;
        compare(source);
        let store = InMemoryStore::default();
        let imported = read_usda(
            source,
            LayerId(1),
            &store.tokens,
            &store.paths,
            &mut NoAssets,
        );
        assert_eq!(
            imported.lower_diagnostics.len(),
            12,
            "{:#?}",
            imported.lower_diagnostics
        );
    }

    #[test]
    fn large_point_arrays_do_not_expand_the_structural_tree() {
        let mut source = String::from("#usda 1.0\ndef Mesh \"A\" { point3f[] points = [");
        for _ in 0..100_000 {
            source.push_str("(1, 2, 3),");
        }
        source.push_str("]\n}\n");
        let store = InMemoryStore::default();
        let imported = read_usda(
            &source,
            LayerId(1),
            &store.tokens,
            &store.paths,
            &mut NoAssets,
        );
        assert!(imported.parse_diagnostics.is_empty());
        assert!(imported.lower_diagnostics.is_empty());
        assert!(imported.emitted.diagnostics.is_empty());
        assert_eq!(imported.stats.numeric_elements, 100_000);
        assert!(imported.stats.syntax_nodes < 30, "no element CST nodes");
        assert!(imported.stats.tokens < 30, "no retained element tokens");
        let points = &imported
            .emitted
            .layer
            .prims()
            .values()
            .find(|prim| !prim.properties.is_empty())
            .unwrap()
            .properties[0]
            .spec
            .default;
        let Some(layerstack::Value::TypedArray(array)) = points else {
            panic!("native buffer")
        };
        assert_eq!(array.len(), 100_000);
        assert_eq!(array.capacity(), 100_000, "compact the final buffer");
    }
}
