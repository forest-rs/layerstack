// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Variant-qualified spec paths used for composed opinion provenance.
//!
//! These paths are not the same as concrete prim namespace paths. They can
//! carry variant selections and property suffixes while still resolving to a
//! concrete authored prim site.
//!
//! Spec: AOUSD Core §8 (paths), §10.5 (variant selection), and the sparse array
//! edits proposal's discussion of sparse-composed `SdfPathExpression`
//! provenance.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::cmp::Ordering;

use crate::{
    interner::{TokenId, TokenInterner},
    path::{PathError, PathId, PathInterner, PropertyPath},
};

/// A variant selection applied at a specific prim host path.
///
/// Provenance needs the host path as well as the `(set, variant)` pair because
/// nested variant selections can apply at different prims, and repeated set
/// names can appear at different hosts along one composed source path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VariantSelectionSite {
    /// Prim path that owns the variant set.
    pub host_path: PathId,
    /// Variant set name.
    pub set: TokenId,
    /// Selected variant name.
    pub variant: TokenId,
}

/// A single spec-path component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SpecComponent {
    /// A concrete prim namespace segment.
    Prim(TokenId),
    /// A variant selection inserted after the preceding prim segment.
    VariantSelection {
        /// Variant set name.
        set: TokenId,
        /// Selected variant name.
        variant: TokenId,
    },
}

/// A variant-qualified spec path.
///
/// Unlike [`Path`], this can represent variant selections and property suffixes
/// while still preserving the concrete prim path used to look up authored data.
///
/// A spec path is its concrete prim path, the variant selections inserted
/// into it (see [`Self::variant_selection_sites`]) and its property suffix.
/// A plain prim spec path holds no selections; building one, or appending
/// prim names to any spec path, allocates nothing beyond interning the prim
/// path. Spec paths order as their components do (see [`Self::components`]).
///
/// [`Path`]: crate::Path
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpecPath {
    prim_path: PathId,
    /// The variant selections, outermost first, each with its host's
    /// namespace depth; `None` for none. Opinion keys clone source paths
    /// frequently, so they are shared.
    selections: Option<Arc<[(u32, VariantSelectionSite)]>>,
    property: PropertySuffix,
}

/// A spec path's property suffix: a token id, or none, in four bytes
/// rather than an `Option<TokenId>`'s eight, which every opinion key and
/// graph node holds a spec path for.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PropertySuffix(u32);

impl PropertySuffix {
    /// No property suffix: an id no interner hands out.
    const NONE: Self = Self(u32::MAX);

    const fn of(property: TokenId) -> Self {
        Self(property.get())
    }

    const fn get(self) -> Option<TokenId> {
        if self.0 == Self::NONE.0 {
            None
        } else {
            Some(TokenId::from_raw(self.0))
        }
    }
}

impl core::fmt::Debug for PropertySuffix {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.get().fmt(f)
    }
}

impl Ord for SpecPath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.prim_path
            .cmp(&other.prim_path)
            .then_with(|| cmp_selections(self.selections(), other.selections()))
            .then_with(|| self.property.get().cmp(&other.property.get()))
    }
}

impl PartialOrd for SpecPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Orders the selections of two spec paths of one prim path as their
/// components order: a prim name before a variant selection, and a path
/// before the longer paths it is a prefix of.
fn cmp_selections(
    a: &[(u32, VariantSelectionSite)],
    b: &[(u32, VariantSelectionSite)],
) -> Ordering {
    for (a, b) in a.iter().zip(b) {
        let (a_depth, a_site) = a;
        let (b_depth, b_site) = b;
        // The shallower selection meets a prim name of the other path.
        let order = b_depth
            .cmp(a_depth)
            .then_with(|| (a_site.set, a_site.variant).cmp(&(b_site.set, b_site.variant)));
        if order.is_ne() {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

impl SpecPath {
    /// Builds a plain prim spec path from a concrete prim path.
    #[must_use]
    pub const fn from_prim_path(prim_path: PathId) -> Self {
        Self {
            prim_path,
            selections: None,
            property: PropertySuffix::NONE,
        }
    }

    /// Builds a plain property spec path from a concrete [`PropertyPath`].
    #[must_use]
    pub fn from_property_path(property_path: PropertyPath) -> Self {
        Self::from_prim_path(property_path.prim_path()).with_property(property_path.property())
    }

    /// Builds a variant-qualified spec path by inserting `selections` after
    /// `variant_host` in `prim_path`.
    ///
    /// `variant_host` must be equal to or a prefix of `prim_path`.
    #[must_use]
    pub fn from_variant_qualified_prim_path(
        prim_path: PathId,
        variant_host: PathId,
        selections: &[(TokenId, TokenId)],
        paths: &PathInterner,
    ) -> Self {
        let sites: Vec<_> = selections
            .iter()
            .copied()
            .map(|(set, variant)| VariantSelectionSite {
                host_path: variant_host,
                set,
                variant,
            })
            .collect();
        Self::from_variant_selection_sites(prim_path, &sites, paths)
    }

    /// Builds a variant-qualified spec path from ordered host-aware selection
    /// sites.
    ///
    /// Each site host must be equal to or a prefix of `prim_path`, and the
    /// provided sites must appear in outer-to-inner order.
    #[must_use]
    pub fn from_variant_selection_sites(
        prim_path: PathId,
        selection_sites: &[VariantSelectionSite],
        paths: &PathInterner,
    ) -> Self {
        let prim = paths.resolve(prim_path);
        let mut depth = 0;
        let selections: Vec<_> = selection_sites
            .iter()
            .map(|site| {
                let host = paths.resolve(site.host_path);
                assert!(
                    host.depth() >= depth && host.depth() > 0 && host.is_prefix_of(prim),
                    "variant selection site host must be equal to or a prefix of the concrete prim path"
                );
                depth = host.depth();
                (u32::try_from(depth).expect("namespace depth"), *site)
            })
            .collect();
        Self {
            prim_path,
            selections: (!selections.is_empty()).then(|| selections.into()),
            property: PropertySuffix::NONE,
        }
    }

    /// Parses a spec path like `/A{v=red}B/C.attr`.
    pub fn parse(
        s: &str,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<Self, SpecPathError> {
        if !s.starts_with('/') {
            return Err(SpecPathError::NotAbsolute);
        }

        let mut selections = Vec::new();
        let mut prim_segments = Vec::new();
        let bytes = s.as_bytes();
        let mut idx = 1_usize;
        let len = bytes.len();
        let mut property = None;

        while idx < len {
            match bytes[idx] {
                b'/' => {
                    idx += 1;
                }
                b'{' => {
                    idx += 1;
                    let start = idx;
                    while idx < len && bytes[idx] != b'=' {
                        idx += 1;
                    }
                    if idx == len {
                        return Err(SpecPathError::MalformedVariantSelection);
                    }
                    let set = &s[start..idx];
                    idx += 1;
                    let variant_start = idx;
                    while idx < len && bytes[idx] != b'}' {
                        idx += 1;
                    }
                    if idx == len {
                        return Err(SpecPathError::MalformedVariantSelection);
                    }
                    let variant = &s[variant_start..idx];
                    idx += 1;
                    let site = VariantSelectionSite {
                        host_path: paths.intern_segments(&prim_segments),
                        set: tokens.intern(set),
                        variant: tokens.intern(variant),
                    };
                    let depth = u32::try_from(prim_segments.len()).expect("namespace depth");
                    selections.push((depth, site));
                }
                b'.' => {
                    if idx + 1 >= len {
                        return Err(SpecPathError::EmptyPropertyName);
                    }
                    property = Some(tokens.intern(&s[idx + 1..]));
                    break;
                }
                _ => {
                    let start = idx;
                    while idx < len {
                        match bytes[idx] {
                            b'/' | b'{' | b'.' => break,
                            _ => idx += 1,
                        }
                    }
                    let segment = &s[start..idx];
                    if segment.is_empty() {
                        return Err(SpecPathError::EmptySegment);
                    }
                    prim_segments.push(tokens.intern(segment));
                }
            }
        }

        let prim_path = paths.intern_segments(&prim_segments);

        Ok(Self {
            prim_path,
            selections: (!selections.is_empty()).then(|| selections.into()),
            property: property.map_or(PropertySuffix::NONE, PropertySuffix::of),
        })
    }

    /// Returns the concrete prim path used to look up authored `PrimSpec` data.
    #[must_use]
    pub const fn prim_path(&self) -> PathId {
        self.prim_path
    }

    /// Returns the property suffix, if any.
    #[must_use]
    pub const fn property(&self) -> Option<TokenId> {
        self.property.get()
    }

    fn selections(&self) -> &[(u32, VariantSelectionSite)] {
        self.selections.as_deref().unwrap_or_default()
    }

    /// Returns the variant selections inserted into the prim path, outermost
    /// first, each with the prim hosting its variant set: `/A{a=x}B{b=y}`
    /// has `a=x` at `/A` and `b=y` at `/A/B`. Each host is the prim path or
    /// one of its ancestors.
    pub fn variant_selection_sites(
        &self,
    ) -> impl ExactSizeIterator<Item = VariantSelectionSite> + '_ {
        self.selections().iter().map(|(_, site)| *site)
    }

    /// Returns the variant selections this path ends in, after its last
    /// prim name, outermost first: `[(a, x), (b, y)]` for `/P{a=x}{b=y}`
    /// (and for `/P{a=x}{b=y}.attr`), empty for `/P` or `/P{a=x}C`. They
    /// name a variant spec of the prim spec at [`SpecPath::prim_path`] (see
    /// [`crate::PrimSpec::variant_spec`]).
    #[must_use]
    pub fn variant_chain(&self) -> Vec<(TokenId, TokenId)> {
        self.variant_selection_sites()
            .filter(|site| site.host_path == self.prim_path)
            .map(|site| (site.set, site.variant))
            .collect()
    }

    /// The variant selection this path ends in, after its last prim name:
    /// the innermost of [`Self::variant_chain`].
    pub(crate) fn trailing_selection(&self) -> Option<VariantSelectionSite> {
        self.variant_selection_sites()
            .last()
            .filter(|site| site.host_path == self.prim_path)
    }

    /// Returns the structured components: each prim name of the prim path,
    /// with the variant selections after the names of their hosts.
    #[must_use]
    pub fn components(&self, paths: &PathInterner) -> Vec<SpecComponent> {
        let names = paths.resolve(self.prim_path).segments();
        let mut components = Vec::with_capacity(names.len() + self.selections().len());
        let mut selections = self.selections().iter().peekable();
        for depth in 0..=names.len() {
            while let Some((_, site)) = selections.next_if(|(at, _)| *at as usize == depth) {
                components.push(SpecComponent::VariantSelection {
                    set: site.set,
                    variant: site.variant,
                });
            }
            if let Some(name) = names.get(depth) {
                components.push(SpecComponent::Prim(*name));
            }
        }
        components
    }

    /// Whether this path has a variant selection: it names a spec inside an
    /// authored variant branch.
    #[must_use]
    pub fn has_variant_selections(&self) -> bool {
        self.selections.is_some()
    }

    /// Returns a copy of this path without its property suffix: the prim
    /// spec that authors the property.
    #[must_use]
    pub(crate) fn prim_spec(&self) -> Self {
        let mut out = self.clone();
        out.property = PropertySuffix::NONE;
        out
    }

    /// Returns the spec path of a child prim of this prim spec, whose
    /// concrete prim path is `child_prim_path`.
    #[must_use]
    pub(crate) fn child(&self, child_prim_path: PathId) -> Self {
        Self {
            prim_path: child_prim_path,
            selections: self.selections.clone(),
            property: PropertySuffix::NONE,
        }
    }

    /// The variant selections with the namespace depth of each one's host,
    /// outermost first.
    pub(crate) fn selection_entries(&self) -> &[(u32, VariantSelectionSite)] {
        self.selections()
    }

    /// This path cut back to its ancestor `prim_path`, `depth` prim names
    /// deep: the selections hosted beneath it are dropped.
    #[must_use]
    pub(crate) fn truncated_to(&self, prim_path: PathId, depth: usize) -> Self {
        let kept = self
            .selections()
            .iter()
            .take_while(|(host_depth, _)| *host_depth as usize <= depth)
            .count();
        let selections = match self.selections.as_ref() {
            Some(all) if kept == all.len() => Some(all.clone()),
            Some(all) if kept > 0 => Some(all[..kept].into()),
            _ => None,
        };
        Self {
            prim_path,
            selections,
            property: PropertySuffix::NONE,
        }
    }

    /// This prim spec path with the variant selection `site`, hosted on its
    /// prim at namespace depth `depth`, appended.
    #[must_use]
    pub(crate) fn with_trailing_selection(&self, site: VariantSelectionSite, depth: u32) -> Self {
        let mut selections: Vec<(u32, VariantSelectionSite)> = self.selections().to_vec();
        selections.push((depth, site));
        Self {
            prim_path: self.prim_path,
            selections: Some(selections.into()),
            property: PropertySuffix::NONE,
        }
    }

    /// This path with its prim path replaced by `prim_path` and the
    /// selections of `selections` (another path, whose prim path is
    /// `prim_path` or an ancestor of it).
    #[must_use]
    pub(crate) fn with_prim_and_selections(prim_path: PathId, selections: &Self) -> Self {
        Self {
            prim_path,
            selections: selections.selections.clone(),
            property: PropertySuffix::NONE,
        }
    }

    /// Returns this prim spec path with the prim names `names` appended:
    /// `/Rock{shape=jagged}` joined with `[Child]` is
    /// `/Rock{shape=jagged}Child`. The property suffix is kept.
    #[must_use]
    pub(crate) fn join_prims(&self, names: &[TokenId], paths: &PathInterner) -> Self {
        Self {
            prim_path: paths.intern_joined(self.prim_path, names),
            selections: self.selections.clone(),
            property: self.property,
        }
    }

    /// Returns a copy of this path with a property suffix attached.
    #[must_use]
    pub fn with_property(&self, property: TokenId) -> Self {
        let mut out = self.clone();
        out.property = PropertySuffix::of(property);
        out
    }

    /// Formats this path in AOUSD-style spec-path syntax.
    #[must_use]
    pub fn display(&self, paths: &PathInterner, tokens: &TokenInterner) -> String {
        let mut out = String::from("/");
        let mut need_slash = false;
        for component in self.components(paths) {
            match component {
                SpecComponent::Prim(segment) => {
                    if need_slash {
                        out.push('/');
                    }
                    out.push_str(tokens.resolve(segment));
                    need_slash = true;
                }
                SpecComponent::VariantSelection { set, variant } => {
                    out.push('{');
                    out.push_str(tokens.resolve(set));
                    out.push('=');
                    out.push_str(tokens.resolve(variant));
                    out.push('}');
                    need_slash = false;
                }
            }
        }
        if let Some(property) = self.property() {
            out.push('.');
            out.push_str(tokens.resolve(property));
        }
        out
    }
}

/// Errors that can occur while parsing a [`SpecPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpecPathError {
    /// Path is not absolute (doesn't start with `/`).
    NotAbsolute,
    /// Path contains an empty prim segment.
    EmptySegment,
    /// A variant selection was malformed.
    MalformedVariantSelection,
    /// Property suffix was present but empty.
    EmptyPropertyName,
    /// Underlying concrete prim path was invalid.
    InvalidPrimPath(PathError),
    /// A property on the pseudo-root (`/.radius`), which holds none. The
    /// checked store helper
    /// ([`InMemoryStore::try_spec_path`](crate::InMemoryStore::try_spec_path))
    /// checks this; [`SpecPath::parse`] does not.
    RootPrimNotAllowed,
    /// The property name is not identifiers joined by `:`. Checked by the
    /// store helper only, as for [`Self::RootPrimNotAllowed`].
    ///
    /// Spec: AOUSD Core §7.3.3 (`PropertyName`).
    InvalidPropertyName,
}

impl From<PathError> for SpecPathError {
    fn from(value: PathError) -> Self {
        Self::InvalidPrimPath(value)
    }
}

impl core::fmt::Display for SpecPathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAbsolute => {
                f.write_str("the path is not absolute: it does not start with `/`")
            }
            Self::EmptySegment => f.write_str("the path has an empty segment"),
            Self::MalformedVariantSelection => {
                f.write_str("a variant selection is not `{set=variant}`")
            }
            Self::EmptyPropertyName => f.write_str("the property name is empty"),
            Self::InvalidPrimPath(error) => write!(f, "invalid prim path: {error}"),
            Self::RootPrimNotAllowed => {
                f.write_str("a property path names a prim, not the pseudo-root `/`")
            }
            Self::InvalidPropertyName => {
                f.write_str("the property name is not a namespaced identifier")
            }
        }
    }
}

impl core::error::Error for SpecPathError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::InvalidPrimPath(error) => Some(error),
            _ => None,
        }
    }
}

/// Checks the names of the spec path text `s` as [`SpecPath::parse`] reads
/// it: each prim segment a prim name, each variant set name an identifier,
/// and a property name on a prim, not the pseudo-root. Interns nothing.
///
/// Spec: AOUSD Core §7.3.3 (names), §8.5 (path grammar).
pub(crate) fn check_names(s: &str) -> Result<(), SpecPathError> {
    use crate::ident::is_identifier;
    let Some(rest) = s.strip_prefix('/') else {
        return Err(SpecPathError::NotAbsolute);
    };
    let mut prims = 0_usize;
    let mut rest = rest;
    while let Some(next) = rest.chars().next() {
        match next {
            '/' => return Err(SpecPathError::EmptySegment),
            '{' => {
                let close = rest
                    .find('}')
                    .ok_or(SpecPathError::MalformedVariantSelection)?;
                let (set, _) = rest[1..close]
                    .split_once('=')
                    .ok_or(SpecPathError::MalformedVariantSelection)?;
                if !is_identifier(set) {
                    return Err(SpecPathError::MalformedVariantSelection);
                }
                rest = &rest[close + 1..];
                if let Some(after) = rest.strip_prefix('/') {
                    rest = after;
                }
            }
            '.' => {
                let property = &rest[1..];
                if property.is_empty() {
                    return Err(SpecPathError::EmptyPropertyName);
                }
                if prims == 0 {
                    return Err(SpecPathError::RootPrimNotAllowed);
                }
                if !crate::path::is_property_name(property) {
                    return Err(SpecPathError::InvalidPropertyName);
                }
                return Ok(());
            }
            _ => {
                let end = rest.find(['/', '{', '.']).unwrap_or(rest.len());
                if !is_identifier(&rest[..end]) {
                    return Err(SpecPathError::InvalidPrimPath(PathError::InvalidPrimName));
                }
                prims += 1;
                rest = &rest[end..];
                if let Some(after) = rest.strip_prefix('/') {
                    if after.is_empty() {
                        return Err(SpecPathError::EmptySegment);
                    }
                    rest = after;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::TokenInterner;
    use crate::path::Path;

    #[test]
    fn from_variant_qualified_prim_path_formats_expected_string() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let host = paths.intern(Path::parse_absolute("/Sarah", &tokens).expect("path"));
        let concrete =
            paths.intern(Path::parse_absolute("/Sarah/FaceRig/EyesRig", &tokens).expect("path"));
        let selection = (tokens.intern("modelComplexity"), tokens.intern("full"));

        let spec = SpecPath::from_variant_qualified_prim_path(concrete, host, &[selection], &paths);

        assert_eq!(
            spec.display(&paths, &tokens),
            "/Sarah{modelComplexity=full}FaceRig/EyesRig"
        );
        assert_eq!(spec.prim_path(), concrete);
    }

    #[test]
    fn from_variant_selection_sites_supports_multiple_hosts() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let foo = paths.intern(Path::parse_absolute("/Foo", &tokens).expect("path"));
        let foo_a_number =
            paths.intern(Path::parse_absolute("/Foo/A/Number", &tokens).expect("path"));
        let which = tokens.intern("which");
        let a = tokens.intern("A");
        let count = tokens.intern("count");
        let one = tokens.intern("one");

        let spec = SpecPath::from_variant_selection_sites(
            foo_a_number,
            &[
                VariantSelectionSite {
                    host_path: foo,
                    set: which,
                    variant: a,
                },
                VariantSelectionSite {
                    host_path: foo_a_number,
                    set: count,
                    variant: one,
                },
            ],
            &paths,
        );

        assert_eq!(
            spec.display(&paths, &tokens),
            "/Foo{which=A}A/Number{count=one}"
        );
    }

    #[test]
    fn from_variant_selection_sites_supports_repeated_set_names_on_same_host() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let host =
            paths.intern(Path::parse_absolute("/DirectlyNestedVariants", &tokens).expect("path"));
        let concrete = paths.intern(
            Path::parse_absolute("/DirectlyNestedVariants/anim_spooky_anim_sphere", &tokens)
                .expect("path"),
        );
        let standin = tokens.intern("standin");
        let shading = tokens.intern("shadingVariant");
        let anim = tokens.intern("anim");
        let spooky = tokens.intern("spooky");

        let spec = SpecPath::from_variant_selection_sites(
            concrete,
            &[
                VariantSelectionSite {
                    host_path: host,
                    set: standin,
                    variant: anim,
                },
                VariantSelectionSite {
                    host_path: host,
                    set: shading,
                    variant: spooky,
                },
                VariantSelectionSite {
                    host_path: host,
                    set: standin,
                    variant: anim,
                },
            ],
            &paths,
        );

        assert_eq!(
            spec.display(&paths, &tokens),
            "/DirectlyNestedVariants{standin=anim}{shadingVariant=spooky}{standin=anim}anim_spooky_anim_sphere"
        );
    }

    #[test]
    fn parse_round_trips_variant_property_path() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();

        let spec = SpecPath::parse("/A{nestedVariantSet=nestedVariant}.test", &tokens, &paths)
            .expect("spec path");

        assert_eq!(
            spec.display(&paths, &tokens),
            "/A{nestedVariantSet=nestedVariant}.test"
        );
        let prim = paths.resolve(spec.prim_path());
        assert_eq!(prim.display(&tokens), "/A");
    }

    #[test]
    fn from_property_path_preserves_plain_property_identity() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let property =
            PropertyPath::parse("/Robot.visibility", &tokens, &paths).expect("property path");

        let spec = SpecPath::from_property_path(property);

        assert_eq!(spec.display(&paths, &tokens), "/Robot.visibility");
        assert_eq!(spec.prim_path(), property.prim_path());
    }
    #[test]
    fn spec_paths_order_as_their_components() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let specs = [
            "/A/B/C",
            "/A/B/C.attr",
            "/A{v=x}B/C",
            "/A{v=y}B/C",
            "/A{w=x}B/C",
            "/A{v=x}{w=y}B/C",
            "/A/B{v=x}C",
            "/A/B/C{v=x}",
            "/A/B/C{v=x}{w=y}",
            "/A{v=x}B{w=y}C",
            "/A{v=x}B/C{w=y}",
            "/A{v=x}B/C{w=y}.attr",
        ]
        .map(|s| SpecPath::parse(s, &tokens, &paths).expect("spec path"));
        for a in &specs {
            for b in &specs {
                let by_components = (a.prim_path(), a.components(&paths), a.property()).cmp(&(
                    b.prim_path(),
                    b.components(&paths),
                    b.property(),
                ));
                assert_eq!(
                    a.cmp(b),
                    by_components,
                    "{} vs {}",
                    a.display(&paths, &tokens),
                    b.display(&paths, &tokens)
                );
                assert_eq!(a == b, by_components.is_eq());
            }
        }
    }

    /// The packed property suffix reads back as the `Option` it holds,
    /// the empty token included, and prints as one.
    #[test]
    fn property_suffixes_round_trip() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let prim = SpecPath::parse("/Rock{shape=jagged}", &tokens, &paths).expect("spec");
        assert_eq!(prim.property(), None);
        for name in [tokens.intern("size"), TokenId::EMPTY] {
            let property = prim.with_property(name);
            assert_eq!(property.property(), Some(name));
            assert_eq!(property.prim_spec(), prim);
            assert!(alloc::format!("{property:?}").contains(&alloc::format!("Some({name:?})")));
        }
        assert!(alloc::format!("{prim:?}").contains("property: None"));
    }

    #[test]
    fn appending_prims_keeps_the_selections() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let spec = SpecPath::parse("/Rock{shape=jagged}", &tokens, &paths).expect("spec");
        let child = tokens.intern("Child");
        let joined = spec.join_prims(&[child], &paths);
        assert_eq!(joined.display(&paths, &tokens), "/Rock{shape=jagged}Child");
        assert_eq!(
            joined,
            SpecPath::parse("/Rock{shape=jagged}Child", &tokens, &paths).expect("spec")
        );
        assert!(joined.variant_chain().is_empty());
        assert_eq!(
            spec.variant_chain(),
            [(tokens.intern("shape"), tokens.intern("jagged"))]
        );
    }
}
