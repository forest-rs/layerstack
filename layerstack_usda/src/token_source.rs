// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Grammar navigation over eager editing tokens or lazy import tokens.
//! Numeric events bypass this cache; rollback retains structural tokens only.

use crate::{
    Span,
    lexer::{Lexer, Token, TokenKind},
    read::PreparedArray,
};
use alloc::vec::Vec;
use core::cell::RefCell;

pub(crate) enum TokenSource<'a> {
    Eager(Vec<Token>),
    Streaming(RefCell<Cursor<'a>>),
}

pub(crate) struct Cursor<'a> {
    source: &'a str,
    lexer: Lexer<'a>,
    tokens: Vec<Token>,
}

impl<'a> TokenSource<'a> {
    pub(crate) fn eager(tokens: Vec<Token>) -> Self {
        Self::Eager(tokens)
    }
    pub(crate) fn streaming(source: &'a str) -> Self {
        Self::Streaming(RefCell::new(Cursor {
            source,
            lexer: Lexer::new(source),
            tokens: Vec::new(),
        }))
    }
    pub(crate) fn get(&self, index: usize) -> Option<Token> {
        match self {
            Self::Eager(tokens) => tokens.get(index).copied(),
            Self::Streaming(cursor) => {
                let mut cursor = cursor.borrow_mut();
                while cursor.tokens.len() <= index {
                    let token = cursor.lexer.next()?;
                    cursor.tokens.push(token);
                }
                cursor.tokens.get(index).copied()
            }
        }
    }
    pub(crate) fn retained(&self) -> usize {
        match self {
            Self::Eager(tokens) => tokens.len(),
            Self::Streaming(cursor) => cursor.borrow().tokens.len(),
        }
    }
    pub(crate) fn numeric_array(
        &self,
        index: usize,
        type_hint: &str,
        width: usize,
    ) -> Option<PreparedArray> {
        let token = self.get(index)?;
        if token.kind != TokenKind::LeftBracket {
            return None;
        }
        let Self::Streaming(cursor) = self else {
            return None;
        };
        let mut cursor = cursor.borrow_mut();
        let (array, rest) =
            crate::read::numeric_array(cursor.source, token.span.start, type_hint, width)?;
        // Discard speculative lookahead past the opening bracket; the event
        // cursor owns the continuation. Invalid shapes leave this state intact.
        cursor.tokens.truncate(index + 1);
        cursor.tokens.push(Token {
            kind: TokenKind::RightBracket,
            span: Span::new(array.span.end - 1, array.span.end),
        });
        cursor.lexer = rest;
        Some(array)
    }
}
