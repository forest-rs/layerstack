// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounds from authored or intrinsic extents, with component-space accumulation.
//!
//! Built-in geometry can compute its extent from points or shape parameters.
//! Other procedural extent plugins are not run; missing geometry is an error.
//! It preserves an oriented range and matrix, rather than repeatedly aligning
//! boxes in every ancestor's coordinates. OpenUSD: `UsdGeomBBoxCache`.

mod reduction;
use reduction::{MIN_CHILDREN, Reduction};

use crate::{
    gf,
    imageable::{Visibility, VisibilityInputs},
    usd_geom::Imageable,
    view::{PrimView, Scene, Time},
    xform::XformCache,
};
use alloc::{vec, vec::Vec};
use layerstack::{HashMap, HashSet, PathId};

/// An axis-aligned double-precision range. A reversed axis denotes emptiness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range3d {
    /// Minimum coordinates.
    pub min: [f64; 3],
    /// Maximum coordinates.
    pub max: [f64; 3],
}
impl Default for Range3d {
    fn default() -> Self {
        Self {
            min: [f64::MAX; 3],
            max: [-f64::MAX; 3],
        }
    }
}
impl Range3d {
    /// Whether any axis has no extent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        (0..3).any(|i| self.min[i] > self.max[i])
    }
    /// Component-wise union, as `GfRange3d::UnionWith`.
    /// The canonical empty range is its identity; finite reversed ranges
    /// retain their endpoints, matching OpenUSD even when empty.
    pub fn union_with(&mut self, other: Self) {
        for i in 0..3 {
            self.min[i] = self.min[i].min(other.min[i]);
            self.max[i] = self.max[i].max(other.max[i]);
        }
    }
}

/// An oriented bound: `range` transformed by the row-vector `matrix`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundingBox {
    /// Range before transformation.
    pub range: Range3d,
    /// Row-vector transform (translation in its last row).
    pub matrix: [[f64; 4]; 4],
}
impl Default for BoundingBox {
    fn default() -> Self {
        Self {
            range: Range3d::default(),
            matrix: gf::IDENTITY,
        }
    }
}
impl BoundingBox {
    /// The aligned enclosing range after applying the matrix.
    #[must_use]
    pub fn aligned_range(&self) -> Range3d {
        if self.range.is_empty() {
            return self.range;
        }
        // GfBBox3d::ComputeAlignedRange, Graphics Gems' interval transform.
        let mut result = Range3d {
            min: self.matrix[3][..3].try_into().unwrap(),
            max: self.matrix[3][..3].try_into().unwrap(),
        };
        for j in 0..3 {
            for i in 0..3 {
                let a = self.range.min[i] * self.matrix[i][j];
                let b = self.range.max[i] * self.matrix[i][j];
                result.min[j] += a.min(b);
                result.max[j] += a.max(b);
            }
        }
        result
    }

    fn transformed(mut self, matrix: &gf::Matrix4) -> Self {
        self.matrix = gf::mul(&self.matrix, matrix);
        self
    }
}

/// An unsupported or invalid input; no partial bound is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundsError {
    /// No composed prim exists at this path.
    MissingPrim(PathId),
    /// This boundable needs an extent computation plugin, or has an invalid extent.
    ExtentUnavailable(PathId),
    /// Point instancers require prototype and per-instance computations.
    PointInstancerUnsupported(PathId),
    /// Component-space conversion requires an invertible world transform.
    SingularTransform(PathId),
}

/// Policies held constant for a cache's lifetime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundsOptions {
    /// Purposes contributing to queried bounds, in any order.
    pub included_purposes: Vec<crate::usd_geom::ImageablePurpose>,
    /// Use `extentsHint` on models, in default/render/proxy/guide order.
    pub use_extents_hint: bool,
    /// Include invisible and non-imageable typed child subtrees, as OpenUSD
    /// does when bypassing its inclusion filter. A queried root is always included.
    pub ignore_visibility: bool,
}
impl Default for BoundsOptions {
    fn default() -> Self {
        Self {
            included_purposes: vec![crate::usd_geom::ImageablePurpose::Default],
            use_extents_hint: false,
            ignore_visibility: false,
        }
    }
}

/// Observable work performed since construction or clearing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoundsStats {
    /// Prim bounds evaluated.
    pub computed: usize,
    /// Prim bounds reused.
    pub hits: usize,
    /// Cached prim bounds evicted by explicit invalidation.
    pub invalidated: usize,
}

type PurposeBounds = Vec<(crate::usd_geom::ImageablePurpose, BoundingBox)>;

#[derive(Clone, Debug)]
struct Entry<T> {
    value: T,
    varying: bool,
    epoch: u64,
}
impl<T> Entry<T> {
    fn valid(&self, epoch: u64) -> bool {
        !self.varying || self.epoch == epoch
    }
}

/// Caller-owned bounds at one time and for one scene.
///
/// Pass each successful live change report to [`Self::apply_changes`]. For
/// manual edits affecting geometry, transforms, purpose, visibility, kinds or
/// topology, [`Self::invalidate`] evicts cached descendants and marks ancestors
/// dirty by indexed dependencies. After an edit, wide static parents retain
/// range reductions so later leaf edits update only their contribution paths.
/// It does not observe edits automatically. Clear between unrelated scenes.
/// Intrinsic extents are computed for meshes, cubes, spheres, cylinders, cones
/// and capsules when a valid authored extent is unavailable. Other extent
/// providers and point instancers are not implemented; missing geometry is an
/// error, never a silently incomplete bound.
///
/// Child inclusion follows OpenUSD's defined, non-abstract, imageable/unknown
/// type traversal and local visibility. A query includes its root even when
/// invisible. Authored boundable extents and model hints prune descendants.
#[derive(Clone, Debug)]
pub struct BoundsCache {
    time: Time,
    options: BoundsOptions,
    entries: HashMap<PathId, Entry<PurposeBounds>>,
    // Inclusion is an input of the parent reduction, including when false.
    // Keep it independently of bounds so excluded subtrees need no evaluation.
    inclusions: HashMap<PathId, Entry<bool>>,
    children: HashMap<PathId, HashSet<PathId>>,
    transforms: XformCache,
    reductions: HashMap<PathId, Reduction>,
    promote: HashSet<PathId>,
    stats: BoundsStats,
    epoch: u64,
}
impl BoundsCache {
    /// An empty cache at `time`, with explicit traversal policies.
    #[must_use]
    pub fn new(time: Time, options: BoundsOptions) -> Self {
        Self {
            time,
            options,
            entries: HashMap::new(),
            inclusions: HashMap::new(),
            children: HashMap::new(),
            transforms: XformCache::new(time),
            reductions: HashMap::new(),
            promote: HashSet::new(),
            stats: BoundsStats::default(),
            epoch: 1,
        }
    }
    /// Work counters since construction or clearing.
    #[must_use]
    pub fn stats(&self) -> BoundsStats {
        self.stats
    }
    /// Number of cached prim bounds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Whether no prim bounds are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Drop every result and reset counters.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.inclusions.clear();
        self.children.clear();
        self.transforms.clear();
        self.reductions.clear();
        self.promote.clear();
        self.stats = BoundsStats::default();
        self.epoch = 1;
    }
    /// Change time in O(1), retaining static bounds and transform state.
    /// Sampled bounds and inclusion decisions are reevaluated on demand;
    /// counters and cache occupancy are retained. Single samples count as
    /// dependencies because numeric time can differ from default time.
    pub fn set_time(&mut self, time: Time) {
        if self.time != time {
            if let Some(epoch) = self.epoch.checked_add(1) {
                self.epoch = epoch;
            } else {
                self.clear();
            }
            self.time = time;
            self.transforms.set_time(time);
        }
    }
    /// Invalidate `path`, cached descendants, and ancestors whose bounds include it.
    ///
    /// Deleted paths may be passed: their interned namespace is sufficient.
    /// Visits indexed descendants and namespace ancestors, rather than scanning
    /// every cached prim. A wide static parent's first edit builds a retained
    /// reduction; later child changes update it in logarithmic work per purpose.
    /// Structural changes retire affected reductions. Animated and small
    /// parents continue to fold their children when recomputed.
    pub fn invalidate(&mut self, scene: &Scene<'_>, path: PathId) {
        self.transforms.invalidate(scene, path);
        self.invalidate_bounds(scene, path);
    }

    /// Applies a complete successful live change report against the resulting
    /// scene. Transform invalidation shares stage topology. Bounds retain their
    /// own reduction dependencies, including excluded children and ancestors.
    /// Pass every report before querying results affected by edits.
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &layerstack::Changes) {
        self.transforms.apply_changes(scene, changes);
        // Resync roots cover removals too. Retained reduction dependencies
        // reach deleted and excluded descendants without revisiting ancestors
        // once for every entry in the exact removal inventory.
        for &path in &changes.resynced {
            self.invalidate_bounds(scene, path);
        }
        for &path in &changes.changed_info_only {
            if changes.properties_for(path).is_none_or(|fields| {
                fields
                    .iter()
                    .any(|field| bounds_property(scene.store().tokens().resolve(field.name)))
            }) {
                self.invalidate_bounds(scene, path);
            }
        }
    }

    fn invalidate_bounds(&mut self, scene: &Scene<'_>, path: PathId) {
        let mut pending = vec![path];
        while let Some(at) = pending.pop() {
            self.stats.invalidated += usize::from(self.entries.remove(&at).is_some());
            self.inclusions.remove(&at);
            self.reductions.remove(&at);
            self.promote.remove(&at);
            if let Some(children) = self.children.remove(&at) {
                pending.extend(children);
            }
        }
        if let Some(parent) = scene.parent(path)
            && let Some(children) = self.children.get_mut(&parent)
        {
            children.remove(&path);
        }
        let mut child = path;
        while let Some(parent) = scene.parent(child) {
            self.stats.invalidated += usize::from(self.entries.remove(&parent).is_some());
            let count = scene.stage().children_of(parent).map_or(0, <[PathId]>::len);
            let retained = self
                .reductions
                .get_mut(&parent)
                .is_some_and(|reduction| reduction.mark(child, count));
            if !retained {
                self.reductions.remove(&parent);
                if count >= MIN_CHILDREN {
                    self.promote.insert(parent);
                } else {
                    self.promote.remove(&parent);
                }
            }
            child = parent;
        }
    }
    /// Bound in the queried prim's own coordinates, retaining its orientation.
    pub fn untransformed_bound(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<BoundingBox, BoundsError> {
        self.resolve(scene, path)?;
        let bounds = &self.entries[&path].value;
        let mut result = BoundingBox::default();
        for (purpose, bbox) in bounds {
            if self.options.included_purposes.contains(purpose) && !bbox.range.is_empty() {
                // All purpose buckets at one prim share the same frame.
                if result.range.is_empty() {
                    result = *bbox;
                } else if !bbox.range.is_empty() {
                    result.range.union_with(bbox.range);
                }
            }
        }
        Ok(result)
    }
    /// Bound with the queried prim's local transform applied (even with reset).
    pub fn local_bound(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<BoundingBox, BoundsError> {
        let bbox = self.untransformed_bound(scene, path)?;
        let local = self
            .transforms
            .local_transform(scene, path)
            .ok_or(BoundsError::MissingPrim(path))?;
        Ok(bbox.transformed(&local.matrix))
    }
    /// Bound in world coordinates, retaining component-space orientation.
    pub fn world_bound(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<BoundingBox, BoundsError> {
        let bbox = self.untransformed_bound(scene, path)?;
        Ok(bbox.transformed(&self.world(scene, path)?))
    }
    /// Bound expressed in `relative_to`'s coordinate frame.
    ///
    /// OpenUSD: `UsdGeomBBoxCache::ComputeRelativeBound`. Applies the
    /// queried prim's world transform and the inverse target world transform,
    /// including across reset boundaries. The target need not be an ancestor.
    /// Unlike `XformCache::relative_transform`, this always converts frames.
    /// Missing prims and singular target transforms return explicit errors.
    pub fn relative_bound(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
        relative_to: PathId,
    ) -> Result<BoundingBox, BoundsError> {
        let bbox = self.untransformed_bound(scene, path)?;
        let inverse = self.inverse_world(scene, relative_to)?;
        Ok(bbox.transformed(&gf::mul(&self.world(scene, path)?, &inverse)))
    }

    fn world(&mut self, scene: &Scene<'_>, path: PathId) -> Result<gf::Matrix4, BoundsError> {
        self.transforms
            .local_to_world(scene, path)
            .ok_or(BoundsError::MissingPrim(path))
    }
    fn inverse_world(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<gf::Matrix4, BoundsError> {
        let (inverse, determinant) = gf::inverse(&self.world(scene, path)?);
        if determinant == 0.0 {
            return Err(BoundsError::SingularTransform(path));
        }
        Ok(inverse)
    }
    fn track(&mut self, scene: &Scene<'_>, path: PathId) {
        let mut child = path;
        while let Some(parent) = scene.parent(child) {
            if !self.children.entry(parent).or_default().insert(child) {
                break;
            }
            child = parent;
        }
    }
    fn resolve(&mut self, scene: &Scene<'_>, path: PathId) -> Result<(), BoundsError> {
        if !scene.stage().has_prim(path) {
            return Err(BoundsError::MissingPrim(path));
        }
        if self.valid(path) {
            self.stats.hits += 1;
            return Ok(());
        }
        // Explicit postorder traversal: authored namespace depth never consumes
        // the Rust call stack. Children are filtered once per recomputed parent.
        let mut work: Vec<(PathId, Option<Vec<PathId>>)> = vec![(path, None)];
        while let Some((current, children)) = work.pop() {
            if self.valid(current) {
                continue;
            }
            let prim = PrimView::new(*scene, current);
            let hint_varying = self.options.use_extents_hint
                && scene.is_model(current)
                && prim.property_might_vary("extentsHint");
            let (bounds, varying) = if let Some(children) = children {
                // A retained reduction was entirely static when built. Only
                // dirty children's inclusion inputs can introduce variability.
                let inclusion_varies = |child: &PathId| {
                    self.inclusions
                        .get(child)
                        .is_some_and(|entry| entry.varying)
                };
                let inclusion_varying = if let Some(reduction) = self.reductions.get(&current) {
                    reduction.dirty.iter().any(inclusion_varies)
                } else {
                    scene
                        .stage()
                        .children_of(current)
                        .unwrap_or(&[])
                        .iter()
                        .any(inclusion_varies)
                };
                let (bounds, varying) = self.compute(scene, current, &children)?;
                (bounds, varying || hint_varying || inclusion_varying)
            } else {
                self.track(scene, current);
                if let Some((bounds, varying)) = self.direct_bounds(scene, current)? {
                    (bounds, hint_varying || varying)
                } else {
                    let candidates: Vec<_> = if let Some(reduction) = self.reductions.get(&current)
                    {
                        reduction.dirty.iter().copied().collect()
                    } else {
                        scene.stage().children_of(current).unwrap_or(&[]).to_vec()
                    };
                    let children: Vec<_> = candidates
                        .into_iter()
                        .filter(|&child| self.include_child(scene, child))
                        .collect();
                    let pending: Vec<_> = children
                        .iter()
                        .rev()
                        .copied()
                        .filter(|child| !self.valid(*child))
                        .collect();
                    work.push((current, Some(children)));
                    work.extend(pending.into_iter().map(|child| (child, None)));
                    continue;
                }
            };
            if varying {
                // Time changes remain O(1). Animated reductions use the normal
                // traversal, rather than maintaining a second temporal index.
                self.reductions.remove(&current);
            }
            self.promote.remove(&current);
            self.stats.computed += 1;
            self.entries.insert(
                current,
                Entry {
                    value: bounds,
                    varying,
                    epoch: self.epoch,
                },
            );
        }
        Ok(())
    }
    fn valid(&self, path: PathId) -> bool {
        self.entries
            .get(&path)
            .is_some_and(|entry| entry.valid(self.epoch))
    }
    fn include_child(&mut self, scene: &Scene<'_>, child: PathId) -> bool {
        if let Some(entry) = self.inclusions.get(&child)
            && entry.valid(self.epoch)
        {
            return entry.value;
        }
        // Even a skipped child depends on its own type/visibility/definition.
        // Index it before returning false, so ancestor invalidation reaches it.
        self.track(scene, child);
        let included = concrete_ancestry(scene, child)
            && (self.options.ignore_visibility
                || ((!scene.is_a(child, "Typed") || scene.is_a(child, "Imageable"))
                    && VisibilityInputs::read(scene, child, self.time).visibility
                        != Some(Visibility::Invisible)));
        let varying = !self.options.ignore_visibility
            && PrimView::new(*scene, child).property_might_vary("visibility");
        self.inclusions.insert(
            child,
            Entry {
                value: included,
                varying,
                epoch: self.epoch,
            },
        );
        included
    }
    fn direct_bounds(
        &self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<Option<(PurposeBounds, bool)>, BoundsError> {
        let prim = PrimView::new(*scene, path);
        if self.options.use_extents_hint
            && scene.is_model(path)
            && let Some(hint) = vectors(&prim, "extentsHint", self.time).filter(|v| v.len() >= 2)
        {
            use crate::usd_geom::ImageablePurpose::{Default, Guide, Proxy, Render};
            return Ok(Some((
                [Default, Render, Proxy, Guide]
                    .into_iter()
                    .zip(hint.as_chunks::<2>().0)
                    .map(|(purpose, extent)| (purpose, box_from_extent(extent)))
                    .collect(),
                prim.property_might_vary("extentsHint"),
            )));
        }
        if scene.is_a(path, "PointInstancer") {
            return Err(BoundsError::PointInstancerUnsupported(path));
        }
        if scene.is_a(path, "Boundable") {
            let (range, varying) = crate::extent::compute(scene, path, self.time)
                .ok_or(BoundsError::ExtentUnavailable(path))?;
            let purpose = Imageable::new(scene, path)
                .expect("Boundable is Imageable")
                .compute_purpose();
            return Ok(Some((
                vec![(
                    purpose,
                    BoundingBox {
                        range,
                        matrix: gf::IDENTITY,
                    },
                )],
                varying,
            )));
        }
        Ok(None)
    }
    fn compute(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
        children: &[PathId],
    ) -> Result<(PurposeBounds, bool), BoundsError> {
        // OpenUSD bboxCache.cpp _ResolvePrim: accumulate in nearest component
        // or subcomponent space, then retain the component-to-local matrix.
        let mut component = path;
        loop {
            if scene.kind(component).is_some_and(|kind| {
                scene.kinds().is_a(kind, crate::kind::COMPONENT)
                    || scene.kinds().is_a(kind, crate::kind::SUBCOMPONENT)
            }) {
                break;
            }
            match scene.parent(component) {
                Some(parent) => component = parent,
                None => break,
            }
        }
        let inverse_component = self.inverse_world(scene, component)?;
        let local_to_component = gf::mul(&self.world(scene, path)?, &inverse_component);
        let (component_to_local, determinant) = gf::inverse(&local_to_component);
        if determinant == 0.0 {
            return Err(BoundsError::SingularTransform(path));
        }
        let mut varying =
            self.transforms.world_might_vary(component) || self.transforms.world_might_vary(path);
        let retained = self.reductions.remove(&path);
        let incremental = retained.is_some();
        let mut reduction = retained.or_else(|| {
            self.promote
                .contains(&path)
                .then(|| Reduction::new(scene.stage().children_of(path).unwrap_or(&[])))
        });
        if let Some(reduction) = &mut reduction {
            // Leave a rebuild request behind if any later child query fails.
            self.promote.insert(path);
            reduction.clear_dirty_leaves();
        }
        let mut result: PurposeBounds = Vec::new();
        for &child in children {
            self.stats.hits += 1;
            let child_to_component = gf::mul(&self.world(scene, child)?, &inverse_component);
            varying |= self.transforms.world_might_vary(child) || self.entries[&child].varying;
            for (purpose, bbox) in &self.entries[&child].value {
                let range = bbox.transformed(&child_to_component).aligned_range();
                if let Some(reduction) = &mut reduction {
                    if !reduction.set(child, purpose, range) {
                        // A scene may author arbitrarily many purpose tokens.
                        // Bound retained storage to four arrays; fall back to
                        // the ordinary fold for this query. This retry cannot
                        // recurse again: both promotion and the tree are gone.
                        self.promote.remove(&path);
                        let all: Vec<_> = scene
                            .stage()
                            .children_of(path)
                            .unwrap_or(&[])
                            .iter()
                            .copied()
                            .filter(|&child| self.include_child(scene, child))
                            .collect();
                        return self.compute(scene, path, &all);
                    }
                    continue;
                }
                if let Some((_, held)) = result.iter_mut().find(|(p, _)| p == purpose) {
                    held.range.union_with(range);
                } else {
                    result.push((
                        purpose.clone(),
                        BoundingBox {
                            range,
                            matrix: component_to_local,
                        },
                    ));
                }
            }
        }
        if let Some(mut reduction) = reduction {
            if incremental {
                reduction.update_dirty();
            } else {
                reduction.build();
            }
            reduction.dirty.clear();
            result = reduction.bounds(component_to_local);
            self.reductions.insert(path, reduction);
        }
        Ok((result, varying))
    }
}
// UsdPrim default traversal predicates inherit undefined and abstract flags.
// Stage's predicates inspect the site's resolved specifier; fold them up the
// namespace here without changing direct queries of an authored boundable.
fn concrete_ancestry(scene: &Scene<'_>, path: PathId) -> bool {
    let mut at = path;
    while let Some(parent) = scene.parent(at) {
        if scene.stage().resolve_specifier(at, scene.store()) != Some(layerstack::Specifier::Def) {
            return false;
        }
        at = parent;
    }
    true
}

fn vectors(prim: &PrimView<'_>, name: &str, time: Time) -> Option<Vec<[f32; 3]>> {
    let read = |value: &layerstack::Value, tokens: &layerstack::TokenInterner| {
        crate::value::read_array(value, tokens, crate::value::read_float3)
    };
    // Decode the resolver's owned value directly; raw_value would clone the
    // whole array before immediately turning it into vectors.
    match time {
        Time::Default => prim.read_value(name, read),
        Time::At {
            code,
            interpolation,
        } => prim.read_value_at(name, code, interpolation, read),
    }
}
fn box_from_extent(extent: &[[f32; 3]]) -> BoundingBox {
    BoundingBox {
        range: Range3d {
            min: extent[0].map(f64::from),
            max: extent[1].map(f64::from),
        },
        matrix: gf::IDENTITY,
    }
}

// Keep this inventory with the built-in extent providers in extent.rs. Unknown
// prim edits (including kind/schema changes) always invalidate conservatively.
pub(crate) fn bounds_property(name: &str) -> bool {
    crate::xform::transform_property(name)
        || matches!(
            name,
            "extent"
                | "extentsHint"
                | "points"
                | "size"
                | "radius"
                | "height"
                | "axis"
                | "purpose"
                | "visibility"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use layerstack::{
        InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, Stage, StageOptions, Value,
    };

    #[test]
    fn wide_reductions_follow_edits_and_retire_on_structure_time_and_errors() {
        use layerstack::{
            LiveStage, PropertyPath,
            edit::{EditTarget, Transaction},
        };
        let mut store = InMemoryStore::default();
        let root = store.path("/World");
        let xform = store.tokens.intern("Xform");
        let cube = store.tokens.intern("Cube");
        let size = store.tokens.intern("size");
        let purpose = store.tokens.intern("purpose");
        let visibility = store.tokens.intern("visibility");
        let render = store.tokens.intern("render");
        let default = store.tokens.intern("default");
        let invisible = store.tokens.intern("invisible");
        let inherited = store.tokens.intern("inherited");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(root, PrimSpec::def().with_type_name(xform));
        let mut leaves = Vec::new();
        for i in 0..70 {
            let path = store.path(&alloc::format!("/World/P{i}"));
            layer.insert_prim(path, PrimSpec::def().with_type_name(cube));
            leaves.push(path);
        }
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, LayerId(1), options);
        let target = EditTarget::for_layer(LayerId(1));
        let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
        cache
            .world_bound(&Scene::new(live.stage(), &store), root)
            .unwrap();
        assert!(
            cache.reductions.is_empty(),
            "cold queries do not allocate reductions"
        );
        let check = |cache: &mut BoundsCache, live: &LiveStage, store: &InMemoryStore| {
            let scene = Scene::new(live.stage(), store);
            let mut fresh = BoundsCache::new(cache.time, cache.options.clone());
            assert_eq!(
                cache.world_bound(&scene, root),
                fresh.world_bound(&scene, root)
            );
        };
        for (field, value) in [
            (size, Value::Double(20.0)),
            (size, Value::Double(3.0)),
            (purpose, Value::Token(render)),
            (purpose, Value::Token(default)),
            (visibility, Value::Token(invisible)),
            (visibility, Value::Token(inherited)),
            (size, Value::Double(1.0)),
        ] {
            let promoted = cache.reductions.contains_key(&root);
            let hits = cache.stats.hits;
            let mut edit = Transaction::new();
            edit.set_default(target.property(PropertyPath::new(leaves[0], field)), value);
            let applied = live.apply(&mut store, &edit).unwrap();
            cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
            check(&mut cache, &live, &store);
            assert!(cache.reductions.contains_key(&root));
            if promoted {
                assert!(
                    cache.stats.hits - hits < 10,
                    "unchanged siblings are not queried"
                );
            }
        }
        // Both pending edits must contribute, including shrinking a prior max.
        let mut edit = Transaction::new();
        edit.set_default(
            target.property(PropertyPath::new(leaves[0], size)),
            Value::Double(2.0),
        );
        edit.set_default(
            target.property(PropertyPath::new(leaves[1], size)),
            Value::Double(30.0),
        );
        let applied = live.apply(&mut store, &edit).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
        check(&mut cache, &live, &store);
        let mut edit = Transaction::new();
        edit.remove_spec(target.prim(leaves[1]));
        let removed = live.apply(&mut store, &edit).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &removed.changes);
        check(&mut cache, &live, &store);
        let restored = live.apply(&mut store, &removed.inverse).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &restored.changes);
        check(&mut cache, &live, &store);
        // Unknown purposes must remain correct without one full-size tree
        // per arbitrary token. Exceeding four buckets falls back to a fold.
        let mut edit = Transaction::new();
        for (i, &leaf) in leaves.iter().take(5).enumerate() {
            let custom = store.tokens.intern(alloc::format!("custom{i}"));
            edit.set_default(
                target.property(PropertyPath::new(leaf, purpose)),
                Value::Token(custom),
            );
        }
        let many_purposes = live.apply(&mut store, &edit).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &many_purposes.changes);
        check(&mut cache, &live, &store);
        assert!(cache.reductions.is_empty());
        let restored = live.apply(&mut store, &many_purposes.inverse).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &restored.changes);
        check(&mut cache, &live, &store);
        // Failed geometry must not leave a half-updated reduction reusable.
        let mut edit = Transaction::new();
        edit.remove_spec(target.prim(leaves[0]));
        edit.create_prim(
            target.prim(leaves[0]),
            layerstack::Specifier::Def,
            Some(store.tokens.intern("Mesh")),
        );
        let bad = live.apply(&mut store, &edit).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &bad.changes);
        for _ in 0..2 {
            check(&mut cache, &live, &store);
        }
        let fixed = live.apply(&mut store, &bad.inverse).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &fixed.changes);
        check(&mut cache, &live, &store);
        // A newly temporal input retires the static reduction.
        let mut edit = Transaction::new();
        for (time, size_value) in [(0.0, 4.0), (2.0, 40.0)] {
            edit.set_time_sample(
                target.property(PropertyPath::new(leaves[0], size)),
                time,
                Value::Double(size_value),
            );
        }
        let animated = live.apply(&mut store, &edit).unwrap();
        cache.apply_changes(&Scene::new(live.stage(), &store), &animated.changes);
        for time in [Time::at(0.0), Time::at(2.0), Time::Default] {
            cache.set_time(time);
            check(&mut cache, &live, &store);
            assert!(cache.reductions.is_empty());
        }
    }

    #[test]
    fn relative_queries_report_missing_and_singular_frames() {
        let mut store = InMemoryStore::default();
        let path = store.path("/Mesh");
        let missing = store.path("/Missing");
        let mesh = store.tokens.intern("Mesh");
        let extent = store.tokens.intern("extent");
        let scale = store.tokens.intern("xformOp:scale");
        let order = store.tokens.intern("xformOpOrder");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            path,
            PrimSpec::def()
                .with_type_name(mesh)
                .with_property(
                    extent,
                    PropertySpec::attribute().with_default(Value::Array(vec![
                        Value::Vec3f([-1.0; 3]),
                        Value::Vec3f([1.0; 3]),
                    ])),
                )
                .with_property(
                    scale,
                    PropertySpec::attribute().with_default(Value::Vec3d([0.0; 3])),
                )
                .with_property(
                    order,
                    PropertySpec::attribute().with_default(Value::Array(vec![Value::Token(scale)])),
                ),
        );
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        let stage = Stage::compose(&mut store, LayerId(1), options);
        let scene = Scene::new(&stage, &store);
        let mut cache = BoundsCache::new(Time::Default, BoundsOptions::default());
        assert_eq!(
            cache.relative_bound(&scene, path, missing),
            Err(BoundsError::MissingPrim(missing))
        );
        assert_eq!(
            cache.relative_bound(&scene, missing, path),
            Err(BoundsError::MissingPrim(missing))
        );
        assert_eq!(
            cache.relative_bound(&scene, path, path),
            Err(BoundsError::SingularTransform(path))
        );
        assert!(
            cache
                .transforms
                .relative_transform(&scene, path, missing)
                .is_none()
        );
        let relative = cache
            .transforms
            .relative_transform(&scene, path, path)
            .unwrap();
        assert_eq!(relative.matrix, gf::IDENTITY, "local walk needs no inverse");
        assert!(!relative.resets_xform_stack);
    }

    #[test]
    fn temporal_reuse_preserves_single_samples_errors_and_epoch_wrap() {
        let mut store = InMemoryStore::default();
        let root = store.path("/World");
        let static_leaf = store.path("/World/Static");
        let animated = store.path("/World/Animated");
        let xform = store.tokens.intern("Xform");
        let mesh = store.tokens.intern("Mesh");
        let extent = store.tokens.intern("extent");
        let value =
            |size: f32| Value::Array(vec![Value::Vec3f([-size; 3]), Value::Vec3f([size; 3])]);
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(root, PrimSpec::def().with_type_name(xform));
        layer.insert_prim(
            static_leaf,
            PrimSpec::def()
                .with_type_name(mesh)
                .with_property(extent, PropertySpec::attribute().with_default(value(1.0))),
        );
        layer.insert_prim(
            animated,
            PrimSpec::def().with_type_name(mesh).with_property(
                extent,
                PropertySpec::attribute()
                    .with_default(Value::Blocked)
                    .with_time_samples(vec![(0.0, value(2.0))]),
            ),
        );
        store.insert_layer(layer);
        let options = StageOptions {
            schemas: Some(Arc::new(crate::openusd(&mut store.tokens))),
            ..StageOptions::default()
        };
        let stage = Stage::compose(&mut store, LayerId(1), options);
        let scene = Scene::new(&stage, &store);
        let mut cache = BoundsCache::new(Time::at(0.0), BoundsOptions::default());
        assert_eq!(cache.world_bound(&scene, root).unwrap().range.max, [2.0; 3]);
        assert_eq!(cache.stats.computed, 3);
        cache.set_time(Time::at(1.0));
        cache.world_bound(&scene, root).unwrap();
        assert_eq!(cache.stats.computed, 5, "static sibling is retained");
        cache.set_time(Time::Default);
        assert_eq!(
            cache.world_bound(&scene, root),
            Err(BoundsError::ExtentUnavailable(animated))
        );
        // A failed evaluation must not make a stale parent current.
        assert_eq!(
            cache.world_bound(&scene, root),
            Err(BoundsError::ExtentUnavailable(animated))
        );
        cache.set_time(Time::at(0.0));
        assert_eq!(cache.world_bound(&scene, root).unwrap().range.max, [2.0; 3]);
        cache.epoch = u64::MAX;
        cache.set_time(Time::at(1.0));
        assert!(cache.is_empty());
        assert_eq!(cache.world_bound(&scene, root).unwrap().range.max, [2.0; 3]);
    }
}
