// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prim names from arbitrary keys.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;

/// Turns an arbitrary key into a valid prim name.
///
/// Every character outside `[A-Za-z0-9_]` becomes `_`, and a name that
/// would be empty or start with a digit gets a leading `_`, so the result
/// always matches `[A-Za-z_][A-Za-z0-9_]*`. That is a USD identifier in
/// every OpenUSD release; the Unicode identifiers AOUSD Core allows
/// (`XID_Start` / `XID_Continue`, §7.3.3) are deliberately not produced,
/// since older readers reject them.
///
/// Distinct keys can map to one name (`"a.b"` and `"a-b"` both become
/// `a_b`); use [`SiblingNames`] to keep siblings apart.
///
/// ```
/// use layerstack_mesh_export::sanitize_name;
///
/// assert_eq!(sanitize_name("column.base-2"), "column_base_2");
/// assert_eq!(sanitize_name("3rd"), "_3rd");
/// assert_eq!(sanitize_name(""), "_");
/// ```
pub fn sanitize_name(key: &str) -> String {
    let mut name = String::with_capacity(key.len() + 1);
    if !key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        name.push('_');
    }
    name.extend(key.chars().map(|c| {
        if c.is_ascii_alphanumeric() || c == '_' {
            c
        } else {
            '_'
        }
    }));
    name
}

/// Hands out unique prim names among one prim's children.
///
/// [`Self::unique`] sanitizes a key ([`sanitize_name`]) and, when that
/// name is taken, appends the smallest free suffix `_2`, `_3`, ...; names
/// the exporter or caller fixes in advance (such as
/// [`PROTOTYPES_SCOPE`](crate::PROTOTYPES_SCOPE)) can be claimed first
/// with [`Self::reserve`]. The result is deterministic: it depends only on
/// the order of the calls.
///
/// ```
/// use layerstack_mesh_export::SiblingNames;
///
/// let mut names = SiblingNames::new();
/// assert!(names.reserve("Prototypes"));
/// assert_eq!(names.unique("column"), "column");
/// assert_eq!(names.unique("column"), "column_2");
/// assert_eq!(names.unique("column.2"), "column_2_2");
/// assert_eq!(names.unique("Prototypes"), "Prototypes_2");
/// ```
#[derive(Clone, Debug, Default)]
pub struct SiblingNames {
    taken: BTreeSet<String>,
}

impl SiblingNames {
    /// No names taken.
    pub fn new() -> Self {
        Self::default()
    }

    /// Claims `name` exactly, without sanitizing it. Returns `false`, and
    /// changes nothing, when it is already taken.
    pub fn reserve(&mut self, name: &str) -> bool {
        if self.taken.contains(name) {
            return false;
        }
        self.taken.insert(name.into())
    }

    /// A name for `key` that no earlier call has returned or reserved, and
    /// claims it.
    pub fn unique(&mut self, key: &str) -> String {
        let base = sanitize_name(key);
        if self.reserve(&base) {
            return base;
        }
        (2_u64..)
            .map(|n| format!("{base}_{n}"))
            .find(|candidate| self.reserve(candidate))
            .expect("a free suffix exists")
    }

    /// Whether `name` is taken.
    pub fn contains(&self, name: &str) -> bool {
        self.taken.contains(name)
    }
}

#[cfg(test)]
mod tests {
    use super::{SiblingNames, sanitize_name};

    #[test]
    fn sanitized_names_are_ascii_identifiers() {
        for (key, want) in [
            ("Tree", "Tree"),
            ("_x", "_x"),
            ("a b/c:d", "a_b_c_d"),
            ("9", "_9"),
            ("", "_"),
            ("-", "__"),
            ("é", "__"),
            ("tile.0", "tile_0"),
        ] {
            assert_eq!(sanitize_name(key), want, "{key:?}");
        }
    }

    #[test]
    fn siblings_get_the_smallest_free_suffix() {
        let mut names = SiblingNames::new();
        assert_eq!(names.unique("a_2"), "a_2");
        assert_eq!(names.unique("a"), "a");
        assert_eq!(names.unique("a"), "a_3", "a_2 is already taken");
        assert_eq!(names.unique("a"), "a_4");
        assert!(!names.reserve("a"), "a is taken");
        assert!(names.contains("a_4") && !names.contains("a_5"));
    }
}
