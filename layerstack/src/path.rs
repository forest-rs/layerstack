// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Paths for `layerstack`.
//!
//! `layerstack` uses segmented, interned paths (similar to OpenUSD prim paths).
//!
//! Spec: AOUSD Core §8 (paths and namespace ordering).

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

use core::{borrow::Borrow, cmp::Ordering};

use crate::interner::{TokenId, TokenInterner};
use crate::interning::Interned;
use smallvec::SmallVec;

/// The segments of a path being assembled, inline up to a depth few prim
/// paths exceed, so a lookup of a joined or moved path does not allocate.
pub(crate) type Segments = SmallVec<[TokenId; 16]>;

/// A stable identifier for an interned path.
///
/// Ids compare in interning order, which threads interning at once make
/// vary from run to run: an order for maps and sets, not for reporting
/// paths ([`PathInterner::cmp_paths`] is namespace order). OpenUSD:
/// `SdfPath::FastLessThan`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathId(u32);

impl PathId {
    /// Returns the underlying numeric identifier.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Creates a `PathId` from a raw integer.
    ///
    /// This is intended for tests and other internal callers that need stable,
    /// synthetic identifiers.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

impl invalidation::DenseKey for PathId {
    #[inline]
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// A concrete property path: a prim namespace path plus a property name.
///
/// This is distinct from [`SpecPath`](crate::spec_path::SpecPath), which owns
/// variant-qualified provenance. `PropertyPath` is for concrete scene-namespace
/// identity such as `/Model.xformOpOrder`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PropertyPath {
    prim_path: PathId,
    property: TokenId,
}

impl PropertyPath {
    /// Builds a property path from an interned prim path and interned property name.
    #[must_use]
    pub const fn new(prim_path: PathId, property: TokenId) -> Self {
        Self {
            prim_path,
            property,
        }
    }

    /// Parses a concrete property path such as `/Prim.attrName`.
    ///
    /// Property paths are concrete scene paths and therefore do not admit
    /// variant selections. Use [`crate::spec_path::SpecPath`] for
    /// variant-qualified provenance paths.
    pub fn parse(
        s: &str,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<Self, PropertyPathError> {
        let (prim, property) = Self::split(s)?;
        let prim_path = Path::parse_absolute(prim, tokens)?;
        Ok(Self {
            prim_path: paths.intern(prim_path),
            property: tokens.intern(property),
        })
    }

    /// Splits `s` into its prim path text and property name, checking
    /// everything [`Self::parse`] does except that the prim path text
    /// parses as an absolute prim path ([`Path::parse_absolute`]), and
    /// interning nothing.
    ///
    /// A reader of many properties of one prim can check that prim's path
    /// once.
    pub fn split(s: &str) -> Result<(&str, &str), PropertyPathError> {
        let Some(dot_pos) = s.rfind('.') else {
            return Err(PropertyPathError::MissingProperty);
        };
        let prim = &s[..dot_pos];
        let property = &s[dot_pos + 1..];
        if property.is_empty() {
            return Err(PropertyPathError::EmptyPropertyName);
        }
        if property.contains('/')
            || property.contains('{')
            || property.contains('}')
            || property.contains('.')
        {
            return Err(PropertyPathError::InvalidPropertyName);
        }
        if prim.contains('{') || prim.contains('}') {
            return Err(PropertyPathError::VariantSelectionNotAllowed);
        }
        if prim == "/" {
            return Err(PropertyPathError::RootPrimNotAllowed);
        }
        if prim.contains('.') {
            return Err(PropertyPathError::InvalidPrimPath);
        }
        Ok((prim, property))
    }

    /// Returns the concrete prim path.
    #[must_use]
    pub const fn prim_path(self) -> PathId {
        self.prim_path
    }

    /// Returns the property token.
    #[must_use]
    pub const fn property(self) -> TokenId {
        self.property
    }

    /// Formats this property path as `/Prim.attrName`.
    #[must_use]
    pub fn display(self, paths: &PathInterner, tokens: &TokenInterner) -> String {
        let mut out = paths.display(self.prim_path, tokens);
        out.push('.');
        out.push_str(tokens.resolve(self.property));
        out
    }
}

/// A concrete relationship or connection target path.
///
/// Target paths may point at a prim or at a property on a prim. They are
/// concrete scene identities, not provenance-bearing spec paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TargetPath {
    /// A prim target such as `/World/Looks/Metal`.
    Prim(PathId),
    /// A property target such as `/World/Looks/Metal.outputs:surface`.
    Property(PropertyPath),
}

impl TargetPath {
    /// Builds a prim target path from an interned prim path.
    #[must_use]
    pub const fn prim(path: PathId) -> Self {
        Self::Prim(path)
    }

    /// Builds a property target path from a concrete [`PropertyPath`].
    #[must_use]
    pub const fn property(path: PropertyPath) -> Self {
        Self::Property(path)
    }

    /// Parses a concrete target path such as `/Prim` or `/Prim.attrName`.
    pub fn parse(
        s: &str,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<Self, TargetPathError> {
        if s.contains('.') {
            return PropertyPath::parse(s, tokens, paths)
                .map(Self::Property)
                .map_err(TargetPathError::Property);
        }
        Path::parse_absolute(s, tokens)
            .map(|path| Self::Prim(paths.intern(path)))
            .map_err(TargetPathError::Prim)
    }

    /// Returns the concrete prim path targeted by this path.
    #[must_use]
    pub const fn prim_path(self) -> PathId {
        match self {
            Self::Prim(path) => path,
            Self::Property(path) => path.prim_path(),
        }
    }

    /// Returns the targeted property path, if this is a property target.
    #[must_use]
    pub const fn property_path(self) -> Option<PropertyPath> {
        match self {
            Self::Prim(_) => None,
            Self::Property(path) => Some(path),
        }
    }

    /// Formats this target path as a concrete prim or property path string.
    #[must_use]
    pub fn display(self, paths: &PathInterner, tokens: &TokenInterner) -> String {
        match self {
            Self::Prim(path) => paths.display(path, tokens),
            Self::Property(path) => path.display(paths, tokens),
        }
    }
}

/// A segmented absolute path.
///
/// v0.1 supports prim-style absolute paths like `/A/B/C`.
///
/// Paths compare by their segments' [`TokenId`]s, an order for maps and
/// sets; [`Path::cmp_with_tokens`] is namespace order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[doc(alias = "SdfPath")]
pub struct Path {
    segments: Box<[TokenId]>,
}

impl Borrow<[TokenId]> for Path {
    fn borrow(&self) -> &[TokenId] {
        &self.segments
    }
}

impl Path {
    /// Returns the root path (`/`).
    #[must_use]
    pub fn root() -> Self {
        Self {
            segments: Vec::new().into_boxed_slice(),
        }
    }

    /// Parses an absolute path and interns each segment as a token.
    ///
    /// ```
    /// use layerstack::{Path, PathError, TokenInterner};
    ///
    /// let tokens = TokenInterner::default();
    ///
    /// let path = Path::parse_absolute("/A/B", &tokens).unwrap();
    /// assert_eq!(path.depth(), 2);
    /// assert_eq!(path.display(&tokens), "/A/B");
    ///
    /// assert_eq!(Path::parse_absolute("relative", &tokens), Err(PathError::NotAbsolute));
    /// ```
    pub fn parse_absolute(s: &str, tokens: &TokenInterner) -> Result<Self, PathError> {
        if !s.starts_with('/') {
            return Err(PathError::NotAbsolute);
        }
        if s == "/" {
            return Ok(Self::root());
        }

        let mut segments = Vec::new();
        for seg in s.split('/').skip(1) {
            if seg.is_empty() {
                return Err(PathError::EmptySegment);
            }
            segments.push(tokens.intern(seg));
        }
        Ok(Self {
            segments: segments.into_boxed_slice(),
        })
    }

    /// Returns the namespace depth.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.segments.len()
    }

    /// Returns the parent path, or `None` if this is the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.segments.is_empty() {
            return None;
        }
        Some(Self {
            segments: self.segments[..self.segments.len() - 1]
                .to_vec()
                .into_boxed_slice(),
        })
    }

    /// Returns `true` if `self` is a prefix of `other`.
    #[must_use]
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        other.segments.starts_with(&self.segments)
    }

    /// Strips `prefix` from `self`, returning the remainder segments if `prefix` matches.
    #[must_use]
    pub fn strip_prefix(&self, prefix: &Self) -> Option<&[TokenId]> {
        if !prefix.is_prefix_of(self) {
            return None;
        }
        Some(&self.segments[prefix.segments.len()..])
    }

    /// Joins additional segments onto this path.
    #[must_use]
    pub fn join(&self, extra: &[TokenId]) -> Self {
        let mut out = Vec::with_capacity(self.segments.len() + extra.len());
        out.extend_from_slice(&self.segments);
        out.extend_from_slice(extra);
        Self {
            segments: out.into_boxed_slice(),
        }
    }

    /// Returns the leaf name segment, if any.
    #[must_use]
    pub fn leaf(&self) -> Option<TokenId> {
        self.segments.last().copied()
    }

    /// Returns the raw namespace segments.
    #[must_use]
    pub fn segments(&self) -> &[TokenId] {
        &self.segments
    }

    /// Formats this path as a string (e.g. `/Robot/Arm`).
    ///
    /// Requires a [`TokenInterner`] to resolve segment names.
    #[must_use]
    pub fn display(&self, tokens: &TokenInterner) -> String {
        if self.segments.is_empty() {
            return String::from("/");
        }
        let mut out = String::new();
        for &seg in self.segments.iter() {
            out.push('/');
            out.push_str(tokens.resolve(seg));
        }
        out
    }

    /// Compares paths using AOUSD-style namespace ordering.
    ///
    /// This compares each segment lexicographically by its resolved token
    /// string, and breaks ties by segment count.
    ///
    /// Spec: AOUSD Core §8 (paths and namespace ordering).
    #[must_use]
    pub fn cmp_with_tokens(&self, other: &Self, tokens: &TokenInterner) -> Ordering {
        for (a, b) in self
            .segments
            .iter()
            .copied()
            .zip(other.segments.iter().copied())
        {
            let seg = tokens.resolve(a).cmp(tokens.resolve(b));
            if seg != Ordering::Equal {
                return seg;
            }
        }
        self.segments.len().cmp(&other.segments.len())
    }
}

/// Interns [`Path`] values to stable [`PathId`]s.
///
/// Interning takes a shared reference, so threads can intern into one
/// interner at once (with the `std` feature, which makes it [`Sync`]). Ids
/// number paths in the order they are first interned; paths interned by
/// several threads at once take ids in whichever order the threads get
/// there, so ordering by id is not namespace order
/// ([`Path::cmp_with_tokens`] is).
///
/// ```
/// use layerstack::{Path, PathInterner, TokenInterner};
///
/// let tokens = TokenInterner::default();
/// let paths = PathInterner::default();
///
/// let path = Path::parse_absolute("/Robot/Arm", &tokens).unwrap();
/// let id = paths.intern(path);
/// assert_eq!(paths.display(id, &tokens), "/Robot/Arm");
/// ```
#[derive(Debug, Default)]
pub struct PathInterner {
    pub(crate) affinity: Arc<()>,
    paths: Interned<PathEntry>,
}

#[cfg(test)]
impl PathInterner {
    /// How many paths are interned.
    pub(crate) fn len(&self) -> u32 {
        self.paths.len()
    }
}

/// An interned path and its parent's id, `None` for the root: interning a
/// path interns its ancestors first, so a parent is read, not looked up.
/// Found by its segments.
#[derive(Clone, Debug)]
struct PathEntry {
    path: Path,
    parent: Option<PathId>,
}

impl PartialEq for PathEntry {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for PathEntry {}

impl core::hash::Hash for PathEntry {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.path.segments().hash(state);
    }
}

impl Borrow<[TokenId]> for PathEntry {
    fn borrow(&self) -> &[TokenId] {
        self.path.segments()
    }
}

impl PathInterner {
    pub(crate) fn snapshot(&self) -> Self {
        Self {
            affinity: Arc::default(),
            paths: self.paths.snapshot(),
        }
    }

    /// Interns a path, and its ancestors, returning a stable [`PathId`].
    #[must_use]
    pub fn intern(&self, path: Path) -> PathId {
        if let Some(id) = self.lookup_segments(path.segments()) {
            return id;
        }
        let parent = self.intern_parent(path.segments());
        let key = Segments::from_slice(path.segments());
        PathId(self.paths.intern(&key[..], || PathEntry { path, parent }).0)
    }

    /// Interns the parent of the path made of `segments`: `None` for the
    /// root.
    fn intern_parent(&self, segments: &[TokenId]) -> Option<PathId> {
        let (_, parent) = segments.split_last()?;
        Some(self.intern_segments(parent))
    }

    fn entry(&self, id: PathId) -> &PathEntry {
        self.paths.get(id.0).expect("path id out of range")
    }

    /// Resolves a [`PathId`] back to a [`Path`].
    #[must_use]
    pub fn resolve(&self, id: PathId) -> &Path {
        &self.entry(id).path
    }

    /// Looks up a path without interning. Returns `None` if the path hasn't been interned.
    #[must_use]
    pub fn lookup(&self, path: &Path) -> Option<PathId> {
        self.lookup_segments(path.segments())
    }

    /// Looks up the path made of `segments` without interning or allocating.
    #[must_use]
    pub(crate) fn lookup_segments(&self, segments: &[TokenId]) -> Option<PathId> {
        self.paths.find(segments).map(PathId)
    }

    /// Interns the path made of `segments`, allocating only for a path not
    /// interned yet.
    pub(crate) fn intern_segments(&self, segments: &[TokenId]) -> PathId {
        if let Some(id) = self.lookup_segments(segments) {
            return id;
        }
        let parent = self.intern_parent(segments);
        let make = || PathEntry {
            path: Path {
                segments: segments.into(),
            },
            parent,
        };
        PathId(self.paths.intern(segments, make).0)
    }

    fn joined(&self, base: PathId, extra: &[TokenId]) -> Segments {
        let mut segments = Segments::from_slice(self.resolve(base).segments());
        segments.extend_from_slice(extra);
        segments
    }

    /// Looks up `base` joined with `extra` (see [`Path::join`]) without
    /// interning or allocating.
    #[must_use]
    pub(crate) fn lookup_joined(&self, base: PathId, extra: &[TokenId]) -> Option<PathId> {
        if extra.is_empty() {
            return Some(base);
        }
        self.lookup_segments(&self.joined(base, extra))
    }

    /// Interns `base` joined with `extra` (see [`Path::join`]).
    pub(crate) fn intern_joined(&self, base: PathId, extra: &[TokenId]) -> PathId {
        if extra.is_empty() {
            return base;
        }
        let segments = self.joined(base, extra);
        self.intern_segments(&segments)
    }

    /// Interns the path of `parent`'s child prim `name`, as
    /// [`Self::intern`] interns `parent`'s path joined with `name`, without
    /// building that path first.
    ///
    /// ```
    /// use layerstack::{Path, PathInterner, TokenInterner};
    ///
    /// let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
    /// let a = paths.intern(Path::parse_absolute("/A", &tokens).unwrap());
    /// let b = paths.intern_child(a, tokens.intern("B"));
    /// assert_eq!(b, paths.intern(Path::parse_absolute("/A/B", &tokens).unwrap()));
    /// ```
    #[must_use]
    pub fn intern_child(&self, parent: PathId, name: TokenId) -> PathId {
        // The parent is interned: looked up once, not again as an ancestor.
        let segments = self.joined(parent, &[name]);
        let make = || PathEntry {
            path: Path {
                segments: segments[..].into(),
            },
            parent: Some(parent),
        };
        PathId(self.paths.intern(&segments[..], make).0)
    }

    /// Interns `path` moved from beneath `from` to beneath `to`: `to` itself
    /// for `from`, and `None` for a path outside `from`.
    pub(crate) fn intern_reparented(
        &self,
        path: PathId,
        from: PathId,
        to: PathId,
    ) -> Option<PathId> {
        if path == from {
            return Some(to);
        }
        let rel = self.resolve(path).strip_prefix(self.resolve(from))?;
        let segments = self.joined(to, rel);
        Some(self.intern_segments(&segments))
    }

    /// Returns the namespace parent of `id`, `None` for the root.
    ///
    /// Interning a path interns its ancestors too, so the parent is always
    /// interned, and reading it takes no lookup. OpenUSD: `SdfPath`'s
    /// parent node.
    #[must_use]
    pub fn parent(&self, id: PathId) -> Option<PathId> {
        self.entry(id).parent
    }

    /// Compares the paths `a` and `b` in namespace order
    /// ([`Path::cmp_with_tokens`]): the order to report paths in, as their
    /// ids follow interning order.
    ///
    /// OpenUSD: `SdfPath::operator<`.
    #[must_use]
    pub fn cmp_paths(&self, a: PathId, b: PathId, tokens: &TokenInterner) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        self.resolve(a).cmp_with_tokens(self.resolve(b), tokens)
    }

    /// Sorts `ids` in namespace order ([`Self::cmp_paths`]) and removes
    /// duplicates.
    pub fn sort_paths(&self, ids: &mut Vec<PathId>, tokens: &TokenInterner) {
        ids.sort_unstable_by(|a, b| self.cmp_paths(*a, *b, tokens));
        ids.dedup();
    }

    /// Formats a [`PathId`] as a string (e.g. `/Robot/Arm`).
    ///
    /// This is a convenience for `self.resolve(id).display(tokens)`.
    #[must_use]
    pub fn display(&self, id: PathId, tokens: &TokenInterner) -> String {
        self.resolve(id).display(tokens)
    }
}

/// Errors that can occur when parsing a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    /// Path is not absolute (doesn't start with `/`).
    NotAbsolute,
    /// Path contains an empty segment (e.g. `//`).
    EmptySegment,
    /// A segment is not a prim name, which is an identifier (`Rock`, not
    /// `.radius` or `Rock{shape=jagged}`). The checked store helpers
    /// ([`InMemoryStore::try_path`](crate::InMemoryStore::try_path)) check
    /// names; [`Path::parse_absolute`] does not.
    ///
    /// Spec: AOUSD Core §7.3.3 (`PrimName`), §8.5 (path grammar).
    InvalidPrimName,
}

impl core::fmt::Display for PathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::NotAbsolute => "the path is not absolute: it does not start with `/`",
            Self::EmptySegment => "the path has an empty segment",
            Self::InvalidPrimName => "a segment of the path is not a prim name (an identifier)",
        })
    }
}

impl core::error::Error for PathError {}

/// Errors that can occur when parsing a [`PropertyPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyPathError {
    /// Path is not absolute (doesn't start with `/`).
    NotAbsolute,
    /// Path contains an empty segment (e.g. `//`).
    EmptySegment,
    /// Property paths must target a concrete prim, not the pseudo-root.
    RootPrimNotAllowed,
    /// Property suffix was present but empty.
    EmptyPropertyName,
    /// Property separator `.` was missing.
    MissingProperty,
    /// A segment of the prim path is not a prim name: it holds a property
    /// separator, or, for the checked store helpers, is not an identifier.
    InvalidPrimPath,
    /// Property name was malformed for a concrete property path: it holds
    /// a path separator, or, for the checked store helpers, is not
    /// identifiers joined by `:`.
    InvalidPropertyName,
    /// Concrete property paths do not admit variant selections.
    VariantSelectionNotAllowed,
}

impl core::fmt::Display for PropertyPathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::NotAbsolute => "the path is not absolute: it does not start with `/`",
            Self::EmptySegment => "the path has an empty segment",
            Self::RootPrimNotAllowed => "a property path names a prim, not the pseudo-root `/`",
            Self::EmptyPropertyName => "the property name is empty",
            Self::MissingProperty => "the path has no property: `.` and a name are missing",
            Self::InvalidPrimPath => "a segment of the prim path is not a prim name",
            Self::InvalidPropertyName => "the property name is not a namespaced identifier",
            Self::VariantSelectionNotAllowed => {
                "a property path holds no variant selections; use a spec path"
            }
        })
    }
}

impl core::error::Error for PropertyPathError {}

/// Errors that can occur when parsing a [`TargetPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetPathError {
    /// The input was parsed as a prim target path and was invalid.
    Prim(PathError),
    /// The input was parsed as a property target path and was invalid.
    Property(PropertyPathError),
}

impl core::fmt::Display for TargetPathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Prim(error) => write!(f, "invalid prim target: {error}"),
            Self::Property(error) => write!(f, "invalid property target: {error}"),
        }
    }
}

impl core::error::Error for TargetPathError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Prim(error) => Some(error),
            Self::Property(error) => Some(error),
        }
    }
}

impl From<PathError> for PropertyPathError {
    fn from(value: PathError) -> Self {
        match value {
            PathError::NotAbsolute => Self::NotAbsolute,
            PathError::EmptySegment => Self::EmptySegment,
            PathError::InvalidPrimName => Self::InvalidPrimPath,
        }
    }
}

/// Checks that each segment of the absolute prim path text `s` is a prim
/// name, interning nothing. AOUSD Core §7.3.3 (`PrimName`), §8.5.
pub(crate) fn check_prim_names(s: &str) -> Result<(), PathError> {
    let Some(rest) = s.strip_prefix('/') else {
        return Err(PathError::NotAbsolute);
    };
    if rest.is_empty() {
        return Ok(());
    }
    for segment in rest.split('/') {
        if segment.is_empty() {
            return Err(PathError::EmptySegment);
        }
        if !crate::ident::is_identifier(segment) {
            return Err(PathError::InvalidPrimName);
        }
    }
    Ok(())
}

/// Whether `name` is a property name: identifiers joined by `:`.
/// AOUSD Core §7.3.3 (`PropertyName`), §8.5.
pub(crate) fn is_property_name(name: &str) -> bool {
    name.split(':').all(crate::ident::is_identifier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::TokenInterner;

    #[test]
    fn interning_a_path_interns_its_ancestors() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let child = paths.intern(Path::parse_absolute("/World/Leaf", &tokens).unwrap());
        let parent = paths
            .lookup(&Path::parse_absolute("/World", &tokens).unwrap())
            .expect("the parent is interned with its child");
        let root = paths.lookup(&Path::root()).expect("and the root");
        assert_eq!(paths.parent(child), Some(parent));
        assert_eq!(paths.parent(parent), Some(root));
        assert_eq!(paths.parent(root), None);
    }

    #[test]
    fn property_path_parse_round_trips() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let property = PropertyPath::parse("/World/Cube.visibility", &tokens, &paths).unwrap();
        assert_eq!(property.display(&paths, &tokens), "/World/Cube.visibility");
    }

    #[test]
    fn property_path_parse_rejects_missing_property() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube", &tokens, &paths),
            Err(PropertyPathError::MissingProperty)
        );
    }

    #[test]
    fn property_path_parse_rejects_empty_property() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube.", &tokens, &paths),
            Err(PropertyPathError::EmptyPropertyName)
        );
    }

    #[test]
    fn property_path_parse_rejects_variant_qualified_path() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube{lod=high}.visibility", &tokens, &paths),
            Err(PropertyPathError::VariantSelectionNotAllowed)
        );
    }

    #[test]
    fn property_path_parse_rejects_invalid_property_name() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube.visibility/child", &tokens, &paths),
            Err(PropertyPathError::InvalidPropertyName)
        );
    }

    #[test]
    fn property_path_parse_rejects_root_property() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/.visibility", &tokens, &paths),
            Err(PropertyPathError::RootPrimNotAllowed)
        );
    }

    #[test]
    fn property_path_parse_rejects_ambiguous_dotted_prim_path() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube.visibility.extra", &tokens, &paths),
            Err(PropertyPathError::InvalidPrimPath)
        );
    }

    #[test]
    fn target_path_parse_round_trips_prim_target() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let target = TargetPath::parse("/World/Cube", &tokens, &paths).unwrap();
        assert_eq!(target.display(&paths, &tokens), "/World/Cube");
        assert_eq!(target.property_path(), None);
    }

    #[test]
    fn target_path_parse_round_trips_property_target() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let target = TargetPath::parse("/World/Cube.visibility", &tokens, &paths).unwrap();
        assert_eq!(target.display(&paths, &tokens), "/World/Cube.visibility");
        assert!(target.property_path().is_some());
    }
}
