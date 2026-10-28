// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Recursive descent parser for the USDA text format.
//!
//! Consumes a token stream from [`crate::lexer`] and produces a lossless
//! CST ([`crate::cst::SyntaxTree`]). Supports error recovery: malformed
//! input produces partial trees with diagnostics rather than hard failures.
//!
//! Spec: AOUSD Core §16.2.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::Span;
use crate::ast::ParseResult;
use crate::cst::{CstParseResult, SyntaxKind, TreeBuilder};
use crate::diagnostic::Diagnostic;
use crate::lexer::{Token, TokenKind, tokenize};
use crate::lower::PrimLowering;

/// Parses a USDA source string into an AST through the shared CST grammar.
///
/// The temporary tree omits consumed trivia and array/tuple punctuation. Use
/// [`parse_cst`] when every token must remain available for lossless editing.
pub fn parse(source: &str) -> ParseResult<'_> {
    let cst_result = parse_tree::<false>(source);
    let mut ast_result = crate::lower::lower(&cst_result.tree, source);
    // Merge CST diagnostics into AST result.
    let mut all_diag = cst_result.diagnostics;
    all_diag.append(&mut ast_result.diagnostics);
    ast_result.diagnostics = all_diag;
    ast_result
}

/// Parses a USDA source string into a lossless CST.
pub fn parse_cst(source: &str) -> CstParseResult {
    parse_tree::<true>(source)
}

fn parse_tree<const LOSSLESS: bool>(source: &str) -> CstParseResult {
    let tokens = tokenize(source);
    let mut parser = Parser::<LOSSLESS>::new(source, tokens);
    parser.parse_source_file();
    let tree = parser.builder.finish();
    CstParseResult {
        tree,
        diagnostics: parser.diagnostics,
    }
}

/// Room for the syntax nodes of a read: the layer's header and metadata,
/// and the open prims' statements but not their closed child prims'.
const READ_NODES: usize = 64;

/// A layer parsed and lowered for import (see [`parse_for_read`]).
pub(crate) struct ReadParse<'a> {
    /// The lowered layer and the diagnostics of lowering.
    pub lowered: ParseResult<'a>,
    /// Syntax and recovery diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Numeric arrays and tuples read straight into their values.
    pub arrays: crate::read::PreparedValues,
    /// The most tokens held at once.
    pub tokens: usize,
    /// The most syntax nodes held at once.
    pub syntax_nodes: usize,
}

/// Parses and lowers `source` for import in one pass: tokens are lexed as
/// the grammar asks and released once consumed, and each prim is lowered as
/// it closes, its syntax then dropped, so the read holds a window of tokens
/// and the syntax of the prims it is in rather than the whole file's.
pub(crate) fn parse_for_read(source: &str) -> ReadParse<'_> {
    let mut parser =
        Parser::<false>::with_tokens(source, crate::token_source::TokenSource::streaming(source));
    parser.direct_arrays = Some(crate::read::PreparedValues::default());
    parser.lowering = Some(PrimLowering::new(source));
    // Holding the syntax of the open prims alone, a read rarely needs more.
    parser.builder = TreeBuilder::with_capacity(source.len() as u32, READ_NODES);
    parser.parse_source_file();
    let arrays = parser
        .direct_arrays
        .take()
        .expect("read mode owns its arrays");
    let lowering = parser.lowering.take().expect("read mode lowers prims");
    let syntax_nodes = parser.builder.peak();
    let tree = parser.builder.finish();
    ReadParse {
        lowered: lowering.finish(&tree),
        diagnostics: parser.diagnostics,
        arrays,
        tokens: parser.tokens.retained(),
        syntax_nodes,
    }
}

// ── Parser ─────────────────────────────────────────────────────────────

/// Where a prim lowered as it closes was opened: its node, and where the
/// prims lowered inside it start (see [`PrimLowering::mark`]).
struct OpenPrim {
    node: usize,
    mark: usize,
}

struct Parser<'a, const LOSSLESS: bool> {
    source: &'a str,
    tokens: crate::token_source::TokenSource<'a>,
    pos: usize,
    /// The token at `pos`, which navigation asks for many times per token.
    current: Option<Token>,
    diagnostics: Vec<Diagnostic>,
    builder: TreeBuilder,
    direct_arrays: Option<crate::read::PreparedValues>,
    /// In read mode, lowers each prim as it closes (see [`parse_for_read`]).
    lowering: Option<PrimLowering<'a>>,
    /// Whether a line break was consumed since the last non-trivia token,
    /// so a property statement there has ended.
    line_break: bool,
    /// Whether a speculative parse may return to an earlier token, so
    /// streamed tokens must not be released.
    speculating: bool,
    /// The position from which consumed tokens are next released.
    release_at: usize,
}

impl<'a, const LOSSLESS: bool> Parser<'a, LOSSLESS> {
    fn new(source: &'a str, tokens: Vec<Token>) -> Self {
        Self::with_tokens(source, crate::token_source::TokenSource::eager(tokens))
    }

    fn with_tokens(source: &'a str, tokens: crate::token_source::TokenSource<'a>) -> Self {
        let current = tokens.get(0);
        Self {
            source,
            tokens,
            pos: 0,
            current,
            diagnostics: Vec::new(),
            builder: TreeBuilder::new(source.len() as u32),
            direct_arrays: None,
            lowering: None,
            line_break: false,
            speculating: false,
            release_at: crate::token_source::RELEASE_AFTER,
        }
    }

    // ── Token navigation ───────────────────────────────────────────

    /// Returns the current token without advancing.
    fn current(&self) -> Option<Token> {
        self.current
    }

    /// Moves to the token at `pos`.
    fn seek(&mut self, pos: usize) {
        self.pos = pos;
        self.current = self.tokens.get(pos);
    }

    /// Returns the kind of the current token.
    fn peek(&self) -> Option<TokenKind> {
        self.current().map(|t| t.kind)
    }

    /// Returns the text of the current token.
    fn current_text(&self) -> &'a str {
        self.current().map_or("", |t| t.text(self.source))
    }

    /// Returns the kind of the next non-trivia token after the current token.
    fn next_non_trivia_kind(&self) -> Option<TokenKind> {
        let mut idx = self.pos + 1;
        while let Some(token) = self.tokens.get(idx) {
            if !is_trivia(token.kind) {
                return Some(token.kind);
            }
            idx += 1;
        }
        None
    }

    /// Advances past the current token, emitting it to the CST builder.
    fn bump(&mut self) -> Option<Token> {
        self.bump_stored::<true>()
    }

    // Some grammar tokens carry no information needed by AST lowering.
    // Consume them identically in both modes, storing them only for editing.
    fn bump_stored<const STORE: bool>(&mut self) -> Option<Token> {
        let tok = self.current;
        if let Some(t) = tok {
            if STORE {
                self.builder.token(SyntaxKind::from(t.kind), t.span);
            }
            self.seek(self.pos + 1);
            if t.kind == TokenKind::Newline {
                self.line_break = true;
            } else if !is_trivia(t.kind) {
                self.line_break = false;
            }
            if is_string_token(t.kind) && !string_is_terminated(t.text(self.source)) {
                // A quoted string ends on its own line, a triple-quoted one
                // anywhere before the end of the file (AOUSD Core §16.2.5).
                self.error(t.span, "unterminated string");
            }
        }
        tok
    }

    /// Emits all trivia tokens (whitespace, newlines, comments) to the builder.
    ///
    /// Every token before the current one is consumed here, so unless a
    /// speculative parse may return to one, the streamed ones are released:
    /// a read holds a window of lookahead, not the whole file's tokens.
    fn eat_trivia(&mut self) {
        while let Some(tok) = self.current() {
            if is_trivia(tok.kind) {
                if LOSSLESS {
                    self.builder.token(SyntaxKind::from(tok.kind), tok.span);
                }
                self.line_break |= tok.kind == TokenKind::Newline;
                self.seek(self.pos + 1);
            } else {
                break;
            }
        }
        if self.pos >= self.release_at && !self.speculating {
            self.tokens.release(self.pos);
            self.release_at = self.pos + crate::token_source::RELEASE_AFTER;
        }
    }

    /// Skips whitespace/newline tokens only (not comments). Emits them to the builder.
    fn eat_whitespace_only(&mut self) {
        while let Some(tok) = self.current() {
            if tok.kind == TokenKind::Whitespace || tok.kind == TokenKind::Newline {
                if LOSSLESS {
                    self.builder.token(SyntaxKind::from(tok.kind), tok.span);
                }
                self.line_break |= tok.kind == TokenKind::Newline;
                self.seek(self.pos + 1);
            } else {
                break;
            }
        }
    }

    /// Returns true if the current non-trivia token matches `kind`.
    /// Emits any skipped trivia.
    #[expect(dead_code, reason = "reserved for future error recovery paths")]
    fn at(&mut self, kind: TokenKind) -> bool {
        self.eat_trivia();
        self.peek() == Some(kind)
    }

    /// Returns true if the current non-trivia token is an ident with given text.
    fn at_keyword(&mut self, kw: &str) -> bool {
        self.eat_trivia();
        self.peek() == Some(TokenKind::Ident) && self.current_text() == kw
    }

    /// Consumes the current token if it matches `kind`.
    fn eat(&mut self, kind: TokenKind) -> Option<&'a str> {
        self.eat_stored::<true>(kind)
    }

    fn eat_stored<const STORE: bool>(&mut self, kind: TokenKind) -> Option<&'a str> {
        self.eat_trivia();
        if self.peek() == Some(kind) {
            let tok = self.bump_stored::<STORE>().unwrap();
            Some(tok.text(self.source))
        } else {
            None
        }
    }

    /// Consumes an ident matching `kw`.
    fn eat_keyword(&mut self, kw: &str) -> bool {
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == kw {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Expects the current token to be `kind`. Emits diagnostic on mismatch.
    fn expect(&mut self, kind: TokenKind) -> Option<&'a str> {
        self.expect_stored::<true>(kind)
    }

    fn expect_stored<const STORE: bool>(&mut self, kind: TokenKind) -> Option<&'a str> {
        self.eat_trivia();
        if self.peek() == Some(kind) {
            let tok = self.bump_stored::<STORE>().unwrap();
            if kind == TokenKind::Equals {
                self.value_on_the_line(tok.span);
            }
            Some(tok.text(self.source))
        } else {
            let span = self.current_span();
            self.error(span, format!("expected {kind:?}"));
            None
        }
    }

    /// Reports a line break straight after the `=` at `span`: an
    /// assignment's value starts on its line (`Assignment = pad<Equals,
    /// InlinePadding>`, `pxr/usd/sdf/textFileFormatParser.h`).
    fn value_on_the_line(&mut self, span: Span) {
        let mut index = self.pos;
        while let Some(tok) = self.tokens.get(index) {
            match tok.kind {
                TokenKind::Whitespace | TokenKind::BlockComment => index += 1,
                TokenKind::Newline | TokenKind::PythonComment | TokenKind::CppComment => {
                    self.error(span, "unexpected line break after `=`");
                    return;
                }
                _ => return,
            }
        }
    }

    /// Reports a line break crossed by the trivia just eaten, before `what`:
    /// a prim's or property's declaration is on one line (OpenUSD's
    /// `TokenSeparator` between its parts is inline padding).
    fn on_the_line(&mut self, what: &str) {
        if self.line_break {
            let span = self.current_span();
            self.error(span, format!("unexpected line break before {what}"));
        }
    }

    /// Ends a metadata or dictionary entry: at a line break, at a `;` on
    /// its line (which is consumed), or before the `close` of its block.
    /// Entries are separated by one line break or `;`
    /// (`StatementSequenceOf`, `pxr/usd/sdf/textFileFormatParser.h`), so
    /// `( a = 1 b = 2 )` is refused, as is a `;` on a later line.
    fn expect_entry_end(&mut self, close: TokenKind) {
        self.expect_statement_end(true, Some(close));
    }

    /// Ends a statement at a line break, at a `;` on its line where
    /// `semicolon` allows one (the `;` is consumed), or before `close` on
    /// its line; reports anything else on the line, without consuming it.
    /// Only comments may come between: OpenUSD pads a statement's end with
    /// `SinglelinePadding` (`pxr/usd/sdf/textFileFormatParser.h`), so a
    /// block comment spanning lines is no line break. The end of the file
    /// is left to whatever reports it unclosed.
    ///
    /// Spec: AOUSD Core §16.2.10 (statement separators).
    fn expect_statement_end(&mut self, semicolon: bool, close: Option<TokenKind>) {
        if self.line_break {
            return;
        }
        let mut index = self.pos;
        while let Some(tok) = self.tokens.get(index) {
            if tok.kind == TokenKind::Newline || !is_trivia(tok.kind) {
                break;
            }
            index += 1;
        }
        let Some(next) = self.tokens.get(index) else {
            return;
        };
        if next.kind == TokenKind::Newline || Some(next.kind) == close {
            return;
        }
        if semicolon && next.kind == TokenKind::Semicolon {
            self.eat_trivia();
            self.bump();
            return;
        }
        let message = if semicolon {
            "expected a line break or `;` at the end of the statement"
        } else {
            "expected a line break at the end of the statement"
        };
        self.error(next.span, message);
    }

    /// Reports and skips a `;` that does not end a statement on its line:
    /// OpenUSD takes one separator after each statement and none before
    /// the first (`StatementSequenceOf`, `PrimItem`,
    /// `pxr/usd/sdf/textFileFormatParser.h`).
    fn refuse_stray_separator(&mut self) -> bool {
        let semicolon = self.peek() == Some(TokenKind::Semicolon);
        if semicolon {
            let span = self.current_span();
            self.error(
                span,
                "unexpected `;` with no statement before it on its line",
            );
            self.bump();
        }
        semicolon
    }

    /// Returns the span of the current token, or empty span at EOF.
    fn current_span(&self) -> Span {
        self.current().map_or(
            Span::new(self.source.len() as u32, self.source.len() as u32),
            |t| t.span,
        )
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::error(span, message));
    }

    /// Peeks at the next non-trivia token after the current one (without emitting).
    fn peek_next_non_trivia(&self) -> Option<TokenKind> {
        self.peek_past_trivia_from(self.pos + 1)
            .map(|(_, kind)| kind)
    }

    /// Peek ahead past trivia from a given position, return (position, kind).
    fn peek_past_trivia_from(&self, start: usize) -> Option<(usize, TokenKind)> {
        let mut i = start;
        while let Some(token) = self.tokens.get(i) {
            if !is_trivia(token.kind) {
                return Some((i, token.kind));
            }
            i += 1;
        }
        None
    }

    fn at_string(&self) -> bool {
        matches!(
            self.current().map(|t| t.kind),
            Some(
                TokenKind::DoubleQuoteString
                    | TokenKind::SingleQuoteString
                    | TokenKind::MultilineDoubleQuoteString
                    | TokenKind::MultilineSingleQuoteString
            )
        )
    }

    /// Peek-only check (no trivia emission) for string at current position after trivia.
    #[expect(dead_code, reason = "reserved for future lookahead paths")]
    fn at_string_after_trivia(&self) -> bool {
        if let Some((_, kind)) = self.peek_past_trivia_from(self.pos) {
            matches!(
                kind,
                TokenKind::DoubleQuoteString
                    | TokenKind::SingleQuoteString
                    | TokenKind::MultilineDoubleQuoteString
                    | TokenKind::MultilineSingleQuoteString
            )
        } else {
            false
        }
    }

    // ── Top-level parsing ──────────────────────────────────────────

    fn parse_source_file(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::SourceFile, start);

        self.parse_header();

        // Optional layer metadata.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftParen) {
            self.parse_layer_metadata();
        }

        // Root prims and reorder statements.
        loop {
            self.eat_trivia();
            if self.current().is_none() {
                break;
            }

            // A root prim ends at a line break or the end of the file, a
            // `reorder rootPrims` statement also at `;` (`LayerSpec`,
            // `pxr/usd/sdf/textFileFormatParser.h`).
            if self.peek() == Some(TokenKind::Ident) && self.current_text() == "reorder" {
                self.parse_reorder_statement();
                self.expect_statement_end(true, None);
            } else if self.peek() == Some(TokenKind::Ident)
                && matches!(self.current_text(), "def" | "over" | "class")
            {
                self.parse_prim();
                self.expect_statement_end(false, None);
            } else {
                let span = self.current_span();
                self.error(span, "expected prim definition or reorder statement");
                self.bump();
            }
        }

        let end = self.source.len() as u32;
        self.builder.finish_node(end);
    }

    fn parse_header(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::Header, start);

        // OpenUSD 26.11 reads no layer that starts with a byte order mark:
        // a file is refused before parsing (`_CheckBOM`,
        // `pxr/usd/sdf/usdaFileFormat.cpp`, which `ByteOrderMark` mirrors),
        // and a string fails its header rule at 1:1.
        // Spec: AOUSD Core §16.2 (USDA is UTF-8 and begins with `#usda`).
        // Recovery reads on past the mark, so it is the only diagnostic.
        if self.source.starts_with('\u{feff}') {
            self.error(
                Span::new(0, 3),
                "expected #usda header, found a UTF-8 byte order mark, which USDA does not allow",
            );
            self.bump();
        }
        // The header `#usda 1.0` is lexed as a PythonComment.
        self.eat_whitespace_only();
        if let Some(tok) = self.current()
            && tok.kind == TokenKind::PythonComment
        {
            let text = tok.text(self.source);
            if text.starts_with("#usda") {
                self.bump();
            } else {
                let span = self.current_span();
                self.error(span, "expected #usda header");
            }
        } else {
            let span = self.current_span();
            self.error(span, "expected #usda header");
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    // ── Layer metadata ─────────────────────────────────────────────

    fn parse_layer_metadata(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::LayerMetadata, start);

        self.expect(TokenKind::LeftParen);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightParen) || self.current().is_none() {
                break;
            }

            if self.refuse_stray_separator() {
                continue;
            }
            if self.peek() == Some(TokenKind::Ident) && self.current_text() == "subLayers" {
                self.parse_sublayers_metadata();
            } else if self.peek() == Some(TokenKind::Ident) && self.current_text() == "relocates" {
                self.parse_relocates_metadata();
            } else if self.peek() == Some(TokenKind::Ident) && self.current_text() == "doc" {
                self.parse_layer_meta_entry_doc();
            } else if self.peek() == Some(TokenKind::Ident) {
                self.parse_generic_layer_meta_entry();
            } else if self.at_string() {
                // String-only doc.
                self.parse_layer_meta_entry_doc_string();
            } else {
                let span = self.current_span();
                self.error(span, "unexpected token in layer metadata");
                self.bump();
                continue;
            }
            self.expect_entry_end(TokenKind::RightParen);
        }

        self.expect(TokenKind::RightParen);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_layer_meta_entry_doc(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::LayerMetaEntry, start);
        self.bump(); // `doc`
        self.expect(TokenKind::Equals);
        self.parse_string_token();
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_layer_meta_entry_doc_string(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::LayerMetaEntry, start);
        self.parse_string_token();
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_generic_layer_meta_entry(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::LayerMetaEntry, start);
        // A list-op statement, as on a prim: OpenUSD writes metadata no
        // schema registers that way when it was authored so
        // (`Sdf_FileIOUtility::WriteListOp`), on the layer too.
        let _op = self.try_parse_listop_prefix();
        self.parse_metadata_entry_inner();
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_sublayers_metadata(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::SubLayersList, start);

        self.eat_keyword("subLayers");
        self.expect(TokenKind::Equals);
        self.expect(TokenKind::LeftBracket);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                break;
            }

            self.parse_sublayer_item();
            self.eat(TokenKind::Comma);
        }

        self.expect(TokenKind::RightBracket);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_sublayer_item(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::SubLayerItem, start);

        self.parse_asset_ref();

        // Optional layer offset params.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftParen) {
            self.parse_layer_offset_params();
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_layer_offset_params(&mut self) {
        let start = self.current_span().start;
        self.builder
            .start_node(SyntaxKind::LayerOffsetParams, start);
        self.bump(); // `(`
        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightParen) || self.current().is_none() {
                break;
            }
            if self.peek() == Some(TokenKind::Ident)
                && matches!(self.current_text(), "offset" | "scale")
            {
                self.bump(); // keyword
                self.expect(TokenKind::Equals);
                self.parse_number_tokens();
            } else if self.peek() == Some(TokenKind::Ident) && self.current_text() == "customData" {
                // Retain the typed reference dictionary per §16.2.17.5.
                self.bump();
                self.expect(TokenKind::Equals);
                self.parse_dictionary_value();
            } else {
                self.bump();
            }
        }
        self.expect(TokenKind::RightParen);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_relocates_metadata(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::RelocatesMap, start);

        self.eat_keyword("relocates");
        self.expect(TokenKind::Equals);
        self.expect(TokenKind::LeftBrace);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBrace) || self.current().is_none() {
                break;
            }

            self.parse_relocate_entry();
            // Pairs are separated by `,` (`RelocatesMapInterior`,
            // `pxr/usd/sdf/textFileFormatParser.h`); the last may have one.
            if self.eat(TokenKind::Comma).is_none() {
                self.eat_trivia();
                if self.peek() != Some(TokenKind::RightBrace) && self.current().is_some() {
                    let span = self.current_span();
                    self.error(span, "expected `,` between relocates");
                }
            }
        }

        self.expect(TokenKind::RightBrace);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_relocate_entry(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::RelocateEntry, start);
        self.parse_path_ref();
        self.expect(TokenKind::Colon);
        self.parse_path_ref();
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    // ── Prim parsing ───────────────────────────────────────────────

    /// In read mode, notes where a prim opens (see [`PrimLowering`]); call
    /// before opening its node.
    fn open_prim(&self) -> Option<OpenPrim> {
        self.lowering.as_ref().map(|lowering| OpenPrim {
            node: self.builder.node_count(),
            mark: lowering.mark(),
        })
    }

    /// In read mode, lowers the prim `opened` that has just closed, and
    /// drops its syntax.
    fn close_prim(&mut self, opened: Option<OpenPrim>) {
        if let (Some(lowering), Some(opened)) = (&mut self.lowering, opened) {
            lowering.close_prim(self.builder.tree(), opened.node as u32, opened.mark);
            self.builder.collapse(opened.node);
        }
    }

    fn parse_prim(&mut self) {
        let start = self.current_span().start;
        let opened = self.open_prim();
        self.builder.start_node(SyntaxKind::PrimDef, start);

        // Specifier: def/over/class.
        self.bump(); // emit specifier ident

        // Optional type name (ident followed by a string).
        self.eat_trivia();
        self.on_the_line("the prim's type or name");
        if self.peek() == Some(TokenKind::Ident)
            && self.peek_next_non_trivia() == Some(TokenKind::DoubleQuoteString)
        {
            self.bump(); // type name
            self.eat_trivia();
            self.on_the_line("the prim's name");
        }

        // Prim name (quoted string).
        self.parse_string_token();

        // Optional metadata block.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftParen) {
            self.parse_prim_metadata();
        }

        // Body: { ... }
        self.expect(TokenKind::LeftBrace);
        self.parse_prim_body();
        self.expect(TokenKind::RightBrace);

        let end = self.current_span().start;
        self.builder.finish_node(end);
        self.close_prim(opened);
    }

    // ── Prim metadata ──────────────────────────────────────────────

    fn parse_prim_metadata(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::PrimMetadata, start);

        self.expect(TokenKind::LeftParen);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightParen) || self.current().is_none() {
                break;
            }

            if self.refuse_stray_separator() {
                continue;
            }
            if self.peek() == Some(TokenKind::Ident) && self.current_text() == "relocates" {
                self.parse_prim_relocates();
                continue;
            }
            self.parse_prim_meta_entry();
            self.expect_entry_end(TokenKind::RightParen);
        }

        self.expect(TokenKind::RightParen);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Refuses a prim's or variant's `relocates` statement, reading past it
    /// as an error node that lowering skips.
    ///
    /// OpenUSD 26.11 removed prim relocates from Sdf: `PrimMetadataItem` no
    /// longer accepts `RelocatesMetadata` (`pxr/usd/sdf/textFileFormatParser.h`),
    /// so a layer with one fails to parse ("Expected )" at the statement).
    /// Only layer metadata holds relocates.
    /// Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`), §10.3.2.6.
    fn parse_prim_relocates(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ErrorNode, start);
        let keyword = self.current_span();
        self.error(
            keyword,
            "relocates are layer metadata; a prim or variant cannot author them",
        );
        self.eat_keyword("relocates");
        self.expect(TokenKind::Equals);
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftBrace) {
            self.bump();
            loop {
                self.eat_trivia();
                if self.peek() == Some(TokenKind::RightBrace) || self.current().is_none() {
                    break;
                }
                self.parse_relocate_entry();
                self.eat(TokenKind::Comma);
            }
            self.expect(TokenKind::RightBrace);
        }
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_prim_meta_entry(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::PrimMetaEntry, start);

        // Check for list-op prefix.
        let _op = self.try_parse_listop_prefix();

        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) {
            let kw = self.current_text();
            match kw {
                "references" | "payload" => {
                    self.bump(); // keyword
                    self.expect(TokenKind::Equals);
                    self.parse_arc_list();
                }
                "inherits" | "specializes" => {
                    self.bump(); // keyword
                    self.expect(TokenKind::Equals);
                    self.parse_path_list();
                }
                "variants" => {
                    self.bump();
                    self.expect(TokenKind::Equals);
                    self.parse_variant_selections();
                }
                "variantSets" => {
                    self.bump();
                    self.expect(TokenKind::Equals);
                    self.parse_name_list();
                }
                "kind" => {
                    self.bump();
                    self.expect(TokenKind::Equals);
                    self.parse_string_token();
                }
                "doc" => {
                    self.bump();
                    self.expect(TokenKind::Equals);
                    self.parse_string_token();
                }
                _ => {
                    self.parse_metadata_entry_inner();
                }
            }
        } else if self.at_string() {
            // doc string
            self.parse_string_token();
        } else {
            let span = self.current_span();
            self.error(span, "unexpected token in prim metadata");
            self.bump();
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn try_parse_listop_prefix(&mut self) -> crate::ast::ListOpKind {
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) {
            match self.current_text() {
                "prepend" => {
                    self.bump();
                    crate::ast::ListOpKind::Prepend
                }
                "append" => {
                    self.bump();
                    crate::ast::ListOpKind::Append
                }
                "add" => {
                    self.bump();
                    crate::ast::ListOpKind::Add
                }
                "delete" => {
                    self.bump();
                    crate::ast::ListOpKind::Delete
                }
                "reorder" => {
                    self.bump();
                    crate::ast::ListOpKind::Reorder
                }
                _ => crate::ast::ListOpKind::Explicit,
            }
        } else {
            crate::ast::ListOpKind::Explicit
        }
    }

    // ── Composition arcs ───────────────────────────────────────────

    fn parse_arc_list(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ArcList, start);

        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "None" {
            self.bump();
        } else if self.peek() == Some(TokenKind::LeftBracket) {
            self.bump(); // [
            loop {
                self.eat_trivia();
                if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                    break;
                }
                self.parse_arc_ref();
                self.eat(TokenKind::Comma);
            }
            self.expect(TokenKind::RightBracket);
        } else {
            // Single arc ref.
            self.parse_arc_ref();
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_arc_ref(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ArcRef, start);

        // Optional asset ref.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::AssetPath) {
            self.parse_asset_ref();
        }

        // Optional path ref.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftAngle) {
            self.parse_path_ref();
        }

        // Optional layer offset params.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftParen) {
            self.parse_layer_offset_params();
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_path_list(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::PathList, start);

        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "None" {
            self.bump();
        } else if self.peek() == Some(TokenKind::LeftBracket) {
            self.bump(); // [
            loop {
                self.eat_trivia();
                if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                    break;
                }
                self.parse_path_ref();
                self.eat(TokenKind::Comma);
            }
            self.expect(TokenKind::RightBracket);
        } else {
            // Single path.
            self.parse_path_ref();
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// A name list (`variantSets`, `reorder nameChildren`, `reorder
    /// properties`, `reorder rootPrims`): one name, or a bracketed list of
    /// at least one. `None` and `[]` are reported, as OpenUSD's text parser
    /// rejects them (its `NameList` rule), and parsed on to recover.
    ///
    /// Spec: AOUSD Core §16.2.17 (`reorder` statements), §16.2.18.
    fn parse_name_list(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::NameList, start);

        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "None" {
            let span = self.current_span();
            self.error(span, "expected a name or a list of names, not `None`");
            self.bump();
        } else if self.at_string() {
            // Single string.
            self.parse_string_token();
        } else {
            let open = self.current_span();
            self.expect(TokenKind::LeftBracket);
            let mut names = 0_usize;
            loop {
                self.eat_trivia();
                if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                    break;
                }
                self.parse_string_token();
                names += 1;
                self.eat(TokenKind::Comma);
            }
            self.expect(TokenKind::RightBracket);
            if names == 0 {
                self.error(open, "expected at least one name in the list");
            }
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_variant_selections(&mut self) {
        let start = self.current_span().start;
        self.builder
            .start_node(SyntaxKind::VariantSelections, start);

        self.expect(TokenKind::LeftBrace);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBrace) || self.current().is_none() {
                break;
            }
            self.parse_variant_selection_entry();
        }

        self.expect(TokenKind::RightBrace);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_variant_selection_entry(&mut self) {
        let start = self.current_span().start;
        self.builder
            .start_node(SyntaxKind::VariantSelectionEntry, start);

        // May have type prefix (`string`), then ident or string key, `=`, string value.
        if self.peek() == Some(TokenKind::Ident) && !self.at_string() {
            let saved_pos = self.pos;
            let saved_node_count = self.builder_node_count();
            self.speculating = true;
            self.bump(); // type name or key
            self.eat_trivia();
            if self.peek() == Some(TokenKind::Ident) || self.at_string() {
                // Was a type prefix — next is the key ident or quoted token.
                if self.at_string() {
                    self.parse_string_token();
                } else {
                    self.bump(); // key
                }
                self.expect(TokenKind::Equals);
                self.parse_string_token();
            } else if self.peek() == Some(TokenKind::Equals) {
                // Not a typed entry — key was an ident.
                self.expect(TokenKind::Equals);
                self.parse_string_token();
            } else {
                // Restore and try as string.
                self.restore_to(saved_pos, saved_node_count);
                if self.at_string() {
                    self.parse_string_token();
                    self.expect(TokenKind::Equals);
                    self.parse_string_token();
                } else {
                    let span = self.current_span();
                    self.error(span, "expected variant selection entry");
                    self.bump();
                }
            }
            self.speculating = false;
        } else if self.at_string() {
            self.parse_string_token();
            self.expect(TokenKind::Equals);
            self.parse_string_token();
        } else {
            let span = self.current_span();
            self.error(span, "expected variant selection entry");
            self.bump();
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Helper to get current node count for rollback.
    fn builder_node_count(&self) -> usize {
        self.builder.node_count()
    }

    /// Rollback parser position and builder nodes.
    fn restore_to(&mut self, pos: usize, node_count: usize) {
        self.seek(pos);
        self.line_break = false;
        self.builder.truncate(node_count);
    }

    // ── Prim body ──────────────────────────────────────────────────

    fn parse_prim_body(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::PrimBody, start);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBrace) || self.current().is_none() {
                break;
            }

            if self.refuse_stray_separator() {
                continue;
            }
            // A prim or variant set ends at a line break, a property or
            // `reorder` statement also at `;`, which it consumes (`PrimItem`,
            // `pxr/usd/sdf/textFileFormatParser.h`): the closing `}` of
            // the prim does not end one on its line.
            if self.peek() == Some(TokenKind::Ident)
                && matches!(self.current_text(), "def" | "over" | "class")
            {
                self.parse_prim();
                self.expect_statement_end(false, None);
            } else if self.peek() == Some(TokenKind::Ident) && self.current_text() == "variantSet" {
                self.parse_variant_set();
                self.expect_statement_end(false, None);
            } else if self.is_reorder_statement() {
                self.parse_reorder_statement();
                self.expect_statement_end(true, None);
            } else if (self.peek() == Some(TokenKind::Ident)
                && (self.current_text() == "rel"
                    || (self.current_text() == "varying" && self.rel_after(self.pos))))
                || self.is_listop_rel()
            {
                self.parse_relationship();
            } else if self.peek() == Some(TokenKind::Ident) && self.current_text() == "custom" {
                // Could be custom attr or custom rel.
                if self.is_custom_rel() {
                    self.parse_relationship();
                } else {
                    self.parse_attribute();
                }
            } else if self.is_listop_connect() || self.is_attribute_start() {
                self.parse_attribute();
            } else {
                let span = self.current_span();
                let text = self.current_text();
                self.error(span, format!("unexpected token in prim body: {text:?}"));
                self.bump();
            }
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Check if current position is a listop-prefixed relationship.
    fn is_listop_rel(&self) -> bool {
        if self.peek() != Some(TokenKind::Ident) || !is_list_op_keyword(self.current_text()) {
            return false;
        }
        let Some((i, TokenKind::Ident)) = self.peek_past_trivia_from(self.pos + 1) else {
            return false;
        };
        let text = self
            .tokens
            .get(i)
            .expect("lookahead checked")
            .text(self.source);
        match text {
            "rel" => true,
            "custom" => self.rel_after(i + 1),
            "varying" => self.rel_after(i),
            _ => false,
        }
    }

    /// Check if `custom` keyword is followed (eventually) by `rel`.
    fn is_custom_rel(&self) -> bool {
        self.rel_after(self.pos + 1)
    }

    /// Whether `rel`, or `varying rel`, follows the token at `start`.
    fn rel_after(&self, start: usize) -> bool {
        self.peek_past_trivia_from(start).is_some_and(|(i, kind)| {
            if kind != TokenKind::Ident {
                return false;
            }
            match self
                .tokens
                .get(i)
                .expect("lookahead checked")
                .text(self.source)
            {
                "rel" => true,
                "varying" => self.peek_past_trivia_from(i + 1).is_some_and(|(i, kind)| {
                    kind == TokenKind::Ident
                        && self
                            .tokens
                            .get(i)
                            .expect("lookahead checked")
                            .text(self.source)
                            == "rel"
                }),
                _ => false,
            }
        })
    }

    /// Check if current position looks like an attribute start.
    fn is_attribute_start(&self) -> bool {
        if self.peek() != Some(TokenKind::Ident) {
            return false;
        }
        let text = self.current_text();
        !matches!(
            text,
            "def"
                | "over"
                | "class"
                | "variantSet"
                | "reorder"
                | "rel"
                | "prepend"
                | "append"
                | "add"
                | "delete"
        )
    }

    /// Check if current position is a listop-prefixed connect attribute.
    fn is_listop_connect(&self) -> bool {
        if self.peek() != Some(TokenKind::Ident) || !is_list_op_keyword(self.current_text()) {
            return false;
        }
        let Some((i, TokenKind::Ident)) = self.peek_past_trivia_from(self.pos + 1) else {
            return false;
        };
        match self
            .tokens
            .get(i)
            .expect("lookahead checked")
            .text(self.source)
        {
            "rel" => false,
            "varying" => !self.rel_after(i),
            "custom" => !self.rel_after(i + 1),
            _ => true,
        }
    }

    // ── Attributes ─────────────────────────────────────────────────

    fn parse_attribute(&mut self) {
        let errors = self.diagnostics.len();
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::AttributeDef, start);

        // Optional list-op prefix, which only a `.connect` statement takes.
        self.eat_trivia();
        let mut list_op = None;
        if self.peek() == Some(TokenKind::Ident) && is_list_op_keyword(self.current_text()) {
            list_op = Some(self.current_span());
            self.bump();
        }

        // `custom`.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "custom" {
            self.bump();
        }

        // `uniform`.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "uniform" {
            self.bump();
        }

        let mut type_name = "";
        let mut is_array = false;

        // Type name.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) {
            type_name = self.current_text();
            self.bump();
        }

        // Array suffix `[]`.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftBracket) {
            is_array = true;
            self.bump();
            self.expect(TokenKind::RightBracket);
        }

        // Attribute name (namespaced), on the type's line.
        self.eat_trivia();
        if !type_name.is_empty() {
            self.on_the_line("the attribute's name");
        }
        self.parse_namespaced_name();

        // What follows: .timeSamples, .connect, =, or metadata (
        self.eat_trivia();
        let suffixed = self.peek() == Some(TokenKind::Dot);
        let connect = suffixed
            && self
                .peek_past_trivia_from(self.pos + 1)
                .and_then(|(i, _)| self.tokens.get(i))
                .is_some_and(|tok| tok.text(self.source) == "connect");
        if let Some(span) = list_op.filter(|_| !connect) {
            // OpenUSD: `AttributeSpec` takes a list-op keyword only before
            // `.connect` (`pxr/usd/sdf/textFileFormatParser.h`).
            self.error(
                span,
                "a list-op keyword applies only to a `.connect` statement",
            );
        }
        if suffixed {
            self.parse_attribute_suffix(start, type_name, is_array);
        } else if self.peek() == Some(TokenKind::Equals) {
            let equals = self.current_span();
            self.bump(); // =
            self.value_on_the_line(equals);
            self.parse_attribute_value(start, None, type_name, is_array);
        }

        // Optional metadata block, which opens on the property's own line
        // (OpenUSD rejects one on the next with "Expected }"); only a
        // declaration or a default value takes one, not a `.connect`,
        // `.timeSamples` or `.spline` statement.
        self.eat_trivia();
        if !self.line_break && self.peek() == Some(TokenKind::LeftParen) {
            if suffixed {
                let span = self.current_span();
                self.error(
                    span,
                    "metadata cannot follow a `.connect`, `.timeSamples` or `.spline` statement",
                );
            }
            self.parse_attribute_metadata();
        }
        self.expect_property_end(errors);

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Ends a property statement at a line break or a `;` on its line,
    /// as [`Self::expect_statement_end`]; OpenUSD wants one even before the
    /// prim's closing brace (`PrimItem`, `pxr/usd/sdf/textFileFormatParser.h`),
    /// so `def "W" { double x = 1 }` is refused. Anything else on the line
    /// is reported and skipped to the end of the statement. A property that
    /// reported an error since `errors` diagnostics were recorded is
    /// skipped silently: its value parser said what went wrong. A word there is not another
    /// property: OpenUSD's parser rejects `keep rel targets = </A>` (`keep`
    /// is no list-op keyword, so `keep` reads as a type name and `rel` as
    /// the name) and `double x = 1 junk` with "Expected }".
    ///
    /// Spec: AOUSD Core §16.2 (property statements).
    fn expect_property_end(&mut self, errors: usize) {
        if self.line_break {
            return;
        }
        let mut index = self.pos;
        while let Some(tok) = self.tokens.get(index) {
            if tok.kind == TokenKind::Newline || !is_trivia(tok.kind) {
                break;
            }
            index += 1;
        }
        let next = self.tokens.get(index).map(|tok| tok.kind);
        if matches!(
            next,
            None | Some(TokenKind::Newline | TokenKind::Semicolon | TokenKind::RightBrace)
        ) {
            self.expect_statement_end(true, None);
            return;
        }
        while self.pos < index {
            self.bump();
        }
        let span = self.current_span();
        let text = self.current_text();
        if self.diagnostics.len() == errors {
            self.error(
            span,
                format!(
                    "unexpected {text:?} after a property; expected '=', '.', '(' or the end of the line"
                ),
            );
        }
        while let Some(tok) = self.current() {
            if matches!(
                tok.kind,
                TokenKind::Newline | TokenKind::Semicolon | TokenKind::RightBrace
            ) {
                break;
            }
            self.bump();
        }
        if self.peek() == Some(TokenKind::Semicolon) {
            self.bump();
        }
    }

    fn parse_attribute_suffix(&mut self, attribute: u32, type_name: &str, is_array: bool) {
        self.eat_trivia();
        if self.peek() != Some(TokenKind::Dot) {
            return;
        }

        // Peek to determine suffix type.
        if let Some((i, TokenKind::Ident)) = self.peek_past_trivia_from(self.pos + 1) {
            match self
                .tokens
                .get(i)
                .expect("lookahead checked")
                .text(self.source)
            {
                "timeSamples" => {
                    self.parse_time_samples_suffix(attribute, type_name, is_array);
                    return;
                }
                "connect" => {
                    self.parse_connection_suffix();
                    return;
                }
                "spline" => {
                    self.parse_spline_suffix();
                    return;
                }
                _ => {}
            }
        }
        // Another attribute field: keep the declaration, report the field
        // and skip its value rather than misreading it as further
        // statements.
        let span = self.current_span();
        self.bump(); // `.`
        let field = if self.peek() == Some(TokenKind::Ident) {
            let text = self.current_text();
            self.bump();
            text
        } else {
            ""
        };
        self.error(
            span,
            format!("unsupported: attribute field `.{field}` is not read; its value is ignored"),
        );
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Equals) {
            self.bump();
            self.eat_trivia();
            self.skip_value_tokens();
        }
    }

    /// Consumes one value's tokens without building value nodes: a bracketed
    /// group (including nested groups) or a single token.
    fn skip_value_tokens(&mut self) {
        let mut depth = 0_usize;
        while let Some(kind) = self.peek() {
            self.bump();
            match kind {
                TokenKind::LeftBrace | TokenKind::LeftBracket | TokenKind::LeftParen => depth += 1,
                TokenKind::RightBrace | TokenKind::RightBracket | TokenKind::RightParen => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
            if depth == 0 {
                return;
            }
        }
    }

    fn parse_time_samples_suffix(&mut self, attribute: u32, type_name: &str, is_array: bool) {
        let start = self.current_span().start;
        self.builder
            .start_node(SyntaxKind::TimeSamplesSuffix, start);
        self.bump(); // `.`
        self.bump(); // `timeSamples`
        self.expect(TokenKind::Equals);
        self.parse_time_sample_map(attribute, type_name, is_array);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// `.spline = { ... }`: the value's tokens are kept in the node, whose
    /// text the lowering reads as a spline.
    fn parse_spline_suffix(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::SplineSuffix, start);
        self.bump(); // `.`
        self.bump(); // `spline`
        self.expect(TokenKind::Equals);
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftBrace) {
            self.bump();
            loop {
                self.eat_trivia();
                match self.peek() {
                    Some(TokenKind::RightBrace) => {
                        self.bump();
                        break;
                    }
                    Some(TokenKind::LeftBrace) => self.parse_dictionary_value(),
                    None => {
                        self.error(self.current_span(), "unterminated spline");
                        break;
                    }
                    _ => {
                        self.bump();
                    }
                }
            }
        } else {
            let span = self.current_span();
            self.error(span, "expected `{` to begin a spline");
        }
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_connection_suffix(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ConnectionSuffix, start);
        self.bump(); // `.`
        self.bump(); // `connect`
        self.expect(TokenKind::Equals);
        self.parse_connect_value();
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_connect_value(&mut self) {
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "None" {
            self.bump();
        } else if self.peek() == Some(TokenKind::LeftBracket) {
            // Path list.
            let start = self.current_span().start;
            self.builder.start_node(SyntaxKind::PathList, start);
            self.bump(); // [
            loop {
                self.eat_trivia();
                if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                    break;
                }
                self.parse_path_ref();
                self.eat(TokenKind::Comma);
            }
            self.expect(TokenKind::RightBracket);
            let end = self.current_span().start;
            self.builder.finish_node(end);
        } else {
            self.parse_path_ref();
        }
    }

    fn parse_attribute_metadata(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::PrimMetadata, start);
        self.expect(TokenKind::LeftParen);
        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightParen) || self.current().is_none() {
                break;
            }
            if self.refuse_stray_separator() {
                continue;
            }
            let entry_start = self.current_span().start;
            self.builder
                .start_node(SyntaxKind::MetadataEntry, entry_start);
            if self.at_string() {
                // Bare doc string (e.g. '''...''' without key = value).
                self.parse_string_token();
            } else {
                self.parse_metadata_entry_inner();
            }
            let entry_end = self.current_span().start;
            self.builder.finish_node(entry_end);
            self.expect_entry_end(TokenKind::RightParen);
        }
        self.expect(TokenKind::RightParen);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    // ── Relationships ──────────────────────────────────────────────

    fn parse_relationship(&mut self) {
        let errors = self.diagnostics.len();
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::RelationshipDef, start);

        // Optional list-op prefix.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && is_list_op_keyword(self.current_text()) {
            self.bump();
        }

        // `custom`.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "custom" {
            self.bump();
        }

        // `varying`: a relationship is uniform unless it says so.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "varying" {
            self.bump();
        }

        // `rel`.
        self.eat_keyword("rel");

        // Name.
        self.parse_namespaced_name();

        // `.default = <path>`, a legacy relationship default OpenUSD still
        // reads (`RelationshipSpec`, `pxr/usd/sdf/textFileFormatParser.h`).
        // The layer has no path value to hold it, so it is reported rather
        // than dropped unseen; any other suffix is not relationship syntax.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Dot) {
            let span = self.current_span();
            self.bump();
            if self.at_keyword("default") {
                self.error(
                    span,
                    "unsupported: a relationship's `.default` value is not kept",
                );
            } else {
                self.error(span, "expected `default` after a relationship name");
            }
            if self.at_keyword("timeSamples") || self.at_keyword("default") {
                self.bump();
                self.expect(TokenKind::Equals);
                self.parse_value_expr(); // consume and discard
            }
        }

        // Optional assignment.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Equals) {
            let equals = self.current_span();
            self.bump();
            self.value_on_the_line(equals);
            self.eat_trivia();
            if self.peek() == Some(TokenKind::Ident) && self.current_text() == "None" {
                self.bump();
            } else if self.peek() == Some(TokenKind::LeftBracket) {
                let list_start = self.current_span().start;
                self.builder.start_node(SyntaxKind::PathList, list_start);
                self.bump(); // [
                loop {
                    self.eat_trivia();
                    if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                        break;
                    }
                    self.parse_path_ref();
                    self.eat(TokenKind::Comma);
                }
                self.expect(TokenKind::RightBracket);
                let list_end = self.current_span().start;
                self.builder.finish_node(list_end);
            } else if self.peek() == Some(TokenKind::LeftAngle) {
                self.parse_path_ref();
            }
        }

        // Optional metadata block, which opens on the property's own line
        // (OpenUSD rejects one on the next with "Expected }").
        self.eat_trivia();
        if !self.line_break && self.peek() == Some(TokenKind::LeftParen) {
            self.parse_attribute_metadata();
        }
        self.expect_property_end(errors);

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    // ── Variant sets ───────────────────────────────────────────────

    fn parse_variant_set(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::VariantSetDef, start);

        self.eat_keyword("variantSet");
        self.parse_string_token();
        self.expect(TokenKind::Equals);

        self.expect(TokenKind::LeftBrace);
        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBrace) || self.current().is_none() {
                break;
            }
            self.parse_variant_branch();
        }
        self.expect(TokenKind::RightBrace);

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_variant_branch(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::VariantBranch, start);

        self.parse_string_token();

        // Optional metadata.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftParen) {
            self.parse_prim_metadata();
        }

        self.expect(TokenKind::LeftBrace);
        self.parse_prim_body();
        self.expect(TokenKind::RightBrace);

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Whether the prim body continues with a `reorder` statement:
    /// `reorder nameChildren = [...]` or `reorder properties = [...]`,
    /// rather than a reordered relationship or connection.
    fn is_reorder_statement(&self) -> bool {
        if self.peek() != Some(TokenKind::Ident) || self.current_text() != "reorder" {
            return false;
        }
        self.peek_past_trivia_from(self.pos + 1)
            .is_some_and(|(i, kind)| {
                kind == TokenKind::Ident
                    && matches!(
                        self.tokens
                            .get(i)
                            .expect("lookahead checked")
                            .text(self.source),
                        "nameChildren" | "properties" | "rootPrims"
                    )
            })
    }

    fn parse_reorder_statement(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ReorderStmt, start);

        self.eat_keyword("reorder");
        // `nameChildren`, `properties`, or `rootPrims`.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) {
            self.bump(); // the keyword
        }
        self.expect(TokenKind::Equals);
        self.parse_name_list();

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    // ── Values ─────────────────────────────────────────────────────

    fn parse_attribute_value(
        &mut self,
        attribute: u32,
        sample: Option<usize>,
        type_name: &str,
        is_array: bool,
    ) {
        self.eat_trivia();
        if is_array
            && self.direct_arrays.is_some()
            && let Some(width) = crate::read::numeric_width(type_name)
        {
            let start = self.current_span().start;
            if let Some(array) = self.tokens.numeric_array(self.pos, type_name, width) {
                let end = self.pos + 2;
                self.builder.start_node(SyntaxKind::ValueExpr, start);
                self.builder.start_node(SyntaxKind::ArrayValue, start);
                self.seek(end);
                self.line_break = false;
                // Private placeholders: emission consumes the matching source
                // range by declaration offset and sample ordinal.
                self.builder.finish_node(array.span.end);
                self.builder.finish_node(self.current_span().start);
                self.direct_arrays
                    .as_mut()
                    .expect("read mode")
                    .insert(crate::read::PreparedKey::new(attribute, sample), array);
                return;
            }
        }
        if !is_array
            && self.direct_arrays.is_some()
            && let Some(width @ 1..) = crate::read::numeric_width(type_name)
        {
            let start = self.current_span().start;
            if let Some(tuple) = self.tokens.numeric_tuple(self.pos, type_name, width) {
                let end = self.pos + 2;
                self.builder.start_node(SyntaxKind::ValueExpr, start);
                self.builder.start_node(SyntaxKind::TupleValue, start);
                self.seek(end);
                self.line_break = false;
                // Placeholders, as for an array above.
                self.builder.finish_node(tuple.span.end);
                self.builder.finish_node(self.current_span().start);
                self.direct_arrays
                    .as_mut()
                    .expect("read mode")
                    .insert(crate::read::PreparedKey::new(attribute, sample), tuple);
                return;
            }
        }
        self.parse_value_expr();
    }

    fn parse_value_expr(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ValueExpr, start);

        self.eat_trivia();
        match self.peek() {
            Some(TokenKind::Number) => {
                self.bump();
            }
            Some(TokenKind::Minus) => {
                self.bump(); // `-`
                self.eat_trivia();
                // `Number <- (Minus)? BasePart (ExponentPart)? / 'inf' /
                // Minus 'inf' / 'nan'` (AOUSD Core §16.2.5).
                if self.peek() == Some(TokenKind::Number)
                    || (self.peek() == Some(TokenKind::Ident) && self.current_text() == "inf")
                {
                    self.bump();
                } else {
                    let span = self.current_span();
                    self.error(span, "expected a number after `-`");
                }
            }
            Some(
                TokenKind::DoubleQuoteString
                | TokenKind::SingleQuoteString
                | TokenKind::MultilineDoubleQuoteString
                | TokenKind::MultilineSingleQuoteString,
            ) => {
                self.bump();
            }
            Some(TokenKind::AssetPath) => {
                self.parse_asset_ref();
            }
            Some(TokenKind::LeftAngle) => {
                self.parse_path_ref();
            }
            Some(TokenKind::LeftParen) => {
                self.parse_tuple_value();
            }
            Some(TokenKind::LeftBracket) => {
                self.parse_array_value();
            }
            Some(TokenKind::LeftBrace) => {
                self.parse_dictionary_value();
            }
            Some(TokenKind::Ident) => {
                if self.current_text() == "edit"
                    && self.next_non_trivia_kind() == Some(TokenKind::LeftBracket)
                {
                    self.parse_array_edit_value();
                } else {
                    self.bump(); // true/false/None/inf/nan/identifier
                }
            }
            _ => {
                let span = self.current_span();
                self.error(span, "expected value");
                self.bump();
            }
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_tuple_value(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::TupleValue, start);
        self.bump_stored::<LOSSLESS>(); // `(`
        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightParen) || self.current().is_none() {
                break;
            }
            self.parse_value_expr();
            self.eat_stored::<LOSSLESS>(TokenKind::Comma);
        }
        self.expect_stored::<LOSSLESS>(TokenKind::RightParen);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_array_value(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ArrayValue, start);
        self.bump_stored::<LOSSLESS>(); // `[`
        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                break;
            }
            self.parse_value_expr();
            // A `,` follows its item on the item's line (OpenUSD refuses
            // `[1\n, 2]` with "parse error matching ListValueClose").
            self.eat_trivia();
            if self.line_break && self.peek() == Some(TokenKind::Comma) {
                let span = self.current_span();
                self.error(span, "unexpected line break before `,`");
            }
            self.eat_stored::<LOSSLESS>(TokenKind::Comma);
        }
        self.expect_stored::<LOSSLESS>(TokenKind::RightBracket);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Parses `edit [instruction; instruction ...]`, whose instructions are
    /// separated by `;` or a line break, as OpenUSD's text format writes
    /// them (`ArrayEditValue`, `StatementSequenceOf` in
    /// `pxr/usd/sdf/textFileFormatParser.h`; `Vt_ArrayEditStreamImpl` in
    /// `pxr/base/vt/arrayEdit.cpp`).
    fn parse_array_edit_value(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ArrayEditValue, start);
        self.bump(); // `edit`
        self.expect(TokenKind::LeftBracket);

        let mut separated = true;
        loop {
            separated |= self.eat_trivia_crossing_newline();
            if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                break;
            }
            if !separated {
                let span = self.current_span();
                self.error(
                    span,
                    "expected `;` or a line break between array edit instructions",
                );
            }
            self.parse_array_edit_instruction();
            separated = self.eat_trivia_crossing_newline();
            if self.peek() == Some(TokenKind::Semicolon) {
                self.bump();
                separated = true;
            }
        }

        self.expect(TokenKind::RightBracket);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Consumes trivia, returning whether it included a line break.
    fn eat_trivia_crossing_newline(&mut self) -> bool {
        let mut newline = false;
        while let Some(tok) = self.current() {
            if !is_trivia(tok.kind) {
                break;
            }
            newline |= tok.kind == TokenKind::Newline;
            self.line_break |= tok.kind == TokenKind::Newline;
            if LOSSLESS {
                self.builder.token(SyntaxKind::from(tok.kind), tok.span);
            }
            self.seek(self.pos + 1);
        }
        newline
    }

    fn parse_array_edit_instruction(&mut self) {
        let start = self.current_span().start;
        self.builder
            .start_node(SyntaxKind::ArrayEditInstruction, start);

        self.eat_trivia();
        let keyword = self.current_text();
        if self.peek() == Some(TokenKind::Ident) {
            self.bump();
        }

        match keyword {
            "write" => {
                self.parse_array_edit_operand();
                self.eat_trivia();
                if self.peek() == Some(TokenKind::Ident) && self.current_text() == "to" {
                    self.bump();
                }
                self.parse_array_edit_index();
            }
            "insert" => {
                self.parse_array_edit_operand();
                self.eat_trivia();
                if self.peek() == Some(TokenKind::Ident) && self.current_text() == "at" {
                    self.bump();
                }
                self.parse_array_edit_index();
            }
            "prepend" | "append" => {
                self.parse_array_edit_operand();
            }
            "erase" => {
                self.parse_array_edit_index();
            }
            "minsize" | "maxsize" | "resize" => {
                self.eat_trivia();
                if matches!(self.peek(), Some(TokenKind::Number | TokenKind::Minus)) {
                    self.parse_value_expr();
                }
                // `minsize N fill <literal>`, `resize N fill <literal>`.
                let fill = self
                    .peek_past_trivia_from(self.pos)
                    .is_some_and(|(i, kind)| {
                        kind == TokenKind::Ident
                            && self
                                .tokens
                                .get(i)
                                .expect("lookahead checked")
                                .text(self.source)
                                == "fill"
                    });
                if keyword != "maxsize" && fill {
                    self.eat_keyword("fill");
                    self.parse_value_expr();
                }
            }
            _ => {
                let span = self.current_span();
                self.error(span, format!("unknown array edit instruction `{keyword}`"));
                // Recover at the next separator or the closing bracket.
                while let Some(tok) = self.current() {
                    if matches!(
                        tok.kind,
                        TokenKind::Newline | TokenKind::Semicolon | TokenKind::RightBracket
                    ) {
                        break;
                    }
                    self.bump();
                }
            }
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_array_edit_operand(&mut self) {
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftBracket) {
            self.parse_array_edit_index();
        } else {
            self.parse_value_expr();
        }
    }

    fn parse_array_edit_index(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ArrayEditIndex, start);
        self.expect(TokenKind::LeftBracket);
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Minus) {
            self.bump();
        }
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Number) {
            self.bump();
        } else {
            let span = self.current_span();
            self.error(span, "expected an array index");
        }
        self.expect(TokenKind::RightBracket);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_dictionary_value(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::DictionaryValue, start);
        self.expect(TokenKind::LeftBrace);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBrace) || self.current().is_none() {
                break;
            }
            if self.refuse_stray_separator() {
                continue;
            }
            self.parse_dictionary_entry();
            // Entries are separated by line breaks or `;`
            // (`StatementSequenceOf<DictionaryValueItem>`,
            // `pxr/usd/sdf/textFileFormatParser.h`).
            self.expect_entry_end(TokenKind::RightBrace);
        }

        self.expect(TokenKind::RightBrace);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_dictionary_entry(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::DictionaryEntry, start);

        // A string dictionary item, `"key": "value"`, as in
        // `prefixSubstitutions` and `suffixSubstitutions` (OpenUSD's
        // `StringDictionaryItem`, `pxr/usd/sdf/textFileFormatParser.h`).
        self.eat_trivia();
        if self.at_string() && self.peek_next_non_trivia() == Some(TokenKind::Colon) {
            self.bump(); // key
            self.expect(TokenKind::Colon);
            self.parse_value_expr();
            self.eat(TokenKind::Comma);
            let end = self.current_span().start;
            self.builder.finish_node(end);
            return;
        }

        // Optional type or `dictionary` keyword.
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "dictionary" {
            self.bump();
        } else if self.peek() == Some(TokenKind::Ident) {
            self.bump(); // type name
            // Array suffix.
            self.eat_trivia();
            if self.peek() == Some(TokenKind::LeftBracket) {
                self.bump();
                self.expect(TokenKind::RightBracket);
            }
        }

        // Key: string or identifier.
        self.eat_trivia();
        if self.at_string() {
            self.bump(); // string key
        } else if self.peek() == Some(TokenKind::Ident) {
            self.bump(); // ident key
        }

        self.expect(TokenKind::Equals);
        self.parse_value_expr();

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_time_sample_map(&mut self, attribute: u32, type_name: &str, is_array: bool) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::TimeSampleMap, start);
        self.expect(TokenKind::LeftBrace);

        let mut sample = 0;
        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBrace) || self.current().is_none() {
                break;
            }
            self.parse_time_sample_entry(attribute, sample, type_name, is_array);
            sample += 1;
            self.eat(TokenKind::Comma);
        }

        self.expect(TokenKind::RightBrace);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_time_sample_entry(
        &mut self,
        attribute: u32,
        sample: usize,
        type_name: &str,
        is_array: bool,
    ) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::TimeSampleEntry, start);

        // Time code (may be negative).
        self.parse_number_tokens();
        self.expect(TokenKind::Colon);

        // Value or None.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "None" {
            self.bump();
        } else {
            self.parse_attribute_value(attribute, Some(sample), type_name, is_array);
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    // ── Primitive token parsers ────────────────────────────────────

    /// Emits a number token (possibly preceded by `-`).
    fn parse_number_tokens(&mut self) {
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Minus) {
            self.bump();
        }
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Number) {
            self.bump();
        }
    }

    /// Emits a string token.
    fn parse_string_token(&mut self) {
        self.eat_trivia();
        match self.peek() {
            Some(
                TokenKind::DoubleQuoteString
                | TokenKind::SingleQuoteString
                | TokenKind::MultilineDoubleQuoteString
                | TokenKind::MultilineSingleQuoteString,
            ) => {
                self.bump();
            }
            _ => {
                let span = self.current_span();
                self.error(span, "expected string");
            }
        }
    }

    /// Parses an asset reference (`@...@` or `@@@...@@@`) into an `AssetRef`
    /// node.
    fn parse_asset_ref(&mut self) {
        self.eat_trivia();
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::AssetRef, start);

        if self.peek() == Some(TokenKind::AssetPath) {
            self.bump();
        } else {
            let span = self.current_span();
            self.error(span, "expected an asset path");
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Parses a path reference (`<...>`) into a `PathRef` node.
    fn parse_path_ref(&mut self) {
        self.eat_trivia();
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::PathRef, start);

        if self.peek() == Some(TokenKind::LeftAngle) {
            self.bump(); // <
        }

        // Consume everything until >.
        while let Some(tok) = self.current() {
            if tok.kind == TokenKind::RightAngle {
                self.bump(); // >
                break;
            }
            self.bump();
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    /// Parses a namespaced name `foo:bar:baz`.
    /// A property name, plain or namespaced (`a:b`). A missing name, and a
    /// `:` without a name after it, are reported, as OpenUSD's text parser
    /// rejects them.
    ///
    /// Spec: AOUSD Core §16.2.16 (property declarations).
    /// The value of `symmetryFunction`: an identifier, which may be C++
    /// namespaced (`Ns::fn`), or nothing at all (`symmetryFunction = `).
    ///
    /// OpenUSD: `SymmetryFunctionMetadata = symmetryFunction Assignment
    /// (Identifier)?` (`pxr/usd/sdf/textFileFormatParser.h`).
    fn parse_symmetry_function(&mut self) {
        while self
            .peek()
            .is_some_and(|kind| matches!(kind, TokenKind::Whitespace | TokenKind::BlockComment))
        {
            self.bump();
        }
        match self.peek() {
            Some(TokenKind::Ident) => {}
            None
            | Some(
                TokenKind::Newline
                | TokenKind::RightParen
                | TokenKind::Semicolon
                | TokenKind::PythonComment
                | TokenKind::CppComment,
            ) => return,
            Some(_) => {
                let span = self.current_span();
                self.error(
                    span,
                    "expected an identifier or nothing after `symmetryFunction =`",
                );
                return;
            }
        }
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ValueExpr, start);
        // `::` at the token `offset` past the current one.
        let colons = |parser: &Self, offset: usize| {
            let kind = |at: usize| parser.tokens.get(parser.pos + at).map(|t| t.kind);
            kind(offset) == Some(TokenKind::Colon) && kind(offset + 1) == Some(TokenKind::Colon)
        };
        if colons(self, 1) {
            self.builder.start_node(SyntaxKind::NamespacedName, start);
            self.bump();
            while colons(self, 0) {
                self.bump();
                self.bump();
                if self.peek() == Some(TokenKind::Ident) {
                    self.bump();
                } else {
                    let span = self.current_span();
                    self.error(span, "expected a name after `::`");
                    break;
                }
            }
            let end = self.current_span().start;
            self.builder.finish_node(end);
        } else {
            self.bump();
        }
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_namespaced_name(&mut self) {
        self.eat_trivia();

        // Check if this is actually a namespaced name (has colons).
        let name_start = self.current_span().start;
        if self.peek() != Some(TokenKind::Ident) {
            let span = self.current_span();
            self.error(span, "expected a property name");
            return;
        }

        // Look ahead to see if there are colons.
        let i = self.pos + 1;
        // No trivia between ident and colon in a namespaced name.
        let has_colon = self
            .tokens
            .get(i)
            .is_some_and(|token| token.kind == TokenKind::Colon);

        if has_colon {
            self.builder
                .start_node(SyntaxKind::NamespacedName, name_start);
            self.bump(); // first ident

            while self.peek() == Some(TokenKind::Colon) {
                self.bump(); // `:`
                if self.peek() == Some(TokenKind::Ident) {
                    self.bump();
                } else {
                    let span = self.current_span();
                    self.error(span, "expected a name after `:`");
                }
            }

            let end = self.current_span().start;
            self.builder.finish_node(end);
        } else {
            // Single ident — just emit the token.
            self.bump();
        }
    }

    fn parse_metadata_entry_inner(&mut self) {
        // [list-op keyword] key = value: a property's metadata may list
        // edit a field as a prim's does (`GeneralListOpMetadata`,
        // `pxr/usd/sdf/textFileFormatParser.h`).
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident)
            && is_list_op_keyword(self.current_text())
            && self.peek_next_non_trivia() == Some(TokenKind::Ident)
        {
            self.bump();
            self.eat_trivia();
        }
        let mut key = "";
        if self.peek() == Some(TokenKind::Ident) {
            key = self.current_text();
            self.bump(); // key
        }
        if key == "symmetryFunction" {
            // Its value may be nothing, so the line may end after `=`.
            self.eat_trivia();
            if self.peek() == Some(TokenKind::Equals) {
                self.bump();
            } else {
                let span = self.current_span();
                self.error(span, "expected Equals");
            }
            self.parse_symmetry_function();
            return;
        }
        self.expect(TokenKind::Equals);

        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "None" {
            self.bump();
        } else if self.peek() == Some(TokenKind::LeftBrace) {
            self.parse_dictionary_value();
        } else {
            self.parse_value_expr();
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Whether `text` is a list-op keyword prefixing a list-edited statement
/// (AOUSD Core §16.2.14). `add` is the legacy list edit that inserts an
/// item only when the list lacks it; unlike `append`, it never moves one.
fn is_list_op_keyword(text: &str) -> bool {
    matches!(text, "prepend" | "append" | "add" | "delete" | "reorder")
}

fn is_string_token(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::DoubleQuoteString
            | TokenKind::SingleQuoteString
            | TokenKind::MultilineDoubleQuoteString
            | TokenKind::MultilineSingleQuoteString
    )
}

/// Whether the string literal `text`, as the lexer delimits it, ends with
/// its closing quote (or three), past its escape sequences: the lexer
/// ends an unterminated quoted string at the line break.
fn string_is_terminated(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some(&quote) = bytes.first() else {
        return false;
    };
    let delimiter = if bytes.len() >= 3 && bytes[1] == quote && bytes[2] == quote {
        3
    } else {
        1
    };
    let mut i = delimiter;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == quote && bytes[i..].iter().take(delimiter).all(|&c| c == quote) => {
                return i + delimiter == bytes.len()
                    && bytes[i..].iter().take(delimiter).count() == delimiter;
            }
            _ => i += 1,
        }
    }
    false
}

fn is_trivia(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Whitespace
            | TokenKind::Newline
            | TokenKind::PythonComment
            | TokenKind::CppComment
            | TokenKind::BlockComment
    )
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::*;

    #[test]
    fn prim_and_variant_relocates_are_refused() {
        // OpenUSD 26.11 parses relocates only as layer metadata.
        for source in [
            "#usda 1.0\ndef \"A\" (\n    relocates = {\n        <B>: <D>\n    }\n    kind = \"group\"\n)\n{\n    def \"B\" {}\n}\n",
            "#usda 1.0\ndef \"A\" (\n    relocates = {\n    }\n    kind = \"group\"\n)\n{\n    def \"B\" {}\n}\n",
            "#usda 1.0\nover \"A\" {\n    variantSet \"v\" = {\n        \"x\" (\n            relocates = { <B>: <C> }\n        ) {}\n    }\n    def \"B\" {}\n}\n",
        ] {
            let result = parse(source);
            let [diagnostic] = result.diagnostics.as_slice() else {
                panic!("{source}: {:?}", result.diagnostics);
            };
            assert_eq!(diagnostic.span.text(source), "relocates", "{source}");
            let prim = &result.layer.prims[0];
            assert!(
                prim.metadata.iter().all(|entry| !matches!(
                    entry,
                    PrimMeta::Custom(MetadataEntry {
                        key: "relocates",
                        ..
                    })
                )),
                "{source}: {:?}",
                prim.metadata
            );
            assert!(!prim.children.is_empty(), "{source}");
        }
        // Layer relocates still parse.
        let layer = parse("#usda 1.0\n(\n    relocates = {\n        </A/B>: </A/C>\n    }\n)\n");
        assert!(layer.diagnostics.is_empty(), "{:?}", layer.diagnostics);
    }

    #[test]
    fn byte_order_mark_is_the_one_header_error() {
        // OpenUSD 26.11 imports no string that starts with a byte order
        // mark; recovery reads the layer behind it without more errors.
        let result = parse("\u{feff}#usda 1.0\ndef \"A\" {}\n");
        let [diagnostic] = result.diagnostics.as_slice() else {
            panic!("diagnostics: {:?}", result.diagnostics);
        };
        assert_eq!(diagnostic.span, Span::new(0, 3));
        assert!(diagnostic.message.contains("byte order mark"));
        assert_eq!(result.layer.prims.len(), 1);
    }

    #[test]
    fn ast_parsing_preserves_lossless_lowering_and_diagnostics() {
        // Both entry points must retain byte spans, header text and newline
        // separators even though AST loading omits trivia and list punctuation.
        let sources = [
            r#"  #usda 1.0
( customLayerData = { string note = "héllo" } )
def Xform "Root" (
    variants = { string look = "red" }
    prepend variantSets = ["look"]
) {
    // A comment between typed values must not change their spans.
    float3[] points = [(1, -0, .5), /* block */ (-1e-3, inf, nan)]
    custom string label = "a\nlabel"
    double weight.timeSamples = { 0: 1, 2: -2 }
    variantSet "look" = {
        "red" { custom token color = "red"; }
    }
    float[] edits = edit [
        write [1, 2] [0]
        minsize 4 fill [3]; erase [1]
    ]
}
"#,
            "#usda 1.0\r\ndef \"A\" { /* multiline\ncomment */ float[] a = edit [resize 3\r\nwrite [2] [1]]; }",
            "#usda 1.0\ndef \"A\" { double a.spline = { 0: 1; post linear, 2: 3; post held, }; }",
            "#usda 1.0\ndef \"A\" { float[] a = edit [resize 3 write [2] [1]] double b.curve = { 1: 0 } }",
            "#bad header\ndef \"A\" { float a = - /* comment */ token }",
            "#usda 1.0\ndef \"A\" { float3[] a = [(1,, 2), (3, 4] }",
            "#usda 1.0\ndef \"A\" { float[] a = [1, 2",
        ];
        for source in sources {
            let cst = parse_cst(source);
            let mut expected = crate::lower::lower(&cst.tree, source);
            let mut diagnostics = cst.diagnostics;
            diagnostics.append(&mut expected.diagnostics);
            expected.diagnostics = diagnostics;
            let actual = parse(source);
            assert_eq!(
                format!("{actual:?}"),
                format!("{expected:?}"),
                "AST content, source spans and diagnostics agree for {source}"
            );
        }
    }

    #[test]
    fn parse_reports_unsupported_attribute_field() {
        // An unknown attribute field is not parsed; the declaration survives
        // and the field is reported instead of being misread.
        let src = "#usda 1.0\ndef \"A\" {\n    double a.curve = {\n        1: 0; post held,\n    }\n    double b = 1\n}\n";
        let result = parse(src);
        assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
        assert!(result.diagnostics[0].message.contains("`.curve`"));
        let children = &result.layer.prims[0].children;
        assert_eq!(children.len(), 2, "{children:?}");
        let PrimChild::Attribute(a) = &children[0] else {
            panic!("expected attribute, got {:?}", children[0]);
        };
        assert_eq!((a.name, a.type_name), ("a", "double"));
        assert!(a.default.is_none());
        let PrimChild::Attribute(b) = &children[1] else {
            panic!("expected attribute, got {:?}", children[1]);
        };
        assert_eq!(b.name, "b");
        assert!(matches!(b.default, Some(Value::Int(1))));
    }

    /// Name lists take one name or a non-empty list, as OpenUSD 26.08
    /// (`Sdf.Layer.ImportFromString`) accepts them, for every list op form
    /// of `variantSets` and every `reorder` statement.
    #[test]
    fn name_lists_follow_openusd() {
        let prim: fn(&str) -> String =
            |field| format!("#usda 1.0\ndef \"P\" (\n    {field}\n)\n{{\n}}\n");
        let body: fn(&str) -> String =
            |statement| format!("#usda 1.0\ndef \"P\"\n{{\n    {statement}\n}}\n");
        let layer: fn(&str) -> String = |statement| format!("#usda 1.0\n\n{statement}\n");
        let mut sources = Vec::new();
        for op in ["", "prepend ", "append ", "delete ", "add ", "reorder "] {
            sources.push((prim, format!("{op}variantSets")));
        }
        sources.push((body, String::from("reorder nameChildren")));
        sources.push((body, String::from("reorder properties")));
        sources.push((layer, String::from("reorder rootPrims")));
        for (wrap, key) in sources {
            for (value, accepted) in [
                ("\"a\"", true),
                ("[\"a\", \"b\"]", true),
                ("[]", false),
                ("None", false),
            ] {
                let src = wrap(&format!("{key} = {value}"));
                let diagnostics = parse(&src).diagnostics;
                assert_eq!(
                    diagnostics.is_empty(),
                    accepted,
                    "{key} = {value}: {diagnostics:?}"
                );
            }
        }
    }

    /// A property needs a name, and each `:` of a namespaced name a name
    /// after it, as OpenUSD 26.08 (`Sdf.Layer.ImportFromString`) accepts
    /// property declarations.
    #[test]
    fn property_names_follow_openusd() {
        for (declaration, accepted) in [
            ("float = 1", false),
            ("float", false),
            ("custom float", false),
            ("float[] = [1]", false),
            ("rel = </P>", false),
            ("rel", false),
            ("float a: = 1", false),
            ("float :a = 1", false),
            ("double a::b = 1", false),
            ("float a:b = 1", true),
            ("float size = 1", true),
        ] {
            let src = format!("#usda 1.0\ndef \"P\"\n{{\n    {declaration}\n}}\n");
            let diagnostics = parse(&src).diagnostics;
            assert_eq!(
                diagnostics.is_empty(),
                accepted,
                "{declaration}: {diagnostics:?}"
            );
        }
    }

    /// Diagnostics for `int[] x = <value>`.
    fn array_edit_diagnostics(value: &str) -> usize {
        let src = format!("#usda 1.2\ndef \"A\"\n{{\n    int[] x = {value}\n}}\n");
        parse(&src).diagnostics.len()
    }

    /// Each form is accepted or rejected as OpenUSD 26.08
    /// (`Sdf.Layer.ImportFromString`) accepts or rejects it.
    #[test]
    fn array_edit_syntax_follows_openusd() {
        for accepted in [
            "edit []",
            "edit [ ]",
            "edit [append 4]",
            "edit [append 1;]",
            "edit [minsize 3 fill 7; resize 2 fill -1; maxsize 9]",
            "edit [\n        write 9 to [0]\n        append 4\n        erase [-2]\n    ]",
        ] {
            assert_eq!(array_edit_diagnostics(accepted), 0, "{accepted}");
        }
        for rejected in [
            "edit (append 4)",
            "edit [append 4, append 5]",
            "edit [write 1 to [end]]",
            "edit [append 1 append 2]",
            "edit [;append 1]",
            "edit [append 1;;append 2]",
        ] {
            assert_ne!(array_edit_diagnostics(rejected), 0, "{rejected}");
        }
    }

    /// `;` separates statements as a line break does (AOUSD Core §16.2.10),
    /// as in the supplemental corpus's `primmetadata.usda` and `BasicOwner`.
    /// OpenUSD 26.08 reads this text to the same specs.
    #[test]
    fn semicolons_separate_statements() {
        let src = "#usda 1.0\n(\n    owner = \"foo\"; startTimeCode = 1\n)\n\nover \"Sphere\" (\n    \
                   kind = \"component\"; displayName = \"d\";\n)\n{\n    double radius = 999; \
                   double b = 1 (hidden = true; doc = \"x\")\n}\n";
        let result = parse(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.layer.metadata.len(), 2);
        assert_eq!(result.layer.prims[0].metadata.len(), 2);
        let children = &result.layer.prims[0].children;
        assert_eq!(children.len(), 2, "{children:?}");
        let PrimChild::Attribute(b) = &children[1] else {
            panic!("expected an attribute");
        };
        assert_eq!(b.metadata.len(), 2);
    }

    #[test]
    fn array_edit_fill_forms_parse() {
        let src = "#usda 1.2\ndef \"A\"\n{\n    int[] x = edit [minsize 3 fill 7; resize 2]\n}\n";
        let result = parse(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let PrimChild::Attribute(a) = &result.layer.prims[0].children[0] else {
            panic!("expected an attribute");
        };
        let Some(Value::ArrayEdit(edit)) = &a.default else {
            panic!("expected an array edit");
        };
        assert!(matches!(
            edit.instructions[..],
            [
                ArrayEditInstruction::MinSizeFill {
                    len: 3,
                    fill: Value::Int(7)
                },
                ArrayEditInstruction::Resize(2)
            ]
        ));
    }

    #[test]
    fn parse_minimal_layer() {
        let result = parse("#usda 1.0\n");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.layer.version, "1.0");
        assert!(result.layer.prims.is_empty());
    }

    #[test]
    fn parse_empty_prim() {
        let result = parse("#usda 1.0\ndef \"Foo\" {\n}\n");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.layer.prims.len(), 1);
        assert_eq!(result.layer.prims[0].name, "Foo");
        assert_eq!(result.layer.prims[0].specifier, Specifier::Def);
    }

    #[test]
    fn parse_typed_prim() {
        let result = parse("#usda 1.0\ndef Mesh \"myMesh\" {\n}\n");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.layer.prims[0].type_name, Some("Mesh"));
        assert_eq!(result.layer.prims[0].name, "myMesh");
    }

    #[test]
    fn parse_sublayers() {
        let src = "#usda 1.0\n(\n    subLayers = [\n        @./sub.usd@\n    ]\n)\n";
        let result = parse(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let meta = &result.layer.metadata;
        assert_eq!(meta.len(), 1);
        if let LayerMeta::SubLayers(items) = &meta[0] {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].asset, "./sub.usd");
        } else {
            panic!("expected SubLayers metadata");
        }
    }

    /// OpenUSD 26.08 rejects each of these with "Expected }": a word after
    /// a property is no property of its own, and `keep` is no list-op
    /// keyword. AOUSD Core §16.2.
    #[test]
    fn words_after_a_property_are_errors() {
        for property in [
            "keep rel targets = </A/B>",
            "bogus rel other = </A>",
            "keep int x = 1",
            "keep double z.connect = </A.y>",
            "double x = 1 junk",
            "double x junk",
            "double a = 1 double b = 2",
            "double meta = 1 (doc = \"d\") junk",
        ] {
            let text =
                format!("#usda 1.0\ndef \"W\" {{\n    {property}\n    double after = 2\n}}\n");
            let parsed = parse_cst(&text);
            assert_eq!(
                parsed.diagnostics.len(),
                1,
                "{property}: {:?}",
                parsed.diagnostics
            );
        }
    }

    /// What OpenUSD 26.08 accepts around a property's end: another
    /// property on the next line or after `;`, metadata opening on the
    /// property's line, a trailing comment, and an unknown type name (a
    /// type name is any identifier to the parser).
    #[test]
    fn properties_end_at_a_line_break_or_semicolon() {
        let text = "#usda 1.0\ndef \"W\" {\n    double a = 1 # note\n    double b\n    double c (\n        doc = \"d\"\n    )\n    double d = 1; double e = 2\n    rel r = </W>\n    notatype f = 1\n}\n";
        let parsed = parse_cst(text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    }

    /// OpenUSD 26.11 wants a line break or `;` after every statement of a
    /// prim body, even before the closing brace, and a line break after a
    /// prim: `def "W" { double x = 1 }` and `} def "B"` are refused, as is
    /// a `;` that ends no statement on its line (`PrimItem`, `LayerSpec`,
    /// `StatementSequenceOf`, `pxr/usd/sdf/textFileFormatParser.h`).
    #[test]
    fn statements_end_at_a_line_break_or_semicolon() {
        for refused in [
            "def \"W\" { double x = 1 }\n",
            "def \"W\" {\n    rel r = </W> /* note */ }\n",
            "def \"W\" {\n    reorder nameChildren = [\"a\"] }\n",
            "def \"W\" {\n    def \"A\" {\n    } def \"B\" {\n    }\n}\n",
            "def \"W\" { def \"A\" {} }\n",
            "def \"W\" {\n    variantSet \"v\" = {\n        \"a\" {\n        }\n    };\n}\n",
            "def \"W\" {\n}\ndef \"A\" {\n} def \"B\" {\n}\n",
            "def \"W\" {\n};\n",
            "def \"W\" {\n    double x = 1;;\n}\n",
            "def \"W\" {\n    double x = 1\n    ;\n}\n",
            "(\n    doc = \"d\";\n    ;\n)\n",
            "def \"W\" (\n    customData = { ; }\n)\n{\n}\n",
            "reorder rootPrims = [\"W\"] def \"W\" {\n}\n",
        ] {
            let parsed = parse_cst(&format!("#usda 1.0\n{refused}"));
            assert!(!parsed.diagnostics.is_empty(), "{refused}");
        }
        for read in [
            "def \"W\" { double x = 1; }\n",
            "def \"W\" {\n    reorder nameChildren = [\"a\"]; }\n",
            "def \"W\" {\n    double x = 1; def \"A\" {\n    } # note\n}\n",
            "def \"W\" {\n    variantSet \"v\" = { \"a\" {} \"b\" {} }\n}\n",
            "def \"W\" (\n    customData = { int a = 1; }\n)\n{\n}\n",
            "reorder rootPrims = [\"W\"]; def \"W\" {}",
        ] {
            let parsed = parse_cst(&format!("#usda 1.0\n{read}"));
            assert!(
                parsed.diagnostics.is_empty(),
                "{read}: {:?}",
                parsed.diagnostics
            );
        }
    }

    /// OpenUSD 26.08 rejects a property's metadata opening on the next
    /// line ("Expected }"); a prim's may.
    #[test]
    fn property_metadata_opens_on_the_property_line() {
        for property in ["double c", "rel r"] {
            let text = format!(
                "#usda 1.0\ndef \"W\" {{\n    {property}\n    (\n        doc = \"d\"\n    )\n}}\n"
            );
            assert!(!parse_cst(&text).diagnostics.is_empty(), "{property}");
        }
        let prim = "#usda 1.0\ndef \"W\"\n(\n    doc = \"d\"\n)\n{\n}\n";
        assert!(parse_cst(prim).diagnostics.is_empty());
    }

    #[test]
    fn parse_attribute_with_value() {
        let src = "#usda 1.0\ndef \"Foo\" {\n    int bar = 42\n}\n";
        let result = parse(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let children = &result.layer.prims[0].children;
        assert_eq!(children.len(), 1);
        if let PrimChild::Attribute(attr) = &children[0] {
            assert_eq!(attr.type_name, "int");
            assert_eq!(attr.name, "bar");
            assert!(matches!(attr.default, Some(Value::Int(42))));
        } else {
            panic!("expected Attribute");
        }
    }

    #[test]
    fn parse_reference_metadata() {
        let src = "#usda 1.0\ndef \"Foo\" (\n    references = @./ref.usd@</Bar>\n) {\n}\n";
        let result = parse(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let meta = &result.layer.prims[0].metadata;
        assert_eq!(meta.len(), 1);
        if let PrimMeta::References(refs) = &meta[0] {
            assert_eq!(refs.kind, ListOpKind::Explicit);
            let items = refs.items.as_ref().unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].asset.as_deref(), Some("./ref.usd"));
            assert_eq!(items[0].prim_path, Some("/Bar"));
        } else {
            panic!("expected References");
        }
    }

    // ── Lossless roundtrip tests ─────────────────────────────────

    /// Helper: assert CST roundtrips source exactly.
    fn assert_roundtrip(src: &str) {
        let result = parse_cst(src);
        assert!(
            result.diagnostics.is_empty(),
            "diagnostics for input: {src:?}\n{:?}",
            result.diagnostics,
        );
        assert_eq!(
            result.tree.to_text(src),
            src,
            "roundtrip failed for: {src:?}"
        );
    }

    #[test]
    fn lossless_roundtrip_minimal() {
        assert_roundtrip("#usda 1.0\n");
    }

    #[test]
    fn lossless_roundtrip_layer_metadata() {
        assert_roundtrip(
            "\
#usda 1.0
(
    subLayers = [
        @./sub.usd@
    ]
)
",
        );
    }

    #[test]
    fn lossless_roundtrip_sublayer_with_offset() {
        assert_roundtrip(
            "\
#usda 1.0
(
    subLayers = [
        @./a.usd@ (offset = 10; scale = 2)
    ]
)
",
        );
    }

    #[test]
    fn lossless_roundtrip_relocates() {
        assert_roundtrip(
            "\
#usda 1.0
(
    relocates = {
        </Old>: </New>
    }
)
",
        );
    }

    #[test]
    fn lossless_roundtrip_doc_metadata() {
        assert_roundtrip("#usda 1.0\n(\n    doc = \"hello world\"\n)\n");
    }

    #[test]
    fn lossless_roundtrip_empty_prim() {
        assert_roundtrip("#usda 1.0\ndef \"Foo\" {\n}\n");
    }

    #[test]
    fn lossless_roundtrip_typed_prim() {
        assert_roundtrip("#usda 1.0\ndef Mesh \"myMesh\" {\n}\n");
    }

    #[test]
    fn lossless_roundtrip_over_prim() {
        assert_roundtrip("#usda 1.0\nover \"Foo\" {\n}\n");
    }

    #[test]
    fn lossless_roundtrip_class_prim() {
        assert_roundtrip("#usda 1.0\nclass \"_Base\" {\n}\n");
    }

    #[test]
    fn lossless_roundtrip_nested_prims() {
        assert_roundtrip(
            "\
#usda 1.0
def Scope \"root\" {
    def Mesh \"child\" {
    }
    over \"other\" {
    }
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_prim_metadata() {
        assert_roundtrip(
            "\
#usda 1.0
def \"Foo\" (
    kind = \"component\"
    doc = \"some doc\"
) {
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_references() {
        assert_roundtrip(
            "\
#usda 1.0
def \"A\" (
    prepend references = @./ref.usd@</Bar>
) {
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_inherits() {
        assert_roundtrip("#usda 1.0\ndef \"A\" (\n    inherits = </Base>\n) {\n}\n");
    }

    #[test]
    fn lossless_roundtrip_specializes() {
        assert_roundtrip("#usda 1.0\ndef \"A\" (\n    specializes = </Base>\n) {\n}\n");
    }

    #[test]
    fn lossless_roundtrip_payload() {
        assert_roundtrip("#usda 1.0\ndef \"A\" (\n    payload = @./p.usd@</Root>\n) {\n}\n");
    }

    #[test]
    fn lossless_roundtrip_int_attribute() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    int x = 42\n}\n");
    }

    #[test]
    fn lossless_roundtrip_float_attribute() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    float y = 3.14\n}\n");
    }

    #[test]
    fn lossless_roundtrip_negative_value() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    int x = -7\n}\n");
    }

    #[test]
    fn lossless_roundtrip_string_attribute() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    string s = \"hello\"\n}\n");
    }

    #[test]
    fn lossless_roundtrip_bool_attribute() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    bool b = true\n}\n");
    }

    #[test]
    fn lossless_roundtrip_blocked_attribute() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    int x = None\n}\n");
    }

    #[test]
    fn lossless_roundtrip_custom_uniform() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    custom uniform int x = 1\n}\n");
    }

    #[test]
    fn lossless_roundtrip_array_type() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    int[] ids = [1, 2, 3]\n}\n");
    }

    #[test]
    fn lossless_roundtrip_tuple_value() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    float3 p = (1.0, 2.0, 3.0)\n}\n");
    }

    #[test]
    fn lossless_roundtrip_namespaced_name() {
        assert_roundtrip(
            "#usda 1.0\ndef \"A\" {\n    color3f primvars:displayColor = (1, 0, 0)\n}\n",
        );
    }

    #[test]
    fn lossless_roundtrip_asset_attribute() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    asset path = @./texture.png@\n}\n");
    }

    #[test]
    fn lossless_roundtrip_relationship() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    rel target = </B>\n}\n");
    }

    #[test]
    fn lossless_roundtrip_relationship_array() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    rel targets = [</B>, </C>]\n}\n");
    }

    #[test]
    fn lossless_roundtrip_custom_rel() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    custom rel foo\n}\n");
    }

    #[test]
    fn lossless_roundtrip_prepend_rel() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    prepend rel p = </B>\n}\n");
    }

    #[test]
    fn lossless_roundtrip_connection() {
        assert_roundtrip("#usda 1.0\ndef \"A\" {\n    float x.connect = </B.x>\n}\n");
    }

    #[test]
    fn lossless_roundtrip_time_samples() {
        assert_roundtrip(
            "\
#usda 1.0
def \"A\" {
    float x.timeSamples = {
        1: 0.0,
        2: 1.0,
    }
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_variant_set() {
        assert_roundtrip(
            "\
#usda 1.0
def \"A\" {
    variantSet \"color\" = {
        \"red\" {
            int r = 255
        }
        \"blue\" {
            int r = 0
        }
    }
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_variant_selections() {
        assert_roundtrip(
            "\
#usda 1.0
def \"A\" (
    variants = {
        string shade = \"red\"
    }
) {
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_reorder_properties() {
        assert_roundtrip(
            "\
#usda 1.0
def \"A\" {
    reorder properties = [\"b\", \"a\"]
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_reorder_root_prims() {
        assert_roundtrip("#usda 1.0\nreorder rootPrims = [\"B\", \"A\"]\n");
    }

    #[test]
    fn lossless_roundtrip_dictionary_value() {
        assert_roundtrip(
            "\
#usda 1.0
def \"A\" (
    customData = {
        string foo = \"bar\"
        int count = 42
    }
) {
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_attribute_metadata() {
        assert_roundtrip(
            "\
#usda 1.0
def \"A\" {
    int x = 1 (
        doc = \"my attr\"
    )
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_multiple_prims() {
        assert_roundtrip(
            "\
#usda 1.0
def \"A\" {
}
def \"B\" {
}
def \"C\" {
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_comments_preserved() {
        assert_roundtrip(
            "\
#usda 1.0
// top-level comment
def \"A\" {
    // inside prim
    int x = 1
}
",
        );
    }

    #[test]
    fn parse_prepend_api_schemas() {
        let src = "#usda 1.0\ndef Mesh \"card\" (\n    prepend apiSchemas = [\"MaterialBindingAPI\"]\n) {\n}\n";
        let result = parse(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let meta = &result.layer.prims[0].metadata;
        assert_eq!(meta.len(), 1);
        if let PrimMeta::Custom(entry) = &meta[0] {
            assert_eq!(entry.key, "apiSchemas");
            assert_eq!(entry.op, ListOpKind::Prepend);
        } else {
            panic!("expected Custom metadata, got {:?}", meta[0]);
        }
    }

    #[test]
    fn lossless_roundtrip_prepend_api_schemas() {
        assert_roundtrip(
            "\
#usda 1.0
def Mesh \"card\" (
    prepend apiSchemas = [\"MaterialBindingAPI\"]
) {
}
",
        );
    }

    #[test]
    fn lossless_roundtrip_complex_scene() {
        assert_roundtrip(
            "\
#usda 1.0
(
    doc = \"complex test\"
    subLayers = [
        @./base.usd@
    ]
)

def Scope \"World\" (
    kind = \"assembly\"
) {
    def Mesh \"hero\" (
        prepend references = @./hero.usd@</Root>
    ) {
        float3[] extent = [(-1, -1, -1), (1, 1, 1)]
        color3f primvars:displayColor = (0.5, 0.5, 0.5)
        rel material:binding = </World/Materials/Default>
    }

    def Scope \"Materials\" {
        def \"Default\" {
        }
    }

    over \"override\" {
        int x = None
    }
}
",
        );
    }
}
