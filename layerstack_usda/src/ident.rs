// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USD identifier character classes.
//!
//! An identifier is `([XID_Start] / '_') [XID_Continue]*` (AOUSD Core
//! §7.3.3, §16.2.8), using the Unicode `XID_Start` / `XID_Continue`
//! properties exactly. `OpenUSD` applies the same rule
//! (`pxr/usd/sdf/path.cpp`, `_IsValidIdentifier`). Approximations such as
//! `char::is_alphanumeric` are wrong in both directions: they admit
//! characters like `²` (U+00B2) and reject combining marks such as U+0301.

/// Whether `c` may start an identifier (`XID_Start` or `_`).
pub(crate) fn is_start(c: char) -> bool {
    c == '_' || unicode_ident::is_xid_start(c)
}

/// Whether `c` may continue an identifier (`XID_Continue`, which includes
/// `_`, digits and combining marks).
pub(crate) fn is_continue(c: char) -> bool {
    unicode_ident::is_xid_continue(c)
}

#[cfg(test)]
mod tests {
    use super::{is_continue, is_start};

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
}
