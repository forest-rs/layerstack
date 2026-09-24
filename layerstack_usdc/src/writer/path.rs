// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Paths as the writer stores them, their `SdfPath` ordering, and the
//! compressed path tree of the PATHS section.
//!
//! Spec: AOUSD Core §16.3.7; OpenUSD `pxr/usd/sdf/crateFile.cpp`
//! (`_WritePaths`, `_BuildCompressedPathDataRecursive`) and `path.cpp`
//! (`SdfPath::operator<`).

use alloc::string::String;
use alloc::vec::Vec;

use super::error::UsdcWriteError;

/// An absolute path the writer supports: the pseudo-root, a prim path, or a
/// prim property path.
///
/// The derived ordering is `SdfPath`'s for these paths: prim parts compare
/// element by element (an ancestor before its descendants, names by byte
/// order), and only then the property part, with no property first. So a
/// prim is followed by its properties and then by its child prims.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CratePath {
    /// Prim names from the root; empty for the pseudo-root.
    pub(crate) prims: Vec<String>,
    /// Property name for a prim property path.
    pub(crate) property: Option<String>,
}

impl CratePath {
    /// The pseudo-root, `/`.
    pub(crate) fn root() -> Self {
        Self {
            prims: Vec::new(),
            property: None,
        }
    }

    /// Parses `/`, `/A/B` or `/A/B.prop:name`.
    pub(crate) fn parse(text: &str) -> Result<Self, UsdcWriteError> {
        let invalid = |reason| UsdcWriteError::InvalidPath {
            path: text.into(),
            reason,
        };
        let Some(rest) = text.strip_prefix('/') else {
            return Err(invalid("not an absolute path"));
        };
        if rest.is_empty() {
            return Ok(Self::root());
        }
        let (prim_part, property) = match rest.split_once('.') {
            Some((prims, property)) => (prims, Some(property)),
            None => (rest, None),
        };
        let mut prims = Vec::new();
        for name in prim_part.split('/') {
            if !is_name(name) {
                return Err(invalid("prim name is not an identifier"));
            }
            prims.push(name.into());
        }
        let property = match property {
            Some(name) if name.split(':').all(is_name) => Some(name.into()),
            Some(_) => return Err(invalid("property name is not a namespaced identifier")),
            None => None,
        };
        Ok(Self { prims, property })
    }

    /// Whether this is a prim property path.
    pub(crate) fn is_property(&self) -> bool {
        self.property.is_some()
    }

    /// Whether this is the pseudo-root.
    pub(crate) fn is_root(&self) -> bool {
        self.prims.is_empty() && self.property.is_none()
    }

    /// The parent path; `None` for the pseudo-root.
    pub(crate) fn parent(&self) -> Option<Self> {
        if self.property.is_some() {
            return Some(Self {
                prims: self.prims.clone(),
                property: None,
            });
        }
        let (_, parent) = self.prims.split_last()?;
        Some(Self {
            prims: parent.to_vec(),
            property: None,
        })
    }

    /// Number of path elements below the pseudo-root.
    pub(crate) fn depth(&self) -> usize {
        self.prims.len() + usize::from(self.property.is_some())
    }

    /// The token stored for this path's last element: the property name for
    /// a prim property path, otherwise the element (empty for the root).
    /// `CrateFile::_AddPath`.
    pub(crate) fn element_token(&self) -> &str {
        match (&self.property, self.prims.last()) {
            (Some(property), _) => property,
            (None, Some(name)) => name,
            (None, None) => "",
        }
    }
}

impl core::fmt::Display for CratePath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.prims.is_empty() && self.property.is_none() {
            return f.write_str("/");
        }
        for name in &self.prims {
            write!(f, "/{name}")?;
        }
        if let Some(property) = &self.property {
            write!(f, ".{property}")?;
        }
        Ok(())
    }
}

/// A structural name check: non-empty, not starting with an ASCII digit, and
/// free of ASCII punctuation (other than `_`), whitespace and control
/// characters, so the name is one path element. Full Unicode identifier
/// validation (§7.3.3) is the caller's job; the `Document` lowering runs the
/// USDA writer's XID check.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let ok = |c: char| !c.is_ascii() || c == '_' || c.is_ascii_alphanumeric();
    ok(first) && !first.is_ascii_digit() && chars.all(ok)
}

/// The three integer arrays of the compressed PATHS section.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PathTree {
    /// Path table index of each entry.
    pub(crate) path_indexes: Vec<i64>,
    /// Token index of each entry's element, negated for prim property paths.
    pub(crate) element_tokens: Vec<i64>,
    /// `0` = sibling only, `-1` = child only, `-2` = leaf, positive = child
    /// follows and the sibling is this many entries ahead.
    pub(crate) jumps: Vec<i64>,
}

/// Encodes `sorted` — every path of the table in `CratePath` order, each
/// with its table index and element token index — as the pre-order tree
/// `_BuildCompressedPathDataRecursive` writes.
///
/// `sorted` must contain the parent of every non-root path (the path table
/// adds ancestors), which makes every subtree contiguous.
pub(crate) fn path_tree(sorted: &[(&CratePath, u32, u32)]) -> PathTree {
    let n = sorted.len();
    // End (exclusive) of each entry's subtree: the next entry at the same or
    // a shallower depth.
    let mut subtree_end = alloc::vec![n; n];
    let mut open: Vec<usize> = Vec::new();
    for (i, (path, _, _)) in sorted.iter().enumerate() {
        while let Some(&top) = open.last() {
            if sorted[top].0.depth() >= path.depth() {
                subtree_end[top] = i;
                open.pop();
            } else {
                break;
            }
        }
        open.push(i);
    }

    let mut tree = PathTree::default();
    for (i, (path, index, token)) in sorted.iter().enumerate() {
        let has_child = i + 1 < subtree_end[i];
        let next = subtree_end[i];
        let has_sibling = next < n && sorted[next].0.depth() == path.depth();
        tree.path_indexes.push(i64::from(*index));
        let token = i64::from(*token);
        tree.element_tokens
            .push(if path.is_property() { -token } else { token });
        tree.jumps.push(match (has_child, has_sibling) {
            (true, true) => (next - i) as i64,
            (false, true) => 0,
            (true, false) => -1,
            (false, false) => -2,
        });
    }
    tree
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    fn p(text: &str) -> CratePath {
        CratePath::parse(text).unwrap()
    }

    #[test]
    fn parses_and_displays() {
        for text in [
            "/",
            "/A",
            "/A/B",
            "/A.x",
            "/A/B.primvars:st:indices",
            "/na\u{ef}ve",
        ] {
            assert_eq!(p(text).to_string(), text, "{text}");
        }
        for bad in [
            "",
            "A",
            "/A/",
            "//A",
            "/A.",
            "/A.b.c",
            "/1A",
            "/A.x:",
            "/A{v=a}",
            "/A.rel[/B]",
            "/A B",
        ] {
            assert!(CratePath::parse(bad).is_err(), "{bad:?} rejected");
        }
    }

    #[test]
    fn orders_like_sdf_path() {
        let mut paths = [
            p("/B"),
            p("/A/C"),
            p("/A.z"),
            p("/A"),
            p("/"),
            p("/A.b"),
            p("/A/C.a"),
            p("/Ab"),
        ];
        paths.sort();
        let sorted: Vec<String> = paths.iter().map(ToString::to_string).collect();
        assert_eq!(
            sorted,
            ["/", "/A", "/A.b", "/A.z", "/A/C", "/A/C.a", "/Ab", "/B"],
            "prim part first, then property; ancestors first"
        );
    }

    #[test]
    fn tree_jumps() {
        // /, /A, /A.x, /A/B, /C
        let paths = [p("/"), p("/A"), p("/A.x"), p("/A/B"), p("/C")];
        let entries: Vec<(&CratePath, u32, u32)> = paths
            .iter()
            .enumerate()
            .zip(0_u32..)
            .map(|((_, path), i)| (path, i, 10 + i))
            .collect();
        let tree = path_tree(&entries);
        assert_eq!(tree.path_indexes, [0, 1, 2, 3, 4], "indexes");
        assert_eq!(tree.element_tokens, [10, 11, -12, 13, 14], "tokens");
        // / has a child only; /A has a child and a sibling (/C, 3 ahead);
        // /A.x a sibling only; /A/B and /C are last children.
        assert_eq!(tree.jumps, [-1, 3, 0, -2, -2], "jumps");
    }
}
