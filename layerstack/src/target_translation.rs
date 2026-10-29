// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Mapping the target paths inside a relational attribute or mapper
//! target from one namespace to another.
//!
//! A relationship may target a relational attribute (`/A.b[/C].d`) or a
//! mapper (`/A.b.mapper[/C]`), and an attribute may connect to a
//! relational attribute. layerstack keeps such a target as a
//! [`PropertyPath`] whose property token is the whole property part
//! (`b[/C].d`), so a plain target pays nothing for the rare kind. Mapping
//! one across an arc, or to an edit target's namespace, maps the paths in
//! its brackets as well as its prim's path, as OpenUSD maps them
//! (`Pcp_TranslatePath`, `pxr/usd/pcp/pathTranslation.cpp`;
//! `UsdEditTarget::MapToSpecPath`, `pxr/usd/usd/editTarget.cpp`):
//!
//! 1. the whole path is mapped as a prim's path is, which changes only its
//!    prim part (`PcpMapFunction::MapSourceToTarget`, whose `_Map` replaces
//!    the prefix with `fixTargetPaths = false`);
//! 2. each path `SdfPath::GetAllTargetPathsRecursively` finds in the
//!    result, the property part's last bracket first and each bracket's
//!    own after it, is mapped the same way, and put back with
//!    `SdfPath::ReplacePrefix(target, mapped)`, which replaces that prefix
//!    in the prim part too and fixes every bracketed path
//!    ([`replace_prefix`]);
//! 3. a path any of whose paths does not map does not map.
//!
//! Only a property token holding a `[` takes this path ([`is_relational`]),
//! and a store none of whose tokens holds one never reads a token's text.
//!
//! Spec: AOUSD Core §8 (paths), §10.2 (arcs map namespace).

use alloc::{boxed::Box, string::String, vec::Vec};

use crate::{
    TokenInterner,
    interner::TokenId,
    path::{PathId, PathInterner, PropertyPath},
};

/// How a namespace mapping maps a prim path, for [`map_relational`].
pub(crate) struct PrimMap<'a> {
    /// Maps an absolute prim path; `None` when the mapping does not.
    pub(crate) map: &'a dyn Fn(PathId) -> Option<PathId>,
    /// Whether the mapping maps a path outside each of its pairs to itself,
    /// which is how it maps a relative path (one `SdfPath::MakeAbsolutePath`
    /// left in a bracket): no pair is a prefix of it.
    pub(crate) root_identity: bool,
}

/// Whether `property`, a property target's token, holds a bracketed path:
/// a relational attribute or a mapper, which [`map_relational`] maps.
#[inline]
pub(crate) fn is_relational(tokens: &TokenInterner, property: TokenId) -> bool {
    tokens.has_brackets() && tokens.resolve(property).contains('[')
}

/// Maps `target`, a relational attribute or mapper target
/// ([`is_relational`]), by `map`: its prim's path and the paths in its
/// brackets, as the [module docs](self) describe; `None` when any of them
/// does not map. A token outside the path grammar maps its prim's path
/// only.
pub(crate) fn map_relational(
    tokens: &TokenInterner,
    paths: &PathInterner,
    target: PropertyPath,
    map: &PrimMap<'_>,
) -> Option<PropertyPath> {
    let text = tokens.resolve(target.property());
    let Some(elements) = parse_elements(text, tokens, paths) else {
        let prim = (map.map)(target.prim_path())?;
        return Some(PropertyPath::new(prim, target.property()));
    };
    let path = Tree {
        prim: Prim::Absolute(target.prim_path()),
        elements,
    };
    let translated = translate(paths, &path, map)?;
    let Prim::Absolute(prim) = translated.prim else {
        return None;
    };
    if translated.elements == path.elements {
        return Some(PropertyPath::new(prim, target.property()));
    }
    let mut out = String::with_capacity(text.len());
    write_elements(&mut out, &translated.elements, tokens, paths);
    Some(PropertyPath::new(prim, tokens.intern(&out)))
}

/// A path's prim part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prim<'t> {
    /// An absolute prim path. A variant selection in it (`/A{v=x}B`) is
    /// a segment of its own, so that a prefix of the path is a prefix of
    /// its segments, as `SdfPath::HasPrefix` counts it.
    Absolute(PathId),
    /// A relative prim part as written (`C`, `../S`, or empty for `.b`).
    Relative(&'t str),
}

/// An element of a path's property part, as `SdfPath` builds it of nodes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Element<'t> {
    /// A property name, a relational attribute's name, a mapper argument
    /// or `expression`.
    Name(&'t str),
    /// A bracketed target path (`[/C]`).
    Target(Box<Tree<'t>>),
    /// A mapper and its path (`.mapper[/C]`).
    Mapper(Box<Tree<'t>>),
}

/// A path: its prim part and its property part's elements, nearest the
/// root first; no elements for a prim path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tree<'t> {
    prim: Prim<'t>,
    elements: Vec<Element<'t>>,
}

impl Tree<'_> {
    /// Whether a bracket of the property part holds a path
    /// (`Sdf_PathNode::ContainsTargetPath`).
    fn contains_target(&self) -> bool {
        self.elements
            .iter()
            .any(|element| !matches!(element, Element::Name(_)))
    }
}

/// `Pcp_TranslatePath`'s mapping of `path` (see the [module docs](self)).
fn translate<'t>(paths: &PathInterner, path: &Tree<'t>, map: &PrimMap<'_>) -> Option<Tree<'t>> {
    let mut translated = map_prim_part(path, map)?;
    let mut targets = Vec::new();
    collect_targets(&translated, &mut targets);
    for target in &targets {
        let mapped = map_prim_part(target, map)?;
        translated = replace_prefix(paths, &translated, target, &mapped, true);
    }
    Some(translated)
}

/// `path` with its prim part mapped by `map` and its brackets as they are
/// (`PcpMapFunction::MapSourceToTarget`); `None` when it does not map.
fn map_prim_part<'t>(path: &Tree<'t>, map: &PrimMap<'_>) -> Option<Tree<'t>> {
    let prim = match path.prim {
        Prim::Absolute(prim) => Prim::Absolute((map.map)(prim)?),
        Prim::Relative(_) => map.root_identity.then_some(path.prim)?,
    };
    Some(Tree {
        prim,
        elements: path.elements.clone(),
    })
}

/// The paths in `path`'s brackets and, after each, the paths in its own,
/// from the property part's last element to its first
/// (`SdfPath::GetAllTargetPathsRecursively`).
fn collect_targets<'t>(path: &Tree<'t>, out: &mut Vec<Tree<'t>>) {
    for element in path.elements.iter().rev() {
        if let Element::Target(target) | Element::Mapper(target) = element {
            out.push((**target).clone());
            collect_targets(target, out);
        }
    }
}

/// Whether the prim part `prefix` is a prefix of `prim`.
fn prim_has_prefix(paths: &PathInterner, prim: Prim<'_>, prefix: Prim<'_>) -> bool {
    match (prim, prefix) {
        (Prim::Absolute(path), Prim::Absolute(prefix)) => {
            path == prefix
                || paths
                    .resolve(path)
                    .segments()
                    .starts_with(paths.resolve(prefix).segments())
        }
        _ => prim == prefix,
    }
}

/// `path` with the prefix `old` replaced by `new`, as
/// `SdfPath::ReplacePrefix` replaces it (`pxr/usd/sdf/path.cpp`): in the
/// prim part, or the property part where `old` is a property path, and,
/// with `fix`, in every bracketed path too, whether or not `old` is a
/// prefix of `path` itself.
fn replace_prefix<'t>(
    paths: &PathInterner,
    path: &Tree<'t>,
    old: &Tree<'t>,
    new: &Tree<'t>,
    fix: bool,
) -> Tree<'t> {
    if old == new {
        return path.clone();
    }
    if path == old {
        return new.clone();
    }
    if old.elements.is_empty() {
        // `_ReplacePrimPrefix`, then `_ReplaceTargetPathPrefixes`.
        let mut out = path.clone();
        if prim_has_prefix(paths, path.prim, old.prim) {
            out.prim = match (path.prim, old.prim, new.prim) {
                (Prim::Absolute(prim), Prim::Absolute(from), Prim::Absolute(to)) => {
                    Prim::Absolute(if prim == from {
                        to
                    } else {
                        paths
                            .intern_reparented(prim, from, to)
                            .expect("the prim is beneath the replaced prefix")
                    })
                }
                _ => new.prim,
            };
        }
        if fix && out.contains_target() {
            out = replace_target_prefixes(paths, &out, old, new);
        }
        return out;
    }
    if path.elements.is_empty() {
        return path.clone();
    }
    let depth = old.elements.len();
    if path.prim != old.prim
        || path.elements.len() < depth
        || path.elements[..depth] != old.elements[..]
    {
        return if fix && path.contains_target() {
            replace_target_prefixes(paths, path, old, new)
        } else {
            path.clone()
        };
    }
    // `_ReplacePropPrefix`: `new` and the rest of the property part.
    let mut out = new.clone();
    for element in &path.elements[depth..] {
        out.elements.push(match element {
            Element::Target(target) if fix => {
                Element::Target(Box::new(replace_prefix(paths, target, old, new, fix)))
            }
            Element::Mapper(target) if fix => {
                Element::Mapper(Box::new(replace_prefix(paths, target, old, new, fix)))
            }
            other => other.clone(),
        });
    }
    out
}

/// `path` with the prefix `old` replaced by `new` in each bracketed path
/// (`SdfPath::_ReplaceTargetPathPrefixes`).
fn replace_target_prefixes<'t>(
    paths: &PathInterner,
    path: &Tree<'t>,
    old: &Tree<'t>,
    new: &Tree<'t>,
) -> Tree<'t> {
    Tree {
        prim: path.prim,
        elements: path
            .elements
            .iter()
            .map(|element| match element {
                Element::Name(name) => Element::Name(name),
                Element::Target(target) => {
                    Element::Target(Box::new(replace_prefix(paths, target, old, new, true)))
                }
                Element::Mapper(target) => {
                    Element::Mapper(Box::new(replace_prefix(paths, target, old, new, true)))
                }
            })
            .collect(),
    }
}

/// A part of a property part's text: a name between `.`s, or the text of a
/// top-level `[...]`.
enum Part<'t> {
    Name(&'t str),
    Bracket(&'t str),
}

/// The elements of `text`, a property part (`b[/C].d`, without the `.`
/// before it), or `None` when it is not of the path grammar's shape.
fn parse_elements<'t>(
    text: &'t str,
    tokens: &TokenInterner,
    paths: &PathInterner,
) -> Option<Vec<Element<'t>>> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0_usize, 0);
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            b'[' => {
                if depth == 0 {
                    if start < at {
                        parts.push(Part::Name(&text[start..at]));
                    }
                    start = at + 1;
                }
                depth += 1;
            }
            b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    parts.push(Part::Bracket(&text[start..at]));
                    start = at + 1;
                }
            }
            b'.' if depth == 0 => {
                if start < at {
                    parts.push(Part::Name(&text[start..at]));
                }
                start = at + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    if start < text.len() {
        parts.push(Part::Name(&text[start..]));
    }
    let mut elements = Vec::with_capacity(parts.len());
    let mut parts = parts.into_iter().peekable();
    while let Some(part) = parts.next() {
        let element = match part {
            Part::Name("mapper") if matches!(parts.peek(), Some(Part::Bracket(_))) => {
                let Some(Part::Bracket(inner)) = parts.next() else {
                    unreachable!("peeked a bracket");
                };
                Element::Mapper(Box::new(parse_path(inner, tokens, paths)?))
            }
            Part::Name(name) => Element::Name(name),
            // A bracket follows a name.
            Part::Bracket(_) if elements.is_empty() => return None,
            Part::Bracket(inner) => Element::Target(Box::new(parse_path(inner, tokens, paths)?)),
        };
        elements.push(element);
    }
    matches!(elements.first(), Some(Element::Name(_))).then_some(elements)
}

/// Parses `text`, a path in a bracket, absolute or relative.
fn parse_path<'t>(text: &'t str, tokens: &TokenInterner, paths: &PathInterner) -> Option<Tree<'t>> {
    // A relative path's leading `..` elements hold dots of their own.
    let mut from = 0;
    while text[from..].starts_with("../") {
        from += 3;
    }
    if &text[from..] == ".." {
        from += 2;
    }
    let mut depth = 0_usize;
    let mut split = None;
    for (at, byte) in text.bytes().enumerate().skip(from) {
        match byte {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth = depth.checked_sub(1)?,
            b'.' if depth == 0 => {
                split = Some(at);
                break;
            }
            _ => {}
        }
    }
    let (prim, property) = match split {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };
    let prim = match prim.strip_prefix('/') {
        Some(names) => Prim::Absolute(intern_prim(names, tokens, paths)?),
        None => Prim::Relative(prim),
    };
    let elements = match property {
        Some(property) => parse_elements(property, tokens, paths)?,
        None => Vec::new(),
    };
    Some(Tree { prim, elements })
}

/// Interns the absolute prim path whose names, after its `/`, are `names`;
/// each variant selection is a segment of its own (`A{v=x}B` is `A`,
/// `{v=x}`, `B`).
fn intern_prim(names: &str, tokens: &TokenInterner, paths: &PathInterner) -> Option<PathId> {
    let mut segments: Vec<TokenId> = Vec::new();
    if !names.is_empty() {
        for name in names.split('/') {
            let mut rest = name;
            while !rest.is_empty() {
                let end = if rest.starts_with('{') {
                    rest.find('}')? + 1
                } else {
                    rest.find('{').unwrap_or(rest.len())
                };
                segments.push(tokens.intern(&rest[..end]));
                rest = &rest[end..];
            }
            if name.is_empty() {
                return None;
            }
        }
    }
    Some(paths.intern_segments(&segments))
}

/// Writes `elements` as a property part's text (`b[/C].d`).
fn write_elements(
    out: &mut String,
    elements: &[Element<'_>],
    tokens: &TokenInterner,
    paths: &PathInterner,
) {
    for (at, element) in elements.iter().enumerate() {
        match element {
            Element::Name(name) => {
                if at > 0 {
                    out.push('.');
                }
                out.push_str(name);
            }
            Element::Target(target) => {
                out.push('[');
                write_path(out, target, tokens, paths);
                out.push(']');
            }
            Element::Mapper(target) => {
                out.push_str(".mapper[");
                write_path(out, target, tokens, paths);
                out.push(']');
            }
        }
    }
}

/// Writes `path` as text.
fn write_path(out: &mut String, path: &Tree<'_>, tokens: &TokenInterner, paths: &PathInterner) {
    match path.prim {
        Prim::Absolute(prim) => {
            let segments = paths.resolve(prim).segments();
            if segments.is_empty() {
                out.push('/');
            }
            let mut after_selection = false;
            for &segment in segments {
                let name = tokens.resolve(segment);
                let selection = name.starts_with('{');
                if !selection && !after_selection {
                    out.push('/');
                }
                out.push_str(name);
                after_selection = selection;
            }
        }
        Prim::Relative(prim) => out.push_str(prim),
    }
    if !path.elements.is_empty() {
        out.push('.');
        write_elements(out, &path.elements, tokens, paths);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::Path;

    /// Maps `text`, a relational target, by the prefix pairs `pairs`
    /// (source, target) with or without the root identity.
    fn mapped(text: &str, pairs: &[(&str, &str)], root_identity: bool) -> Option<String> {
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let path = |text: &str| paths.intern(Path::parse_absolute(text, &tokens).unwrap());
        let pairs: Vec<(PathId, PathId)> = pairs
            .iter()
            .map(|(source, target)| (path(source), path(target)))
            .collect();
        let map = |prim: PathId| {
            let resolved = paths.resolve(prim);
            pairs
                .iter()
                .filter(|(source, _)| paths.resolve(*source).is_prefix_of(resolved))
                .max_by_key(|(source, _)| paths.resolve(*source).depth())
                .map(|&(source, target)| {
                    if source == prim {
                        target
                    } else {
                        paths.intern_reparented(prim, source, target).unwrap()
                    }
                })
                .or(root_identity.then_some(prim))
        };
        let (prim, property) = text.split_once('.').unwrap();
        let target = PropertyPath::new(path(prim), tokens.intern(property));
        assert!(is_relational(&tokens, target.property()));
        let map = PrimMap {
            map: &map,
            root_identity,
        };
        map_relational(&tokens, &paths, target, &map).map(|t| t.display(&paths, &tokens))
    }

    #[test]
    fn bracketed_paths_map_as_the_owning_prim_does() {
        let reference = [("/Ref", "/P")];
        for (from, to) in [
            ("/Ref.b[/Ref/C].d", "/P.b[/P/C].d"),
            ("/Ref.b.mapper[/Ref/C]", "/P.b.mapper[/P/C]"),
            ("/Ref.b[/Ref/C.d[/Ref/E].f].g", "/P.b[/P/C.d[/P/E].f].g"),
            (
                "/Ref.b[/Ref/C].d.mapper[/Ref/E]",
                "/P.b[/P/C].d.mapper[/P/E]",
            ),
            ("/Ref/C.b[/Ref/E].d", "/P/C.b[/P/E].d"),
            ("/Ref.b[/Ref/C{v=x}D].d", "/P.b[/P/C{v=x}D].d"),
            ("/Ref.b[/Ref/C{v=x}].d", "/P.b[/P/C{v=x}].d"),
        ] {
            assert_eq!(
                mapped(from, &reference, false).as_deref(),
                Some(to),
                "{from}"
            );
        }
        // A bracketed path, nested or not, outside the reference's
        // namespace does not map, and neither does a relative one.
        for from in [
            "/Ref.b[/Other].d",
            "/Ref.b.mapper[/Other]",
            "/Ref.b[/Ref/C.d[/Other].f].g",
            "/Ref.b[C].d.mapper[/Ref/E]",
        ] {
            assert_eq!(mapped(from, &reference, false), None, "{from}");
        }
        // A root identity maps both to themselves.
        assert_eq!(
            mapped("/Ref.b[C].d.mapper[/Other]", &reference, true).as_deref(),
            Some("/P.b[C].d.mapper[/Other]")
        );
    }

    /// `SdfPath::ReplacePrefix` replaces a mapped bracketed path's prefix
    /// in the prim part too: `/A` maps to `/A/B`, so `/A/B`, the mapped
    /// owner, becomes `/A/B/B`.
    #[test]
    fn a_mapped_prefix_is_replaced_in_the_owning_prim_too() {
        let reference = [("/A", "/A/B")];
        assert_eq!(
            mapped("/A.b[/A].d", &reference, false).as_deref(),
            Some("/A/B/B.b[/A/B].d")
        );
        assert_eq!(
            mapped("/A/X.b[/A/X].d", &reference, false).as_deref(),
            Some("/A/B/X.b[/A/B/X].d")
        );
    }
}
