// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Grammar navigation over eager editing tokens or lazy import tokens.
//! Numeric events bypass this cache; rollback retains structural tokens only.

use crate::{
    Span,
    lexer::{Lexer, Token, TokenKind},
    read::PreparedValue,
};
use alloc::vec::Vec;
use core::cell::RefCell;

pub(crate) enum TokenSource<'a> {
    Eager(Vec<Token>),
    Streaming(RefCell<Cursor<'a>>),
}

/// Lazily lexed tokens: a window from `base` on, lexed as lookahead asks and
/// released once the parser can no longer return to them.
pub(crate) struct Cursor<'a> {
    source: &'a str,
    lexer: Lexer<'a>,
    /// Index of `tokens[0]` in the whole token stream.
    base: usize,
    tokens: Vec<Token>,
    /// The most tokens the window held at once.
    peak: usize,
}

/// Consumed tokens a window keeps before it moves its lookahead to the
/// front, so a release costs a short move once per this many tokens.
pub(crate) const RELEASE_AFTER: usize = 32;

/// Room for the consumed tokens and the grammar's lookahead past them (a
/// few tokens), so that a window rarely grows.
const WINDOW: usize = 2 * RELEASE_AFTER;

impl<'a> TokenSource<'a> {
    pub(crate) fn eager(tokens: Vec<Token>) -> Self {
        Self::Eager(tokens)
    }
    pub(crate) fn streaming(source: &'a str) -> Self {
        Self::Streaming(RefCell::new(Cursor {
            source,
            lexer: Lexer::new(source),
            base: 0,
            tokens: Vec::with_capacity(WINDOW),
            peak: 0,
        }))
    }
    pub(crate) fn get(&self, index: usize) -> Option<Token> {
        match self {
            Self::Eager(tokens) => tokens.get(index).copied(),
            Self::Streaming(cursor) => {
                let mut cursor = cursor.borrow_mut();
                let at = index
                    .checked_sub(cursor.base)
                    .expect("released tokens are never read again");
                while cursor.tokens.len() <= at {
                    let token = cursor.lexer.next()?;
                    cursor.tokens.push(token);
                    cursor.peak = cursor.peak.max(cursor.tokens.len());
                }
                cursor.tokens.get(at).copied()
            }
        }
    }
    /// Drops streamed tokens before `index`, which the parser will not read
    /// again: it has consumed them and holds no checkpoint at or before them.
    /// Eager tokens stay, as editing reads them all.
    pub(crate) fn release(&self, index: usize) {
        if let Self::Streaming(cursor) = self {
            let mut cursor = cursor.borrow_mut();
            let consumed = index.saturating_sub(cursor.base).min(cursor.tokens.len());
            cursor.tokens.drain(..consumed);
            cursor.base += consumed;
        }
    }
    /// The most tokens held at once: all of them when eager.
    pub(crate) fn retained(&self) -> usize {
        match self {
            Self::Eager(tokens) => tokens.len(),
            Self::Streaming(cursor) => cursor.borrow().peak,
        }
    }
    pub(crate) fn numeric_array(
        &self,
        index: usize,
        type_hint: &str,
        width: usize,
    ) -> Option<PreparedValue> {
        self.numeric(
            index,
            TokenKind::LeftBracket,
            TokenKind::RightBracket,
            |source, start| crate::read::numeric_array(source, start, type_hint, width),
        )
    }
    pub(crate) fn numeric_tuple(
        &self,
        index: usize,
        type_hint: &str,
        width: usize,
    ) -> Option<PreparedValue> {
        self.numeric(
            index,
            TokenKind::LeftParen,
            TokenKind::RightParen,
            |source, start| crate::read::numeric_tuple(source, start, type_hint, width),
        )
    }
    /// Reads the numeric value opening with the `open` token at `index`
    /// through `read`, then stands its last token in for the tokens it read
    /// (a `close` at index `index + 1`).
    fn numeric(
        &self,
        index: usize,
        open: TokenKind,
        close: TokenKind,
        read: impl FnOnce(&'a str, u32) -> Option<(PreparedValue, Lexer<'a>)>,
    ) -> Option<PreparedValue> {
        let token = self.get(index)?;
        if token.kind != open {
            return None;
        }
        let Self::Streaming(cursor) = self else {
            return None;
        };
        let mut cursor = cursor.borrow_mut();
        let (value, rest) = read(cursor.source, token.span.start)?;
        // Discard speculative lookahead past the opening token; the event
        // cursor owns the continuation. Invalid shapes leave this state intact.
        let keep = index + 1 - cursor.base;
        cursor.tokens.truncate(keep);
        cursor.tokens.push(Token {
            kind: close,
            span: Span::new(value.span.end - 1, value.span.end),
        });
        cursor.lexer = rest;
        Some(value)
    }
}
