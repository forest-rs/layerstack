// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Paths as the writer stores them, their `SdfPath` ordering, and the
//! compressed path tree of the PATHS section.
//!
//! Spec: AOUSD Core §16.3.7, §8 (paths); OpenUSD `pxr/usd/sdf/crateFile.cpp`
//! (`_WritePaths`, `_BuildCompressedPathDataRecursive`), `path.cpp`
//! (`SdfPath::operator<`) and `pathNode.h` (`Sdf_PathNode::Compare`).

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

/// One element of a path's property part.
///
/// The variants are in `Sdf_PathNode::NodeType` order (a prim property,
/// then a target, a mapper and a relational attribute node), so the
/// derived ordering is `Sdf_PathNode::Compare`'s: by node type, then a
/// name by byte order or a bracketed path in `SdfPath` order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PropertyElement {
    /// The property of a prim or variant (`.b`).
    Property(String),
    /// A target path (`[/C]`), after a property or a relational
    /// attribute.
    Target(CratePath),
    /// A mapper (`.mapper[/C]`), after a property or a relational
    /// attribute.
    Mapper(CratePath),
    /// A relational attribute (`.d`), after a target.
    RelationalAttribute(String),
}

/// An absolute path the writer supports: the pseudo-root, a prim path, a
/// variant set or variant path (`/A{v=}`, `/A{v=x}`), a prim inside a
/// variant (`/A{v=x}B`), a property path of a prim or variant, or a path
/// below a property (`/A.b[/C]`, `/A.b[/C].d`, `/A.b.mapper[/C]`, ...).
///
/// The derived ordering is `SdfPath`'s for these paths: prim parts compare
/// element by element (an ancestor before its descendants, names by byte
/// order, a prim before a variant selection), and only then the property
/// parts, likewise, with no property first. So a prim is followed by its
/// properties, then by its child prims, then by its variant sets and
/// variants.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CratePath {
    /// Prim part elements from the root; empty for the pseudo-root.
    pub(crate) elements: Vec<Element>,
    /// Property part elements, the property first; empty for a path that
    /// is not below a property.
    pub(crate) property: Vec<PropertyElement>,
}

impl CratePath {
    /// The pseudo-root, `/`.
    pub(crate) fn root() -> Self {
        Self {
            elements: Vec::new(),
            property: Vec::new(),
        }
    }

    /// Parses `/`, `/A/B`, `/A{v=x}B`, `/A{v=}`, `/A/B.prop:name`, or a
    /// path below a property such as `/A.b[/C].d` or `/A.b.mapper[/C]`.
    ///
    /// A variant selection follows a prim or another selection and is
    /// followed by a child prim name, another selection, a property or
    /// the end; a variant set (`{v=}`) ends the path. A property part
    /// follows `SdfPath`'s grammar (`PropElts`, `pxr/usd/sdf/pathParser.h`)
    /// as far as these elements go: a property name, then a target or a
    /// mapper; a relational attribute after a target, then a target or a
    /// mapper again; the bracketed paths absolute.
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
        let mut property = Vec::new();
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
                    property = parse_property(&rest[1..]).map_err(invalid)?;
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
            && (i + 1 < elements.len() || !property.is_empty())
        {
            return Err(invalid("a variant set path has no children"));
        }
        Ok(Self { elements, property })
    }

    /// Whether this is a prim property path (`SdfPath::IsPrimPropertyPath`):
    /// a property of a prim or variant, the only property path a spec has
    /// here and the one the path tree stores by its name.
    pub(crate) fn is_prim_property(&self) -> bool {
        self.property.len() == 1
    }

    /// Whether this is the pseudo-root.
    pub(crate) fn is_root(&self) -> bool {
        self.elements.is_empty() && self.property.is_empty()
    }

    /// The last element of the prim part, for a path that is not below a
    /// property.
    pub(crate) fn last_element(&self) -> Option<&Element> {
        self.elements.last().filter(|_| self.property.is_empty())
    }

    /// The parent path, `SdfPath::GetParentPath`: the path less its last
    /// element, so a property's parent is its prim, a target's its
    /// property and a variant's the prim or variant holding its set;
    /// `None` for the pseudo-root.
    pub(crate) fn parent(&self) -> Option<Self> {
        if let Some((_, parent)) = self.property.split_last() {
            return Some(Self {
                elements: self.elements.clone(),
                property: parent.to_vec(),
            });
        }
        let (_, parent) = self.elements.split_last()?;
        Some(Self {
            elements: parent.to_vec(),
            property: Vec::new(),
        })
    }

    /// The bracketed path of a target path (`SdfPath::GetTargetPath`,
    /// `/C` for `/A.b[/C]`), which `CrateFile::_AddPath` adds to the path
    /// table before the target path's parent; `None` for any other path,
    /// a mapper's included.
    pub(crate) fn target(&self) -> Option<&Self> {
        match self.property.last()? {
            PropertyElement::Target(target) => Some(target),
            _ => None,
        }
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
                    property: Vec::new(),
                })
            }
            _ => None,
        }
    }

    /// Number of path elements below the pseudo-root.
    pub(crate) fn depth(&self) -> usize {
        self.elements.len() + self.property.len()
    }

    /// The token stored for this path's last element, as `CrateFile::_AddPath`
    /// stores it: the property name for a prim property path, otherwise
    /// the element's text (`SdfPath::GetElementToken`: `B`, `{v=x}`,
    /// `[/C]`, `.mapper[/C]`, `.d`, empty for the root).
    pub(crate) fn element_token(&self) -> Cow<'_, str> {
        match (self.property.last(), self.elements.last()) {
            (Some(PropertyElement::Property(name)), _) => Cow::Borrowed(name),
            (Some(PropertyElement::Target(target)), _) => Cow::Owned(alloc::format!("[{target}]")),
            (Some(PropertyElement::Mapper(target)), _) => {
                Cow::Owned(alloc::format!(".mapper[{target}]"))
            }
            (Some(PropertyElement::RelationalAttribute(name)), _) => {
                Cow::Owned(alloc::format!(".{name}"))
            }
            (None, Some(Element::Prim(name))) => Cow::Borrowed(name),
            (None, Some(Element::Variant { set, variant })) => {
                Cow::Owned(alloc::format!("{{{set}={variant}}}"))
            }
            (None, None) => Cow::Borrowed(""),
        }
    }
}

/// Parses a property part, `text` being what follows its `.`: a property
/// name, then the elements [`CratePath::parse`] lists, or the reason it
/// is not one.
fn parse_property(mut text: &str) -> Result<Vec<PropertyElement>, &'static str> {
    // A name ends where a bracketed path or the next element starts.
    let name_end = |text: &str| text.find(['[', '.']).unwrap_or(text.len());
    let end = name_end(text);
    if !is_property_name(&text[..end]) {
        return Err("property name is not a namespaced identifier");
    }
    let mut property = alloc::vec![PropertyElement::Property(text[..end].into())];
    text = &text[end..];
    while !text.is_empty() {
        let after_target = matches!(property.last(), Some(PropertyElement::Target(_)));
        let after_mapper = matches!(property.last(), Some(PropertyElement::Mapper(_)));
        if after_mapper {
            return Err("a mapper ends the path");
        }
        if after_target {
            // Only a relational attribute follows a target.
            let Some(rest) = text.strip_prefix('.') else {
                return Err("a target is followed by a relational attribute");
            };
            let end = name_end(rest);
            if !is_property_name(&rest[..end]) {
                return Err("relational attribute name is not a namespaced identifier");
            }
            property.push(PropertyElement::RelationalAttribute(rest[..end].into()));
            text = &rest[end..];
        } else if let Some(rest) = text.strip_prefix(".mapper[") {
            let (target, rest) = bracketed(rest)?;
            property.push(PropertyElement::Mapper(target));
            text = rest;
        } else if let Some(rest) = text.strip_prefix('[') {
            let (target, rest) = bracketed(rest)?;
            property.push(PropertyElement::Target(target));
            text = rest;
        } else {
            return Err("a property is followed by a target or a mapper");
        }
    }
    Ok(property)
}

/// Parses the absolute path of a bracketed element, `text` following its
/// `[`, and returns it with the text after its `]`.
fn bracketed(text: &str) -> Result<(CratePath, &str), &'static str> {
    let mut depth = 0_usize;
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' if depth == 0 => {
                let target = CratePath::parse(&text[..at])
                    .map_err(|_| "bracketed path is not an absolute path")?;
                if target.is_root() {
                    return Err("bracketed path is the pseudo-root");
                }
                return Ok((target, &text[at + 1..]));
            }
            b']' => depth -= 1,
            _ => {}
        }
    }
    Err("bracketed path is not closed")
}

/// A property name: namespaced identifiers (`primvars:st`).
fn is_property_name(name: &str) -> bool {
    name.split(':').all(is_name)
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
        for element in &self.property {
            match element {
                PropertyElement::Property(name) | PropertyElement::RelationalAttribute(name) => {
                    write!(f, ".{name}")?;
                }
                PropertyElement::Target(target) => write!(f, "[{target}]")?,
                PropertyElement::Mapper(target) => write!(f, ".mapper[{target}]")?,
            }
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
        tree.element_tokens.push(if path.is_prim_property() {
            -token
        } else {
            token
        });
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
            "/A.b[/C]",
            "/A.b[/C].d",
            "/A.b[/C].d:e",
            "/A.b.mapper[/C]",
            "/A.b[/C].d.mapper[/E]",
            "/A.b[/C].d[/E]",
            "/A.b[/C.d[/E].f].g",
            "/A.b[/C{v=x}].d",
            "/A{v=a}.b[/C].d",
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
            "/A.rel[B]",
            "/A.rel[/]",
            "/A.rel[/B",
            "/A.rel[/B].",
            "/A.rel[/B][/C]",
            "/A.rel.mapper[/B].arg",
            "/A.rel.expression",
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
            p("/A.b[/C].d"),
            p("/A.b.mapper[/C]"),
            p("/A.b[/C]"),
            p("/A.b[/B].z"),
        ];
        paths.sort();
        let sorted: Vec<String> = paths.iter().map(ToString::to_string).collect();
        assert_eq!(
            sorted,
            [
                "/",
                "/A",
                "/A.b",
                "/A.b[/B].z",
                "/A.b[/C]",
                "/A.b[/C].d",
                "/A.b.mapper[/C]",
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
             variant selections; targets before mappers"
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
