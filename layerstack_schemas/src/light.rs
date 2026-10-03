// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Light discovery with authored model caches and caller-owned snapshot queries.
//! OpenUSD: `UsdLuxLightListAPI` (AOUSD Core §11.4–11.5, §12.5 relationships).
mod helpers;
pub use helpers::*;

mod nodes;
pub use nodes::*;

use crate::{
    PrimView, Scene, SchemaEdit,
    usd_lux::{LightListApi, LightListApiEdit, LightListApiLightListCacheBehavior as Behavior},
};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec,
    vec::Vec,
};
use layerstack::{PathId, PropertyKind, PropertyType, TargetPath, Value};

/// Which descendants and authored caches contribute to discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LightListMode {
    /// Traverse all active, defined, concrete descendants; ignore authored caches.
    IgnoreCache,
    /// Traverse only the model hierarchy and consume caches according to their behavior.
    ConsultModelHierarchyCache,
}
/// An invalid discovery root or cache edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LightListError {
    /// The root prim does not exist. The pseudo-root is valid for discovery.
    MissingPrim(PathId),
    /// A cache property has an incompatible declaration.
    WrongPropertyType,
    /// A cache cannot be authored on the pseudo-root.
    PseudoRoot,
}
impl core::fmt::Display for LightListError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid light list: {self:?}")
    }
}
impl core::error::Error for LightListError {}

/// Work performed by a snapshot query since construction or clearing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightListStats {
    /// Calls to compute, including failed queries.
    pub queries: usize,
    /// Calls served entirely by the query's retained results.
    pub cache_hits: usize,
    /// Prims visited on uncached traversals, including the root.
    pub visited_prims: usize,
    /// Authored caches consumed, including empty lists.
    pub authored_cache_reads: usize,
    /// Number of retained root/mode results.
    pub entries: usize,
    /// Number of target identities retained across all entries.
    pub stored_targets: usize,
}

/// Retains sorted light lists for one immutable scene snapshot, keyed by root and mode.
///
/// The borrowed scene ties results to both stage and store. Drop the query before
/// editing and construct a new one afterward; custom stores must preserve the
/// snapshot's identities. This runtime cache is separate from USD's authored
/// `lightList` caches, which applications must invalidate when their contents age.
/// Cache hits return a borrowed slice without copying targets or traversing prims.
#[derive(Debug)]
pub struct LightListQuery<'a> {
    scene: Scene<'a>,
    results: BTreeMap<(PathId, LightListMode), Vec<TargetPath>>,
    stats: LightListStats,
}
impl<'a> LightListQuery<'a> {
    /// Creates an empty query cache for `scene`.
    #[must_use]
    pub fn new(scene: Scene<'a>) -> Self {
        Self {
            scene,
            results: BTreeMap::new(),
            stats: LightListStats::default(),
        }
    }
    /// Computes or reuses the result for `root` and `mode`.
    /// Returns an error for a missing root; failures are not cached.
    pub fn compute(
        &mut self,
        root: PathId,
        mode: LightListMode,
    ) -> Result<&[TargetPath], LightListError> {
        self.stats.queries += 1;
        let key = (root, mode);
        if self.results.contains_key(&key) {
            self.stats.cache_hits += 1;
        } else {
            let targets = discover(self.scene, root, mode, &mut self.stats)?;
            self.stats.entries += 1;
            self.stats.stored_targets += targets.len();
            self.results.insert(key, targets);
        }
        Ok(&self.results[&key])
    }
    /// Current work and retained-storage counts.
    #[must_use]
    pub fn stats(&self) -> LightListStats {
        self.stats
    }
    /// Releases retained results and resets the counters.
    pub fn clear(&mut self) {
        self.results.clear();
        self.stats = LightListStats::default();
    }
}

/// USD's defined flag includes ancestor definitions, while the core's
/// `is_defined` answers the prim's own resolved specifier.
fn defined(scene: Scene<'_>, path: PathId) -> bool {
    let mut at = Some(path);
    while let Some(path) = at {
        if scene.parent(path).is_some() && !scene.stage().is_defined(path, scene.store()) {
            return false;
        }
        at = scene.parent(path);
    }
    true
}
fn discover(
    scene: Scene<'_>,
    root: PathId,
    mode: LightListMode,
    stats: &mut LightListStats,
) -> Result<Vec<TargetPath>, LightListError> {
    let paths = scene.store().paths();
    if paths.resolve(root).depth() != 0 && !scene.stage().has_prim(root) {
        return Err(LightListError::MissingPrim(root));
    }
    let mut found = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(path) = stack.pop() {
        stats.visited_prims += 1;
        let prim = PrimView::new(scene, path);
        if mode == LightListMode::ConsultModelHierarchyCache && paths.resolve(path).depth() != 0 {
            let behavior = prim.read_value("lightList:cacheBehavior", crate::value::read_token);
            if matches!(behavior, Some("consumeAndContinue" | "consumeAndHalt")) {
                stats.authored_cache_reads += 1;
                if let Some(property) = prim.property_path("lightList") {
                    found.extend(crate::view::forwarded_targets(&scene, property));
                }
                if behavior == Some("consumeAndHalt") {
                    continue;
                }
            }
        }
        if scene.has_api(path, "LightAPI", None) || scene.is_a(path, "LightFilter") {
            found.insert(TargetPath::Prim(path));
        }
        for &child in scene
            .stage()
            .children_of(path)
            .unwrap_or_default()
            .iter()
            .rev()
        {
            if defined(scene, child)
                && !scene.stage().is_abstract(child, scene.store())
                && PrimView::new(scene, child).metadata_value("active") != Some(Value::Bool(false))
                && (mode == LightListMode::IgnoreCache || scene.is_model(child))
            {
                stack.push(child);
            }
        }
    }
    let mut found: Vec<_> = found.into_iter().collect();
    found.sort_by_cached_key(|p| p.display(paths, scene.store().tokens()));
    Ok(found)
}
impl Scene<'_> {
    /// Discovers lights and filters below `root`, including the root itself.
    ///
    /// Authored caches may contain missing prims, properties or non-light targets;
    /// these are preserved, as in OpenUSD. Forwarded relationships are resolved.
    /// Instance proxies contribute ordinary scene paths; point-instancer copies
    /// have no individual prim paths. Results are sorted by namespace spelling.
    /// No `LightListAPI` application is required, including on the pseudo-root.
    /// Returns an error only for a missing root.
    pub fn compute_light_list(
        &self,
        root: PathId,
        mode: LightListMode,
    ) -> Result<Vec<TargetPath>, LightListError> {
        discover(*self, root, mode, &mut LightListStats::default())
    }
}
impl LightListApi<'_> {
    /// Discovers this prim's light list. See [`Scene::compute_light_list`].
    pub fn compute_light_list(
        &self,
        mode: LightListMode,
    ) -> Result<Vec<TargetPath>, LightListError> {
        self.scene().compute_light_list(self.path(), mode)
    }
}
impl LightListApiEdit {
    fn check_cache(&self, edit: &mut SchemaEdit<'_>) -> Result<(), LightListError> {
        if !edit.exists(self.path()) {
            return Err(LightListError::MissingPrim(self.path()));
        }
        if edit.is_pseudo_root(self.path()) {
            return Err(LightListError::PseudoRoot);
        }
        if edit
            .property_kind(self.path(), "lightList")
            .is_some_and(|k| k != PropertyKind::Relationship)
            || edit
                .property_kind(self.path(), "lightList:cacheBehavior")
                .is_some_and(|k| k != PropertyKind::Attribute)
            || edit
                .attribute_type(self.path(), "lightList:cacheBehavior")
                .is_some_and(|ty| ty.is_array || ty.type_name.as_ref() != "token")
        {
            return Err(LightListError::WrongPropertyType);
        }
        Ok(())
    }
    fn behavior(&self, edit: &mut SchemaEdit<'_>, value: Behavior) {
        if edit
            .attribute_type(self.path(), "lightList:cacheBehavior")
            .is_none()
        {
            let empty = edit.tokens().intern("");
            edit.create_attribute(
                self.path(),
                "lightList:cacheBehavior",
                PropertyType::new("token", false, Value::Token(empty)),
            );
        }
        self.set_light_list_cache_behavior(edit, value);
    }
    /// Stores a sorted, deduplicated cache and sets `consumeAndContinue`.
    /// Targets outside this prim's subtree are discarded. Existing declarations
    /// retain their metadata and custom qualifier. Edits remain transactional.
    /// Returns an error for incompatible declarations before collecting operations.
    pub fn store_light_list(
        &self,
        edit: &mut SchemaEdit<'_>,
        targets: &[TargetPath],
    ) -> Result<&Self, LightListError> {
        self.check_cache(edit)?;
        let targets = edit.subtree_targets(self.path(), targets);
        edit.ensure_relationship(self.path(), "lightList", false);
        edit.set_targets(self.path(), "lightList", &targets);
        self.behavior(edit, Behavior::ConsumeAndContinue);
        Ok(self)
    }
    /// Marks the authored cache `ignore`, keeping its relationship targets intact.
    /// Returns an error for incompatible declarations before collecting operations.
    pub fn invalidate_light_list(
        &self,
        edit: &mut SchemaEdit<'_>,
    ) -> Result<&Self, LightListError> {
        self.check_cache(edit)?;
        self.behavior(edit, Behavior::Ignore);
        Ok(self)
    }
}
