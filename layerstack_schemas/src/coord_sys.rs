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

use alloc::{sync::Arc, vec::Vec};

use hashbrown::HashMap;
use layerstack::{PathId, PropertyPath, TargetPath};

use crate::view::{Scene, forwarded_targets};

/// The applied schema that binds coordinate systems.
const COORD_SYS_API: &str = "CoordSysAPI";

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

/// The coordinate systems the prim at `path` binds itself, in the order its
/// `CoordSysAPI` instances are applied. An instance whose relationship has
/// no target (a blocked binding) binds nothing; a `coordSys:*:binding`
/// relationship without the applied instance is not a binding.
///
/// OpenUSD: `UsdShadeCoordSysAPI::GetLocalBindingsForPrim`.
#[must_use]
#[doc(alias = "GetLocalBindingsForPrim")]
pub fn local_coord_sys_bindings(scene: &Scene<'_>, path: PathId) -> Vec<CoordSysBinding> {
    let mut bindings = Vec::new();
    for name in scene.instances(path, COORD_SYS_API) {
        let relationship = alloc::format!("coordSys:{name}:binding");
        let Some(token) = scene.store().tokens().lookup(&relationship) else {
            continue;
        };
        let relationship = PropertyPath::new(path, token);
        if let Some(&target) = forwarded_targets(scene, relationship).first() {
            bindings.push(CoordSysBinding {
                name: Arc::from(name),
                relationship,
                target,
            });
        }
    }
    bindings
}

/// What a [`CoordSysCache`] has done since it was made or cleared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
