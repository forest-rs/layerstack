//! Paths for `layerstack`.
//!
//! `layerstack` uses segmented, interned paths (similar to `OpenUSD` prim paths).
//!
//! Spec: AOUSD Core §8 (paths and namespace ordering).

use alloc::{boxed::Box, vec::Vec};

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
}

/// Errors that can occur when parsing a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    /// Path is not absolute (doesn't start with `/`).
    NotAbsolute,
    /// Path contains an empty segment (e.g. `//`).
    EmptySegment,
}
