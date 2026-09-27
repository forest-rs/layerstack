// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Name globs of path patterns: `*` any run of characters, `?` one
//! character, `[a-z0-9]` and `[!a-z]` one character in or out of a class,
//! `\c` a literal `c`. The whole name must match.
//!
//! OpenUSD: `Sdf_GlobPattern` (`pxr/usd/sdf/globPattern.cpp`), which
//! matches the same names with a faster search.

use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Literal(char),
    Any,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    Star,
}

/// A compiled name glob.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Glob {
    tokens: Vec<Token>,
}

impl Glob {
    /// Compiles `pattern`; `None` for an unclosed class or a trailing `\`,
    /// which OpenUSD cannot compile either.
    pub(crate) fn compile(pattern: &str) -> Option<Self> {
        let mut tokens = Vec::new();
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '*' => {
                    if tokens.last() != Some(&Token::Star) {
                        tokens.push(Token::Star);
                    }
                }
                '?' => tokens.push(Token::Any),
                '\\' => tokens.push(Token::Literal(chars.next()?)),
                '[' => {
                    let negated = chars.next_if(|c| *c == '!' || *c == '^').is_some();
                    let mut ranges = Vec::new();
                    let mut first = true;
                    loop {
                        let lo = chars.next()?;
                        if lo == ']' && !first {
                            break;
                        }
                        first = false;
                        // `a-z`, unless the `-` is the class's last
                        // character.
                        let mut ahead = chars.clone();
                        if ahead.next() == Some('-')
                            && let Some(hi) = ahead.next()
                            && hi != ']'
                        {
                            chars.next();
                            chars.next();
                            ranges.push((lo, hi));
                        } else {
                            ranges.push((lo, lo));
                        }
                    }
                    tokens.push(Token::Class { negated, ranges });
                }
                c => tokens.push(Token::Literal(c)),
            }
        }
        Some(Self { tokens })
    }

    /// Whether `name` matches the whole glob.
    pub(crate) fn matches(&self, name: &str) -> bool {
        let chars: Vec<char> = name.chars().collect();
        // Iterative matching with backtracking to the last `*`.
        let (mut t, mut c) = (0, 0);
        let mut star: Option<(usize, usize)> = None;
        while c < chars.len() {
            let step = match self.tokens.get(t) {
                Some(Token::Star) => {
                    star = Some((t, c));
                    t += 1;
                    continue;
                }
                Some(Token::Any) => true,
                Some(Token::Literal(l)) => *l == chars[c],
                Some(Token::Class { negated, ranges }) => {
                    let inside = ranges
                        .iter()
                        .any(|(lo, hi)| (*lo..=*hi).contains(&chars[c]));
                    inside != *negated
                }
                None => false,
            };
            if step {
                t += 1;
                c += 1;
            } else if let Some((star_t, star_c)) = star {
                t = star_t + 1;
                c = star_c + 1;
                star = Some((star_t, star_c + 1));
            } else {
                return false;
            }
        }
        self.tokens[t..].iter().all(|token| *token == Token::Star)
    }
}

#[cfg(test)]
mod tests {
    use super::Glob;

    #[test]
    fn globs_match_whole_names() {
        let matches = |glob: &str, name: &str| Glob::compile(glob).expect(glob).matches(name);
        assert!(matches("*", "") && matches("*", "anything"));
        assert!(matches("A*", "A") && matches("A*", "Abc") && !matches("A*", "bA"));
        assert!(matches("*b*", "abc") && !matches("*b*", "ac"));
        assert!(matches("a?c", "abc") && !matches("a?c", "ac"));
        assert!(matches("a*c*e", "abcde") && matches("a*c*e", "ace") && !matches("a*c*e", "acd"));
        assert!(matches("[a-c]x", "bx") && !matches("[a-c]x", "dx"));
        assert!(matches("[!a-c]x", "dx") && !matches("[!a-c]x", "ax"));
        assert!(matches("[a-]", "-") && matches("[-a]", "-") && matches("[a-]", "a"));
        assert!(matches("?", "é") && !matches("??", "é"));
        assert!(matches("Plain", "Plain") && !matches("Plain", "Plainer"));
        assert!(Glob::compile("[!]").is_none(), "an unclosed class");
        assert!(Glob::compile("[a").is_none());
    }
}
