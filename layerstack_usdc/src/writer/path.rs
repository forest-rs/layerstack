// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Paths as the writer stores them, their `SdfPath` ordering, and the
//! compressed path tree of the PATHS section.
//!
//! Spec: AOUSD Core §16.3.7, §8 (paths); OpenUSD `pxr/usd/sdf/crateFile.cpp`
//! (`_WritePaths`, `_BuildCompressedPathDataRecursive`), `path.cpp`
//! (`SdfPath::operator<`) and `pathNode.h` (`Sdf_PathNode::Compare`).

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::cmp::Ordering;

use super::error::UsdcWriteError;

/// One element of a path's prim part.
///
/// The derived ordering is `Sdf_PathNode`'s for these nodes: a prim node
/// before a variant selection node, prim nodes by name, and variant
/// selection nodes by set name, then variant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Element<'a> {
    /// A prim name (`/A`).
    Prim(&'a str),
    /// A variant selection (`{set=variant}`); an empty variant names the
    /// variant set itself (`{set=}`).
    Variant {
        /// Variant set name.
        set: &'a str,
        /// Variant name, or empty for the variant set.
        variant: &'a str,
    },
}

/// The ordinary namespace prefix is already a canonical key into the input
/// text. Only elements starting at the first variant selection need storage.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
struct PrimPart<'a> {
    names: &'a str,
    suffix: Vec<Element<'a>>,
}

impl<'a> PrimPart<'a> {
    fn elements(&self) -> impl Iterator<Item = Element<'a>> + '_ {
        self.names
            .split('/')
            .skip(1)
            .map(Element::Prim)
            .chain(self.suffix.iter().copied())
    }

    fn last(&self) -> Option<Element<'a>> {
        self.suffix.last().copied().or_else(|| {
            self.names
                .rsplit_once('/')
                .map(|(_, name)| Element::Prim(name))
        })
    }

    fn parent(&self) -> Option<Self> {
        if let Some((_, prefix)) = self.suffix.split_last() {
            return Some(Self {
                names: self.names,
                suffix: prefix.to_vec(),
            });
        }
        self.names.rsplit_once('/').map(|(names, _)| Self {
            names,
            suffix: Vec::new(),
        })
    }
}

impl Ord for PrimPart<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.suffix.is_empty() && other.suffix.is_empty() {
            // '/' precedes every byte allowed in an identifier. Comparing
            // slash-delimited names thus compares their elements in Sdf order.
            self.names.cmp(other.names)
        } else {
            self.elements().cmp(other.elements())
        }
    }
}

impl PartialOrd for PrimPart<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// One element of a path's property part.
///
/// The variants are in `Sdf_PathNode::NodeType` order (a prim property,
/// then a target, a mapper, a relational attribute, a mapper argument and
/// an expression node), so the derived ordering is
/// `Sdf_PathNode::Compare`'s: by node type, then a name by byte order or a
/// bracketed path in `SdfPath` order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PropertyElement<'a> {
    /// A target path (`[/C]`), after a property or a relational
    /// attribute.
    Target(CratePath<'a>),
    /// A mapper (`.mapper[/C]`), after a property or a relational
    /// attribute.
    Mapper(CratePath<'a>),
    /// A relational attribute (`.d`), after a target.
    RelationalAttribute(&'a str),
    /// A mapper argument (`.arg`), after a mapper.
    MapperArg(&'a str),
    /// An expression (`.expression`), after a property or a relational
    /// attribute.
    Expression,
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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CratePath<'a> {
    prim: PrimPart<'a>,
    /// The first property name, also the property grouping key in SPECS.
    pub(crate) property: Option<&'a str>,
    /// Steps below the named property.
    below: Vec<PropertyElement<'a>>,
}

impl<'a> CratePath<'a> {
    /// The pseudo-root, `/`.
    pub(crate) fn root() -> Self {
        Self {
            prim: PrimPart::default(),
            property: None,
            below: Vec::new(),
        }
    }

    /// Parses `/`, `/A/B`, `/A{v=x}B`, `/A{v=}`, `/A/B.prop:name`, or a
    /// path below a property such as `/A.b[/C].d` or `/A.b.mapper[/C]`.
    ///
    /// A variant selection follows a prim or another selection and is
    /// followed by a child prim name, another selection, a property or
    /// the end; a variant set (`{v=}`) ends the path. A property part
    /// follows `SdfPath`'s grammar (`PropElts`, `pxr/usd/sdf/pathParser.h`):
    /// a property name, then a target, a mapper or an expression; a
    /// relational attribute after a target, then a target, a mapper or an
    /// expression again; a mapper argument after a mapper; the bracketed
    /// paths absolute.
    pub(crate) fn parse(text: &'a str) -> Result<Self, UsdcWriteError> {
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
        let mut prim = PrimPart::default();
        let mut property = None;
        let mut below = Vec::new();
        loop {
            let end = rest.find(['/', '{', '.']).unwrap_or(rest.len());
            let name = &rest[..end];
            if !is_name(name) {
                return Err(invalid("prim name is not an identifier"));
            }
            rest = &rest[end..];
            if prim.suffix.is_empty() {
                prim.names = &text[..text.len() - rest.len()];
            } else {
                prim.suffix.push(Element::Prim(name));
            }
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
                prim.suffix.push(Element::Variant { set, variant });
                rest = after;
            }
            let after_selection = matches!(prim.suffix.last(), Some(Element::Variant { .. }));
            match rest.chars().next() {
                None => break,
                Some('.') => {
                    let (name, suffix) = parse_property(&rest[1..]).map_err(invalid)?;
                    property = Some(name);
                    below = suffix;
                    break;
                }
                Some('/') if !after_selection => rest = &rest[1..],
                Some(_) if after_selection => {}
                Some(_) => return Err(invalid("prim name is not an identifier")),
            }
        }
        let is_set =
            |e: &Element<'_>| matches!(e, Element::Variant { variant, .. } if variant.is_empty());
        if let Some(i) = prim.suffix.iter().position(is_set)
            && (i + 1 < prim.suffix.len() || property.is_some())
        {
            return Err(invalid("a variant set path has no children"));
        }
        Ok(Self {
            prim,
            property,
            below,
        })
    }

    /// Whether this is a prim property path (`SdfPath::IsPrimPropertyPath`):
    /// a property of a prim or variant, the only property path a spec has
    /// here and the one the path tree stores by its name.
    pub(crate) fn is_prim_property(&self) -> bool {
        self.property.is_some() && self.below.is_empty()
    }

    /// Whether this is the pseudo-root.
    pub(crate) fn is_root(&self) -> bool {
        self.prim.names.is_empty()
    }

    /// The last element of the prim part, for a path that is not below a
    /// property.
    pub(crate) fn last_element(&self) -> Option<Element<'a>> {
        self.prim.last().filter(|_| self.property.is_none())
    }

    /// The parent path, `SdfPath::GetParentPath`: the path less its last
    /// element, so a property's parent is its prim, a target's its
    /// property and a variant's the prim or variant holding its set;
    /// `None` for the pseudo-root.
    pub(crate) fn parent(&self) -> Option<Self> {
        if self.property.is_some() {
            let (property, below) = match self.below.split_last() {
                Some((_, prefix)) => (self.property, prefix.to_vec()),
                None => (None, Vec::new()),
            };
            return Some(Self {
                prim: self.prim.clone(),
                property,
                below,
            });
        }
        Some(Self {
            prim: self.prim.parent()?,
            property: None,
            below: Vec::new(),
        })
    }

    /// The bracketed path of a target path (`SdfPath::GetTargetPath`,
    /// `/C` for `/A.b[/C]`), which `CrateFile::_AddPath` adds to the path
    /// table before the target path's parent; `None` for any other path,
    /// a mapper's included.
    pub(crate) fn target(&self) -> Option<&Self> {
        match self.below.last()? {
            PropertyElement::Target(target) => Some(target),
            _ => None,
        }
    }

    /// The variant set path of a variant path (`/A{v=}` for `/A{v=x}`).
    pub(crate) fn variant_set(&self) -> Option<Self> {
        match self.last_element()? {
            Element::Variant { set, variant } if !variant.is_empty() => {
                let mut path = self.clone();
                *path.prim.suffix.last_mut()? = Element::Variant { set, variant: "" };
                Some(path)
            }
            _ => None,
        }
    }

    /// Number of path elements below the pseudo-root.
    pub(crate) fn depth(&self) -> usize {
        self.prim.names.bytes().filter(|&c| c == b'/').count()
            + self.prim.suffix.len()
            + usize::from(self.property.is_some())
            + self.below.len()
    }

    /// The token stored for this path's last element, as `CrateFile::_AddPath`
    /// stores it: the property name for a prim property path, otherwise
    /// the element's text (`SdfPath::GetElementToken`: `B`, `{v=x}`,
    /// `[/C]`, `.mapper[/C]`, `.d`, empty for the root).
    pub(crate) fn element_token(&self) -> Cow<'_, str> {
        if self.is_prim_property() {
            return Cow::Borrowed(self.property.expect("a prim property"));
        }
        match (self.below.last(), self.prim.last()) {
            (Some(PropertyElement::Target(target)), _) => Cow::Owned(alloc::format!("[{target}]")),
            (Some(PropertyElement::Mapper(target)), _) => {
                Cow::Owned(alloc::format!(".mapper[{target}]"))
            }
            (
                Some(PropertyElement::RelationalAttribute(name) | PropertyElement::MapperArg(name)),
                _,
            ) => Cow::Owned(alloc::format!(".{name}")),
            (Some(PropertyElement::Expression), _) => Cow::Borrowed(".expression"),
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
fn parse_property(mut text: &str) -> Result<(&str, Vec<PropertyElement<'_>>), &'static str> {
    // A name ends where a bracketed path or the next element starts.
    let name_end = |text: &str| text.find(['[', '.']).unwrap_or(text.len());
    let end = name_end(text);
    if !is_property_name(&text[..end]) {
        return Err("property name is not a namespaced identifier");
    }
    let name = &text[..end];
    let mut property = Vec::new();
    text = &text[end..];
    while !text.is_empty() {
        let last = property.last();
        match last {
            Some(PropertyElement::MapperArg(_) | PropertyElement::Expression) => {
                return Err("a mapper argument or an expression ends the path");
            }
            Some(PropertyElement::Mapper(_)) => {
                // Only a mapper argument follows a mapper.
                let Some(rest) = text.strip_prefix('.') else {
                    return Err("a mapper is followed by a mapper argument");
                };
                let end = name_end(rest);
                if !is_name(&rest[..end]) {
                    return Err("mapper argument name is not an identifier");
                }
                property.push(PropertyElement::MapperArg(&rest[..end]));
                text = &rest[end..];
            }
            Some(PropertyElement::Target(_)) => {
                // Only a relational attribute follows a target.
                let Some(rest) = text.strip_prefix('.') else {
                    return Err("a target is followed by a relational attribute");
                };
                let end = name_end(rest);
                if !is_property_name(&rest[..end]) {
                    return Err("relational attribute name is not a namespaced identifier");
                }
                property.push(PropertyElement::RelationalAttribute(&rest[..end]));
                text = &rest[end..];
            }
            None | Some(PropertyElement::RelationalAttribute(_)) => {
                if let Some(rest) = text.strip_prefix(".mapper[") {
                    let (target, rest) = bracketed(rest)?;
                    property.push(PropertyElement::Mapper(target));
                    text = rest;
                } else if let Some(rest) = text.strip_prefix('[') {
                    let (target, rest) = bracketed(rest)?;
                    property.push(PropertyElement::Target(target));
                    text = rest;
                } else if let Some(rest) = text.strip_prefix(".expression")
                    && !rest.starts_with(|c: char| c != '.' && c != '[')
                {
                    property.push(PropertyElement::Expression);
                    text = rest;
                } else {
                    return Err("a property is followed by a target, a mapper or an expression");
                }
            }
        }
    }
    Ok((name, property))
}

/// Parses the absolute path of a bracketed element, `text` following its
/// `[`, and returns it with the text after its `]`.
///
/// A relative bracketed path, which OpenUSD's text parser keeps where
/// `SdfPath::MakeAbsolutePath` does not anchor it (`</X/Y.b[Z].d>`), is
/// refused: OpenUSD 26.11 cannot save one either, its
/// `CrateFile::_AddPath` adding the bracketed path and then its parents
/// (`Z`, `.`, `..`, `../..`, ...), which never reach the absolute root,
/// until the stack overflows (`pxr/usd/sdf/crateFile.cpp`).
fn bracketed(text: &str) -> Result<(CratePath<'_>, &str), &'static str> {
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

impl core::fmt::Display for CratePath<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_root() {
            return f.write_str("/");
        }
        let mut after_selection = false;
        for element in self.prim.elements() {
            match element {
                Element::Prim(name) if after_selection => f.write_str(name)?,
                Element::Prim(name) => write!(f, "/{name}")?,
                Element::Variant { set, variant } => write!(f, "{{{set}={variant}}}")?,
            }
            after_selection = matches!(element, Element::Variant { .. });
        }
        if let Some(name) = self.property {
            write!(f, ".{name}")?;
        }
        for element in &self.below {
            match element {
                PropertyElement::RelationalAttribute(name) | PropertyElement::MapperArg(name) => {
                    write!(f, ".{name}")?;
                }
                PropertyElement::Expression => f.write_str(".expression")?,
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
pub(crate) fn path_tree(sorted: &[(&CratePath<'_>, u32, u32)]) -> PathTree {
    let n = sorted.len();
    let mut tree = PathTree {
        path_indexes: Vec::with_capacity(n),
        element_tokens: Vec::with_capacity(n),
        jumps: alloc::vec![-2; n],
    };
    // The next path at the same depth is a sibling; a shallower path
    // closes a subtree. Each entry opens and closes exactly once.
    let mut open: Vec<(usize, usize)> = Vec::new();
    for (i, (path, index, token)) in sorted.iter().enumerate() {
        let depth = path.depth();
        while let Some(&(at, level)) = open.last() {
            if level < depth {
                break;
            }
            open.pop();
            tree.jumps[at] = match (i > at + 1, level == depth) {
                (true, true) => (i - at) as i64,
                (false, true) => 0,
                (true, false) => -1,
                (false, false) => -2,
            };
        }
        open.push((i, depth));
        tree.path_indexes.push(i64::from(*index));
        let token = i64::from(*token);
        tree.element_tokens.push(if path.is_prim_property() {
            -token
        } else {
            token
        });
    }
    for (at, _) in open {
        if at + 1 < n {
            tree.jumps[at] = -1;
        }
    }
    tree
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::*;

    fn p(text: &str) -> CratePath<'_> {
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
            "/A.rel.mapper[/B].arg",
            "/A.rel.expression",
            "/A.b[/C].d.expression",
            "/A.b[/C].d.mapper[/E].f",
            // A relational attribute named `expression`, as `SdfPath` reads it.
            "/A.rel[/B].expression",
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
            "/A.rel.mapper[/B].arg.c",
            "/A.rel.mapper[/B][/C]",
            "/A.rel.expression.c",
            "/A.rel.expression[/B]",
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
    fn unselected_prefixes_compare_in_element_order() {
        let mut paths = [
            "/A0", "/A_", "/A/X", "/A{v=x}", "/A", "/A0{v=x}", "/A.aa", "/A\u{c9}", "/A{v=x}X",
        ]
        .map(p);
        paths.sort();
        let text: Vec<_> = paths.iter().map(ToString::to_string).collect();
        assert_eq!(
            text,
            [
                "/A", "/A.aa", "/A/X", "/A{v=x}", "/A{v=x}X", "/A0", "/A0{v=x}", "/A_", "/A\u{c9}"
            ]
        );
    }

    #[test]
    fn orders_mapper_arguments_and_expressions_like_sdf_path() {
        // `sorted(Sdf.Path(...))` in OpenUSD 26.11.
        let mut paths = [
            "/A.b.expression",
            "/A.b.mapper[/C].z",
            "/A.b.mapper[/C].a",
            "/A.b[/C]",
            "/A.b.mapper[/C]",
            "/A.b[/C].d.expression",
            "/A.b[/C].d",
        ]
        .map(p);
        paths.sort();
        let sorted: Vec<String> = paths.iter().map(ToString::to_string).collect();
        assert_eq!(
            sorted,
            [
                "/A.b[/C]",
                "/A.b[/C].d",
                "/A.b[/C].d.expression",
                "/A.b.mapper[/C]",
                "/A.b.mapper[/C].a",
                "/A.b.mapper[/C].z",
                "/A.b.expression"
            ]
        );
    }

    #[test]
    fn tree_jumps() {
        // /, /A, /A.x, /A/B, /C
        let paths = [p("/"), p("/A"), p("/A.x"), p("/A/B"), p("/C")];
        let entries: Vec<(&CratePath<'_>, u32, u32)> = paths
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
