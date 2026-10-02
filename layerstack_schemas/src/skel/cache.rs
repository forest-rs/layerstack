// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit retained evaluation for one stage/store pair.
use super::dual_quaternion::Palette;
use super::normals::{NormalMatrix, inverse_transpose};
use super::skinning::{SkinningDefinition, SkinningInputs};
use super::{BlendShapeContribution, DeformationInputs, DeformationRevisions, SkinningMethod};
use super::{SkelError, SkeletonDefinition, SkinningQuery, invalid, read, skin_points_in_place};
use crate::{PrimView, Scene, Time, gf, usd_skel::Skeleton};
use alloc::{sync::Arc, vec::Vec};
use layerstack::{Changes, HashMap, HashSet, PathId, PropertyPath, TargetPath};

/// Cumulative retained evaluator work counters. Clearing entries does not reset
/// counters; use [`SkelCache::reset_stats`] to start a measurement window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkelCacheStats {
    /// Geometry output queries satisfied by retained arrays.
    pub hits: u64,
    /// Geometry output arrays recomputed, including failed computations.
    pub misses: u64,
    /// Skeleton definition snapshots constructed.
    pub skeleton_builds: u64,
    /// Geometry binding snapshots constructed.
    pub binding_builds: u64,
    /// Joint pose palettes evaluated (one per skeleton/time when needed).
    pub pose_evaluations: u64,
    /// Bind matrices inverted while preparing retained palettes.
    pub inverse_bind_matrices: u64,
    /// Influence array pairs resolved/flattened.
    pub influence_resolutions: u64,
    /// Shared animation weight arrays evaluated, including failed evaluations.
    pub blend_weight_evaluations: u64,
    /// Binding-order weight/contribution arrays evaluated, including failures.
    pub blend_shape_evaluations: u64,
    /// Joint inverse-transpose matrices prepared for normal palettes.
    pub normal_matrices: u64,
    /// Joint DQS decompositions prepared for shared point/normal palettes.
    pub dual_quaternion_joints: u64,
    /// Point vertices submitted to the deformation kernel.
    pub point_vertices: u64,
    /// Normal vectors submitted to the normal kernel.
    pub normal_vectors: u64,
    /// Deformed mesh point hulls reduced, excluding retained-bound hits.
    pub bound_evaluations: u64,
    /// Deformed points submitted to bound reduction.
    pub bound_vertices: u64,
    /// Retained entries affected by explicit edit invalidation.
    pub invalidations: u64,
}
/// Retained array payload occupancy. Bytes include matrix, influence, weight and
/// output arrays; exclude definitions, strings, hash buckets and Arc headers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkelCacheMemory {
    /// Retained skeleton definitions.
    pub skeletons: usize,
    /// Retained geometry bindings.
    pub bindings: usize,
    /// Initialized array elements, in bytes.
    pub used_bytes: usize,
    /// Reserved array capacity, in bytes, including unused buffer capacity.
    pub capacity_bytes: usize,
}
#[derive(Clone, Debug)]
enum Field {
    Named(PathId, &'static str),
    Forwarded(PropertyPath),
}
#[derive(Clone, Debug, Default)]
pub(super) struct Dependencies {
    roots: Vec<PathId>,
    fields: Vec<Field>,
}
impl Dependencies {
    pub(super) fn add(&mut self, path: PathId, names: &[&'static str]) {
        if !self.roots.contains(&path) {
            self.roots.push(path);
        }
        self.fields
            .extend(names.iter().map(|&name| Field::Named(path, name)));
    }
    pub(super) fn ancestry(&mut self, scene: &Scene<'_>, mut path: PathId, names: &[&'static str]) {
        loop {
            self.add(path, names);
            let Some(parent) = scene.parent(path) else {
                break;
            };
            path = parent;
        }
    }
    pub(super) fn relationship(&mut self, scene: &Scene<'_>, start: PropertyPath) {
        let mut pending = alloc::vec![start];
        let mut visited = HashSet::new();
        while let Some(property) = pending.pop() {
            if !visited.insert(property) {
                continue;
            }
            self.add(property.prim_path(), &[]);
            self.fields.push(Field::Forwarded(property));
            if let Some(targets) = scene.stage().resolve_target_list_path(property) {
                pending.extend(targets.value.into_iter().filter_map(|t| match t {
                    TargetPath::Property(p) => Some(p),
                    _ => None,
                }));
            }
        }
    }
    fn bindings(&mut self, scene: &Scene<'_>, mut path: PathId) {
        loop {
            self.add(
                path,
                &[
                    "skel:skeleton",
                    "skel:animationSource",
                    "skel:joints",
                    "primvars:skel:jointIndices",
                    "primvars:skel:jointWeights",
                    "primvars:skel:jointIndices:indices",
                    "primvars:skel:jointWeights:indices",
                    "primvars:skel:geomBindTransform",
                    "primvars:skel:skinningMethod",
                ],
            );
            for name in ["skel:skeleton", "skel:animationSource"] {
                if let Some(p) = PrimView::new(*scene, path).property_path(name) {
                    self.relationship(scene, p);
                }
            }
            let Some(parent) = scene.parent(path) else {
                break;
            };
            path = parent;
        }
    }
    fn resynced(&self, scene: &Scene<'_>, changes: &Changes) -> bool {
        changes.resynced.iter().any(|&changed| {
            let changed = scene.store().paths().resolve(changed);
            self.roots
                .iter()
                .any(|&p| changed.is_prefix_of(scene.store().paths().resolve(p)))
        })
    }
    pub(super) fn changed(&self, scene: &Scene<'_>, changes: &Changes) -> bool {
        self.resynced(scene, changes)
            || changes.changed_info_only.iter().any(|&path| {
                self.roots.contains(&path)
                    && changes.properties_for(path).is_none_or(|fields| {
                        fields.iter().any(|changed| {
                            self.fields.iter().any(|f| match f {
                                Field::Named(p, name) => {
                                    *p == path
                                        && scene.store().tokens().resolve(changed.name) == *name
                                }
                                Field::Forwarded(p) => {
                                    p.prim_path() == path && p.property() == changed.name
                                }
                            })
                        })
                    })
            })
    }
}
struct Rig {
    definition_revision: u64,
    pose_revision: u64,
    weight_revision: u64,
    definition: Arc<SkeletonDefinition>,
    definitions: Dependencies,
    pose_dependencies: Dependencies,
    weight_dependencies: Dependencies,
    inverse: Option<Result<Vec<gf::Matrix4>, SkelError>>,
    palette: Option<Result<Vec<gf::Matrix4>, SkelError>>,
    normals: Option<Vec<Result<NormalMatrix, SkelError>>>,
    dual_points: Option<Palette>,
    dual_normals: Option<Palette>,
    weights: Option<Result<Vec<f32>, SkelError>>,
    pose_epoch: u64,
    weight_epoch: u64,
    pose_version: u64,
    pose_varying: bool,
    weights_varying: bool,
}
impl Rig {
    fn temporal(&mut self, scene: &Scene<'_>) {
        self.pose_varying = self.definition.animation_path.is_some_and(|p| {
            ["translations", "rotations", "scales"]
                .iter()
                .any(|name| PrimView::new(*scene, p).property_might_vary(name))
        });
        self.weights_varying = self
            .definition
            .animation_path
            .is_some_and(|p| PrimView::new(*scene, p).property_might_vary("blendShapeWeights"));
    }
}
struct Binding {
    definition_revision: u64,
    input_revision: u64,
    shape_revision: u64,
    shape_weight_revision: u64,
    shape_contributions: Vec<BlendShapeContribution>,
    definition: Arc<SkinningDefinition>,
    definitions: Dependencies,
    point_dependencies: Dependencies,
    normal_dependencies: Dependencies,
    inputs: Option<SkinningInputs>,
    input_epoch: u64,
    pose_version: u64,
    inputs_varying: bool,
    points_varying: bool,
    normals_varying: bool,
    points: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    point_valid: bool,
    normal_valid: bool,
    point_epoch: u64,
    normal_epoch: u64,
    normal_transforms: Vec<NormalMatrix>,
    normal_bind: Option<NormalMatrix>,
    normal_palette_revision: (u64, u64),
    normal_dual_quaternions: Vec<super::DualQuaternionJoint>,
    deformed_normals: Vec<[f32; 3]>,
    deformed_normal_valid: bool,
    deformed_normal_epoch: u64,
    deformed_bounds: Option<crate::bounds::Range3d>,
    shape_mapping: Vec<Option<usize>>,
    shape_weights: Vec<f32>,
}
impl Binding {
    fn temporal(&mut self, scene: &Scene<'_>, rig: &Rig) {
        let d = &self.definition;
        self.inputs_varying = [
            (Some(d.indices), "primvars:skel:jointIndices"),
            (Some(d.indices), "primvars:skel:jointIndices:indices"),
            (Some(d.weights), "primvars:skel:jointWeights"),
            (Some(d.weights), "primvars:skel:jointWeights:indices"),
            (d.geom_bind, "primvars:skel:geomBindTransform"),
        ]
        .iter()
        .any(|&(path, name)| {
            path.is_some_and(|p| PrimView::new(*scene, p).property_might_vary(name))
        });
        let prim = PrimView::new(*scene, d.geometry);
        self.points_varying = self.inputs_varying
            || rig.pose_varying
            || prim.property_might_vary("points")
            || (d.blend_shapes.is_some() && rig.weights_varying);
        self.normals_varying = self.inputs_varying
            || rig.pose_varying
            || ["normals", "points", "faceVertexIndices"]
                .iter()
                .any(|name| prim.property_might_vary(name));
    }
}

/// Retains skeletal definitions, shared pose palettes, influence arrays and
/// geometry buffers for one stage/store pair. It owns no scene borrow.
/// Pass every successful edit report to [`Self::apply_changes`] before queries;
/// call [`Self::clear`] before switching to another stage/store pair.
/// Time changes are O(1); static results survive and temporal work is lazy.
/// Missing/unbound bindings and failed definitions are retried, not retained.
/// AOUSD Core §12.3–12.5; OpenUSD `UsdSkelCache`/`UsdSkelSkeletonQuery` semantics.
pub struct SkelCache {
    time: Time,
    epoch: u64,
    next_revision: u64,
    rigs: HashMap<PathId, Rig>,
    bindings: HashMap<PathId, Binding>,
    stats: SkelCacheStats,
}
impl core::fmt::Debug for SkelCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SkelCache")
            .field("time", &self.time)
            .field("memory", &self.memory())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}
impl SkelCache {
    /// Creates an empty evaluator at an explicit time.
    #[must_use]
    pub fn new(time: Time) -> Self {
        Self {
            time,
            epoch: 1,
            next_revision: 1,
            rigs: HashMap::new(),
            bindings: HashMap::new(),
            stats: SkelCacheStats::default(),
        }
    }
    /// Current default/numeric time, including interpolation policy.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }
    /// Sets time without traversing entries. A single sample counts as temporal
    /// because default and numeric reads can differ; masked samples are treated
    /// conservatively. Static entries retain their results across time changes.
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
    /// Drops retained definitions and buffers; preserves time, counters and revision
    /// allocation, so previously issued upload revisions are never reused.
    pub fn clear(&mut self) {
        self.rigs.clear();
        self.bindings.clear();
    }
    /// Current cumulative work counters.
    #[must_use]
    pub fn stats(&self) -> SkelCacheStats {
        self.stats
    }
    /// Resets counters without changing retained entries or time.
    pub fn reset_stats(&mut self) {
        self.stats = SkelCacheStats::default();
    }
    /// Array payload occupancy; excludes definitions and container overhead.
    #[must_use]
    pub fn memory(&self) -> SkelCacheMemory {
        let mut m = SkelCacheMemory {
            skeletons: self.rigs.len(),
            bindings: self.bindings.len(),
            ..SkelCacheMemory::default()
        };
        fn add<T>(m: &mut SkelCacheMemory, v: &Vec<T>) {
            m.used_bytes += v.len() * size_of::<T>();
            m.capacity_bytes += v.capacity() * size_of::<T>();
        }
        for r in self.rigs.values() {
            if let Some(Ok(v)) = &r.inverse {
                add(&mut m, v);
            }
            if let Some(Ok(v)) = &r.palette {
                add(&mut m, v);
            }
            if let Some(v) = &r.normals {
                add(&mut m, v);
            }
            for dq in [&r.dual_points, &r.dual_normals].into_iter().flatten() {
                let (used, capacity) = dq.occupancy();
                m.used_bytes += used;
                m.capacity_bytes += capacity;
            }
            if let Some(Ok(v)) = &r.weights {
                add(&mut m, v);
            }
        }
        for b in self.bindings.values() {
            if let Some(i) = &b.inputs {
                add(&mut m, &i.binding.indices);
                add(&mut m, &i.binding.weights);
                add(&mut m, &i.transforms);
            }
            add(&mut m, &b.points);
            add(&mut m, &b.normals);
            add(&mut m, &b.normal_transforms);
            add(&mut m, &b.normal_dual_quaternions);
            add(&mut m, &b.deformed_normals);
            add(&mut m, &b.shape_mapping);
            add(&mut m, &b.shape_weights);
            add(&mut m, &b.shape_contributions);
        }
        m
    }
    fn revision(&mut self) -> u64 {
        let revision = self.next_revision;
        self.next_revision = revision
            .checked_add(1)
            .expect("skeletal revision space exhausted");
        revision
    }
    fn ensure_rig(&mut self, scene: &Scene<'_>, path: PathId) -> Result<(), SkelError> {
        if self.rigs.contains_key(&path) {
            return Ok(());
        }
        let q = Skeleton::new(scene, path)
            .ok_or_else(|| invalid(path, "joints"))?
            .query()?;
        let definition = q.definition;
        let mut definitions = Dependencies::default();
        definitions.add(path, &["joints", "restTransforms", "bindTransforms"]);
        definitions.ancestry(scene, path, &["skel:animationSource"]);
        for &p in definitions.roots.clone().iter() {
            if let Some(property) = PrimView::new(*scene, p).property_path("skel:animationSource") {
                definitions.relationship(scene, property);
            }
        }
        let mut pose_dependencies = Dependencies::default();
        let mut weight_dependencies = Dependencies::default();
        if let Some(animation) = definition.animation_path {
            definitions.add(animation, &["joints", "blendShapes"]);
            pose_dependencies.add(animation, &["translations", "rotations", "scales"]);
            weight_dependencies.add(animation, &["blendShapeWeights"]);
        }
        let mut rig = Rig {
            definition_revision: self.revision(),
            pose_revision: 0,
            weight_revision: 0,
            definition,
            definitions,
            pose_dependencies,
            weight_dependencies,
            inverse: None,
            palette: None,
            normals: None,
            dual_points: None,
            dual_normals: None,
            weights: None,
            pose_epoch: 0,
            weight_epoch: 0,
            pose_version: 0,
            pose_varying: false,
            weights_varying: false,
        };
        rig.temporal(scene);
        self.rigs.insert(path, rig);
        self.stats.skeleton_builds += 1;
        Ok(())
    }
    fn ensure_palette(&mut self, scene: &Scene<'_>, path: PathId) -> Result<(), SkelError> {
        self.ensure_rig(scene, path)?;
        let rig = &self.rigs[&path];
        let stale = rig.palette.is_none() || (rig.pose_varying && rig.pose_epoch != self.epoch);
        let revision = if stale { self.revision() } else { 0 };
        let rig = self.rigs.get_mut(&path).expect("prepared rig");
        if stale {
            let computed = (|| {
                // Preserve snapshot validation order: bind availability/count,
                // pose inputs, then bind invertibility.
                rig.definition.validated_bind_transforms()?;
                let animated = rig
                    .definition
                    .query(scene)?
                    .skeleton_transforms(self.time)?;
                if rig.inverse.is_none() {
                    self.stats.inverse_bind_matrices +=
                        u64::try_from(rig.definition.joints.len()).unwrap_or(u64::MAX);
                    rig.inverse = Some(rig.definition.inverse_bind_transforms());
                }
                let inverse = rig
                    .inverse
                    .as_ref()
                    .expect("prepared inverse")
                    .as_ref()
                    .map_err(Clone::clone)?;
                let mut palette = match rig.palette.take() {
                    Some(Ok(v)) => v,
                    _ => Vec::new(),
                };
                palette.clear();
                palette.extend(inverse.iter().zip(animated).map(|(i, a)| gf::mul(i, &a)));
                Ok(palette)
            })();
            rig.palette = Some(computed);
            rig.normals = None;
            rig.dual_points = None;
            rig.dual_normals = None;
            rig.pose_epoch = self.epoch;
            rig.pose_revision = revision;
            rig.pose_version = rig.pose_version.wrapping_add(1);
            self.stats.pose_evaluations += 1;
        }
        rig.palette
            .as_ref()
            .expect("prepared palette")
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }
    fn ensure_weights(&mut self, scene: &Scene<'_>, path: PathId) -> Result<(), SkelError> {
        let rig = &self.rigs[&path];
        let stale =
            rig.weights.is_none() || (rig.weights_varying && rig.weight_epoch != self.epoch);
        let revision = if stale { self.revision() } else { 0 };
        let rig = self.rigs.get_mut(&path).expect("prepared rig");
        if stale {
            self.stats.blend_weight_evaluations += 1;
            rig.weights = Some(
                rig.definition
                    .query(scene)?
                    .blend_shape_weights(self.time, &rig.definition.blend_shapes),
            );
            rig.weight_epoch = self.epoch;
            rig.weight_revision = revision;
        }
        rig.weights
            .as_ref()
            .expect("prepared weights")
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }
    fn ensure_binding(&mut self, scene: &Scene<'_>, path: PathId) -> Result<bool, SkelError> {
        if self.bindings.contains_key(&path) {
            return Ok(true);
        }
        let Some(q) = SkinningQuery::prepare(scene, path, |rig| {
            self.ensure_rig(scene, rig)?;
            self.rigs[&rig].definition.query(scene)
        })?
        else {
            return Ok(false);
        };
        let definition = q.definition;
        let definition_revision = self.revision();
        let rig = &self.rigs[&definition.skeleton];
        let mut definitions = Dependencies::default();
        definitions.bindings(scene, path);
        definitions.add(path, &["skel:blendShapes", "skel:blendShapeTargets"]);
        if let Some(p) = PrimView::new(*scene, path).property_path("skel:blendShapeTargets") {
            definitions.relationship(scene, p);
        }
        if let Some(shapes) = &definition.blend_shapes {
            for &shape in shapes.targets() {
                // Arbitrary inbetween names and weight metadata are definitions.
                definitions.add(shape, &[]);
                for token in scene.stage().authored_property_names(shape, scene.store()) {
                    definitions
                        .fields
                        .push(Field::Forwarded(PropertyPath::new(shape, token)));
                }
            }
        }
        let mut point_dependencies = Dependencies::default();
        point_dependencies.add(path, &["points"]);
        let mut normal_dependencies = Dependencies::default();
        normal_dependencies.add(path, &["normals", "points", "faceVertexIndices"]);
        let shape_mapping = definition
            .blend_shapes
            .as_ref()
            .map_or_else(Vec::new, |shapes| {
                shapes
                    .names()
                    .iter()
                    .map(|n| rig.definition.blend_shapes.iter().position(|r| r == n))
                    .collect()
            });
        let mut binding = Binding {
            definition_revision,
            input_revision: 0,
            shape_revision: 0,
            shape_weight_revision: 0,
            shape_contributions: Vec::new(),
            definition,
            definitions,
            point_dependencies,
            normal_dependencies,
            inputs: None,
            input_epoch: 0,
            pose_version: 0,
            inputs_varying: false,
            points_varying: false,
            normals_varying: false,
            points: Vec::new(),
            normals: Vec::new(),
            point_valid: false,
            normal_valid: false,
            point_epoch: 0,
            normal_epoch: 0,
            normal_transforms: Vec::new(),
            normal_bind: None,
            normal_palette_revision: (0, 0),
            normal_dual_quaternions: Vec::new(),
            deformed_normals: Vec::new(),
            deformed_normal_valid: false,
            deformed_normal_epoch: 0,
            deformed_bounds: None,
            shape_mapping,
            shape_weights: Vec::new(),
        };
        binding.temporal(scene, rig);
        self.bindings.insert(path, binding);
        self.stats.binding_builds += 1;
        Ok(true)
    }
    fn ensure_inputs(&mut self, scene: &Scene<'_>, path: PathId) -> Result<(), SkelError> {
        let skel = self.bindings[&path].definition.skeleton;
        self.ensure_palette(scene, skel)?;
        let b = &self.bindings[&path];
        let stale = b.inputs.is_none() || (b.inputs_varying && b.input_epoch != self.epoch);
        let revision = if stale { self.revision() } else { 0 };
        let rig = &self.rigs[&skel];
        let binding = self.bindings.get_mut(&path).expect("prepared binding");
        let palette = rig
            .palette
            .as_ref()
            .expect("prepared palette")
            .as_ref()
            .map_err(Clone::clone)?;
        if stale {
            let query = binding
                .definition
                .query(scene, rig.definition.query(scene)?);
            binding.inputs = Some(query.inputs_with_shared_palette(self.time, palette)?);
            binding.input_epoch = self.epoch;
            binding.input_revision = revision;
            self.stats.influence_resolutions += 1;
        } else if binding.pose_version != rig.pose_version
            && let Some(inputs) = &mut binding.inputs
        {
            let transforms = &mut inputs.transforms;
            transforms.clear();
            if let Some(mapping) = &binding.definition.joint_mapping {
                transforms.extend(
                    mapping
                        .iter()
                        .map(|i| i.map_or(gf::IDENTITY, |i| palette[i])),
                );
            }
        }
        binding.pose_version = rig.pose_version;
        Ok(())
    }
    fn ensure_shape_weights(&mut self, scene: &Scene<'_>, path: PathId) -> Result<(), SkelError> {
        if self.bindings[&path].definition.blend_shapes.is_none() {
            return Ok(());
        }
        let skel = self.bindings[&path].definition.skeleton;
        self.ensure_weights(scene, skel)?;
        let b = &self.bindings[&path];
        let weight_revision = self.rigs[&skel].weight_revision;
        if b.shape_revision != 0 && b.shape_weight_revision == weight_revision {
            return Ok(());
        }
        let revision = self.revision();
        let weights = self.rigs[&skel]
            .weights
            .as_ref()
            .expect("prepared weights")
            .as_ref()
            .map_err(Clone::clone)?;
        let b = self.bindings.get_mut(&path).expect("prepared binding");
        b.shape_weights.clear();
        b.shape_weights
            .extend(b.shape_mapping.iter().map(|i| i.map_or(0., |i| weights[i])));
        self.stats.blend_shape_evaluations += 1;
        let contributions = b
            .definition
            .blend_shapes
            .as_ref()
            .expect("bound shapes")
            .compute_weights(&b.shape_weights)?;
        b.shape_contributions.clear();
        b.shape_contributions.extend(contributions);
        b.shape_weight_revision = weight_revision;
        b.shape_revision = revision;
        Ok(())
    }
    fn ensure_dual_points(&mut self, skeleton: PathId) -> Result<(), SkelError> {
        let rig = self.rigs.get_mut(&skeleton).expect("prepared rig");
        if rig.dual_points.is_none() {
            let palette = rig
                .palette
                .as_ref()
                .expect("prepared palette")
                .as_ref()
                .map_err(Clone::clone)?;
            self.stats.dual_quaternion_joints += u64::try_from(palette.len()).unwrap_or(u64::MAX);
            rig.dual_points = Some(Palette::points(palette));
        }
        Ok(())
    }
    /// Resolves borrowed palettes, influences and shape contributions without
    /// reading or deforming points/normals. Validate against adapter-owned vertex
    /// counts before upload. Repeated queries retain all input arrays; revisions
    /// identify components rebuilt/resampled by time changes or explicit edits.
    /// Uses standalone inherited-binding semantics; scoped discovery queries can
    /// instead use `SkinningQuery::binding_inputs` with their captured mapping.
    pub fn deformation_inputs(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
    ) -> Result<Option<DeformationInputs<'_>>, SkelError> {
        if !self.ensure_binding(scene, geometry)? {
            return Ok(None);
        }
        self.ensure_inputs(scene, geometry)?;
        self.ensure_shape_weights(scene, geometry)?;
        let b = &self.bindings[&geometry];
        let skel = b.definition.skeleton;
        let dual = b.inputs.as_ref().expect("prepared inputs").binding.method
            == SkinningMethod::DualQuaternion;
        if dual {
            self.ensure_dual_points(skel)?;
        }
        let b = &self.bindings[&geometry];
        let rig = &self.rigs[&skel];
        let inputs = b.inputs.as_ref().expect("prepared inputs");
        let palette = rig
            .palette
            .as_ref()
            .expect("prepared palette")
            .as_ref()
            .map_err(Clone::clone)?;
        Ok(Some(DeformationInputs {
            time: self.time,
            definition: &b.definition,
            binding: &inputs.binding,
            shared_transforms: palette,
            transforms: if b.definition.joint_mapping.is_some() {
                &inputs.transforms
            } else {
                palette
            },
            dual_quaternions: if dual {
                Some(rig.dual_points.as_ref().expect("prepared DQS").joints())
            } else {
                None
            },
            shape_weights: &b.shape_weights,
            contributions: &b.shape_contributions,
            revisions: DeformationRevisions {
                skeleton_definition: rig.definition_revision,
                binding_definition: b.definition_revision,
                inputs: b.input_revision,
                pose: rig.pose_revision,
                blend_weights: b.shape_revision,
            },
        }))
    }
    /// Visits retained inputs in the supplied sample order, then geometry order.
    /// Empty requests leave time unchanged. Duplicate times and paths are kept;
    /// time/interpolation policies are forwarded exactly. Static definitions and
    /// rig work are shared through ordinary cache queries. The visitor must copy
    /// or pack borrowed data before returning; no history or vertices are stored.
    /// Stops on query/visitor errors and leaves the cache at the last attempted
    /// time, including on failure. `E` can wrap `SkelError` with adapter context.
    pub fn for_each_deformation_sample<E>(
        &mut self,
        scene: &Scene<'_>,
        geometries: &[PathId],
        times: &[Time],
        mut visit: impl FnMut(Time, PathId, Option<DeformationInputs<'_>>) -> Result<(), E>,
    ) -> Result<(), E>
    where
        E: From<SkelError>,
    {
        if geometries.is_empty() {
            return Ok(());
        }
        for &time in times {
            self.set_time(time);
            for &path in geometries {
                let inputs = self.deformation_inputs(scene, path).map_err(E::from)?;
                visit(time, path, inputs)?;
            }
        }
        Ok(())
    }
    /// Shared inverse-bind/animated palette, in skeleton joint order. The slice
    /// remains valid until the next mutable cache operation. Errors are retained
    /// until an edit or relevant time change invalidates them.
    pub fn skinning_transforms(
        &mut self,
        scene: &Scene<'_>,
        skeleton: PathId,
    ) -> Result<&[gf::Matrix4], SkelError> {
        self.ensure_palette(scene, skeleton)?;
        self.rigs[&skeleton]
            .palette
            .as_ref()
            .expect("prepared palette")
            .as_ref()
            .map(Vec::as_slice)
            .map_err(Clone::clone)
    }
    /// Animated blend shapes followed by joint skinning, in skeleton space.
    /// Returns `None` for unbound geometry or absent joint influences. Retains
    /// successful outputs and reuses their array capacity across frames.
    pub fn deformed_points(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
    ) -> Result<Option<&[[f32; 3]]>, SkelError> {
        if !self.ensure_binding(scene, geometry)? {
            return Ok(None);
        }
        let b = &self.bindings[&geometry];
        if b.point_valid && (!b.points_varying || b.point_epoch == self.epoch) {
            self.stats.hits += 1;
            return Ok(Some(&self.bindings[&geometry].points));
        }
        self.stats.misses += 1;
        self.ensure_inputs(scene, geometry)?;
        let skel = self.bindings[&geometry].definition.skeleton;
        if self.bindings[&geometry].definition.blend_shapes.is_some() {
            self.ensure_weights(scene, skel)?;
        }
        let dual = self.bindings[&geometry]
            .inputs
            .as_ref()
            .expect("prepared inputs")
            .binding
            .method
            == SkinningMethod::DualQuaternion;
        if dual {
            self.ensure_dual_points(skel)?;
        }
        let points = read(
            &PrimView::new(*scene, geometry),
            "points",
            self.time,
            crate::value::read_float3_array,
        )
        .ok_or_else(|| invalid(geometry, "points"))?;
        self.ensure_shape_weights(scene, geometry)?;
        let b = self.bindings.get_mut(&geometry).expect("prepared binding");
        b.point_valid = false;
        b.deformed_bounds = None;
        b.points.clear();
        b.points.extend(points);
        if let Some(shapes) = &b.definition.blend_shapes {
            shapes.deform_contributions(&b.shape_contributions, &mut b.points, false)?;
        }
        let i = b.inputs.as_ref().expect("prepared inputs");
        let palette = self.rigs[&skel]
            .palette
            .as_ref()
            .expect("prepared palette")
            .as_ref()
            .map_err(Clone::clone)?;
        let transforms = if b.definition.joint_mapping.is_some() {
            &i.transforms[..]
        } else {
            &palette[..]
        };
        if dual {
            i.influences().validate(b.points.len(), transforms.len())?;
            self.rigs[&skel]
                .dual_points
                .as_ref()
                .expect("prepared DQS palette")
                .view(b.definition.joint_mapping.as_deref())
                .points(&i.binding.bind, i.influences(), &mut b.points);
        } else {
            skin_points_in_place(&i.binding.bind, transforms, i.influences(), &mut b.points)?;
        }
        self.stats.point_vertices += u64::try_from(b.points.len()).unwrap_or(u64::MAX);
        b.point_valid = true;
        b.point_epoch = self.epoch;
        Ok(Some(&b.points))
    }
    /// Retained hull of all deformed mesh points in skeleton space. Ignores
    /// authored extents/hints and reuses CPU point results. This query does not
    /// represent width-bearing curves/points or renderer displacement bounds.
    pub fn deformed_mesh_bounds(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
    ) -> Result<Option<crate::bounds::Range3d>, SkelError> {
        if !scene.is_a(geometry, "Mesh") {
            return Err(invalid(geometry, "points"));
        }
        if self.deformed_points(scene, geometry)?.is_none() {
            return Ok(None);
        }
        let b = self.bindings.get_mut(&geometry).expect("prepared binding");
        if b.deformed_bounds.is_none() {
            self.stats.bound_evaluations += 1;
            self.stats.bound_vertices += u64::try_from(b.points.len()).unwrap_or(u64::MAX);
            b.deformed_bounds = Some(super::bounds::range(&b.points)?);
        }
        Ok(b.deformed_bounds)
    }
    /// Oriented world bound: retained skeleton-space point hull paired with the
    /// Skeleton's world transform. `xforms` must use the same time and scene;
    /// pass edit reports to both caches. Call `aligned_range` for a world AABB.
    pub fn deformed_world_mesh_bounds(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
        xforms: &mut crate::XformCache,
    ) -> Result<Option<crate::bounds::BoundingBox>, SkelError> {
        let Some(range) = self.deformed_mesh_bounds(scene, geometry)? else {
            return Ok(None);
        };
        let skeleton = self.bindings[&geometry].definition.skeleton;
        super::bounds::world(range, scene, skeleton, self.time, xforms).map(Some)
    }
    fn ensure_normal_inputs(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
    ) -> Result<(), SkelError> {
        self.ensure_inputs(scene, geometry)?;
        let skel = self.bindings[&geometry].definition.skeleton;
        let rig = self.rigs.get_mut(&skel).expect("prepared rig");
        if rig.normals.is_none() {
            let palette = rig
                .palette
                .as_ref()
                .expect("prepared palette")
                .as_ref()
                .map_err(Clone::clone)?;
            self.stats.normal_matrices += u64::try_from(palette.len()).unwrap_or(u64::MAX);
            rig.normals = Some(
                palette
                    .iter()
                    .enumerate()
                    .map(|(i, m)| inverse_transpose(m, Some(i)))
                    .collect(),
            );
        }
        let dual = self.bindings[&geometry]
            .inputs
            .as_ref()
            .expect("prepared inputs")
            .binding
            .method
            == SkinningMethod::DualQuaternion;
        if dual && rig.dual_normals.is_none() {
            let matrices = rig.normals.as_ref().expect("prepared normals");
            self.stats.dual_quaternion_joints += u64::try_from(matrices.len()).unwrap_or(u64::MAX);
            // Singular matrices are rejected in the selected binding order
            // below; their unused shared entries must not poison other subsets.
            rig.dual_normals = Some(Palette::normals(matrices.iter().map(|m| {
                m.as_ref()
                    .copied()
                    .unwrap_or(super::decomposition::IDENTITY)
            })));
        }
        let normal_palette = rig.normals.as_ref().expect("prepared normals");
        let b = self.bindings.get_mut(&geometry).expect("prepared binding");
        let revision = (rig.pose_revision, b.input_revision);
        if b.normal_palette_revision == revision {
            return Ok(());
        }
        let i = b.inputs.as_ref().expect("prepared inputs");
        let bind = inverse_transpose(&i.binding.bind, None)?;
        b.normal_transforms.clear();
        if let Some(mapping) = &b.definition.joint_mapping {
            let identity = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
            for index in mapping {
                b.normal_transforms.push(match index {
                    Some(i) => *normal_palette[*i].as_ref().map_err(|_| {
                        SkelError::SingularNormalTransform {
                            joint: Some(b.normal_transforms.len()),
                        }
                    })?,
                    None => identity,
                });
            }
        } else {
            for matrix in normal_palette {
                b.normal_transforms
                    .push(*matrix.as_ref().map_err(Clone::clone)?);
            }
        }
        b.normal_dual_quaternions.clear();
        if dual {
            let joints = rig
                .dual_normals
                .as_ref()
                .expect("prepared DQS normal palette")
                .joints();
            if let Some(mapping) = &b.definition.joint_mapping {
                b.normal_dual_quaternions.extend(
                    mapping
                        .iter()
                        .map(|i| i.map_or(super::DualQuaternionJoint::IDENTITY, |i| joints[i])),
                );
            } else {
                b.normal_dual_quaternions.extend_from_slice(joints);
            }
        }
        b.normal_bind = Some(bind);
        b.normal_palette_revision = revision;
        Ok(())
    }
    /// Borrowed inverse-transpose matrices and prepared normal DQS components,
    /// in binding order, without reading vertex/normal buffers or shape weights.
    /// Unused singular rig joints do not invalidate custom binding subsets.
    pub fn normal_inputs(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
    ) -> Result<Option<super::NormalSkinningInputs<'_>>, SkelError> {
        if !self.ensure_binding(scene, geometry)? {
            return Ok(None);
        }
        self.ensure_normal_inputs(scene, geometry)?;
        let b = &self.bindings[&geometry];
        let i = b.inputs.as_ref().expect("prepared inputs");
        Ok(Some(super::NormalSkinningInputs {
            binding: &i.binding,
            bind: b.normal_bind.expect("prepared normal bind"),
            transforms: &b.normal_transforms,
            dual_quaternions: (i.binding.method == SkinningMethod::DualQuaternion)
                .then_some(b.normal_dual_quaternions.as_slice()),
            pose_revision: b.normal_palette_revision.0,
            input_revision: b.normal_palette_revision.1,
        }))
    }
    /// Skinned sampled normals without shape offsets. Retains results separately
    /// from `deformed_normals` and shares normal palettes across both operations.
    pub fn skinned_normals(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
    ) -> Result<Option<&[[f32; 3]]>, SkelError> {
        self.evaluate_normals(scene, geometry, false)
    }
    /// Applies animated normal offsets before skinning and normalization.
    /// Point-indexed shape offsets expand to mesh face-varying corners. Results
    /// refresh on shape-weight edits independently of skin-only normal outputs.
    pub fn deformed_normals(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
    ) -> Result<Option<&[[f32; 3]]>, SkelError> {
        self.evaluate_normals(scene, geometry, true)
    }
    fn evaluate_normals(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
        shapes: bool,
    ) -> Result<Option<&[[f32; 3]]>, SkelError> {
        if !self.ensure_binding(scene, geometry)? {
            return Ok(None);
        }
        let b = &self.bindings[&geometry];
        let varying = b.normals_varying
            || (shapes
                && b.definition.blend_shapes.is_some()
                && self.rigs[&b.definition.skeleton].weights_varying);
        let (valid, epoch) = if shapes {
            (b.deformed_normal_valid, b.deformed_normal_epoch)
        } else {
            (b.normal_valid, b.normal_epoch)
        };
        if valid && (!varying || epoch == self.epoch) {
            self.stats.hits += 1;
            let b = &self.bindings[&geometry];
            return Ok(Some(if shapes {
                &b.deformed_normals
            } else {
                &b.normals
            }));
        }
        self.stats.misses += 1;
        self.ensure_normal_inputs(scene, geometry)?;
        if shapes {
            self.ensure_shape_weights(scene, geometry)?;
        }
        let prim = PrimView::new(*scene, geometry);
        let normals = read(&prim, "normals", self.time, crate::value::read_float3_array)
            .ok_or_else(|| invalid(geometry, "normals"))?;
        let interpolation = prim
            .property_metadata("normals")
            .and_then(|m| m.interpolation())
            .unwrap_or("vertex");
        let b = self.bindings.get_mut(&geometry).expect("prepared binding");
        if shapes {
            b.deformed_normal_valid = false;
        } else {
            b.normal_valid = false;
        }
        let i = b.inputs.as_ref().expect("prepared inputs");
        let (point_count, corners) =
            if interpolation == "faceVarying" && scene.is_a(geometry, "Mesh") {
                let points = read(&prim, "points", self.time, crate::value::read_float3_array)
                    .ok_or_else(|| invalid(geometry, "points"))?;
                let corners = read(
                    &prim,
                    "faceVertexIndices",
                    self.time,
                    crate::value::read_int_array,
                )
                .ok_or_else(|| invalid(geometry, "faceVertexIndices"))?;
                super::normals::validate_corners(points.len(), &corners, normals.len())?;
                (points.len(), Some(corners))
            } else if matches!(interpolation, "vertex" | "varying")
                || (interpolation == "constant"
                    && b.definition.interpolation == super::InfluenceInterpolation::Constant)
            {
                (normals.len(), None)
            } else {
                return Err(invalid(geometry, "normals"));
            };
        i.influences()
            .validate(point_count, b.normal_transforms.len())?;
        let output = if shapes {
            &mut b.deformed_normals
        } else {
            &mut b.normals
        };
        output.clear();
        output.extend(normals);
        if shapes && let Some(query) = &b.definition.blend_shapes {
            if interpolation == "constant" && query.has_normal_contributions(&b.shape_contributions)
            {
                return Err(invalid(geometry, "normals"));
            }
            if let Some(corners) = &corners {
                query.deform_corner_normals(
                    &b.shape_contributions,
                    point_count,
                    corners,
                    output,
                )?;
            } else {
                query.deform_contributions(&b.shape_contributions, output, true)?;
            }
        }
        let bind = b.normal_bind.as_ref().expect("prepared normal bind");
        if i.binding.method == SkinningMethod::DualQuaternion {
            self.rigs[&b.definition.skeleton]
                .dual_normals
                .as_ref()
                .expect("prepared DQS normal palette")
                .view(b.definition.joint_mapping.as_deref())
                .normals(bind, i.influences(), corners.as_deref(), output);
        } else {
            super::normals::deform(
                bind,
                &b.normal_transforms,
                i.influences(),
                corners.as_deref(),
                output,
            );
        }
        self.stats.normal_vectors += u64::try_from(output.len()).unwrap_or(u64::MAX);
        if shapes {
            b.deformed_normal_valid = true;
            b.deformed_normal_epoch = self.epoch;
        } else {
            b.normal_valid = true;
            b.normal_epoch = self.epoch;
        }
        Ok(Some(output))
    }
    /// Invalidates retained inputs affected by a successful live edit report.
    /// Definition changes rebuild affected snapshots; pose/weight edits preserve
    /// inverse binds and independent geometry inputs. Forwarded relationships,
    /// external animation/shape definitions, removals and undo are dependencies.
    /// Precise property inventories let unrelated fields retain their results.
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &Changes) {
        let removed: HashSet<_> = self
            .rigs
            .iter()
            .filter_map(|(&p, r)| r.definitions.changed(scene, changes).then_some(p))
            .collect();
        let mut poses = HashSet::new();
        let mut weights = HashSet::new();
        for (&path, rig) in &mut self.rigs {
            if removed.contains(&path) {
                continue;
            }
            if rig.pose_dependencies.changed(scene, changes) {
                rig.palette = None;
                rig.normals = None;
                rig.dual_points = None;
                rig.dual_normals = None;
                poses.insert(path);
                self.stats.invalidations += 1;
            }
            if rig.weight_dependencies.changed(scene, changes) {
                rig.weights = None;
                weights.insert(path);
                self.stats.invalidations += 1;
            }
            rig.temporal(scene);
        }
        let mut remove_bindings = Vec::new();
        for (&path, b) in &mut self.bindings {
            let skel = b.definition.skeleton;
            // Shape definitions can gain arbitrary new inbetween names, so any
            // shape-property edit refreshes that local snapshot.
            let shape_changed = b.definition.blend_shapes.as_ref().is_some_and(|s| {
                changes
                    .changed_info_only
                    .iter()
                    .any(|p| s.targets().contains(p))
            });
            if removed.contains(&skel) || b.definitions.changed(scene, changes) || shape_changed {
                remove_bindings.push(path);
                continue;
            }
            let point = b.point_dependencies.changed(scene, changes)
                || poses.contains(&skel)
                || (weights.contains(&skel) && b.definition.blend_shapes.is_some());
            let normal = b.normal_dependencies.changed(scene, changes) || poses.contains(&skel);
            if point || normal {
                self.stats.invalidations += 1;
            }
            if point {
                b.point_valid = false;
            }
            if normal {
                b.normal_valid = false;
                b.deformed_normal_valid = false;
            }
            if weights.contains(&skel) && b.definition.blend_shapes.is_some() {
                b.deformed_normal_valid = false;
            }
            if let Some(rig) = self.rigs.get(&skel) {
                b.temporal(scene, rig);
            }
        }
        self.stats.invalidations +=
            u64::try_from(removed.len() + remove_bindings.len()).unwrap_or(u64::MAX);
        for path in remove_bindings {
            self.bindings.remove(&path);
        }
        for path in removed {
            self.rigs.remove(&path);
        }
    }
}
