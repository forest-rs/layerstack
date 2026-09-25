// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Paths as the writer stores them, their `SdfPath` ordering, and the
//! compressed path tree of the PATHS section.
//!
//! Spec: AOUSD Core §16.3.7; OpenUSD `pxr/usd/sdf/crateFile.cpp`
//! (`_WritePaths`, `_BuildCompressedPathDataRecursive`) and `path.cpp`
//! (`SdfPath::operator<`).

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use super::error::UsdcWriteError;

/// One element of a path's prim part.
///
/// The derived ordering is `Sdf_PathNode`'s for these nodes: a prim node
/// before a variant selection node, prim nodes by name, and variant
/// selection nodes by set name, then variant name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Element {
    /// A prim name (`/A`).
    Prim(String),
    /// A variant selection (`{set=variant}`); an empty variant names the
    /// variant set itself (`{set=}`).
    Variant {
        /// Variant set name.
        set: String,
        /// Variant name, or empty for the variant set.
        variant: String,
    },
}

/// An absolute path the writer supports: the pseudo-root, a prim path, a
/// variant set or variant path (`/A{v=}`, `/A{v=x}`), a prim inside a
/// variant (`/A{v=x}B`), or a property path of a prim or variant.
///
/// The derived ordering is `SdfPath`'s for these paths: prim parts compare
/// element by element (an ancestor before its descendants, names by byte
/// order, a prim before a variant selection), and only then the property
/// part, with no property first. So a prim is followed by its properties,
/// then by its child prims, then by its variant sets and variants.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CratePath {
    /// Prim part elements from the root; empty for the pseudo-root.
    pub(crate) elements: Vec<Element>,
    /// Property name for a property path.
    pub(crate) property: Option<String>,
}

impl CratePath {
    /// The pseudo-root, `/`.
    pub(crate) fn root() -> Self {
        Self {
            elements: Vec::new(),
            property: None,
        }
    }

    /// Parses `/`, `/A/B`, `/A{v=x}B`, `/A{v=}` or `/A/B.prop:name`.
    ///
    /// A variant selection follows a prim or another selection and is
    /// followed by a child prim name, another selection, a property or
    /// the end; a variant set (`{v=}`) ends the path.
    pub(crate) fn parse(text: &str) -> Result<Self, UsdcWriteError> {
        let invalid = |reason| UsdcWriteError::InvalidPath {
            path: text.into(),
            reason,
        };
        let Some(mut rest) = text.strip_prefix('/') else {
            return Err(invalid("not an absolute path"));
        };
        if rest.is_empty() {
            return Ok(Self::root());
        }
        let mut elements = Vec::new();
        let mut property = None;
        loop {
            let end = rest.find(['/', '{', '.']).unwrap_or(rest.len());
            let name = &rest[..end];
            if !is_name(name) {
                return Err(invalid("prim name is not an identifier"));
            }
            elements.push(Element::Prim(name.into()));
            rest = &rest[end..];
            while let Some(selection) = rest.strip_prefix('{') {
                let Some((inner, after)) = selection.split_once('}') else {
                    return Err(invalid("variant selection is not closed"));
                };
                let Some((set, variant)) = inner.split_once('=') else {
                    return Err(invalid("variant selection has no `=`"));
                };
                if !is_name(set) || !(variant.is_empty() || is_variant_name(variant)) {
                    return Err(invalid("variant selection names are not valid"));
                }
                elements.push(Element::Variant {
                    set: set.into(),
                    variant: variant.into(),
                });
                rest = after;
            }
            let after_selection = matches!(elements.last(), Some(Element::Variant { .. }));
            match rest.chars().next() {
                None => break,
                Some('.') => {
                    let name = &rest[1..];
                    if !name.split(':').all(is_name) {
                        return Err(invalid("property name is not a namespaced identifier"));
                    }
                    property = Some(name.into());
                    break;
                }
                Some('/') if !after_selection => rest = &rest[1..],
                Some(_) if after_selection => {}
                Some(_) => return Err(invalid("prim name is not an identifier")),
            }
        }
        let is_set =
            |e: &Element| matches!(e, Element::Variant { variant, .. } if variant.is_empty());
        if let Some(i) = elements.iter().position(is_set)
            && (i + 1 < elements.len() || property.is_some())
        {
            return Err(invalid("a variant set path has no children"));
        }
        Ok(Self { elements, property })
    }

    /// Whether this is a property path.
    pub(crate) fn is_property(&self) -> bool {
        self.property.is_some()
    }

    /// Whether this is the pseudo-root.
    pub(crate) fn is_root(&self) -> bool {
        self.elements.is_empty() && self.property.is_none()
    }

    /// The last element of the prim part, for a path that is not a
    /// property path.
    pub(crate) fn last_element(&self) -> Option<&Element> {
        self.elements.last().filter(|_| self.property.is_none())
    }

    /// The parent path, `SdfPath::GetParentPath`: the prim part of a
    /// property path, otherwise the prim part less its last element (so a
    /// variant's parent is the prim or variant holding its set); `None`
    /// for the pseudo-root.
    pub(crate) fn parent(&self) -> Option<Self> {
        if self.property.is_some() {
            return Some(Self {
                elements: self.elements.clone(),
                property: None,
            });
        }
        let (_, parent) = self.elements.split_last()?;
        Some(Self {
            elements: parent.to_vec(),
            property: None,
        })
    }

    /// The variant set path of a variant path (`/A{v=}` for `/A{v=x}`).
    pub(crate) fn variant_set(&self) -> Option<Self> {
        match self.last_element()? {
            Element::Variant { set, variant } if !variant.is_empty() => {
                let mut elements = self.elements.clone();
                elements.pop();
                elements.push(Element::Variant {
                    set: set.clone(),
                    variant: String::new(),
                });
                Some(Self {
                    elements,
                    property: None,
                })
            }
            _ => None,
        }
    }

    /// Number of path elements below the pseudo-root.
    pub(crate) fn depth(&self) -> usize {
        self.elements.len() + usize::from(self.property.is_some())
    }

    /// The token stored for this path's last element, as `CrateFile::_AddPath`
    /// stores it: the property name for a property path, otherwise the
    /// element's text (`B`, `{v=x}`, empty for the root).
    pub(crate) fn element_token(&self) -> Cow<'_, str> {
        match (&self.property, self.elements.last()) {
            (Some(property), _) => Cow::Borrowed(property),
            (None, Some(Element::Prim(name))) => Cow::Borrowed(name),
            (None, Some(Element::Variant { set, variant })) => {
                Cow::Owned(alloc::format!("{{{set}={variant}}}"))
            }
            (None, None) => Cow::Borrowed(""),
        }
    }
}

impl core::fmt::Display for CratePath {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_root() {
            return f.write_str("/");
        }
        let mut after_selection = false;
        for element in &self.elements {
            match element {
                Element::Prim(name) if after_selection => f.write_str(name)?,
                Element::Prim(name) => write!(f, "/{name}")?,
                Element::Variant { set, variant } => write!(f, "{{{set}={variant}}}")?,
            }
            after_selection = matches!(element, Element::Variant { .. });
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

/// A structural variant name check, as [`is_name`] checks names: an
/// optional leading `.`, then at least one character that is not ASCII
/// punctuation other than `_`, `|` and `-`, whitespace or a control
/// character (OpenUSD's `VariantName` path grammar, `pathParser.h`).
fn is_variant_name(name: &str) -> bool {
    let rest = name.strip_prefix('.').unwrap_or(name);
    let ok = |c: char| !c.is_ascii() || matches!(c, '_' | '|' | '-') || c.is_ascii_alphanumeric();
    !rest.is_empty() && rest.chars().all(ok)
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
            "/A{v=a}",
            "/A{v=}",
            "/A{v=a}B/C.x",
            "/A{v=a}{w=.b-c|d}",
            "/A{v=a}.x",
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
            "/A{v=a}/B",
            "/A{v=}B",
            "/A{v=}.x",
            "/A{v}",
            "/A{v=a",
            "/{v=a}",
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
            p("/A{v=b}"),
            p("/A{v=}"),
            p("/A{u=z}C"),
            p("/A{v=b}.a"),
        ];
        paths.sort();
        let sorted: Vec<String> = paths.iter().map(ToString::to_string).collect();
        assert_eq!(
            sorted,
            [
                "/",
                "/A",
                "/A.b",
                "/A.z",
                "/A/C",
                "/A/C.a",
                "/A{u=z}C",
                "/A{v=}",
                "/A{v=b}",
                "/A{v=b}.a",
                "/Ab",
                "/B"
            ],
            "prim part first, then property; ancestors first; prims before \
             variant selections"
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
