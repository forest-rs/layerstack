// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Coordinate-system bindings with inheritance: the named spaces a prim and
//! its ancestors bind through `CoordSysAPI`, as OpenUSD's
//! `UsdShadeCoordSysAPI::FindBindingsWithInheritanceForPrim` resolves them.
//!
//! Shading reads a binding such as `worldSpace` or `modelSpace` to project
//! textures or evaluate procedural patterns in that prim's space. A prim
//! binds a name through an applied `CoordSysAPI:<name>` instance and its
//! `coordSys:<name>:binding` relationship; descendants inherit the nearest
//! ancestor's binding of each name. [`CoordSysCache`] shares each ancestor's
//! resolved list across the prims below it.

use alloc::{format, string::String, sync::Arc, vec::Vec};

use hashbrown::HashMap;
use layerstack::{PathId, PropertyKind, PropertyPath, TargetPath};

use crate::SchemaEdit;
use crate::diagnostic::{Describe, Names, with_store};
use crate::usd_shade::{CoordSysApi, CoordSysApiEdit};
use crate::view::{Scene, forwarded_targets};

/// The applied schema that binds coordinate systems.
const COORD_SYS_API: &str = "CoordSysAPI";

/// The namespace of the binding relationships.
const COORD_SYS: &str = "coordSys";

/// The binding relationship of the `CoordSysAPI` instance `instance`.
fn binding_relationship(instance: &str) -> String {
    format!("{COORD_SYS}:{instance}:binding")
}

/// One bound coordinate system: its name, the relationship that binds it and
/// the prim (or property) it targets.
///
/// OpenUSD: `UsdShadeCoordSysAPI::Binding`, whose `name` is the
/// relationship's base name (`worldSpace:binding`); here it is the instance
/// name alone (`worldSpace`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordSysBinding {
    /// The `CoordSysAPI` instance name (`worldSpace`).
    pub name: Arc<str>,
    /// The `coordSys:<name>:binding` relationship that binds it.
    pub relationship: PropertyPath,
    /// The relationship's first forwarded target: the space's prim.
    pub target: TargetPath,
}

impl CoordSysBinding {
    /// The name OpenUSD gives the binding: its relationship's name without
    /// the `coordSys:` namespace (`worldSpace:binding`).
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::Binding::name`, from
    /// `GetBindingBaseName`.
    #[doc(alias = "GetBindingBaseName")]
    #[must_use]
    pub fn base_name(&self) -> String {
        format!("{}:binding", self.name)
    }
}

/// `name` without its leading `coordSys:` namespace, or `name` itself when
/// it is not in that namespace: `coordSys:worldSpace:binding` gives
/// `worldSpace:binding`, `coordSys:` the empty name, and `coordSys`,
/// `coordSysX:y` or `other:coordSys:a` are unchanged.
///
/// OpenUSD: the static `UsdShadeCoordSysAPI::GetBindingBaseName`, which is
/// `SdfPath::StripPrefixNamespace(name, "coordSys")`.
#[doc(alias = "UsdShadeCoordSysAPI::GetBindingBaseName")]
#[doc(alias = "GetBindingBaseName")]
#[must_use]
pub fn coord_sys_binding_base_name(name: &str) -> &str {
    name.strip_prefix(COORD_SYS)
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or(name)
}

/// Whether the property `name` may belong to a coordinate-system binding:
/// whether it starts with `coordSys`, as text (so `coordSystem` does too).
///
/// OpenUSD: `UsdShadeCoordSysAPI::CanContainPropertyName`.
#[doc(alias = "UsdShadeCoordSysAPI::CanContainPropertyName")]
#[doc(alias = "CanContainPropertyName")]
#[must_use]
pub fn can_contain_coord_sys_property_name(name: &str) -> bool {
    name.starts_with(COORD_SYS)
}

/// Whether the prim at `path` binds coordinate systems itself: whether it
/// has any `CoordSysAPI` instance applied, bound or not.
///
/// OpenUSD: `UsdShadeCoordSysAPI::HasLocalBindingsForPrim`.
#[doc(alias = "UsdShadeCoordSysAPI::HasLocalBindingsForPrim")]
#[doc(alias = "HasLocalBindingsForPrim")]
#[must_use]
pub fn has_local_coord_sys_bindings(scene: &Scene<'_>, path: PathId) -> bool {
    !scene.instances(path, COORD_SYS_API).is_empty()
}

/// The binding the prim at `path` authors for the instance `name`, applied
/// or not: its relationship's first forwarded target.
fn instance_binding(scene: &Scene<'_>, path: PathId, name: &str) -> Option<CoordSysBinding> {
    let token = scene.store().tokens().lookup(&binding_relationship(name))?;
    let relationship = PropertyPath::new(path, token);
    let target = *forwarded_targets(scene, relationship).first()?;
    Some(CoordSysBinding {
        name: Arc::from(name),
        relationship,
        target,
    })
}

/// The coordinate systems the prim at `path` binds itself, in the order its
/// `CoordSysAPI` instances are applied. An instance whose relationship has
/// no target (a blocked binding) binds nothing; a `coordSys:*:binding`
/// relationship without the applied instance is not a binding.
///
/// OpenUSD: `UsdShadeCoordSysAPI::GetLocalBindingsForPrim`.
#[must_use]
#[doc(alias = "GetLocalBindingsForPrim")]
pub fn local_coord_sys_bindings(scene: &Scene<'_>, path: PathId) -> Vec<CoordSysBinding> {
    scene
        .instances(path, COORD_SYS_API)
        .into_iter()
        .filter_map(|name| instance_binding(scene, path, name))
        .collect()
}

impl CoordSysApi<'_> {
    /// The binding this instance authors on its prim: the first forwarded
    /// target of its `coordSys:<instance>:binding` relationship; `None` for
    /// no relationship or one without targets (a blocked binding). Ancestors
    /// are not consulted ([`CoordSysCache::binding`] inherits). Like C++, it
    /// reads an instance [`Self::from_instance_unchecked`] names whether or
    /// not it is applied.
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::GetLocalBinding`.
    #[doc(alias = "UsdShadeCoordSysAPI::GetLocalBinding")]
    #[doc(alias = "GetLocalBinding")]
    #[must_use]
    pub fn local_binding(&self) -> Option<CoordSysBinding> {
        instance_binding(&self.scene(), self.path(), self.instance())
    }

    /// The name OpenUSD gives this instance's binding: its relationship's
    /// name without the `coordSys:` namespace (`<instance>:binding`).
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::GetBindingBaseName`.
    #[doc(alias = "UsdShadeCoordSysAPI::GetBindingBaseName")]
    #[doc(alias = "GetBindingBaseName")]
    #[must_use]
    pub fn binding_base_name(&self) -> String {
        format!("{}:binding", self.instance())
    }
}

/// Why a coordinate-system binding edit authored nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CoordSysError {
    /// The binding relationship to clear does not exist: neither a layer nor
    /// an applied instance declares it.
    NoBindingRelationship {
        /// The relationship.
        relationship: PropertyPath,
    },
}

impl CoordSysError {
    /// The error in words, the relationship written as its path with
    /// `store`, the store the stage was composed from. `Display` writes the
    /// same with ids.
    #[must_use]
    pub fn display<'a>(
        &'a self,
        store: &'a dyn layerstack::LayerStore,
    ) -> impl core::fmt::Display + 'a {
        with_store(self, store)
    }
}

impl Describe for CoordSysError {
    fn describe(&self, f: &mut core::fmt::Formatter<'_>, names: Names<'_>) -> core::fmt::Result {
        f.write_str("cannot clear the coordinate-system binding: ")?;
        match self {
            Self::NoBindingRelationship { relationship } => write!(
                f,
                "{} does not exist; apply the `CoordSysAPI` instance or bind it first",
                names.property(*relationship)
            ),
        }
    }
}

impl core::fmt::Display for CoordSysError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.describe(f, Names(None))
    }
}

impl core::error::Error for CoordSysError {}

impl CoordSysApiEdit {
    /// Binds this instance's coordinate system to `target` (the prim whose
    /// space it names, expected to be `Xformable`): authors `target` as the
    /// only, explicit target of `coordSys:<instance>:binding`, creating the
    /// relationship (not `custom`) if needed, whether or not the instance
    /// is applied; apply it ([`CoordSysApi::apply`]) for the binding to
    /// count.
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::Bind(const SdfPath &)`.
    #[doc(alias = "UsdShadeCoordSysAPI::Bind")]
    #[doc(alias = "Bind")]
    pub fn bind(&self, edit: &mut SchemaEdit<'_>, target: TargetPath) -> &Self {
        let name = binding_relationship(self.instance());
        edit.ensure_relationship(self.path(), &name, false);
        edit.set_targets(self.path(), &name, &[target]);
        self
    }

    /// Blocks this instance's binding: authors an explicit empty target
    /// list, creating the relationship as [`Self::bind`] does, so the name
    /// binds nothing here and is inherited from an ancestor.
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::BlockBinding()`.
    #[doc(alias = "UsdShadeCoordSysAPI::BlockBinding")]
    #[doc(alias = "BlockBinding")]
    pub fn block_binding(&self, edit: &mut SchemaEdit<'_>) -> &Self {
        let name = binding_relationship(self.instance());
        edit.ensure_relationship(self.path(), &name, false);
        edit.set_targets(self.path(), &name, &[]);
        self
    }

    /// Clears this instance's binding in the edit target: with
    /// `remove_spec`, removes the relationship's spec there (dropping any
    /// metadata it authors); otherwise clears its targets and keeps the
    /// spec, creating it if needed, as C++ does. Weaker layers' targets
    /// show through either way.
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::ClearBinding(bool removeSpec)`, which
    /// calls `UsdRelationship::ClearTargets`.
    ///
    /// # Errors
    ///
    /// [`CoordSysError::NoBindingRelationship`] when the relationship does
    /// not exist (C++ returns `false`); nothing is authored.
    #[doc(alias = "UsdShadeCoordSysAPI::ClearBinding")]
    #[doc(alias = "ClearBinding")]
    pub fn clear_binding(
        &self,
        edit: &mut SchemaEdit<'_>,
        remove_spec: bool,
    ) -> Result<&Self, CoordSysError> {
        let name = binding_relationship(self.instance());
        if edit.property_kind(self.path(), &name) != Some(PropertyKind::Relationship) {
            let token = edit.tokens().intern(&name);
            return Err(CoordSysError::NoBindingRelationship {
                relationship: PropertyPath::new(self.path(), token),
            });
        }
        if remove_spec {
            edit.remove_property(self.path(), &name);
        } else {
            edit.clear_targets(self.path(), &name);
        }
        Ok(self)
    }
}

/// What a [`CoordSysCache`] has done since it was made or cleared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CoordSysCacheStats {
    /// Prims whose own bindings were read.
    pub prims_read: usize,
    /// Queries answered from held results.
    pub hits: usize,
}

/// Inherited coordinate-system bindings of many prims, sharing each
/// ancestor's resolved list.
///
/// The caller owns it; nothing is global. It holds results for the
/// [`Scene`] it is queried with; after an edit, [`Self::invalidate`] drops a
/// prim's results and its descendants', and [`Self::clear`] everything.
#[derive(Clone, Debug, Default)]
pub struct CoordSysCache {
    inherited: HashMap<PathId, Arc<[CoordSysBinding]>>,
    stats: CoordSysCacheStats,
}

impl CoordSysCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The coordinate systems bound at the prim at `path`: its own bindings
    /// first, in applied order, then each ancestor's (nearest first) for the
    /// names not yet bound. A blocked binding binds nothing, so the name is
    /// inherited from further up.
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::FindBindingsWithInheritanceForPrim`.
    #[doc(alias = "FindBindingsWithInheritanceForPrim")]
    pub fn bindings(&mut self, scene: &Scene<'_>, path: PathId) -> Arc<[CoordSysBinding]> {
        // Walk up to the nearest held (or root) result, then resolve down.
        let mut pending = Vec::new();
        let mut at = Some(path);
        let mut above: Arc<[CoordSysBinding]> = Arc::from([]);
        while let Some(prim) = at {
            if let Some(held) = self.inherited.get(&prim) {
                self.stats.hits += 1;
                above = held.clone();
                break;
            }
            pending.push(prim);
            at = scene.parent(prim);
        }
        for prim in pending.into_iter().rev() {
            self.stats.prims_read += 1;
            let mut bindings = local_coord_sys_bindings(scene, prim);
            if bindings.is_empty() {
                self.inherited.insert(prim, above.clone());
                continue;
            }
            bindings.extend(
                above
                    .iter()
                    .filter(|inherited| !bindings.iter().any(|own| own.name == inherited.name))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            above = Arc::from(bindings);
            self.inherited.insert(prim, above.clone());
        }
        above
    }

    /// The binding of the coordinate system `name` at the prim at `path`:
    /// the nearest one among the prim and its ancestors with a target.
    ///
    /// OpenUSD: `UsdShadeCoordSysAPI::FindBindingWithInheritance`.
    #[doc(alias = "FindBindingWithInheritance")]
    pub fn binding(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
        name: &str,
    ) -> Option<CoordSysBinding> {
        self.bindings(scene, path)
            .iter()
            .find(|binding| &*binding.name == name)
            .cloned()
    }

    /// Drops the results held for the prim at `path` and every prim below
    /// it. Call it after an edit to the prim's applied `CoordSysAPI`
    /// instances or its `coordSys:*:binding` relationships. After an edit to
    /// a relationship elsewhere that a binding forwards through, call
    /// [`Self::clear`].
    pub fn invalidate(&mut self, scene: &Scene<'_>, path: PathId) {
        let paths = scene.store().paths();
        let changed = paths.resolve(path);
        self.inherited
            .retain(|held, _| !changed.is_prefix_of(paths.resolve(*held)));
    }

    /// Drops everything held, and the statistics.
    pub fn clear(&mut self) {
        self.inherited.clear();
        self.stats = CoordSysCacheStats::default();
    }

    /// What it has computed and reused.
    #[must_use]
    pub fn stats(&self) -> CoordSysCacheStats {
        self.stats
    }
}
