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

/// Orders names as OpenUSD's `TfDictionaryLessThan` does
/// (`pxr/base/tf/stringUtils.cpp`, `TfDictionaryLessThan::_LessImpl`):
/// ASCII letters ignoring case, with `_` before every letter; runs of
/// digits by their value, ignoring leading zeros; other bytes by value;
/// and names that differ only in case or leading zeros by their first
/// difference, where uppercase comes first and a zero last (`X1` <
/// `x1` < `x01`).
///
/// This is the order OpenUSD gives property names
/// (`UsdPrim::GetPropertyNames`) and the order its text format writes
/// properties and metadata fields in (`pxr/usd/sdf/fileIO_Common.h`).
///
/// Spec: AOUSD Core §8.2 (element ordering), with one difference that
/// follows OpenUSD: digits come before `_`, where §8.2 puts `_` before
/// letters and digits (its example orders `foo_bar` before
/// `foo00001bar`; OpenUSD, after).
///
/// ```
/// use layerstack::ident::dictionary_cmp;
///
/// let mut names = ["b", "a10", "a9", "B", "a09", "a_b", "aB"];
/// names.sort_by(|a, b| dictionary_cmp(a, b));
/// assert_eq!(names, ["a9", "a09", "a10", "a_b", "aB", "B", "b"]);
/// ```
#[must_use]
#[inline]
pub fn dictionary_cmp(a: &str, b: &str) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    let (x, y) = (a.as_bytes(), b.as_bytes());
    // The common case, decided as `_LessImpl` decides it: names that first
    // differ in two letters of one case go by those letters, and a name
    // that is a prefix of the other comes first.
    let same = x.iter().zip(y).take_while(|(p, q)| p == q).count();
    match (x.get(same), y.get(same)) {
        (None, None) => return Ordering::Equal,
        (None, Some(_)) => return Ordering::Less,
        (Some(_), None) => return Ordering::Greater,
        (Some(p), Some(q))
            if (p.is_ascii_lowercase() && q.is_ascii_lowercase())
                || (p.is_ascii_uppercase() && q.is_ascii_uppercase()) =>
        {
            return p.cmp(q);
        }
        _ => {}
    }
    if dictionary_less(x, y) {
        Ordering::Less
    } else if a == b {
        Ordering::Equal
    } else {
        Ordering::Greater
    }
}

/// `TfDictionaryLessThan::_LessImpl`, step for step. A position at the end
/// of a string reads as the NUL that ends a C string.
fn dictionary_less(l: &[u8], r: &[u8]) -> bool {
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    // The first difference at or after `(i, j)` within `n` bytes.
    let mismatch = |mut i: usize, mut j: usize, n: usize| {
        let end = i + n;
        while i < end && l[i] == r[j] {
            i += 1;
            j += 1;
        }
        (i, j)
    };
    let digit = |c: u8| c.is_ascii_digit();

    let mut end = l.len().min(r.len());
    let (mut i, mut j) = mismatch(0, 0, end);
    if i == end && l.len() == r.len() {
        return false;
    }
    while i != end {
        let (x, y) = (l[i], r[j]);
        let ascii = x < 0x80 && y < 0x80;
        if ascii && (x & !0x20) != (y & !0x20) && x >= 0x40 && y >= 0x40 {
            // `(c + 5) & 31` puts `_` before every letter.
            return (x.wrapping_add(5) & 31) < (y.wrapping_add(5) & 31);
        } else if digit(x) || digit(y) {
            if !(digit(x) && digit(y)) {
                if i == 0 {
                    return x < y;
                }
                // One digit: a digit run that goes on is greater.
                return if digit(l[i - 1]) { digit(y) } else { digit(x) };
            }
            // Both digits: compare the two runs by value.
            let run = |s: &[u8], at: usize| {
                let mut start = at;
                while start > 0 && digit(s[start - 1]) {
                    start -= 1;
                }
                let mut stop = at;
                while stop < s.len() && digit(s[stop]) {
                    stop += 1;
                }
                while start != stop && s[start] == b'0' {
                    start += 1;
                }
                (start, stop)
            };
            let ((mut ls, le), (mut rs, re)) = (run(l, i), run(r, j));
            while ls != le && rs != re && l[ls] == r[rs] {
                ls += 1;
                rs += 1;
            }
            if (ls == le) != (rs == re) {
                return ls == le;
            }
            if ls != le {
                return (le - ls, l[ls]) < (re - rs, r[rs]);
            }
            // Equal values: go on after both runs.
            (i, j) = (le, re);
            end = i + (l.len() - i).min(r.len() - j);
        } else if !(x.is_ascii_alphabetic() && y.is_ascii_alphabetic()) {
            return x < y;
        } else {
            // The same letter in another case.
            i += 1;
            j += 1;
        }
        (i, j) = mismatch(i, j, end - i);
    }
    if i != l.len() || j != r.len() {
        return i == l.len();
    }
    // Only case or leading zeros differ: the first difference decides.
    let (i, j) = mismatch(0, 0, l.len().min(r.len()));
    let (x, y) = (at(l, i), at(r, j));
    y == b'0' || (x != b'0' && x < y)
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{dictionary_cmp, is_continue, is_identifier, is_start};

    /// The order of usd-core 26.8's
    /// `sorted(names, key=functools.cmp_to_key(Tf.DictionaryStrcmp))`.
    #[test]
    fn dictionary_order_matches_openusd() {
        let expected = [
            "0a",
            "1",
            "01",
            "10",
            "_0",
            "_x",
            "A",
            "a",
            "a0",
            "a00",
            "a1!",
            "a1_",
            "a1b",
            "a2",
            "a9",
            "a09",
            "a009",
            "a10",
            "a12",
            "a:b",
            "a[",
            "a^",
            "a~",
            "a_",
            "a_b",
            "aB",
            "Ab1",
            "aB01",
            "ab1",
            "b",
            "e",
            "f",
            "Points",
            "points",
            "points2",
            "points10",
            "primvars:normals",
            "primvars:st",
            "primvars_st",
            "X01",
            "x1",
            "x01",
            "x001",
            "x1y",
            "x01Y",
            "é",
        ];
        assert_sorts_to(&expected);
    }

    /// AOUSD Core §8.2's example, as usd-core 26.8 orders it.
    #[test]
    fn element_ordering_example_as_openusd_orders_it() {
        assert_sorts_to(&[
            "_foobar",
            "a0",
            "ab",
            "aü",
            "foo00001bar",
            "foo001bar001abc",
            "foo001bar002abc",
            "foo0001bar0002xyz",
            "foo_bar",
            "Foobar",
            "foobar",
        ]);
    }

    /// The fast path decides as the port of `_LessImpl` does, for every
    /// pair of names up to three characters long over letters of both
    /// cases, `_`, `:`, digits, a zero and a non-ASCII letter.
    #[test]
    fn fast_path_agrees_with_the_port() {
        use alloc::string::String;
        use core::cmp::Ordering;

        let alphabet = ["a", "b", "A", "B", "_", ":", "0", "1", "9", "é"];
        let mut names = alloc::vec![String::new()];
        for _ in 0..3 {
            let longer: Vec<String> = names
                .iter()
                .flat_map(|n| alphabet.iter().map(move |c| alloc::format!("{n}{c}")))
                .collect();
            names.extend(longer);
        }
        names.sort();
        names.dedup();
        for a in &names {
            for b in &names {
                let port = if super::dictionary_less(a.as_bytes(), b.as_bytes()) {
                    Ordering::Less
                } else if a == b {
                    Ordering::Equal
                } else {
                    Ordering::Greater
                };
                assert_eq!(dictionary_cmp(a, b), port, "{a:?} vs {b:?}");
            }
        }
    }

    /// `expected` sorts to itself from reversed, and every pair compares
    /// one way: a strict total order.
    fn assert_sorts_to(expected: &[&str]) {
        let mut names: Vec<&str> = expected.iter().rev().copied().collect();
        names.sort_by(|a, b| dictionary_cmp(a, b));
        assert_eq!(names, expected);
        for (i, a) in expected.iter().enumerate() {
            for (j, b) in expected.iter().enumerate() {
                assert_eq!(dictionary_cmp(a, b), i.cmp(&j), "{a:?} vs {b:?}");
            }
        }
    }

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
