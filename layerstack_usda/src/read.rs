// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene import without materializing numeric property arrays as syntax trees.

use alloc::{collections::BTreeMap, vec::Vec};
use layerstack::{AssetResolver, LayerId, PathInterner, TokenInterner};

use crate::{
    Span, ast,
    diagnostic::Diagnostic,
    emit::EmitResult,
    lexer::{Lexer, Token, TokenKind},
};

/// Work counters for one USDA import, excluding recursively resolved layers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadStats {
    /// Tokens retained for grammar navigation, including fallback values.
    pub tokens: usize,
    /// Nodes retained for structural lowering.
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
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> ReadResult {
    let (parsed, arrays, token_count) = crate::parser::parse_for_read(source);
    let stats = ReadStats {
        tokens: token_count,
        syntax_nodes: parsed.tree.len(),
        numeric_arrays: arrays.len(),
        numeric_elements: arrays.values().map(|array| array.count).sum(),
    };
    let lowered = crate::lower::lower(&parsed.tree, source);
    drop(parsed.tree);
    let emitted =
        crate::emit::emit_with_arrays(&lowered.layer, layer_id, tokens, paths, resolver, arrays);
    ReadResult {
        emitted,
        parse_diagnostics: parsed.diagnostics,
        lower_diagnostics: lowered.diagnostics,
        stats,
    }
}

pub(crate) type PreparedArrays = BTreeMap<(u32, Option<usize>), PreparedArray>;

pub(crate) struct PreparedArray {
    pub span: Span,
    pub count: usize,
    pub value: layerstack::Value,
    pub errors: Vec<alloc::string::String>,
}

pub(crate) fn numeric_width(name: &str) -> Option<usize> {
    match name {
        "bool" | "uchar" | "int" | "uint" | "int64" | "uint64" | "half" | "float" | "double"
        | "timecode" => Some(0),
        "int2" | "half2" | "float2" | "double2" | "texCoord2h" | "texCoord2f" | "texCoord2d" => {
            Some(2)
        }
        "int3" | "half3" | "float3" | "double3" | "color3h" | "color3f" | "color3d" | "point3h"
        | "point3f" | "point3d" | "normal3h" | "normal3f" | "normal3d" | "vector3h"
        | "vector3f" | "vector3d" | "texCoord3h" | "texCoord3f" | "texCoord3d" => Some(3),
        "int4" | "half4" | "float4" | "double4" | "color4h" | "color4f" | "color4d" | "quath"
        | "quatf" | "quatd" => Some(4),
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

struct LexicalCursor<'a> {
    source: &'a str,
    lexer: Lexer<'a>,
    lookahead: Option<Token>,
}

impl<'a> LexicalCursor<'a> {
    fn peek(&mut self) -> Option<Token> {
        if self.lookahead.is_none() {
            self.lookahead = self.lexer.find(|token| !trivia(token.kind));
        }
        self.lookahead
    }
    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        self.peek().filter(|token| token.kind == kind)?;
        self.lookahead.take()
    }
    fn scalar(&mut self) -> Option<ast::Value<'a>> {
        let negative = self.eat(TokenKind::Minus).is_some();
        let token = self.peek()?;
        let text = token.text(self.source);
        let value = match token.kind {
            TokenKind::Number => {
                let value = crate::lower::parse_number_value(text);
                if !negative {
                    value
                } else {
                    match value {
                        ast::Value::Int(0) if text == "0" => ast::Value::Number(-0.0),
                        ast::Value::Int(n) => ast::Value::Int(-n),
                        ast::Value::UInt(n) => ast::Value::Number(-(n as f64)),
                        ast::Value::Number(n) => ast::Value::Number(-n),
                        _ => unreachable!("numeric literal"),
                    }
                }
            }
            TokenKind::Ident => match (text, negative) {
                ("inf", false) => ast::Value::Number(f64::INFINITY),
                ("inf", true) => ast::Value::Number(f64::NEG_INFINITY),
                ("nan", false) => ast::Value::Number(f64::NAN),
                ("true" | "True", false) => ast::Value::Bool(true),
                ("false" | "False", false) => ast::Value::Bool(false),
                _ => return None,
            },
            _ => return None,
        };
        self.lookahead = None;
        Some(value)
    }
}

/// Consume typed numeric events once, keeping a checkpoint for generic
/// grammar recovery. Conversion errors stay deferred until attribute emission
/// so phase boundaries, diagnostic ordering and ignored declarations match.
pub(crate) fn numeric_array<'a>(
    source: &'a str,
    start: u32,
    type_hint: &str,
    width: usize,
) -> Option<(PreparedArray, Lexer<'a>)> {
    let mut cursor = LexicalCursor {
        source,
        lexer: Lexer::at_offset(source, start),
        lookahead: None,
    };
    cursor.eat(TokenKind::LeftBracket)?;
    let mut events = NumericEvents {
        cursor,
        type_hint,
        width,
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
        PreparedArray {
            span: Span::new(start, end),
            count: events.count,
            value,
            errors: events.errors,
        },
        events.cursor.lexer,
    ))
}

struct NumericEvents<'a, 't> {
    cursor: LexicalCursor<'a>,
    type_hint: &'t str,
    width: usize,
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
        if let Some(end) = self.cursor.eat(TokenKind::RightBracket) {
            self.end = Some(end.span.end);
            return None;
        }
        let mut components = core::array::from_fn::<_, 4, _>(|_| ast::Value::Blocked);
        let valid = (|| {
            if self.width != 0 {
                self.cursor.eat(TokenKind::LeftParen)?;
            }
            for component in &mut components[..self.width.max(1)] {
                *component = self.cursor.scalar()?;
                if self.width != 0 {
                    self.cursor.eat(TokenKind::Comma);
                }
            }
            if self.width != 0 {
                self.cursor.eat(TokenKind::RightParen)?;
            }
            Some(())
        })()
        .is_some();
        if !valid {
            self.malformed = true;
            return None;
        }
        self.count += 1;
        self.cursor.eat(TokenKind::Comma);
        match crate::numeric::element(&components[..self.width.max(1)], self.type_hint, self.width)
        {
            Ok(value) => Some(value),
            Err(message) => {
                self.errors.push(message);
                Some(layerstack::Value::Blocked)
            }
        }
    }
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
        Bool, UChar, Int, UInt, Int64, UInt64, Half, Float, Double, TimeCode, Vec2d, Vec3d, Vec4d,
        Vec2f, Vec3f, Vec4f, Vec2h, Vec3h, Vec4h, Vec2i, Vec3i, Vec4i, Quatd, Quatf, Quath
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
            _: &mut TokenInterner,
            _: &mut PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            Err(AssetResolveError::NotFound)
        }
        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
    }

    fn compare(source: &str) -> ReadStats {
        let mut old = InMemoryStore::default();
        let parsed = crate::parser::parse(source);
        let emitted = crate::emit::emit(
            &parsed.layer,
            LayerId(1),
            &mut old.tokens,
            &mut old.paths,
            &mut NoAssets,
        );
        let mut new = InMemoryStore::default();
        let imported = read_usda(
            source,
            LayerId(1),
            &mut new.tokens,
            &mut new.paths,
            &mut NoAssets,
        );
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
            "timecode",
        ] {
            assert_eq!(
                compare(&format!(
                    "#usda 1.0\ndef \"A\" {{ {ty}[] values = [0, 1, 2,] }}"
                ))
                .numeric_arrays,
                1
            );
            assert_eq!(
                compare(&format!("#usda 1.0\ndef \"A\" {{ {ty}[] values = [] }}")).numeric_arrays,
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
                "#usda 1.0\ndef \"A\" {{ {ty}[] values = [({tuple}), ({tuple}),] }}"
            ));
            assert_eq!(stats.numeric_elements, 2);
        }
        compare(
            "#usda 1.0\ndef \"A\" { double[] values = [-0, -0.0, inf, -inf, nan, 18446744073709551615, -9223372036854775808] }",
        );
        compare("#usda 1.0\ndef \"A\" { float3[] values = [(1/*a*/2, 3,), (4, - 0, nan)] }");
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
        "x" { point3f[] points = [(1, 2, 3)] }
        "y" { point3f[] points = [(4, 5, 6)] }
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
    def "Child" { float[] weights = [0.5, 1.0] }
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
    fn large_point_arrays_do_not_expand_the_structural_tree() {
        let mut source = String::from("#usda 1.0\ndef Mesh \"A\" { point3f[] points = [");
        for _ in 0..100_000 {
            source.push_str("(1, 2, 3),");
        }
        source.push_str("] }");
        let mut store = InMemoryStore::default();
        let imported = read_usda(
            &source,
            LayerId(1),
            &mut store.tokens,
            &mut store.paths,
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
            .prims
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
