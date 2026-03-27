//! Paths for `layerstack`.
//!
//! `layerstack` uses segmented, interned paths (similar to `OpenUSD` prim paths).
//!
//! Spec: AOUSD Core §8 (paths and namespace ordering).

use alloc::{boxed::Box, string::String, vec::Vec};

use core::cmp::Ordering;

use crate::interner::{TokenId, TokenInterner};
use hashbrown::HashMap;

/// A stable identifier for an interned path.
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
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<Self, PropertyPathError> {
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
        let prim_path = Path::parse_absolute(prim, tokens)?;
        Ok(Self {
            prim_path: paths.intern(prim_path),
            property: tokens.intern(property),
        })
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
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Path {
    segments: Box<[TokenId]>,
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
    pub fn parse_absolute(s: &str, tokens: &mut TokenInterner) -> Result<Self, PathError> {
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
#[derive(Debug, Default)]
pub struct PathInterner {
    by_path: HashMap<Path, PathId>,
    paths: Vec<Path>,
}

impl PathInterner {
    /// Interns a path, returning a stable [`PathId`].
    #[must_use]
    pub fn intern(&mut self, path: Path) -> PathId {
        if let Some(id) = self.by_path.get(&path) {
            return *id;
        }
        let id = PathId(u32::try_from(self.paths.len()).expect("path interner overflow"));
        self.paths.push(path.clone());
        self.by_path.insert(path, id);
        id
    }

    /// Resolves a [`PathId`] back to a [`Path`].
    #[must_use]
    pub fn resolve(&self, id: PathId) -> &Path {
        &self.paths[usize::try_from(id.0).expect("path id out of range")]
    }

    /// Looks up a path without interning. Returns `None` if the path hasn't been interned.
    #[must_use]
    pub fn lookup(&self, path: &Path) -> Option<PathId> {
        self.by_path.get(path).copied()
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
}

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
    /// Concrete prim paths do not admit property separators in namespace segments.
    InvalidPrimPath,
    /// Property name was malformed for a concrete property path.
    InvalidPropertyName,
    /// Concrete property paths do not admit variant selections.
    VariantSelectionNotAllowed,
}

/// Errors that can occur when parsing a [`TargetPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetPathError {
    /// The input was parsed as a prim target path and was invalid.
    Prim(PathError),
    /// The input was parsed as a property target path and was invalid.
    Property(PropertyPathError),
}

impl From<PathError> for PropertyPathError {
    fn from(value: PathError) -> Self {
        match value {
            PathError::NotAbsolute => Self::NotAbsolute,
            PathError::EmptySegment => Self::EmptySegment,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::TokenInterner;

    #[test]
    fn property_path_parse_round_trips() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let property =
            PropertyPath::parse("/World/Cube.visibility", &mut tokens, &mut paths).unwrap();
        assert_eq!(property.display(&paths, &tokens), "/World/Cube.visibility");
    }

    #[test]
    fn property_path_parse_rejects_missing_property() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube", &mut tokens, &mut paths),
            Err(PropertyPathError::MissingProperty)
        );
    }

    #[test]
    fn property_path_parse_rejects_empty_property() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube.", &mut tokens, &mut paths),
            Err(PropertyPathError::EmptyPropertyName)
        );
    }

    #[test]
    fn property_path_parse_rejects_variant_qualified_path() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube{lod=high}.visibility", &mut tokens, &mut paths),
            Err(PropertyPathError::VariantSelectionNotAllowed)
        );
    }

    #[test]
    fn property_path_parse_rejects_invalid_property_name() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube.visibility/child", &mut tokens, &mut paths),
            Err(PropertyPathError::InvalidPropertyName)
        );
    }

    #[test]
    fn property_path_parse_rejects_root_property() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/.visibility", &mut tokens, &mut paths),
            Err(PropertyPathError::RootPrimNotAllowed)
        );
    }

    #[test]
    fn property_path_parse_rejects_ambiguous_dotted_prim_path() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        assert_eq!(
            PropertyPath::parse("/World/Cube.visibility.extra", &mut tokens, &mut paths),
            Err(PropertyPathError::InvalidPrimPath)
        );
    }

    #[test]
    fn target_path_parse_round_trips_prim_target() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let target = TargetPath::parse("/World/Cube", &mut tokens, &mut paths).unwrap();
        assert_eq!(target.display(&paths, &tokens), "/World/Cube");
        assert_eq!(target.property_path(), None);
    }

    #[test]
    fn target_path_parse_round_trips_property_target() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let target = TargetPath::parse("/World/Cube.visibility", &mut tokens, &mut paths).unwrap();
        assert_eq!(target.display(&paths, &tokens), "/World/Cube.visibility");
        assert!(target.property_path().is_some());
    }
}
