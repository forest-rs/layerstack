// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Addressing specs of a [`Layer`] by variant-qualified spec path.
//!
//! A layer stores a prim spec authored inside variant branches
//! (`/Rock{shape=jagged}Child`) at its namespace path, with the branches
//! as its [`PrimSpec::outer_variant_sites`], and a variant spec
//! (`/Rock{shape=jagged}`) inside the spec holding its variant set: the
//! prim spec hosting it, or for a set nested in another branch
//! (`/Rock{shape=jagged}{size=big}`), that branch's variant spec. This
//! module turns a [`SpecPath`] into that storage location.

use alloc::vec::Vec;

use crate::{
    HashMap,
    doc::{FieldEntry, FieldValue, Layer, PrimSpec, Value, VariantSetSpec, VariantSpec, get_field},
    interner::TokenId,
    path::{Path, PathId, PathInterner},
    property::{PropertyEntry, PropertySpec, PropertyType, get_property},
    spec_path::{SpecComponent, SpecPath, VariantSelectionSite},
};

/// Where a prim-like spec (one that holds fields, properties and variant
/// selections) is stored in a layer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Loc {
    /// The prim spec at `path` authored inside the branches `sites`
    /// (outermost first; empty outside every branch). Every site is hosted
    /// at a strict ancestor of `path`.
    Prim {
        path: PathId,
        sites: Vec<VariantSelectionSite>,
    },
    /// The variant spec of the last of `sites`, hosted at `host`; the
    /// others are the branches enclosing it, those hosted at `host` too
    /// last (`/Rock{a=x}{b=y}`).
    Variant {
        host: PathId,
        sites: Vec<VariantSelectionSite>,
    },
}

impl Loc {
    /// The storage location of the prim or variant spec path `path` (its
    /// property suffix is ignored), interning the variant hosts' paths;
    /// `None` for a path editing does not reach yet: a variant spec
    /// nested in another branch of the same prim (`/Rock{a=x}{b=y}`).
    pub(crate) fn of(path: &SpecPath, paths: &mut PathInterner) -> Option<Self> {
        Self::with_hosts(path, |host| Some(paths.intern(host)))
    }

    /// [`Loc::of`] without interning: also `None` if a variant host's path
    /// was never interned, since no spec is stored there then.
    pub(crate) fn lookup(path: &SpecPath, paths: &PathInterner) -> Option<Self> {
        Self::with_hosts(path, |host| paths.lookup(&host))
    }

    fn with_hosts(
        path: &SpecPath,
        mut host_id: impl FnMut(Path) -> Option<PathId>,
    ) -> Option<Self> {
        let mut prefix = Vec::new();
        let mut sites = Vec::new();
        let mut last_is_variant = false;
        for component in path.components() {
            match *component {
                SpecComponent::Prim(name) => {
                    prefix.push(name);
                    last_is_variant = false;
                }
                SpecComponent::VariantSelection { set, variant } => {
                    if last_is_variant {
                        return None;
                    }
                    sites.push(VariantSelectionSite {
                        host_path: host_id(Path::root().join(&prefix))?,
                        set,
                        variant,
                    });
                    last_is_variant = true;
                }
            }
        }
        Some(if last_is_variant {
            Self::Variant {
                host: path.prim_path(),
                sites,
            }
        } else {
            Self::Prim {
                path: path.prim_path(),
                sites,
            }
        })
    }

    /// The variant branches enclosing this spec, outermost first; for a
    /// variant spec, ending with its own selection.
    pub(crate) fn sites(&self) -> &[VariantSelectionSite] {
        match self {
            Self::Prim { sites, .. } | Self::Variant { sites, .. } => sites,
        }
    }

    /// The namespace path of the prim this spec belongs to: the prim spec's
    /// path, or the variant's host.
    pub(crate) fn prim_path(&self) -> PathId {
        match self {
            Self::Prim { path, .. } => *path,
            Self::Variant { host, .. } => *host,
        }
    }

    /// For a variant spec, the location of the spec holding its variant
    /// set, and the selection naming it: the prim spec hosting the set, or
    /// for a set nested in another branch of the same prim
    /// (`/Rock{a=x}{b=y}`), that branch's variant spec (`/Rock{a=x}`).
    ///
    /// Spec: AOUSD Core §7.3.6 (prim and variant specs contain variant set
    /// specs).
    pub(crate) fn variant_parts(&self) -> Option<(Self, VariantSelectionSite)> {
        let Self::Variant { host, sites } = self else {
            return None;
        };
        let (site, outer) = sites.split_last()?;
        let holder = match outer.last() {
            Some(enclosing) if enclosing.host_path == *host => Self::Variant {
                host: *host,
                sites: outer.to_vec(),
            },
            _ => Self::Prim {
                path: *host,
                sites: outer.to_vec(),
            },
        };
        Some((holder, *site))
    }

    /// The location of the spec that lists this prim spec among its
    /// children (`primChildren`) and the prim's name; `None` for the
    /// pseudo-root and for a variant spec.
    pub(crate) fn parent(&self, paths: &mut PathInterner) -> Option<(Self, TokenId)> {
        let Self::Prim { path, sites } = self else {
            return None;
        };
        let namespace = paths.resolve(*path);
        let name = namespace.leaf()?;
        let parent = paths.intern(namespace.parent()?);
        let parent_loc = match sites.last() {
            Some(site) if site.host_path == parent => Self::Variant {
                host: parent,
                sites: sites.clone(),
            },
            _ => Self::Prim {
                path: parent,
                sites: sites.clone(),
            },
        };
        Some((parent_loc, name))
    }
}

/// A prim-like spec found at a [`Loc`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum SpecRef<'a> {
    Prim(&'a PrimSpec),
    Variant(&'a VariantSpec),
}

impl<'a> SpecRef<'a> {
    pub(crate) fn fields(self) -> &'a [FieldEntry] {
        match self {
            Self::Prim(spec) => &spec.fields,
            Self::Variant(spec) => &spec.fields,
        }
    }

    pub(crate) fn properties(self) -> &'a [PropertyEntry] {
        match self {
            Self::Prim(spec) => &spec.properties,
            Self::Variant(spec) => &spec.properties,
        }
    }

    pub(crate) fn children(self) -> &'a [TokenId] {
        match self {
            Self::Prim(spec) => &spec.authored_children,
            Self::Variant(spec) => &spec.authored_children,
        }
    }

    pub(crate) fn variant_selection(self, set: TokenId) -> Option<TokenId> {
        match self {
            Self::Prim(spec) => spec.variant_selections.get(&set).copied(),
            Self::Variant(spec) => spec.variant_selections.get(&set).copied(),
        }
    }

    /// The variant sets this spec holds: a prim spec's own, or those nested
    /// in a variant spec.
    pub(crate) fn variant_sets(self) -> &'a HashMap<TokenId, VariantSetSpec> {
        match self {
            Self::Prim(spec) => &spec.variant_sets,
            Self::Variant(spec) => &spec.variant_sets,
        }
    }

    /// The `variantSets` order of the sets this spec holds.
    pub(crate) fn variant_set_order(self) -> &'a [TokenId] {
        match self {
            Self::Prim(spec) => &spec.variant_set_order,
            Self::Variant(spec) => &spec.variant_set_order,
        }
    }
}

/// A prim-like spec found at a [`Loc`], mutably.
#[derive(Debug)]
pub(crate) enum SpecMut<'a> {
    Prim(&'a mut PrimSpec),
    Variant(&'a mut VariantSpec),
}

impl<'a> SpecMut<'a> {
    pub(crate) fn fields(&mut self) -> &mut Vec<FieldEntry> {
        match self {
            Self::Prim(spec) => &mut spec.fields,
            Self::Variant(spec) => &mut spec.fields,
        }
    }

    pub(crate) fn properties(&mut self) -> &mut Vec<PropertyEntry> {
        match self {
            Self::Prim(spec) => &mut spec.properties,
            Self::Variant(spec) => &mut spec.properties,
        }
    }

    pub(crate) fn children(&mut self) -> &mut Vec<TokenId> {
        match self {
            Self::Prim(spec) => &mut spec.authored_children,
            Self::Variant(spec) => &mut spec.authored_children,
        }
    }

    pub(crate) fn variant_selections(&mut self) -> &mut HashMap<TokenId, TokenId> {
        match self {
            Self::Prim(spec) => &mut spec.variant_selections,
            Self::Variant(spec) => &mut spec.variant_selections,
        }
    }

    /// The variant sets this spec holds and their `variantSets` order (see
    /// [`SpecRef::variant_sets`]).
    pub(crate) fn into_variant_sets(
        self,
    ) -> (
        &'a mut HashMap<TokenId, VariantSetSpec>,
        &'a mut Vec<TokenId>,
    ) {
        match self {
            Self::Prim(spec) => (&mut spec.variant_sets, &mut spec.variant_set_order),
            Self::Variant(spec) => (&mut spec.variant_sets, &mut spec.variant_set_order),
        }
    }
}

/// Returns the spec stored at `loc`.
pub(crate) fn spec_at<'a>(layer: &'a Layer, loc: &Loc) -> Option<SpecRef<'a>> {
    match loc {
        Loc::Prim { path, sites } => layer.prim_spec_in(*path, sites).map(SpecRef::Prim),
        Loc::Variant { .. } => {
            let (holder, site) = loc.variant_parts()?;
            let variant = spec_at(layer, &holder)?
                .variant_sets()
                .get(&site.set)?
                .variants
                .get(&site.variant)?;
            Some(SpecRef::Variant(variant))
        }
    }
}

/// Returns the spec stored at `loc`, mutably.
pub(crate) fn spec_at_mut<'a>(layer: &'a mut Layer, loc: &Loc) -> Option<SpecMut<'a>> {
    match loc {
        Loc::Prim { path, sites } => prim_spec_mut(layer, *path, sites).map(SpecMut::Prim),
        Loc::Variant { .. } => {
            let (holder, site) = loc.variant_parts()?;
            let (sets, _) = spec_at_mut(layer, &holder)?.into_variant_sets();
            let variant = sets.get_mut(&site.set)?.variants.get_mut(&site.variant)?;
            Some(SpecMut::Variant(variant))
        }
    }
}

/// The prim spec at `path` authored in the branches `sites`, mutably.
pub(crate) fn prim_spec_mut<'a>(
    layer: &'a mut Layer,
    path: PathId,
    sites: &[VariantSelectionSite],
) -> Option<&'a mut PrimSpec> {
    if layer
        .prims
        .get(&path)
        .is_some_and(|spec| spec.outer_variant_sites == sites)
    {
        return layer.prims.get_mut(&path);
    }
    layer
        .variant_prims
        .get_mut(&path)?
        .iter_mut()
        .find(|spec| spec.outer_variant_sites == sites)
}

/// Returns `true` when `value` can be the value of an attribute declared
/// as `ty`: a value of the declared scalar type (a tuple type also takes
/// an array of as many components), an array of those for an array type,
/// a sparse array edit for an array type, or a value block.
///
/// A declared type whose scalar form is unknown ([`Value::Null`]) accepts
/// any value.
///
/// Spec: AOUSD Core §6.2–§6.3 (value types), §7.6.4.1.1 (`typeName`),
/// §12.3 (value blocks).
pub(crate) fn conforms(ty: &PropertyType, value: &Value) -> bool {
    match value {
        Value::Blocked => true,
        Value::ArrayEdit(_) => ty.is_array,
        Value::Array(items) if ty.is_array => items.iter().all(|v| scalar_conforms(ty, v)),
        _ if ty.is_array => false,
        _ => scalar_conforms(ty, value),
    }
}

fn scalar_conforms(ty: &PropertyType, value: &Value) -> bool {
    let expected = &ty.default_scalar;
    if matches!(expected, Value::Null) {
        return true;
    }
    if core::mem::discriminant(expected) == core::mem::discriminant(value) {
        return true;
    }
    // Tuples may also be spelled as arrays of their components.
    let Value::Array(components) = value else {
        return false;
    };
    let (count, component) = match expected {
        Value::Vec2d(_) => (2, Value::Double(0.0)),
        Value::Vec3d(_) => (3, Value::Double(0.0)),
        Value::Vec4d(_) | Value::Quatd(_) => (4, Value::Double(0.0)),
        Value::Vec2f(_) => (2, Value::Float(0.0)),
        Value::Vec3f(_) => (3, Value::Float(0.0)),
        Value::Vec4f(_) | Value::Quatf(_) => (4, Value::Float(0.0)),
        Value::Vec2h(_) => (2, Value::Half(0)),
        Value::Vec3h(_) => (3, Value::Half(0)),
        Value::Vec4h(_) | Value::Quath(_) => (4, Value::Half(0)),
        Value::Vec2i(_) => (2, Value::Int(0)),
        Value::Vec3i(_) => (3, Value::Int(0)),
        Value::Vec4i(_) => (4, Value::Int(0)),
        Value::Matrix2d(_) => (4, Value::Double(0.0)),
        Value::Matrix3d(_) => (9, Value::Double(0.0)),
        Value::Matrix4d(_) => (16, Value::Double(0.0)),
        _ => return false,
    };
    components.len() == count
        && components
            .iter()
            .all(|c| core::mem::discriminant(c) == core::mem::discriminant(&component))
}

/// Reading authored specs by spec path.
impl Layer {
    /// Returns the property spec at the spec path `path`: a property of a
    /// prim spec (`/Rock.size`), of a prim spec inside variant branches
    /// (`/Rock{shape=jagged}Child.size`) or of a variant spec
    /// (`/Rock{shape=jagged}.roughness`).
    ///
    /// Returns `None` when `path` has no property suffix or names no
    /// authored spec. Paths are looked up, not interned: a path the
    /// interner has never seen names no spec.
    ///
    /// OpenUSD: `SdfLayer::GetPropertyAtPath`.
    #[must_use]
    pub fn property_at(&self, path: &SpecPath, paths: &PathInterner) -> Option<&PropertySpec> {
        let loc = Loc::lookup(path, paths)?;
        get_property(spec_at(self, &loc)?.properties(), path.property()?)
    }

    /// Returns the metadata field `key` authored on the prim, variant or
    /// property spec at `path`.
    ///
    /// OpenUSD: `SdfSpec::GetInfo`.
    #[must_use]
    pub fn metadata_at(
        &self,
        path: &SpecPath,
        key: TokenId,
        paths: &PathInterner,
    ) -> Option<&FieldValue> {
        let loc = Loc::lookup(path, paths)?;
        let spec = spec_at(self, &loc)?;
        match path.property() {
            Some(name) => get_property(spec.properties(), name)?.metadata(key),
            None => get_field(spec.fields(), &key),
        }
    }

    /// Returns the variant selection for `set` authored on the prim or
    /// variant spec at `path`.
    ///
    /// OpenUSD: `SdfPrimSpec::GetVariantSelections`.
    #[must_use]
    pub fn variant_selection_at(
        &self,
        path: &SpecPath,
        set: TokenId,
        paths: &PathInterner,
    ) -> Option<TokenId> {
        let loc = Loc::lookup(path, paths)?;
        spec_at(self, &loc)?.variant_selection(set)
    }

    /// Returns `true` if the layer authors a prim or variant spec at `path`
    /// (its property suffix is ignored).
    ///
    /// OpenUSD: `SdfLayer::HasSpec` for a prim or variant path.
    #[must_use]
    pub fn has_spec_at(&self, path: &SpecPath, paths: &PathInterner) -> bool {
        Loc::lookup(path, paths).is_some_and(|loc| spec_at(self, &loc).is_some())
    }
}
