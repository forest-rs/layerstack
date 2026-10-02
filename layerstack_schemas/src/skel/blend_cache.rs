// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained morph-only bindings; no skeleton topology or skinning dependency.
use super::{
    BlendShapeContribution, BlendShapeQuery, SkelError, cache::Dependencies, inherited_target,
    invalid, length, read, tokens,
};
use crate::{PrimView, Scene, Time};
use alloc::{string::String, vec::Vec};
use layerstack::{Changes, HashMap, HashSet, PathId};

/// Borrowed morph definitions and mapped animation weights at an explicit time.
/// No points or normals are read. Compare revisions only within the same cache;
/// copy/pack data before mutating it. Definitions, precision and buffers remain
/// separate from renderer resources and previous-frame history.
#[derive(Clone, Copy, Debug)]
pub struct BlendShapeInputs<'a> {
    query: &'a BlendShapeQuery,
    weights: &'a [f32],
    contributions: &'a [BlendShapeContribution],
    definition_revision: u64,
    weight_revision: u64,
    time: Time,
}
impl BlendShapeInputs<'_> {
    /// Local dense/sparse definitions and sample knots.
    #[must_use]
    pub fn blend_shapes(&self) -> &BlendShapeQuery {
        self.query
    }
    /// Animation weights in binding shape-name order; unmapped names use zero.
    #[must_use]
    pub fn weights(&self) -> &[f32] {
        self.weights
    }
    /// Evaluated inbetween sample contributions.
    #[must_use]
    pub fn contributions(&self) -> &[BlendShapeContribution] {
        self.contributions
    }
    /// Conservative captured-definition stamp, never reused after `clear`.
    #[must_use]
    pub fn definition_revision(&self) -> u64 {
        self.definition_revision
    }
    /// Conservative weight/contribution stamp, independent of vertex buffers.
    #[must_use]
    pub fn weight_revision(&self) -> u64 {
        self.weight_revision
    }
    /// Explicit time and interpolation policy used for the weights.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }
    /// Validates every shape sample against an adapter-owned point buffer.
    pub fn validate_point_count(&self, count: usize) -> Result<(), SkelError> {
        self.query.validate_point_count(count)
    }
}
/// Cumulative work for morph-only retained evaluation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlendShapeCacheStats {
    /// Shape binding snapshots constructed.
    pub definition_builds: u64,
    /// Mapped animation weights/contributions evaluated, including failures.
    pub weight_evaluations: u64,
    /// Output queries satisfied by retained arrays.
    pub hits: u64,
    /// Output queries requiring recomputation, including failures.
    pub misses: u64,
    /// Point vertices submitted to shape accumulation.
    pub point_vertices: u64,
    /// Normal vectors submitted to shape accumulation.
    pub normal_vectors: u64,
    /// Retained bindings invalidated by edits.
    pub invalidations: u64,
}
/// Retained morph array occupancy; excludes definitions and container overhead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlendShapeCacheMemory {
    /// Retained geometry bindings.
    pub bindings: usize,
    /// Initialized array payload bytes.
    pub used_bytes: usize,
    /// Reserved array payload bytes.
    pub capacity_bytes: usize,
}
struct Binding {
    query: BlendShapeQuery,
    animation: Option<PathId>,
    order: Vec<String>,
    mapping: Vec<Option<usize>>,
    definitions: Dependencies,
    weight_dependencies: Dependencies,
    point_dependencies: Dependencies,
    normal_dependencies: Dependencies,
    definition_revision: u64,
    weight_revision: u64,
    weights: Vec<f32>,
    contributions: Vec<BlendShapeContribution>,
    weight_valid: bool,
    weight_epoch: u64,
    points: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    point_valid: bool,
    normal_valid: bool,
    point_epoch: u64,
    normal_epoch: u64,
    weights_varying: bool,
    points_varying: bool,
    normals_varying: bool,
}
impl Binding {
    fn temporal(&mut self, scene: &Scene<'_>) {
        self.weights_varying = self
            .animation
            .is_some_and(|p| PrimView::new(*scene, p).property_might_vary("blendShapeWeights"));
        let prim = PrimView::new(*scene, self.query.geometry_path());
        self.points_varying = self.weights_varying || prim.property_might_vary("points");
        self.normals_varying = self.weights_varying
            || ["normals", "points", "faceVertexIndices"]
                .iter()
                .any(|n| prim.property_might_vary(n));
    }
}
/// Retains standalone blend-shape bindings and outputs for one stage/store pair.
/// `skel:animationSource` inherits from the geometry's applied binding APIs,
/// including empty relationships and forwarding. No skeleton or joint influences
/// are required. Pass every successful edit (including undo) to `apply_changes`;
/// clear before switching scenes. Normal results stay in geometry space without
/// normalization. AOUSD Core §12.3–12.5; `UsdSkelBlendShapeQuery` accumulation.
pub struct BlendShapeCache {
    time: Time,
    epoch: u64,
    next_revision: u64,
    bindings: HashMap<PathId, Binding>,
    stats: BlendShapeCacheStats,
}
impl core::fmt::Debug for BlendShapeCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BlendShapeCache")
            .field("time", &self.time)
            .field("stats", &self.stats)
            .field("memory", &self.memory())
            .finish_non_exhaustive()
    }
}
impl BlendShapeCache {
    /// Creates an empty cache at an explicit time.
    #[must_use]
    pub fn new(time: Time) -> Self {
        Self {
            time,
            epoch: 1,
            next_revision: 1,
            bindings: HashMap::new(),
            stats: BlendShapeCacheStats::default(),
        }
    }
    /// Current time and interpolation policy.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }
    /// Changes time in constant work; static arrays remain valid.
    pub fn set_time(&mut self, time: Time) {
        if self.time != time {
            self.time = time;
            self.epoch = self.epoch.wrapping_add(1);
            if self.epoch == 0 {
                self.clear();
                self.epoch = 1;
            }
        }
    }
    /// Drops definitions and arrays, preserving time, counters and revision allocation.
    pub fn clear(&mut self) {
        self.bindings.clear();
    }
    /// Cumulative work counters.
    #[must_use]
    pub fn stats(&self) -> BlendShapeCacheStats {
        self.stats
    }
    /// Resets work counters without invalidating retained entries.
    pub fn reset_stats(&mut self) {
        self.stats = BlendShapeCacheStats::default();
    }
    /// Retained array payload occupancy, excluding definitions and hash buckets.
    #[must_use]
    pub fn memory(&self) -> BlendShapeCacheMemory {
        let mut m = BlendShapeCacheMemory {
            bindings: self.bindings.len(),
            ..BlendShapeCacheMemory::default()
        };
        fn add<T>(m: &mut BlendShapeCacheMemory, v: &Vec<T>) {
            m.used_bytes += v.len() * size_of::<T>();
            m.capacity_bytes += v.capacity() * size_of::<T>();
        }
        for b in self.bindings.values() {
            add(&mut m, &b.mapping);
            add(&mut m, &b.weights);
            add(&mut m, &b.contributions);
            add(&mut m, &b.points);
            add(&mut m, &b.normals);
        }
        m
    }
    fn revision(&mut self) -> u64 {
        let r = self.next_revision;
        self.next_revision = r
            .checked_add(1)
            .expect("blend-shape revision space exhausted");
        r
    }
    fn ensure_binding(&mut self, scene: &Scene<'_>, path: PathId) -> Result<bool, SkelError> {
        if self.bindings.contains_key(&path) {
            return Ok(true);
        }
        if !scene.stage().has_prim(path) {
            return Ok(false);
        }
        let Some(query) = BlendShapeQuery::new(scene, path)? else {
            return Ok(false);
        };
        let animation = inherited_target(scene, path, "skel:animationSource", "SkelAnimation")?;
        let order = animation
            .and_then(|p| tokens(&PrimView::new(*scene, p), "blendShapes"))
            .unwrap_or_default();
        let mut seen = HashSet::new();
        if order.iter().any(|n| !seen.insert(n)) {
            return Err(invalid(
                animation.expect("animation with order"),
                "blendShapes",
            ));
        }
        drop(seen);
        let mapping = query
            .names()
            .iter()
            .map(|n| order.iter().position(|s| s == n))
            .collect();
        let mut definitions = Dependencies::default();
        definitions.add(path, &["skel:blendShapes", "skel:blendShapeTargets"]);
        definitions.ancestry(scene, path, &["skel:animationSource"]);
        let mut parent = Some(path);
        while let Some(p) = parent {
            for name in ["skel:animationSource", "skel:blendShapeTargets"] {
                if let Some(property) = PrimView::new(*scene, p).property_path(name) {
                    definitions.relationship(scene, property);
                }
            }
            parent = scene.parent(p);
        }
        for &target in query.targets() {
            definitions.add(target, &[]);
        }
        if let Some(animation) = animation {
            definitions.add(animation, &["blendShapes"]);
        }
        let mut weight_dependencies = Dependencies::default();
        if let Some(p) = animation {
            weight_dependencies.add(p, &["blendShapeWeights"]);
        }
        let mut point_dependencies = Dependencies::default();
        point_dependencies.add(path, &["points"]);
        let mut normal_dependencies = Dependencies::default();
        normal_dependencies.add(path, &["normals", "points", "faceVertexIndices"]);
        let mut b = Binding {
            query,
            animation,
            order,
            mapping,
            definitions,
            weight_dependencies,
            point_dependencies,
            normal_dependencies,
            definition_revision: self.revision(),
            weight_revision: 0,
            weights: Vec::new(),
            contributions: Vec::new(),
            weight_valid: false,
            weight_epoch: 0,
            points: Vec::new(),
            normals: Vec::new(),
            point_valid: false,
            normal_valid: false,
            point_epoch: 0,
            normal_epoch: 0,
            weights_varying: false,
            points_varying: false,
            normals_varying: false,
        };
        b.temporal(scene);
        self.bindings.insert(path, b);
        self.stats.definition_builds += 1;
        Ok(true)
    }
    fn ensure_weights(&mut self, scene: &Scene<'_>, path: PathId) -> Result<(), SkelError> {
        let b = &self.bindings[&path];
        if b.weight_valid && (!b.weights_varying || b.weight_epoch == self.epoch) {
            return Ok(());
        }
        let revision = self.revision();
        let b = self
            .bindings
            .get_mut(&path)
            .expect("prepared shape binding");
        self.stats.weight_evaluations += 1;
        let raw = b.animation.and_then(|p| {
            read(
                &PrimView::new(*scene, p),
                "blendShapeWeights",
                self.time,
                crate::value::read_float_array,
            )
        });
        if let Some(weights) = &raw {
            length(
                b.animation.expect("animated weights"),
                "blendShapeWeights",
                b.order.len(),
                weights.len(),
            )?;
        }
        b.weights.clear();
        b.weights.extend(
            b.mapping
                .iter()
                .map(|i| raw.as_ref().zip(*i).map_or(0., |(w, i)| w[i])),
        );
        let contributions = b.query.compute_weights(&b.weights)?;
        b.contributions.clear();
        b.contributions.extend(contributions);
        b.weight_revision = revision;
        b.weight_epoch = self.epoch;
        b.weight_valid = true;
        Ok(())
    }
    /// Resolves morph inputs without reading points/normals or skeletal data.
    /// Missing local shape bindings return `None` and are retried after edits.
    pub fn inputs(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<Option<BlendShapeInputs<'_>>, SkelError> {
        if !self.ensure_binding(scene, path)? {
            return Ok(None);
        }
        self.ensure_weights(scene, path)?;
        let b = &self.bindings[&path];
        Ok(Some(BlendShapeInputs {
            query: &b.query,
            weights: &b.weights,
            contributions: &b.contributions,
            definition_revision: b.definition_revision,
            weight_revision: b.weight_revision,
            time: self.time,
        }))
    }
    /// Shape-deformed sampled points in geometry space, without joint skinning.
    pub fn deformed_points(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<Option<&[[f32; 3]]>, SkelError> {
        self.output(scene, path, false)
    }
    /// Shape-deformed normals in geometry space, without normalization. Supports
    /// vertex/varying and mesh face-varying normals; constant normals reject
    /// active point-indexed offsets. Uniform normals return an explicit error.
    pub fn deformed_normals(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
    ) -> Result<Option<&[[f32; 3]]>, SkelError> {
        self.output(scene, path, true)
    }
    fn output(
        &mut self,
        scene: &Scene<'_>,
        path: PathId,
        normals: bool,
    ) -> Result<Option<&[[f32; 3]]>, SkelError> {
        if !self.ensure_binding(scene, path)? {
            return Ok(None);
        }
        let b = &self.bindings[&path];
        let (valid, epoch, varying) = if normals {
            (b.normal_valid, b.normal_epoch, b.normals_varying)
        } else {
            (b.point_valid, b.point_epoch, b.points_varying)
        };
        if valid && (!varying || epoch == self.epoch) {
            self.stats.hits += 1;
            let b = &self.bindings[&path];
            return Ok(Some(if normals { &b.normals } else { &b.points }));
        }
        self.stats.misses += 1;
        self.ensure_weights(scene, path)?;
        let prim = PrimView::new(*scene, path);
        let name = if normals { "normals" } else { "points" };
        let values = read(&prim, name, self.time, crate::value::read_float3_array)
            .ok_or_else(|| invalid(path, name))?;
        let b = self
            .bindings
            .get_mut(&path)
            .expect("prepared shape binding");
        if normals {
            b.normal_valid = false;
        } else {
            b.point_valid = false;
        }
        let output = if normals {
            &mut b.normals
        } else {
            &mut b.points
        };
        output.clear();
        output.extend(values);
        let interpolation = prim
            .property_metadata("normals")
            .and_then(|m| m.interpolation())
            .unwrap_or("vertex");
        if normals && interpolation == "faceVarying" && scene.is_a(path, "Mesh") {
            let points = read(&prim, "points", self.time, crate::value::read_float3_array)
                .ok_or_else(|| invalid(path, "points"))?;
            let corners = read(
                &prim,
                "faceVertexIndices",
                self.time,
                crate::value::read_int_array,
            )
            .ok_or_else(|| invalid(path, "faceVertexIndices"))?;
            b.query
                .deform_corner_normals(&b.contributions, points.len(), &corners, output)?;
        } else {
            if normals
                && (!matches!(interpolation, "vertex" | "varying" | "constant")
                    || (interpolation == "constant"
                        && b.query.has_normal_contributions(&b.contributions)))
            {
                return Err(invalid(path, "normals"));
            }
            b.query
                .deform_contributions(&b.contributions, output, normals)?;
        }
        if normals {
            self.stats.normal_vectors += u64::try_from(output.len()).unwrap_or(u64::MAX);
            b.normal_valid = true;
            b.normal_epoch = self.epoch;
        } else {
            self.stats.point_vertices += u64::try_from(output.len()).unwrap_or(u64::MAX);
            b.point_valid = true;
            b.point_epoch = self.epoch;
        }
        Ok(Some(output))
    }
    /// Applies precise edit invalidation, including forwarded sources, shape
    /// definition changes, removals and undo. Weight-only edits preserve definitions.
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &Changes) {
        self.bindings.retain(|_, b| {
            let shape_changed = changes
                .changed_info_only
                .iter()
                .any(|p| b.query.targets().contains(p));
            if b.definitions.changed(scene, changes) || shape_changed {
                self.stats.invalidations += 1;
                return false;
            }
            let weights = b.weight_dependencies.changed(scene, changes);
            let points = b.point_dependencies.changed(scene, changes) || weights;
            let normals = b.normal_dependencies.changed(scene, changes) || weights;
            if weights {
                b.weight_valid = false;
            }
            if points {
                b.point_valid = false;
            }
            if normals {
                b.normal_valid = false;
            }
            if points || normals {
                self.stats.invalidations += 1;
            }
            b.temporal(scene);
            true
        });
    }
}
