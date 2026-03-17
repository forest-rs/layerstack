//! Tokenizer for the USDA text format.
//!
//! Produces a stream of [`Token`]s with [`Span`] information from a USDA
//! source string. Every byte of the input is accounted for — whitespace,
//! comments, and punctuation all produce tokens, enabling lossless CST
//! construction.
//!
//! Spec: AOUSD Core §16.2.

use crate::Span;

// ── Token kinds ────────────────────────────────────────────────────────

/// The kind of a lexical token.
///
/// Whitespace and comments are preserved as distinct token kinds so the
/// CST can reconstruct the original source byte-for-byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TokenKind {
    // ── Trivia (whitespace + comments) ──────────────────────────────
    /// Contiguous spaces and/or tabs (no newlines).
    Whitespace,
    /// A newline sequence: `\n`, `\r`, or `\r\n`.
    Newline,
    /// A Python-style comment: `# ...` to end of line.
    PythonComment,
    /// A C++ single-line comment: `// ...` to end of line.
    CppComment,
    /// A C-style block comment: `/* ... */`.
    BlockComment,

    // ── Literals ────────────────────────────────────────────────────
    /// A numeric literal (integer or float, including `inf`, `-inf`, `nan`).
    Number,
    /// A single-line single-quoted string: `'...'`.
    SingleQuoteString,
    /// A single-line double-quoted string: `"..."`.
    DoubleQuoteString,
    /// A multi-line single-quoted string: `'''...'''`.
    MultilineSingleQuoteString,
    /// A multi-line double-quoted string: `"""..."""`.
    MultilineDoubleQuoteString,

    // ── Identifiers and keywords ────────────────────────────────────
    /// An identifier or keyword (e.g., `def`, `over`, `class`, `int`,
    /// user-defined names). Keywords are distinguished from identifiers
    /// during parsing, not lexing.
    Ident,

    // ── Punctuation ─────────────────────────────────────────────────
    /// `(`
    LeftParen,
    /// `)`
    RightParen,
    /// `[`
    LeftBracket,
    /// `]`
    RightBracket,
    /// `{`
    LeftBrace,
    /// `}`
    RightBrace,
    /// `<`
    LeftAngle,
    /// `>`
    RightAngle,
    /// `@` (asset reference delimiter).
    At,
    /// `&`
    Ampersand,
    /// `*`
    Asterisk,
    /// `:`
    Colon,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `=`
    Equals,
    /// `-`
    Minus,
    /// `+`
    Plus,
    /// `#` when not starting a comment (e.g., in `#usda`).
    /// In practice, `#` at the start of a line begins a comment or
    /// the layer header; the parser decides.
    Pound,
    /// `;`
    Semicolon,

    // ── Special ─────────────────────────────────────────────────────
    /// An unrecognized byte sequence (error recovery).
    Error,
}

/// A token with its kind and source span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// Byte range in the source text.
    pub span: Span,
}

impl Token {
    /// Extracts the token's text from the source.
    #[inline]
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        self.span.text(source)
    }
}

// ── Lexer ──────────────────────────────────────────────────────────────

/// A USDA tokenizer.
///
/// Iterates over a source string, yielding [`Token`]s. All bytes are
/// covered — the concatenation of every token's text reproduces the
/// original input.
#[derive(Debug)]
pub struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: u32,
}

impl<'a> Lexer<'a> {
    /// Creates a new lexer for the given source text.
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
        }
    }

    /// Peeks at the next byte without consuming it.
    #[inline]
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos as usize).copied()
    }

    /// Peeks at the byte `n` positions ahead.
    #[inline]
    fn peek_at(&self, n: u32) -> Option<u8> {
        self.bytes.get((self.pos + n) as usize).copied()
    }

    /// Advances the position by `n` bytes.
    #[inline]
    fn advance(&mut self, n: u32) {
        self.pos += n;
    }

    /// Produces a token from `start` to the current position.
    #[inline]
    fn token(&self, kind: TokenKind, start: u32) -> Token {
        Token {
            kind,
            span: Span::new(start, self.pos),
        }
    }

    /// Lexes a contiguous run of whitespace (spaces and tabs, no newlines).
    fn lex_whitespace(&mut self, start: u32) -> Token {
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' => self.advance(1),
                _ => break,
            }
        }
        self.token(TokenKind::Whitespace, start)
    }

    /// Lexes a newline: `\n`, `\r\n`, or bare `\r`.
    fn lex_newline(&mut self, start: u32) -> Token {
        let b = self.bytes[start as usize];
        if b == b'\r' && self.peek_at(1) == Some(b'\n') {
            self.advance(2);
        } else {
            self.advance(1);
        }
        self.token(TokenKind::Newline, start)
    }

    /// Lexes a Python-style comment: `# ...` to end of line.
    fn lex_python_comment(&mut self, start: u32) -> Token {
        self.advance(1); // skip `#`
        while let Some(b) = self.peek() {
            if b == b'\n' || b == b'\r' {
                break;
            }
            self.advance(1);
        }
        self.token(TokenKind::PythonComment, start)
    }

    /// Lexes a C++ single-line comment: `// ...` to end of line.
    fn lex_cpp_comment(&mut self, start: u32) -> Token {
        self.advance(2); // skip `//`
        while let Some(b) = self.peek() {
            if b == b'\n' || b == b'\r' {
                break;
            }
            self.advance(1);
        }
        self.token(TokenKind::CppComment, start)
    }

    /// Lexes a C-style block comment: `/* ... */`.
    fn lex_block_comment(&mut self, start: u32) -> Token {
        self.advance(2); // skip `/*`
        loop {
            match self.peek() {
                None => break, // unterminated — error recovery
                Some(b'*') if self.peek_at(1) == Some(b'/') => {
                    self.advance(2);
                    break;
                }
                _ => self.advance(1),
            }
        }
        self.token(TokenKind::BlockComment, start)
    }

    /// Lexes a numeric literal.
    ///
    /// Handles integers, floats, exponents, `inf`, `-inf`, `nan`.
    /// Note: the leading `-` for negative numbers is lexed as a separate
    /// [`TokenKind::Minus`]; the parser combines them. The keywords `inf`
    /// and `nan` are lexed as [`TokenKind::Ident`] and recognized by the
    /// parser.
    fn lex_number(&mut self, start: u32) -> Token {
        // Integer or float part.
        self.eat_digits();

        // Optional fractional part.
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) {
            self.advance(1); // skip `.`
            self.eat_digits();
        } else if self.peek() == Some(b'.') {
            // Trailing dot with no digits: `42.` — still valid per grammar.
            self.advance(1);
        }

        // Optional exponent.
        if let Some(b'e' | b'E') = self.peek() {
            self.advance(1);
            if let Some(b'+' | b'-') = self.peek() {
                self.advance(1);
            }
            self.eat_digits();
        }

        self.token(TokenKind::Number, start)
    }

    /// Lexes a number starting with `.` (e.g., `.5`).
    fn lex_dot_number(&mut self, start: u32) -> Token {
        self.advance(1); // skip `.`
        self.eat_digits();

        // Optional exponent.
        if let Some(b'e' | b'E') = self.peek() {
            self.advance(1);
            if let Some(b'+' | b'-') = self.peek() {
                self.advance(1);
            }
            self.eat_digits();
        }

        self.token(TokenKind::Number, start)
    }

    /// Consumes a run of ASCII digits.
    fn eat_digits(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.advance(1);
        }
    }

    /// Lexes a single-quoted string (single-line or multi-line).
    fn lex_single_quote_string(&mut self, start: u32) -> Token {
        if self.peek_at(1) == Some(b'\'') && self.peek_at(2) == Some(b'\'') {
            self.lex_multiline_string(start, b'\'', TokenKind::MultilineSingleQuoteString)
        } else {
            self.lex_singleline_string(start, b'\'', TokenKind::SingleQuoteString)
        }
    }

    /// Lexes a double-quoted string (single-line or multi-line).
    fn lex_double_quote_string(&mut self, start: u32) -> Token {
        if self.peek_at(1) == Some(b'"') && self.peek_at(2) == Some(b'"') {
            self.lex_multiline_string(start, b'"', TokenKind::MultilineDoubleQuoteString)
        } else {
            self.lex_singleline_string(start, b'"', TokenKind::DoubleQuoteString)
        }
    }

    /// Lexes a single-line string delimited by `quote`.
    fn lex_singleline_string(&mut self, start: u32, quote: u8, kind: TokenKind) -> Token {
        self.advance(1); // opening quote
        loop {
            match self.peek() {
                None | Some(b'\n') | Some(b'\r') => break, // unterminated
                Some(b'\\') => self.advance(2),            // escape sequence
                Some(b) if b == quote => {
                    self.advance(1);
                    break;
                }
                _ => self.advance(1),
            }
        }
        self.token(kind, start)
    }

    /// Lexes a multi-line string delimited by triple `quote`.
    fn lex_multiline_string(&mut self, start: u32, quote: u8, kind: TokenKind) -> Token {
        self.advance(3); // opening triple quote
        loop {
            match self.peek() {
                None => break, // unterminated
                Some(b'\\') => self.advance(2),
                Some(b)
                    if b == quote
                        && self.peek_at(1) == Some(quote)
                        && self.peek_at(2) == Some(quote) =>
                {
                    self.advance(3);
                    break;
                }
                _ => self.advance(1),
            }
        }
        self.token(kind, start)
    }

    /// Lexes an identifier or keyword.
    fn lex_ident(&mut self, start: u32) -> Token {
        // First char is already validated as XID_Start or `_`.
        // Advance past it (may be multi-byte UTF-8).
        let ch = self.current_char();
        self.advance(ch.len_utf8() as u32);

        // Continue with XID_Continue characters.
        while self.pos < self.bytes.len() as u32 {
            let ch = self.current_char();
            if unicode_xid_continue(ch) {
                self.advance(ch.len_utf8() as u32);
            } else {
                break;
            }
        }

        self.token(TokenKind::Ident, start)
    }

    /// Decodes the current UTF-8 character at `self.pos`.
    fn current_char(&self) -> char {
        let remaining = &self.source[self.pos as usize..];
        remaining.chars().next().unwrap_or('\0')
    }
}

impl Iterator for Lexer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        let b = self.peek()?;
        let start = self.pos;

        let tok = match b {
            // Whitespace (spaces/tabs).
            b' ' | b'\t' => self.lex_whitespace(start),

            // Newlines.
            b'\n' | b'\r' => self.lex_newline(start),

            // `#` — could be a comment or the layer header `#usda`.
            // We lex it as a comment; the parser can check if the first
            // comment token is actually the header.
            b'#' => self.lex_python_comment(start),

            // `/` — could be `//` comment, `/*` block comment, or
            // standalone `/` (path separator in PathRef).
            b'/' => match self.peek_at(1) {
                Some(b'/') => self.lex_cpp_comment(start),
                Some(b'*') => self.lex_block_comment(start),
                _ => {
                    self.advance(1);
                    // `/` doesn't have its own token kind in the grammar —
                    // it only appears inside path references which are
                    // parsed at a higher level. Emit as Error for now;
                    // the parser handles path parsing.
                    self.token(TokenKind::Error, start)
                }
            },

            // Strings.
            b'\'' => self.lex_single_quote_string(start),
            b'"' => self.lex_double_quote_string(start),

            // Numbers.
            b'0'..=b'9' => self.lex_number(start),
            b'.' if self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) => {
                self.lex_dot_number(start)
            }

            // Punctuation.
            b'(' => {
                self.advance(1);
                self.token(TokenKind::LeftParen, start)
            }
            b')' => {
                self.advance(1);
                self.token(TokenKind::RightParen, start)
            }
            b'[' => {
                self.advance(1);
                self.token(TokenKind::LeftBracket, start)
            }
            b']' => {
                self.advance(1);
                self.token(TokenKind::RightBracket, start)
            }
            b'{' => {
                self.advance(1);
                self.token(TokenKind::LeftBrace, start)
            }
            b'}' => {
                self.advance(1);
                self.token(TokenKind::RightBrace, start)
            }
            b'<' => {
                self.advance(1);
                self.token(TokenKind::LeftAngle, start)
            }
            b'>' => {
                self.advance(1);
                self.token(TokenKind::RightAngle, start)
            }
            b'@' => {
                self.advance(1);
                self.token(TokenKind::At, start)
            }
            b'&' => {
                self.advance(1);
                self.token(TokenKind::Ampersand, start)
            }
            b'*' => {
                self.advance(1);
                self.token(TokenKind::Asterisk, start)
            }
            b':' => {
                self.advance(1);
                self.token(TokenKind::Colon, start)
            }
            b',' => {
                self.advance(1);
                self.token(TokenKind::Comma, start)
            }
            b'.' => {
                self.advance(1);
                self.token(TokenKind::Dot, start)
            }
            b'=' => {
                self.advance(1);
                self.token(TokenKind::Equals, start)
            }
            b'-' => {
                self.advance(1);
                self.token(TokenKind::Minus, start)
            }
            b'+' => {
                self.advance(1);
                self.token(TokenKind::Plus, start)
            }
            b';' => {
                self.advance(1);
                self.token(TokenKind::Semicolon, start)
            }

            // Identifiers (ASCII fast path, then Unicode).
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.lex_ident(start),

            // Non-ASCII: check for Unicode XID_Start.
            _ if b >= 0x80 => {
                let ch = self.current_char();
                if unicode_xid_start(ch) {
                    self.lex_ident(start)
                } else {
                    // Skip the full character.
                    self.advance(ch.len_utf8() as u32);
                    self.token(TokenKind::Error, start)
                }
            }

            // Unrecognized byte.
            _ => {
                self.advance(1);
                self.token(TokenKind::Error, start)
            }
        };

        Some(tok)
    }
}

// ── Unicode identifier support ─────────────────────────────────────────

/// Checks if `c` is a valid identifier start per UAX #31 (`XID_Start` or `_`).
///
/// Spec: AOUSD Core §7.3.3, §16.2.8.
fn unicode_xid_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

/// Checks if `c` is a valid identifier continuation per UAX #31 (`XID_Continue`).
fn unicode_xid_continue(c: char) -> bool {
    // XID_Continue includes XID_Start plus digits, combining marks, etc.
    // `char::is_alphanumeric()` covers letters + digits; `_` is separate.
    c == '_' || c.is_alphanumeric()
}

// ── Convenience ────────────────────────────────────────────────────────

/// Tokenizes the entire source into a `Vec` of tokens.
///
/// Useful for testing and for parsers that want random access to the
/// token stream.
pub fn tokenize(source: &str) -> alloc::vec::Vec<Token> {
    Lexer::new(source).collect()
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: tokenize and return `(kind, text)` pairs.
    fn tok(source: &str) -> alloc::vec::Vec<(TokenKind, &str)> {
        tokenize(source)
            .iter()
            .map(|t| (t.kind, t.text(source)))
            .collect()
    }

    /// Helper: tokenize, filter trivia, return `(kind, text)` pairs.
    fn tok_no_trivia(source: &str) -> alloc::vec::Vec<(TokenKind, &str)> {
        tok(source)
            .into_iter()
            .filter(|(k, _)| {
                !matches!(
                    k,
                    TokenKind::Whitespace
                        | TokenKind::Newline
                        | TokenKind::PythonComment
                        | TokenKind::CppComment
                        | TokenKind::BlockComment
                )
            })
            .collect()
    }

    // ── Whitespace and newlines ─────────────────────────────────────

    #[test]
    fn whitespace_spaces_and_tabs() {
        let tokens = tok("  \t ");
        assert_eq!(tokens, [(TokenKind::Whitespace, "  \t ")]);
    }

    #[test]
    fn newline_lf() {
        let tokens = tok("\n");
        assert_eq!(tokens, [(TokenKind::Newline, "\n")]);
    }

    #[test]
    fn newline_crlf() {
        let tokens = tok("\r\n");
        assert_eq!(tokens, [(TokenKind::Newline, "\r\n")]);
    }

    #[test]
    fn newline_cr() {
        let tokens = tok("\r");
        assert_eq!(tokens, [(TokenKind::Newline, "\r")]);
    }

    #[test]
    fn mixed_whitespace_and_newlines() {
        let tokens = tok("  \n\t");
        assert_eq!(
            tokens,
            [
                (TokenKind::Whitespace, "  "),
                (TokenKind::Newline, "\n"),
                (TokenKind::Whitespace, "\t"),
            ]
        );
    }

    // ── Comments ────────────────────────────────────────────────────

    #[test]
    fn python_comment() {
        let tokens = tok("# hello world\n");
        assert_eq!(
            tokens,
            [
                (TokenKind::PythonComment, "# hello world"),
                (TokenKind::Newline, "\n"),
            ]
        );
    }

    #[test]
    fn cpp_single_line_comment() {
        let tokens = tok("// comment\n");
        assert_eq!(
            tokens,
            [
                (TokenKind::CppComment, "// comment"),
                (TokenKind::Newline, "\n"),
            ]
        );
    }

    #[test]
    fn block_comment() {
        let tokens = tok("/* block */");
        assert_eq!(tokens, [(TokenKind::BlockComment, "/* block */")]);
    }

    #[test]
    fn block_comment_multiline() {
        let tokens = tok("/* line1\nline2 */");
        assert_eq!(tokens, [(TokenKind::BlockComment, "/* line1\nline2 */")]);
    }

    #[test]
    fn block_comment_unterminated() {
        let tokens = tok("/* oops");
        assert_eq!(tokens, [(TokenKind::BlockComment, "/* oops")]);
    }

    // ── Numbers ─────────────────────────────────────────────────────

    #[test]
    fn integer() {
        let tokens = tok_no_trivia("42");
        assert_eq!(tokens, [(TokenKind::Number, "42")]);
    }

    #[test]
    fn float_with_fraction() {
        let tokens = tok_no_trivia("3.14");
        assert_eq!(tokens, [(TokenKind::Number, "3.14")]);
    }

    #[test]
    fn float_dot_leading() {
        let tokens = tok_no_trivia(".5");
        assert_eq!(tokens, [(TokenKind::Number, ".5")]);
    }

    #[test]
    fn float_trailing_dot() {
        let tokens = tok_no_trivia("42.");
        assert_eq!(tokens, [(TokenKind::Number, "42.")]);
    }

    #[test]
    fn float_exponent() {
        let tokens = tok_no_trivia("1e10");
        assert_eq!(tokens, [(TokenKind::Number, "1e10")]);
    }

    #[test]
    fn float_exponent_signed() {
        let tokens = tok_no_trivia("2.5E-3");
        assert_eq!(tokens, [(TokenKind::Number, "2.5E-3")]);
    }

    #[test]
    fn negative_number_is_two_tokens() {
        let tokens = tok_no_trivia("-42");
        assert_eq!(tokens, [(TokenKind::Minus, "-"), (TokenKind::Number, "42")]);
    }

    // ── Strings ─────────────────────────────────────────────────────

    #[test]
    fn double_quote_string() {
        let tokens = tok_no_trivia(r#""hello""#);
        assert_eq!(tokens, [(TokenKind::DoubleQuoteString, r#""hello""#)]);
    }

    #[test]
    fn single_quote_string() {
        let tokens = tok_no_trivia("'world'");
        assert_eq!(tokens, [(TokenKind::SingleQuoteString, "'world'")]);
    }

    #[test]
    fn string_with_escape() {
        let tokens = tok_no_trivia(r#""he\"llo""#);
        assert_eq!(tokens, [(TokenKind::DoubleQuoteString, r#""he\"llo""#)]);
    }

    #[test]
    fn multiline_double_quote_string() {
        let tokens = tok_no_trivia("\"\"\"multi\nline\"\"\"");
        assert_eq!(
            tokens,
            [(
                TokenKind::MultilineDoubleQuoteString,
                "\"\"\"multi\nline\"\"\""
            )]
        );
    }

    #[test]
    fn multiline_single_quote_string() {
        let tokens = tok_no_trivia("'''multi\nline'''");
        assert_eq!(
            tokens,
            [(TokenKind::MultilineSingleQuoteString, "'''multi\nline'''")]
        );
    }

    #[test]
    fn unterminated_string() {
        let tokens = tok_no_trivia("\"oops\n");
        // Unterminated string ends at newline.
        assert_eq!(tokens, [(TokenKind::DoubleQuoteString, "\"oops")]);
    }

    // ── Identifiers ─────────────────────────────────────────────────

    #[test]
    fn simple_ident() {
        let tokens = tok_no_trivia("foo_bar");
        assert_eq!(tokens, [(TokenKind::Ident, "foo_bar")]);
    }

    #[test]
    fn keyword_lexed_as_ident() {
        // Keywords are distinguished from identifiers during parsing.
        let tokens = tok_no_trivia("def over class");
        assert_eq!(
            tokens,
            [
                (TokenKind::Ident, "def"),
                (TokenKind::Ident, "over"),
                (TokenKind::Ident, "class"),
            ]
        );
    }

    #[test]
    fn underscore_ident() {
        let tokens = tok_no_trivia("_private __dunder");
        assert_eq!(
            tokens,
            [
                (TokenKind::Ident, "_private"),
                (TokenKind::Ident, "__dunder"),
            ]
        );
    }

    // ── Punctuation ─────────────────────────────────────────────────

    #[test]
    fn all_punctuation() {
        let tokens = tok_no_trivia("()[]{}< >@&*:,.=-+;");
        let kinds: alloc::vec::Vec<_> = tokens.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            [
                TokenKind::LeftParen,
                TokenKind::RightParen,
                TokenKind::LeftBracket,
                TokenKind::RightBracket,
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                TokenKind::LeftAngle,
                TokenKind::RightAngle,
                TokenKind::At,
                TokenKind::Ampersand,
                TokenKind::Asterisk,
                TokenKind::Colon,
                TokenKind::Comma,
                TokenKind::Dot,
                TokenKind::Equals,
                TokenKind::Minus,
                TokenKind::Plus,
                TokenKind::Semicolon,
            ]
        );
    }

    // ── Lossless round-trip ─────────────────────────────────────────

    #[test]
    fn lossless_roundtrip() {
        let source = "#usda 1.0\n(\n    subLayers = [\n        @./sub.usd@\n    ]\n)\n\ndef \"Foo\" {\n    int bar = 42\n}\n";
        let tokens = tokenize(source);
        let reconstructed: alloc::string::String = tokens.iter().map(|t| t.text(source)).collect();
        assert_eq!(reconstructed, source);
    }

    // ── Realistic snippets ──────────────────────────────────────────

    #[test]
    fn layer_header() {
        let tokens = tok("#usda 1.0");
        // `#usda 1.0` lexes as: PythonComment "#usda 1.0"
        // (the parser will recognize this special comment as the header)
        assert_eq!(tokens, [(TokenKind::PythonComment, "#usda 1.0")]);
    }

    #[test]
    fn prim_def() {
        let tokens = tok_no_trivia("def Mesh \"myMesh\" { }");
        assert_eq!(
            tokens,
            [
                (TokenKind::Ident, "def"),
                (TokenKind::Ident, "Mesh"),
                (TokenKind::DoubleQuoteString, "\"myMesh\""),
                (TokenKind::LeftBrace, "{"),
                (TokenKind::RightBrace, "}"),
            ]
        );
    }

    #[test]
    fn attribute_with_value() {
        let tokens = tok_no_trivia("float3 extent = (1.0, 2.0, 3.0)");
        assert_eq!(
            tokens,
            [
                (TokenKind::Ident, "float3"),
                (TokenKind::Ident, "extent"),
                (TokenKind::Equals, "="),
                (TokenKind::LeftParen, "("),
                (TokenKind::Number, "1.0"),
                (TokenKind::Comma, ","),
                (TokenKind::Number, "2.0"),
                (TokenKind::Comma, ","),
                (TokenKind::Number, "3.0"),
                (TokenKind::RightParen, ")"),
            ]
        );
    }

    #[test]
    fn reference_metadata() {
        let tokens = tok_no_trivia("references = @./model.usd@</Root>");
        assert_eq!(
            tokens,
            [
                (TokenKind::Ident, "references"),
                (TokenKind::Equals, "="),
                (TokenKind::At, "@"),
                (TokenKind::Dot, "."),
                (TokenKind::Error, "/"), // standalone `/`
                (TokenKind::Ident, "model"),
                (TokenKind::Dot, "."),
                (TokenKind::Ident, "usd"),
                (TokenKind::At, "@"),
                (TokenKind::LeftAngle, "<"),
                (TokenKind::Error, "/"), // standalone `/`
                (TokenKind::Ident, "Root"),
                (TokenKind::RightAngle, ">"),
            ]
        );
    }

    // ── Error recovery ──────────────────────────────────────────────

    #[test]
    fn error_token_for_unknown_byte() {
        let tokens = tok_no_trivia("\x01");
        assert_eq!(tokens, [(TokenKind::Error, "\x01")]);
    }

    #[test]
    fn continues_after_error() {
        let tokens = tok_no_trivia("\x01 foo");
        assert_eq!(
            tokens,
            [(TokenKind::Error, "\x01"), (TokenKind::Ident, "foo")]
        );
    }
}
