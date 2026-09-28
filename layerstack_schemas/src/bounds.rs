// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bounds from authored extents, with component-space accumulation.
//!
//! This deliberately bounded implementation does not run procedural extent
//! plugins: a boundable without a valid authored extent returns an error.
//! It preserves an oriented range and matrix, rather than repeatedly aligning
//! boxes in every ancestor's coordinates. OpenUSD: `UsdGeomBBoxCache`.

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

/// Caller-owned bounds at one time and for one scene.
///
/// Pass each successful live change report to [`Self::apply_changes`]. For
/// manual edits affecting geometry, transforms, purpose, visibility, kinds or
/// topology, [`Self::invalidate`] evicts cached descendants and ancestors by
/// indexed dependencies. It does not observe edits automatically. Clear between
/// unrelated scenes. Extent providers and point instancers are not implemented;
/// missing geometry is an error, never a silently incomplete bound.
///
/// Child inclusion follows OpenUSD's defined, non-abstract, imageable/unknown
/// type traversal and local visibility. A query includes its root even when
/// invisible. Authored boundable extents and model hints prune descendants.
#[derive(Clone, Debug)]
pub struct BoundsCache {
    time: Time,
    options: BoundsOptions,
    entries: HashMap<PathId, PurposeBounds>,
    // Inclusion is an input of the parent reduction, including when false.
    // Keep it independently of bounds so excluded subtrees need no evaluation.
    inclusions: HashMap<PathId, bool>,
    children: HashMap<PathId, HashSet<PathId>>,
    transforms: XformCache,
    stats: BoundsStats,
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
            stats: BoundsStats::default(),
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
        self.stats = BoundsStats::default();
    }
    /// Change time and clear cached results if it differs.
    pub fn set_time(&mut self, time: Time) {
        if self.time != time {
            self.clear();
            self.time = time;
            self.transforms.set_time(time);
        }
    }
    /// Evict `path`, cached descendants, and ancestors whose bounds include it.
    ///
    /// Deleted paths may be passed: their interned namespace is sufficient.
    /// Visits indexed descendants and namespace ancestors, rather than scanning
    /// every cached prim.
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
        for &path in changes.resynced.iter().chain(&changes.changed_info_only) {
            self.invalidate_bounds(scene, path);
        }
    }

    fn invalidate_bounds(&mut self, scene: &Scene<'_>, path: PathId) {
        let mut pending = vec![path];
        while let Some(at) = pending.pop() {
            self.stats.invalidated += usize::from(self.entries.remove(&at).is_some());
            self.inclusions.remove(&at);
            if let Some(children) = self.children.remove(&at) {
                pending.extend(children);
            }
        }
        if let Some(parent) = scene.parent(path)
            && let Some(children) = self.children.get_mut(&parent)
        {
            children.remove(&path);
        }
        let mut at = scene.parent(path);
        while let Some(path) = at {
            self.stats.invalidated += usize::from(self.entries.remove(&path).is_some());
            at = scene.parent(path);
        }
    }
    /// Bound in the queried prim's own coordinates, retaining its orientation.
    pub fn untransformed_bound(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<BoundingBox, BoundsError> {
        self.resolve(scene, path)?;
        let bounds = &self.entries[&path];
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
        if self.entries.contains_key(&path) {
            self.stats.hits += 1;
            return Ok(());
        }
        // Explicit postorder traversal: authored namespace depth never consumes
        // the Rust call stack. Children are filtered once per recomputed parent.
        let mut work: Vec<(PathId, Option<Vec<PathId>>)> = vec![(path, None)];
        while let Some((current, children)) = work.pop() {
            if self.entries.contains_key(&current) {
                continue;
            }
            let bounds = if let Some(children) = children {
                self.compute(scene, current, &children)?
            } else {
                self.track(scene, current);
                if let Some(bounds) = self.direct_bounds(scene, current)? {
                    bounds
                } else {
                    let children: Vec<_> = scene
                        .stage()
                        .children_of(current)
                        .unwrap_or(&[])
                        .iter()
                        .copied()
                        .filter(|&child| self.include_child(scene, child))
                        .collect();
                    let pending: Vec<_> = children
                        .iter()
                        .rev()
                        .copied()
                        .filter(|child| !self.entries.contains_key(child))
                        .collect();
                    work.push((current, Some(children)));
                    work.extend(pending.into_iter().map(|child| (child, None)));
                    continue;
                }
            };
            self.stats.computed += 1;
            self.entries.insert(current, bounds);
        }
        Ok(())
    }
    fn include_child(&mut self, scene: &Scene<'_>, child: PathId) -> bool {
        if let Some(&included) = self.inclusions.get(&child) {
            return included;
        }
        // Even a skipped child depends on its own type/visibility/definition.
        // Index it before returning false, so ancestor invalidation reaches it.
        self.track(scene, child);
        let included = concrete_ancestry(scene, child)
            && (self.options.ignore_visibility
                || ((!scene.is_a(child, "Typed") || scene.is_a(child, "Imageable"))
                    && VisibilityInputs::read(scene, child, self.time).visibility
                        != Some(Visibility::Invisible)));
        self.inclusions.insert(child, included);
        included
    }
    fn direct_bounds(
        &self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<Option<PurposeBounds>, BoundsError> {
        let prim = PrimView::new(*scene, path);
        if self.options.use_extents_hint
            && scene.is_model(path)
            && let Some(hint) =
                vectors(scene, &prim, "extentsHint", self.time).filter(|v| v.len() >= 2)
        {
            use crate::usd_geom::ImageablePurpose::{Default, Guide, Proxy, Render};
            return Ok(Some(
                [Default, Render, Proxy, Guide]
                    .into_iter()
                    .zip(hint.as_chunks::<2>().0)
                    .map(|(purpose, extent)| (purpose, box_from_extent(extent)))
                    .collect(),
            ));
        }
        if scene.is_a(path, "PointInstancer") {
            return Err(BoundsError::PointInstancerUnsupported(path));
        }
        if scene.is_a(path, "Boundable") {
            let extent = vectors(scene, &prim, "extent", self.time)
                .filter(|v| v.len() == 2)
                .ok_or(BoundsError::ExtentUnavailable(path))?;
            let purpose = Imageable::new(scene, path)
                .expect("Boundable is Imageable")
                .compute_purpose();
            return Ok(Some(vec![(purpose, box_from_extent(&extent))]));
        }
        Ok(None)
    }
    fn compute(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
        children: &[PathId],
    ) -> Result<PurposeBounds, BoundsError> {
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
        let mut result: PurposeBounds = Vec::new();
        for &child in children {
            self.stats.hits += 1;
            let child_to_component = gf::mul(&self.world(scene, child)?, &inverse_component);
            for (purpose, bbox) in &self.entries[&child] {
                let range = bbox.transformed(&child_to_component).aligned_range();
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
        Ok(result)
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

fn vectors(
    scene: &Scene<'_>,
    prim: &PrimView<'_>,
    name: &str,
    time: Time,
) -> Option<Vec<[f32; 3]>> {
    crate::value::read_array(
        &prim.raw_value(name, time)?,
        scene.store().tokens(),
        crate::value::read_float3,
    )
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
