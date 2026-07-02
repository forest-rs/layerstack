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

/// Parses a USDA source string into an AST (via CST → lower pipeline).
pub fn parse(source: &str) -> ParseResult<'_> {
    let cst_result = parse_cst(source);
    let mut ast_result = crate::lower::lower(&cst_result.tree, source);
    // Merge CST diagnostics into AST result.
    let mut all_diag = cst_result.diagnostics;
    all_diag.append(&mut ast_result.diagnostics);
    ast_result.diagnostics = all_diag;
    ast_result
}

/// Parses a USDA source string into a lossless CST.
pub fn parse_cst(source: &str) -> CstParseResult {
    let tokens = tokenize(source);
    let mut parser = Parser::new(source, tokens);
    parser.parse_source_file();
    let tree = parser.builder.finish();
    CstParseResult {
        tree,
        diagnostics: parser.diagnostics,
    }
}

// ── Parser ─────────────────────────────────────────────────────────────

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    builder: TreeBuilder,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, tokens: Vec<Token>) -> Self {
        Self {
            source,
            tokens,
            pos: 0,
            diagnostics: Vec::new(),
            builder: TreeBuilder::new(source.len() as u32),
        }
    }

    // ── Token navigation ───────────────────────────────────────────

    /// Returns the current token without advancing.
    fn current(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
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
        let tok = self.tokens.get(self.pos).copied();
        if let Some(t) = tok {
            self.builder.token(SyntaxKind::from(t.kind), t.span);
            self.pos += 1;
        }
        tok
    }

    /// Emits all trivia tokens (whitespace, newlines, comments) to the builder.
    fn eat_trivia(&mut self) {
        while let Some(tok) = self.current() {
            if is_trivia(tok.kind) {
                self.builder.token(SyntaxKind::from(tok.kind), tok.span);
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Skips whitespace/newline tokens only (not comments). Emits them to the builder.
    fn eat_whitespace_only(&mut self) {
        while let Some(tok) = self.current() {
            if tok.kind == TokenKind::Whitespace || tok.kind == TokenKind::Newline {
                self.builder.token(SyntaxKind::from(tok.kind), tok.span);
                self.pos += 1;
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
        self.eat_trivia();
        if self.peek() == Some(kind) {
            let tok = self.bump().unwrap();
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
        self.eat_trivia();
        if self.peek() == Some(kind) {
            let tok = self.bump().unwrap();
            Some(tok.text(self.source))
        } else {
            let span = self.current_span();
            self.error(span, format!("expected {kind:?}"));
            None
        }
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
        let mut i = self.pos + 1;
        while i < self.tokens.len() {
            if !is_trivia(self.tokens[i].kind) {
                return Some(self.tokens[i].kind);
            }
            i += 1;
        }
        None
    }

    /// Peek ahead past trivia from a given position, return (position, kind).
    fn peek_past_trivia_from(&self, start: usize) -> Option<(usize, TokenKind)> {
        let mut i = start;
        while i < self.tokens.len() {
            if !is_trivia(self.tokens[i].kind) {
                return Some((i, self.tokens[i].kind));
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

            if self.peek() == Some(TokenKind::Ident) && self.current_text() == "reorder" {
                self.parse_reorder_statement();
            } else if self.peek() == Some(TokenKind::Ident)
                && matches!(self.current_text(), "def" | "over" | "class")
            {
                self.parse_prim();
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
            }
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
                // Parse and discard per §16.2.17.5.
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
            self.eat(TokenKind::Comma);
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

    fn parse_prim(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::PrimDef, start);

        // Specifier: def/over/class.
        self.bump(); // emit specifier ident

        // Optional type name (ident followed by a string).
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident)
            && self.peek_next_non_trivia() == Some(TokenKind::DoubleQuoteString)
        {
            self.bump(); // type name
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

            self.parse_prim_meta_entry();
        }

        self.expect(TokenKind::RightParen);
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
                "append" | "add" => {
                    self.bump();
                    crate::ast::ListOpKind::Append
                }
                "delete" => {
                    self.bump();
                    crate::ast::ListOpKind::Delete
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
        if self.peek() == Some(TokenKind::At) {
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

    fn parse_name_list(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::NameList, start);

        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "None" {
            self.bump();
        } else if self.at_string() {
            // Single string.
            self.parse_string_token();
        } else {
            self.expect(TokenKind::LeftBracket);
            loop {
                self.eat_trivia();
                if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                    break;
                }
                self.parse_string_token();
                self.eat(TokenKind::Comma);
            }
            self.expect(TokenKind::RightBracket);
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
        self.pos = pos;
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

            if self.peek() == Some(TokenKind::Ident)
                && matches!(self.current_text(), "def" | "over" | "class")
            {
                self.parse_prim();
            } else if self.peek() == Some(TokenKind::Ident) && self.current_text() == "variantSet" {
                self.parse_variant_set();
            } else if self.peek() == Some(TokenKind::Ident) && self.current_text() == "reorder" {
                self.parse_reorder_statement();
            } else if (self.peek() == Some(TokenKind::Ident) && self.current_text() == "rel")
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
        if self.peek() != Some(TokenKind::Ident) {
            return false;
        }
        if !matches!(self.current_text(), "prepend" | "append" | "add" | "delete") {
            return false;
        }
        let mut i = self.pos + 1;
        while i < self.tokens.len() && is_trivia(self.tokens[i].kind) {
            i += 1;
        }
        // Check for `custom` then `rel` or directly `rel`.
        if i < self.tokens.len() && self.tokens[i].kind == TokenKind::Ident {
            let text = self.tokens[i].text(self.source);
            if text == "rel" {
                return true;
            }
            if text == "custom" {
                // Look one more ahead for `rel`.
                i += 1;
                while i < self.tokens.len() && is_trivia(self.tokens[i].kind) {
                    i += 1;
                }
                return i < self.tokens.len()
                    && self.tokens[i].kind == TokenKind::Ident
                    && self.tokens[i].text(self.source) == "rel";
            }
        }
        false
    }

    /// Check if `custom` keyword is followed (eventually) by `rel`.
    fn is_custom_rel(&self) -> bool {
        let mut i = self.pos + 1;
        while i < self.tokens.len() && is_trivia(self.tokens[i].kind) {
            i += 1;
        }
        i < self.tokens.len()
            && self.tokens[i].kind == TokenKind::Ident
            && self.tokens[i].text(self.source) == "rel"
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
        if self.peek() != Some(TokenKind::Ident) {
            return false;
        }
        if !matches!(self.current_text(), "prepend" | "append" | "add" | "delete") {
            return false;
        }
        let mut i = self.pos + 1;
        while i < self.tokens.len() && is_trivia(self.tokens[i].kind) {
            i += 1;
        }
        // If next is `rel`, it's a relationship.
        if i < self.tokens.len()
            && self.tokens[i].kind == TokenKind::Ident
            && self.tokens[i].text(self.source) == "rel"
        {
            return false;
        }
        // If next is `custom`, look further.
        if i < self.tokens.len()
            && self.tokens[i].kind == TokenKind::Ident
            && self.tokens[i].text(self.source) == "custom"
        {
            i += 1;
            while i < self.tokens.len() && is_trivia(self.tokens[i].kind) {
                i += 1;
            }
            if i < self.tokens.len()
                && self.tokens[i].kind == TokenKind::Ident
                && self.tokens[i].text(self.source) == "rel"
            {
                return false;
            }
        }
        i < self.tokens.len() && self.tokens[i].kind == TokenKind::Ident
    }

    // ── Attributes ─────────────────────────────────────────────────

    fn parse_attribute(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::AttributeDef, start);

        // Optional list-op prefix.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident)
            && matches!(self.current_text(), "prepend" | "append" | "add" | "delete")
        {
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

        // Type name.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) {
            self.bump();
        }

        // Array suffix `[]`.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftBracket) {
            self.bump();
            self.expect(TokenKind::RightBracket);
        }

        // Attribute name (namespaced).
        self.parse_namespaced_name();

        // What follows: .timeSamples, .connect, =, or metadata (
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Dot) {
            self.parse_attribute_suffix();
        } else if self.peek() == Some(TokenKind::Equals) {
            self.bump(); // =
            self.parse_value_expr();
        }

        // Optional metadata block.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftParen) {
            self.parse_attribute_metadata();
        }

        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_attribute_suffix(&mut self) {
        self.eat_trivia();
        if self.peek() != Some(TokenKind::Dot) {
            return;
        }

        // Peek to determine suffix type.
        let next = self.peek_next_non_trivia();
        if next == Some(TokenKind::Ident) {
            let mut i = self.pos + 1;
            while i < self.tokens.len() && is_trivia(self.tokens[i].kind) {
                i += 1;
            }
            if i < self.tokens.len() {
                let text = self.tokens[i].text(self.source);
                if text == "timeSamples" {
                    self.parse_time_samples_suffix();
                    return;
                } else if text == "connect" {
                    self.parse_connection_suffix();
                    return;
                }
            }
        }
        // Unknown suffix — just emit the dot.
        self.bump();
    }

    fn parse_time_samples_suffix(&mut self) {
        let start = self.current_span().start;
        self.builder
            .start_node(SyntaxKind::TimeSamplesSuffix, start);
        self.bump(); // `.`
        self.bump(); // `timeSamples`
        self.expect(TokenKind::Equals);
        self.parse_time_sample_map();
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
        }
        self.expect(TokenKind::RightParen);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    // ── Relationships ──────────────────────────────────────────────

    fn parse_relationship(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::RelationshipDef, start);

        // Optional list-op prefix.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident)
            && matches!(self.current_text(), "prepend" | "append" | "add" | "delete")
        {
            self.bump();
        }

        // `custom`.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) && self.current_text() == "custom" {
            self.bump();
        }

        // `rel`.
        self.eat_keyword("rel");

        // Name.
        self.parse_namespaced_name();

        // Optional `.timeSamples` or `.default` (uncommon but legal per grammar).
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Dot) {
            self.bump();
            if self.at_keyword("timeSamples") || self.at_keyword("default") {
                self.bump();
                self.expect(TokenKind::Equals);
                self.parse_value_expr(); // consume and discard
            }
        }

        // Optional assignment.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Equals) {
            self.bump();
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

        // Optional metadata block.
        self.eat_trivia();
        if self.peek() == Some(TokenKind::LeftParen) {
            self.parse_attribute_metadata();
        }

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
                if self.peek() == Some(TokenKind::Number) {
                    self.bump();
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
            Some(TokenKind::At) => {
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
                    && self.next_non_trivia_kind() == Some(TokenKind::LeftParen)
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
        self.bump(); // `(`
        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightParen) || self.current().is_none() {
                break;
            }
            self.parse_value_expr();
            self.eat(TokenKind::Comma);
        }
        self.expect(TokenKind::RightParen);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_array_value(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ArrayValue, start);
        self.bump(); // `[`
        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBracket) || self.current().is_none() {
                break;
            }
            self.parse_value_expr();
            self.eat(TokenKind::Comma);
        }
        self.expect(TokenKind::RightBracket);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_array_edit_value(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::ArrayEditValue, start);
        self.bump(); // `edit`
        self.expect(TokenKind::LeftParen);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightParen) || self.current().is_none() {
                break;
            }
            self.parse_array_edit_instruction();
            self.eat(TokenKind::Comma);
        }

        self.expect(TokenKind::RightParen);
        let end = self.current_span().start;
        self.builder.finish_node(end);
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
            }
            _ => {}
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
        if self.peek() == Some(TokenKind::Number)
            || (self.peek() == Some(TokenKind::Ident) && self.current_text() == "end")
        {
            self.bump();
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
            self.parse_dictionary_entry();
        }

        self.expect(TokenKind::RightBrace);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_dictionary_entry(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::DictionaryEntry, start);

        // Optional type or `dictionary` keyword.
        self.eat_trivia();
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

    fn parse_time_sample_map(&mut self) {
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::TimeSampleMap, start);
        self.expect(TokenKind::LeftBrace);

        loop {
            self.eat_trivia();
            if self.peek() == Some(TokenKind::RightBrace) || self.current().is_none() {
                break;
            }
            self.parse_time_sample_entry();
            self.eat(TokenKind::Comma);
        }

        self.expect(TokenKind::RightBrace);
        let end = self.current_span().start;
        self.builder.finish_node(end);
    }

    fn parse_time_sample_entry(&mut self) {
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
            self.parse_value_expr();
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

    /// Parses an asset reference (`@...@`) into an `AssetRef` node.
    fn parse_asset_ref(&mut self) {
        self.eat_trivia();
        let start = self.current_span().start;
        self.builder.start_node(SyntaxKind::AssetRef, start);

        if self.peek() == Some(TokenKind::At) {
            self.bump(); // opening @
        }

        // Consume everything until closing @.
        while let Some(tok) = self.current() {
            if tok.kind == TokenKind::At {
                self.bump(); // closing @
                break;
            }
            self.bump();
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
    fn parse_namespaced_name(&mut self) {
        self.eat_trivia();

        // Check if this is actually a namespaced name (has colons).
        let name_start = self.current_span().start;
        if self.peek() != Some(TokenKind::Ident) {
            return;
        }

        // Look ahead to see if there are colons.
        let i = self.pos + 1;
        // No trivia between ident and colon in a namespaced name.
        let has_colon = i < self.tokens.len() && self.tokens[i].kind == TokenKind::Colon;

        if has_colon {
            self.builder
                .start_node(SyntaxKind::NamespacedName, name_start);
            self.bump(); // first ident

            while self.peek() == Some(TokenKind::Colon) {
                self.bump(); // `:`
                if self.peek() == Some(TokenKind::Ident) {
                    self.bump();
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
        // key = value
        self.eat_trivia();
        if self.peek() == Some(TokenKind::Ident) {
            self.bump(); // key
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
            assert_eq!(items[0].asset, Some("./ref.usd"));
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
