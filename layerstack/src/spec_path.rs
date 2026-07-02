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

use alloc::{boxed::Box, string::String, vec::Vec};

use crate::{
    interner::{TokenId, TokenInterner},
    path::{Path, PathError, PathId, PathInterner, PropertyPath},
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpecPath {
    prim_path: PathId,
    components: Box<[SpecComponent]>,
    property: Option<TokenId>,
}

impl SpecPath {
    /// Builds a plain prim spec path from a concrete prim path.
    #[must_use]
    pub fn from_prim_path(prim_path: PathId, paths: &PathInterner) -> Self {
        let components = paths
            .resolve(prim_path)
            .segments()
            .iter()
            .copied()
            .map(SpecComponent::Prim)
            .collect();
        Self {
            prim_path,
            components,
            property: None,
        }
    }

    /// Builds a plain property spec path from a concrete [`PropertyPath`].
    #[must_use]
    pub fn from_property_path(property_path: PropertyPath, paths: &PathInterner) -> Self {
        Self::from_prim_path(property_path.prim_path(), paths)
            .with_property(property_path.property())
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
        let mut components = Vec::new();
        let mut emitted_sites = 0_usize;
        let mut prefix_segments = Vec::new();

        for depth in 0..prim.depth() {
            let segment = prim.segments()[depth];
            prefix_segments.push(segment);
            let prefix_path = Path::root().join(&prefix_segments);
            components.push(SpecComponent::Prim(segment));

            while emitted_sites < selection_sites.len()
                && paths.resolve(selection_sites[emitted_sites].host_path) == &prefix_path
            {
                let site = selection_sites[emitted_sites];
                components.push(SpecComponent::VariantSelection {
                    set: site.set,
                    variant: site.variant,
                });
                emitted_sites += 1;
            }
        }

        assert!(
            emitted_sites == selection_sites.len(),
            "variant selection site host must be equal to or a prefix of the concrete prim path"
        );

        Self {
            prim_path,
            components: components.into_boxed_slice(),
            property: None,
        }
    }

    /// Parses a spec path like `/A{v=red}B/C.attr`.
    pub fn parse(
        s: &str,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<Self, SpecPathError> {
        if !s.starts_with('/') {
            return Err(SpecPathError::NotAbsolute);
        }

        let mut components = Vec::new();
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
                    components.push(SpecComponent::VariantSelection {
                        set: tokens.intern(set),
                        variant: tokens.intern(variant),
                    });
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
                    let token = tokens.intern(segment);
                    prim_segments.push(token);
                    components.push(SpecComponent::Prim(token));
                }
            }
        }

        let prim_path = if prim_segments.is_empty() {
            paths.intern(Path::root())
        } else {
            paths.intern(Path::root().join(&prim_segments))
        };

        Ok(Self {
            prim_path,
            components: components.into_boxed_slice(),
            property,
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
        self.property
    }

    /// Returns the structured components.
    #[must_use]
    pub fn components(&self) -> &[SpecComponent] {
        &self.components
    }

    /// Returns a copy of this path with a property suffix attached.
    #[must_use]
    pub fn with_property(&self, property: TokenId) -> Self {
        let mut out = self.clone();
        out.property = Some(property);
        out
    }

    /// Formats this path in AOUSD-style spec-path syntax.
    #[must_use]
    pub fn display(&self, tokens: &TokenInterner) -> String {
        let mut out = String::from("/");
        let mut need_slash = false;
        for component in self.components.iter().copied() {
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
        if let Some(property) = self.property {
            out.push('.');
            out.push_str(tokens.resolve(property));
        }
        out
    }
}

/// Errors that can occur while parsing a [`SpecPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl From<PathError> for SpecPathError {
    fn from(value: PathError) -> Self {
        Self::InvalidPrimPath(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::TokenInterner;

    #[test]
    fn from_variant_qualified_prim_path_formats_expected_string() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let host = paths.intern(Path::parse_absolute("/Sarah", &mut tokens).expect("path"));
        let concrete = paths
            .intern(Path::parse_absolute("/Sarah/FaceRig/EyesRig", &mut tokens).expect("path"));
        let selection = (tokens.intern("modelComplexity"), tokens.intern("full"));

        let spec = SpecPath::from_variant_qualified_prim_path(concrete, host, &[selection], &paths);

        assert_eq!(
            spec.display(&tokens),
            "/Sarah{modelComplexity=full}FaceRig/EyesRig"
        );
        assert_eq!(spec.prim_path(), concrete);
    }

    #[test]
    fn from_variant_selection_sites_supports_multiple_hosts() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let foo = paths.intern(Path::parse_absolute("/Foo", &mut tokens).expect("path"));
        let foo_a_number =
            paths.intern(Path::parse_absolute("/Foo/A/Number", &mut tokens).expect("path"));
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

        assert_eq!(spec.display(&tokens), "/Foo{which=A}A/Number{count=one}");
    }

    #[test]
    fn from_variant_selection_sites_supports_repeated_set_names_on_same_host() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let host = paths
            .intern(Path::parse_absolute("/DirectlyNestedVariants", &mut tokens).expect("path"));
        let concrete = paths.intern(
            Path::parse_absolute(
                "/DirectlyNestedVariants/anim_spooky_anim_sphere",
                &mut tokens,
            )
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
            spec.display(&tokens),
            "/DirectlyNestedVariants{standin=anim}{shadingVariant=spooky}{standin=anim}anim_spooky_anim_sphere"
        );
    }

    #[test]
    fn parse_round_trips_variant_property_path() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();

        let spec = SpecPath::parse(
            "/A{nestedVariantSet=nestedVariant}.test",
            &mut tokens,
            &mut paths,
        )
        .expect("spec path");

        assert_eq!(
            spec.display(&tokens),
            "/A{nestedVariantSet=nestedVariant}.test"
        );
        let prim = paths.resolve(spec.prim_path());
        assert_eq!(prim.display(&tokens), "/A");
    }

    #[test]
    fn from_property_path_preserves_plain_property_identity() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let property = PropertyPath::parse("/Robot.visibility", &mut tokens, &mut paths)
            .expect("property path");

        let spec = SpecPath::from_property_path(property, &paths);

        assert_eq!(spec.display(&tokens), "/Robot.visibility");
        assert_eq!(spec.prim_path(), property.prim_path());
    }
}
