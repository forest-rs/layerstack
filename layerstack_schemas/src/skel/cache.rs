// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit retained evaluation for one stage/store pair.
use super::SkinningMethod;
use super::dual_quaternion::Palette;
use super::normals::{NormalMatrix, inverse_transpose};
use super::skinning::{SkinningDefinition, SkinningInputs};
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
    /// Joint inverse-transpose matrices prepared for normal palettes.
    pub normal_matrices: u64,
    /// Joint DQS decompositions prepared for shared point/normal palettes.
    pub dual_quaternion_joints: u64,
    /// Point vertices submitted to the deformation kernel.
    pub point_vertices: u64,
    /// Normal vectors submitted to the normal kernel.
    pub normal_vectors: u64,
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
struct Dependencies {
    roots: Vec<PathId>,
    fields: Vec<Field>,
}
impl Dependencies {
    fn add(&mut self, path: PathId, names: &[&'static str]) {
        if !self.roots.contains(&path) {
            self.roots.push(path);
        }
        self.fields
            .extend(names.iter().map(|&name| Field::Named(path, name)));
    }
    fn ancestry(&mut self, scene: &Scene<'_>, mut path: PathId, names: &[&'static str]) {
        loop {
            self.add(path, names);
            let Some(parent) = scene.parent(path) else {
                break;
            };
            path = parent;
        }
    }
    fn relationship(&mut self, scene: &Scene<'_>, start: PropertyPath) {
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
    fn changed(&self, scene: &Scene<'_>, changes: &Changes) -> bool {
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
    /// Drops retained definitions and buffers; preserves time and counters.
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
            add(&mut m, &b.shape_mapping);
            add(&mut m, &b.shape_weights);
        }
        m
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
        let rig = self.rigs.get_mut(&path).expect("prepared rig");
        if rig.palette.is_none() || (rig.pose_varying && rig.pose_epoch != self.epoch) {
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
        let rig = self.rigs.get_mut(&path).expect("prepared rig");
        if rig.weights.is_none() || (rig.weights_varying && rig.weight_epoch != self.epoch) {
            rig.weights = Some(
                rig.definition
                    .query(scene)?
                    .blend_shape_weights(self.time, &rig.definition.blend_shapes),
            );
            rig.weight_epoch = self.epoch;
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
        let rig = &self.rigs[&skel];
        let binding = self.bindings.get_mut(&path).expect("prepared binding");
        let palette = rig
            .palette
            .as_ref()
            .expect("prepared palette")
            .as_ref()
            .map_err(Clone::clone)?;
        if binding.inputs.is_none() || (binding.inputs_varying && binding.input_epoch != self.epoch)
        {
            let query = binding
                .definition
                .query(scene, rig.definition.query(scene)?);
            binding.inputs = Some(query.inputs_with_shared_palette(self.time, palette)?);
            binding.input_epoch = self.epoch;
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
            let rig = self.rigs.get_mut(&skel).expect("prepared rig");
            if rig.dual_points.is_none() {
                let palette = rig
                    .palette
                    .as_ref()
                    .expect("prepared palette")
                    .as_ref()
                    .map_err(Clone::clone)?;
                self.stats.dual_quaternion_joints +=
                    u64::try_from(palette.len()).unwrap_or(u64::MAX);
                rig.dual_points = Some(Palette::points(palette));
            }
        }
        let weights = self.rigs[&skel]
            .weights
            .as_ref()
            .and_then(|w| w.as_ref().ok())
            .map_or(&[][..], Vec::as_slice);
        let points = read(
            &PrimView::new(*scene, geometry),
            "points",
            self.time,
            crate::value::read_float3_array,
        )
        .ok_or_else(|| invalid(geometry, "points"))?;
        let b = self.bindings.get_mut(&geometry).expect("prepared binding");
        b.point_valid = false;
        b.points.clear();
        b.points.extend(points);
        if let Some(shapes) = &b.definition.blend_shapes {
            b.shape_weights.clear();
            b.shape_weights
                .extend(b.shape_mapping.iter().map(|i| i.map_or(0., |i| weights[i])));
            shapes.deform_points_in_place(&b.shape_weights, &mut b.points)?;
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
    /// Skinned sampled normals, without blend-shape normal offsets. Supports the
    /// same interpolation as `SkinningQuery::compute_skinned_normals`; shared
    /// joint normal palettes are prepared once per skeleton pose.
    pub fn skinned_normals(
        &mut self,
        scene: &Scene<'_>,
        geometry: PathId,
    ) -> Result<Option<&[[f32; 3]]>, SkelError> {
        if !self.ensure_binding(scene, geometry)? {
            return Ok(None);
        }
        let b = &self.bindings[&geometry];
        if b.normal_valid && (!b.normals_varying || b.normal_epoch == self.epoch) {
            self.stats.hits += 1;
            return Ok(Some(&self.bindings[&geometry].normals));
        }
        self.stats.misses += 1;
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
        let prim = PrimView::new(*scene, geometry);
        let normals = read(&prim, "normals", self.time, crate::value::read_float3_array)
            .ok_or_else(|| invalid(geometry, "normals"))?;
        let interpolation = prim
            .property_metadata("normals")
            .and_then(|m| m.interpolation())
            .unwrap_or("vertex");
        let b = self.bindings.get_mut(&geometry).expect("prepared binding");
        b.normal_valid = false;
        let i = b.inputs.as_ref().expect("prepared inputs");
        let joint_count = b
            .definition
            .joint_mapping
            .as_ref()
            .map_or(normal_palette.len(), Vec::len);
        let corners = if interpolation == "faceVarying" && scene.is_a(geometry, "Mesh") {
            let points = read(&prim, "points", self.time, crate::value::read_float3_array)
                .ok_or_else(|| invalid(geometry, "points"))?;
            let corners = read(
                &prim,
                "faceVertexIndices",
                self.time,
                crate::value::read_int_array,
            )
            .ok_or_else(|| invalid(geometry, "faceVertexIndices"))?;
            i.influences().validate(points.len(), joint_count)?;
            if corners.len() != normals.len() {
                return Err(SkelError::InvalidDeformation {
                    element: None,
                    reason: "face-varying normal count",
                });
            }
            for (element, &corner) in corners.iter().enumerate() {
                if usize::try_from(corner)
                    .ok()
                    .is_none_or(|p| p >= points.len())
                {
                    return Err(SkelError::InvalidDeformation {
                        element: Some(element),
                        reason: "face corner point index",
                    });
                }
            }
            Some(corners)
        } else if matches!(interpolation, "vertex" | "varying")
            || (interpolation == "constant"
                && b.definition.interpolation == super::InfluenceInterpolation::Constant)
        {
            i.influences().validate(normals.len(), joint_count)?;
            None
        } else {
            return Err(invalid(geometry, "normals"));
        };
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
        b.normals.clear();
        b.normals.extend(normals);
        if dual {
            rig.dual_normals
                .as_ref()
                .expect("prepared DQS normal palette")
                .view(b.definition.joint_mapping.as_deref())
                .normals(&bind, i.influences(), corners.as_deref(), &mut b.normals);
        } else {
            super::normals::deform(
                &bind,
                &b.normal_transforms,
                i.influences(),
                corners.as_deref(),
                &mut b.normals,
            );
        }
        self.stats.normal_vectors += u64::try_from(b.normals.len()).unwrap_or(u64::MAX);
        b.normal_valid = true;
        b.normal_epoch = self.epoch;
        Ok(Some(&b.normals))
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
