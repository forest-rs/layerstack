// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USD identifiers.
//!
//! An identifier is `([XID_Start] / '_') [XID_Continue]*` (AOUSD Core
//! §7.3.3, §16.2.8), using the Unicode `XID_Start` / `XID_Continue`
//! properties exactly. OpenUSD applies the same rule
//! (`pxr/usd/sdf/path.cpp`, `_IsValidIdentifier`). Approximations such as
//! `char::is_alphanumeric` are wrong in both directions: they admit
//! characters like `²` (U+00B2) and reject combining marks such as U+0301
//! and `Other_ID_Start` characters such as `℘` (U+2118).
//!
//! ```
//! use layerstack::ident::is_identifier;
//!
//! assert!(is_identifier("é") && is_identifier("℘") && is_identifier("_1"));
//! assert!(!is_identifier("a²") && !is_identifier("1a") && !is_identifier(""));
//! ```

/// Whether `c` may start an identifier (`XID_Start` or `_`).
#[must_use]
pub fn is_start(c: char) -> bool {
    c == '_' || unicode_ident::is_xid_start(c)
}

/// Whether `c` may continue an identifier (`XID_Continue`, which includes
/// `_`, digits and combining marks).
#[must_use]
pub fn is_continue(c: char) -> bool {
    unicode_ident::is_xid_continue(c)
}

/// Whether `text` is an identifier: an [`is_start`] character, then
/// [`is_continue`] characters.
///
/// OpenUSD: `SdfPath::IsValidIdentifier`.
#[must_use]
pub fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(is_start) && chars.all(is_continue)
}

#[cfg(test)]
mod tests {
    use super::{is_continue, is_identifier, is_start};

    #[test]
    fn follows_xid_tables() {
        assert!(is_start('_') && is_start('a') && is_start('é'), "starts");
        assert!(!is_start('1') && !is_start('\u{0301}'), "non-starts");
        assert!(
            is_continue('1') && is_continue('_'),
            "digits and underscore"
        );
        assert!(is_continue('\u{0301}'), "combining acute accent continues");
        assert!(!is_continue('²'), "superscript two is not XID_Continue");
        assert!(!is_continue(':') && !is_continue('-'), "punctuation");
    }

    /// Each agrees with usd-core 26.8's `Sdf.Path.IsValidIdentifier`.
    #[test]
    fn identifiers_match_openusd() {
        for (text, valid) in [
            ("é", true),
            ("℘", true),
            ("a²", false),
            ("a·b", true),
            ("a\u{0301}", true),
            ("\u{0301}a", false),
            ("٣", false),
            ("a٣", true),
            ("ⅰ", true),
            ("_1", true),
            ("日本", true),
            ("a-b", false),
            ("1a", false),
            ("", false),
        ] {
            assert_eq!(is_identifier(text), valid, "{text:?}");
        }
    }
}
