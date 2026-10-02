// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CPU linear-blend skinning with explicit joint and geometry spaces.
use super::{
    BlendShapeQuery, SkelError, SkeletonQuery, inherited_target_with_root, invalid, read, tokens,
};
use crate::{
    PrimView, Scene, Time, gf,
    primvar::Primvar,
    usd_skel::{SkelRoot, Skeleton},
};
use alloc::{vec, vec::Vec};
use layerstack::{HashMap, PathId, Specifier};

/// How joint influence blocks apply to geometry points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InfluenceInterpolation {
    /// One influence block shared by all points (rigid deformation).
    Constant,
    /// One influence block per point.
    Vertex,
}
/// Borrowed, unnormalized influence blocks, as `UsdSkelSkinPoints` consumes.
/// Indices address the supplied transform order; weights are used as authored.
#[derive(Clone, Copy, Debug)]
pub struct JointInfluences<'a> {
    /// Joint index per influence; zero-weight indices must also be valid.
    pub indices: &'a [i32],
    /// Weight per influence; weights need not sum to one.
    pub weights: &'a [f32],
    /// Positive number of influences in each block.
    pub element_size: usize,
    /// Whether each point has its own block.
    pub interpolation: InfluenceInterpolation,
}
impl JointInfluences<'_> {
    /// Validates block lengths, positive element size, all joint indices and
    /// finite weights against the intended point and transform counts.
    pub fn validate(&self, point_count: usize, joint_count: usize) -> Result<(), SkelError> {
        let blocks = match self.interpolation {
            InfluenceInterpolation::Constant => 1,
            InfluenceInterpolation::Vertex => point_count,
        };
        let count = blocks
            .checked_mul(self.element_size)
            .ok_or(SkelError::InvalidDeformation {
                element: None,
                reason: "influence count overflow",
            })?;
        if self.element_size == 0 || self.indices.len() != count || self.weights.len() != count {
            return Err(SkelError::InvalidDeformation {
                element: None,
                reason: "influence array lengths or element size",
            });
        }
        for (element, (&index, &weight)) in self.indices.iter().zip(self.weights).enumerate() {
            if usize::try_from(index).ok().is_none_or(|i| i >= joint_count) {
                return Err(SkelError::InvalidDeformation {
                    element: Some(element),
                    reason: "joint index out of range",
                });
            }
            if !weight.is_finite() {
                return Err(SkelError::InvalidDeformation {
                    element: Some(element),
                    reason: "nonfinite influence weight",
                });
            }
        }
        Ok(())
    }
}

/// Computes CPU linear-blend points in skeleton space. Apply `geom_bind` first,
/// then each inverse-bind/animated joint matrix, accumulating authored weights.
/// Matches `UsdSkelSkinPoints` float rounding; weights are never normalized.
/// Invalid indices, sizes or nonfinite weights return no partial output.
/// AOUSD Core §6.3 (row-vector matrices); OpenUSD `UsdSkelSkinPointsLBS`.
pub fn skin_points(
    geom_bind: &[[f64; 4]; 4],
    joint_transforms: &[[[f64; 4]; 4]],
    influences: JointInfluences<'_>,
    points: &[[f32; 3]],
) -> Result<Vec<[f32; 3]>, SkelError> {
    influences.validate(points.len(), joint_transforms.len())?;
    #[cfg(feature = "simd")]
    {
        let level =
            fearless_simd::Level::try_detect().unwrap_or_else(fearless_simd::Level::baseline);
        if !level.is_fallback() {
            return Ok(
                fearless_simd::dispatch!(level, simd => super::simd::skin_points(simd, geom_bind, joint_transforms, influences, points)),
            );
        }
    }
    if is_affine(geom_bind) {
        Ok(skin_points_validated::<false>(
            geom_bind,
            joint_transforms,
            influences,
            points,
        ))
    } else {
        Ok(skin_points_validated::<true>(
            geom_bind,
            joint_transforms,
            influences,
            points,
        ))
    }
}
pub(super) fn is_affine(matrix: &gf::Matrix4) -> bool {
    matrix[0][3] == 0. && matrix[1][3] == 0. && matrix[2][3] == 0. && matrix[3][3] == 1.
}
fn skin_points_validated<const PROJECT_BIND: bool>(
    bind: &gf::Matrix4,
    joints: &[gf::Matrix4],
    influences: JointInfluences<'_>,
    points: &[[f32; 3]],
) -> Vec<[f32; 3]> {
    if influences.interpolation == InfluenceInterpolation::Constant {
        points
            .iter()
            .map(|&point| {
                skin_point::<PROJECT_BIND>(
                    bind,
                    joints,
                    influences.indices,
                    influences.weights,
                    point,
                )
            })
            .collect()
    } else {
        points
            .iter()
            .zip(influences.indices.chunks_exact(influences.element_size))
            .zip(influences.weights.chunks_exact(influences.element_size))
            .map(|((&point, indices), weights)| {
                skin_point::<PROJECT_BIND>(bind, joints, indices, weights, point)
            })
            .collect()
    }
}
/// Skins a reusable point buffer without allocating an output array. All
/// influence validation precedes mutation; an error leaves `points` unchanged.
/// Uses the same rounding and skeleton-space result as [`skin_points`].
pub fn skin_points_in_place(
    geom_bind: &[[f64; 4]; 4],
    joint_transforms: &[[[f64; 4]; 4]],
    influences: JointInfluences<'_>,
    points: &mut [[f32; 3]],
) -> Result<(), SkelError> {
    influences.validate(points.len(), joint_transforms.len())?;
    #[cfg(feature = "simd")]
    {
        let level =
            fearless_simd::Level::try_detect().unwrap_or_else(fearless_simd::Level::baseline);
        if !level.is_fallback() {
            fearless_simd::dispatch!(level, simd => super::simd::skin_points_in_place(simd, geom_bind, joint_transforms, influences, points));
            return Ok(());
        }
    }
    if is_affine(geom_bind) {
        skin_points_in_place_validated::<false>(geom_bind, joint_transforms, influences, points);
    } else {
        skin_points_in_place_validated::<true>(geom_bind, joint_transforms, influences, points);
    }
    Ok(())
}
fn skin_points_in_place_validated<const PROJECT_BIND: bool>(
    bind: &gf::Matrix4,
    joints: &[gf::Matrix4],
    influences: JointInfluences<'_>,
    points: &mut [[f32; 3]],
) {
    if influences.interpolation == InfluenceInterpolation::Constant {
        for point in points {
            *point = skin_point::<PROJECT_BIND>(
                bind,
                joints,
                influences.indices,
                influences.weights,
                *point,
            );
        }
    } else {
        for ((point, indices), weights) in points
            .iter_mut()
            .zip(influences.indices.chunks_exact(influences.element_size))
            .zip(influences.weights.chunks_exact(influences.element_size))
        {
            *point = skin_point::<PROJECT_BIND>(bind, joints, indices, weights, *point);
        }
    }
}
#[inline]
fn skin_point<const PROJECT_BIND: bool>(
    bind: &gf::Matrix4,
    joints: &[gf::Matrix4],
    indices: &[i32],
    weights: &[f32],
    point: [f32; 3],
) -> [f32; 3] {
    let initial = transform_point(point, bind, PROJECT_BIND);
    let mut result = [0.; 3];
    for (&index, &weight) in indices.iter().zip(weights) {
        if weight != 0. {
            let transformed = transform_point(
                initial,
                &joints[usize::try_from(index).expect("validated index")],
                false,
            );
            for (out, value) in result.iter_mut().zip(transformed) {
                *out += value * weight;
            }
        }
    }
    result
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "UsdSkelSkinPoints rounds transformed points to GfVec3f"
)]
fn transform_point(point: [f32; 3], matrix: &gf::Matrix4, project: bool) -> [f32; 3] {
    let component = |j| {
        f64::from(point[0]) * matrix[0][j]
            + f64::from(point[1]) * matrix[1][j]
            + f64::from(point[2]) * matrix[2][j]
            + matrix[3][j]
    };
    let w = if project { component(3) } else { 1. };
    // GfProject leaves directions (w == 0) unchanged.
    let inverse_w = if w == 0. { 1. } else { 1. / w };
    core::array::from_fn(|j| (component(j) * inverse_w) as f32)
}

// Binding properties inherit separately, only from API-bearing ancestors with
// authored values. Blocks and empty declarations do not replace a parent input
// (UsdSkel_CacheImpl::_Populate, AOUSD Core §12.3.6).
fn inherited_property(
    scene: &Scene<'_>,
    mut path: PathId,
    name: &str,
    root: Option<PathId>,
) -> Option<PathId> {
    loop {
        if scene.has_api(path, "SkelBindingAPI", None)
            && PrimView::new(*scene, path).has_authored_value(name)
        {
            return Some(path);
        }
        if Some(path) == root {
            return None;
        }
        path = scene.parent(path)?;
    }
}

/// A geometry binding snapshot. Definition and influence metadata are retained;
/// sampled values resolve at explicit times. Rebuild after scene edits.
/// Output points are in skeleton space, before the Skeleton's scene Xform.
#[derive(Clone, Debug)]
pub struct SkinningQuery<'a> {
    pub(super) scene: Scene<'a>,
    pub(super) geometry: PathId,
    pub(super) skeleton: SkeletonQuery<'a>,
    indices: PathId,
    weights: PathId,
    geom_bind: Option<PathId>,
    method: Option<PathId>,
    joint_mapping: Option<Vec<Option<usize>>>,
    element_size: usize,
    interpolation: InfluenceInterpolation,
    dependencies: Vec<PathId>,
    pub(super) blend_shapes: Option<BlendShapeQuery>,
}
pub(super) struct SkinningInputs {
    pub bind: gf::Matrix4,
    pub transforms: Vec<gf::Matrix4>,
    pub indices: Vec<i32>,
    pub weights: Vec<f32>,
    pub element_size: usize,
    pub interpolation: InfluenceInterpolation,
}
impl SkinningInputs {
    pub(super) fn influences(&self) -> JointInfluences<'_> {
        JointInfluences {
            indices: &self.indices,
            weights: &self.weights,
            element_size: self.element_size,
            interpolation: self.interpolation,
        }
    }
}
impl<'a> SkinningQuery<'a> {
    /// Prepares inherited bindings and influence metadata. Unbound geometry or
    /// geometry without joint influences returns `None`; partial bindings error.
    /// Indexed primvars are flattened when evaluated, using their element size.
    pub fn new(scene: &Scene<'a>, geometry: PathId) -> Result<Option<Self>, SkelError> {
        Self::prepare(scene, geometry, |path| {
            Skeleton::new(scene, path)
                .expect("validated skeleton target")
                .query()
        })
    }
    pub(super) fn prepare(
        scene: &Scene<'a>,
        geometry: PathId,
        load: impl FnOnce(PathId) -> Result<SkeletonQuery<'a>, SkelError>,
    ) -> Result<Option<Self>, SkelError> {
        Self::prepare_with_root(scene, geometry, None, load)
    }
    fn prepare_with_root(
        scene: &Scene<'a>,
        geometry: PathId,
        root: Option<PathId>,
        load: impl FnOnce(PathId) -> Result<SkeletonQuery<'a>, SkelError>,
    ) -> Result<Option<Self>, SkelError> {
        // AOUSD Core §12.3.6; UsdSkelCache::Populate starts binding inheritance
        // at the supplied traversal root, which remains inclusive.
        if !scene.is_a(geometry, "Boundable")
            || scene.is_a(geometry, "Skeleton")
            || scene.is_a(geometry, "SkelRoot")
        {
            return Ok(None);
        }
        let Some(skeleton_path) =
            inherited_target_with_root(scene, geometry, "skel:skeleton", "Skeleton", root)?
        else {
            return Ok(None);
        };
        let indices = inherited_property(scene, geometry, "primvars:skel:jointIndices", root);
        let weights = inherited_property(scene, geometry, "primvars:skel:jointWeights", root);
        if indices.is_none() && weights.is_none() {
            return Ok(None);
        }
        let indices = indices.ok_or_else(|| invalid(geometry, "primvars:skel:jointIndices"))?;
        let weights = weights.ok_or_else(|| invalid(geometry, "primvars:skel:jointWeights"))?;
        let index_var = Primvar::new(scene, indices, "skel:jointIndices")
            .ok_or_else(|| invalid(indices, "primvars:skel:jointIndices"))?;
        let weight_var = Primvar::new(scene, weights, "skel:jointWeights")
            .ok_or_else(|| invalid(weights, "primvars:skel:jointWeights"))?;
        let element_size = usize::try_from(index_var.element_size())
            .ok()
            .filter(|&v| v > 0)
            .ok_or_else(|| invalid(indices, "primvars:skel:jointIndices"))?;
        if index_var.element_size() != weight_var.element_size()
            || index_var.interpolation() != weight_var.interpolation()
        {
            return Err(invalid(geometry, "primvars:skel:jointWeights"));
        }
        let interpolation = match index_var.interpolation() {
            "constant" => InfluenceInterpolation::Constant,
            "vertex" => InfluenceInterpolation::Vertex,
            _ => return Err(invalid(indices, "primvars:skel:jointIndices")),
        };
        let skeleton = load(skeleton_path)?;
        let order_source = inherited_property(scene, geometry, "skel:joints", root);
        let joint_mapping = if let Some(source) = order_source {
            let order = tokens(&PrimView::new(*scene, source), "skel:joints")
                .ok_or_else(|| invalid(source, "skel:joints"))?;
            let indices: HashMap<_, _> = skeleton
                .joints()
                .iter()
                .enumerate()
                .map(|(i, name)| (name, i))
                .collect();
            Some(order.iter().map(|j| indices.get(j).copied()).collect())
        } else {
            None
        };
        let geom_bind =
            inherited_property(scene, geometry, "primvars:skel:geomBindTransform", root);
        let method = inherited_property(scene, geometry, "primvars:skel:skinningMethod", root);
        let mut dependencies = vec![geometry, skeleton_path, indices, weights];
        dependencies.extend(skeleton.animation_path());
        dependencies.extend([geom_bind, method, order_source].into_iter().flatten());
        let blend_shapes = BlendShapeQuery::new(scene, geometry)?;
        if let Some(shapes) = &blend_shapes {
            dependencies.extend(shapes.targets());
        }
        dependencies.sort_unstable();
        dependencies.dedup();
        Ok(Some(Self {
            scene: *scene,
            geometry,
            skeleton,
            indices,
            weights,
            geom_bind,
            method,
            joint_mapping,
            element_size,
            interpolation,
            dependencies,
            blend_shapes,
        }))
    }
    /// Geometry this binding deforms.
    #[must_use]
    pub fn geometry_path(&self) -> PathId {
        self.geometry
    }
    /// Skeleton definition and animation query used by this binding.
    #[must_use]
    pub fn skeleton_query(&self) -> &SkeletonQuery<'a> {
        &self.skeleton
    }
    /// Prim inputs consulted by this snapshot, including external animation.
    /// Rebuild if their properties or binding ancestry change.
    #[must_use]
    pub fn dependencies(&self) -> &[PathId] {
        &self.dependencies
    }
    /// Number of influences in each constant or vertex block.
    #[must_use]
    pub fn element_size(&self) -> usize {
        self.element_size
    }
    /// Influence block interpolation.
    #[must_use]
    pub fn interpolation(&self) -> InfluenceInterpolation {
        self.interpolation
    }
    /// Skins caller-supplied geometry-local points with animated joint matrices.
    /// Blend-shape offsets are not applied by this method.
    pub fn skin_points(&self, points: &[[f32; 3]], time: Time) -> Result<Vec<[f32; 3]>, SkelError> {
        let inputs = self.inputs(time)?;
        skin_points(
            &inputs.bind,
            &inputs.transforms,
            inputs.influences(),
            points,
        )
    }
    pub(super) fn inputs(&self, time: Time) -> Result<SkinningInputs, SkelError> {
        let method = self
            .method
            .and_then(|p| {
                PrimView::new(self.scene, p)
                    .read_value("primvars:skel:skinningMethod", crate::value::read_token)
            })
            .unwrap_or("classicLinear");
        if method != "classicLinear" {
            return Err(SkelError::UnsupportedSkinningMethod {
                prim: self.geometry,
            });
        }
        let geom_bind = self
            .geom_bind
            .and_then(|p| {
                read(
                    &PrimView::new(self.scene, p),
                    "primvars:skel:geomBindTransform",
                    time,
                    crate::value::read_matrix4d,
                )
            })
            .unwrap_or(gf::IDENTITY);
        let transforms = self.skeleton.skinning_transforms(time)?;
        let ordered = self.joint_mapping.as_ref().map(|mapping| {
            mapping
                .iter()
                .map(|i| i.map_or(gf::IDENTITY, |i| transforms[i]))
                .collect::<Vec<_>>()
        });
        let flattened = |path, name, property| {
            Primvar::new(&self.scene, path, name)
                .ok_or_else(|| invalid(path, property))?
                .compute_flattened(time)
                .map_err(|source| SkelError::Primvar { prim: path, source })?
                .ok_or_else(|| invalid(path, property))
        };
        let indices = crate::value::read_int_array(
            &flattened(
                self.indices,
                "skel:jointIndices",
                "primvars:skel:jointIndices",
            )?,
            self.scene.store().tokens(),
        )
        .ok_or_else(|| invalid(self.indices, "primvars:skel:jointIndices"))?;
        let weights = crate::value::read_float_array(
            &flattened(
                self.weights,
                "skel:jointWeights",
                "primvars:skel:jointWeights",
            )?,
            self.scene.store().tokens(),
        )
        .ok_or_else(|| invalid(self.weights, "primvars:skel:jointWeights"))?;
        Ok(SkinningInputs {
            bind: geom_bind,
            transforms: ordered.unwrap_or(transforms),
            indices,
            weights,
            element_size: self.element_size,
            interpolation: self.interpolation,
        })
    }
    /// Skins caller-supplied vertex/varying normals; blend-shape normal offsets
    /// are not applied. Uses inverse transposes and normalizes the result.
    pub fn skin_normals(
        &self,
        normals: &[[f32; 3]],
        time: Time,
    ) -> Result<Vec<[f32; 3]>, SkelError> {
        let inputs = self.inputs(time)?;
        super::skin_normals(
            &inputs.bind,
            &inputs.transforms,
            inputs.influences(),
            normals,
        )
    }
    /// Reads sampled normals and skins them. Supports vertex/varying and mesh
    /// face-varying normals; constant normals require constant influences.
    /// Uniform normals are rejected. Blend-shape offsets are not applied.
    pub fn compute_skinned_normals(&self, time: Time) -> Result<Vec<[f32; 3]>, SkelError> {
        let prim = PrimView::new(self.scene, self.geometry);
        let normals = read(&prim, "normals", time, crate::value::read_float3_array)
            .ok_or_else(|| invalid(self.geometry, "normals"))?;
        let interpolation = prim
            .property_metadata("normals")
            .and_then(|m| m.interpolation())
            .unwrap_or("vertex");
        let inputs = self.inputs(time)?;
        match interpolation {
            "vertex" | "varying" => super::skin_normals(
                &inputs.bind,
                &inputs.transforms,
                inputs.influences(),
                &normals,
            ),
            "constant" if self.interpolation == InfluenceInterpolation::Constant => {
                super::skin_normals(
                    &inputs.bind,
                    &inputs.transforms,
                    inputs.influences(),
                    &normals,
                )
            }
            "faceVarying" if self.scene.is_a(self.geometry, "Mesh") => {
                let points = read(&prim, "points", time, crate::value::read_float3_array)
                    .ok_or_else(|| invalid(self.geometry, "points"))?;
                let corners = read(
                    &prim,
                    "faceVertexIndices",
                    time,
                    crate::value::read_int_array,
                )
                .ok_or_else(|| invalid(self.geometry, "faceVertexIndices"))?;
                super::skin_face_varying_normals(
                    &inputs.bind,
                    &inputs.transforms,
                    inputs.influences(),
                    points.len(),
                    &corners,
                    &normals,
                )
            }
            _ => Err(invalid(self.geometry, "normals")),
        }
    }
    /// Computes the rigid binding's skeleton-space matrix at an explicit time.
    /// Requires constant influences; uses OpenUSD's float-frame rounding.
    pub fn compute_rigid_transform(&self, time: Time) -> Result<gf::Matrix4, SkelError> {
        let inputs = self.inputs(time)?;
        super::rigid_skinning_transform(&inputs.bind, &inputs.transforms, inputs.influences())
    }
    /// Reads the geometry's sampled `points`, then computes skeleton-space skinning.
    pub fn compute_skinned_points(&self, time: Time) -> Result<Vec<[f32; 3]>, SkelError> {
        let points = read(
            &PrimView::new(self.scene, self.geometry),
            "points",
            time,
            crate::value::read_float3_array,
        )
        .ok_or_else(|| invalid(self.geometry, "points"))?;
        self.skin_points(&points, time)
    }
}
impl<'a> SkelRoot<'a> {
    /// Discovers skinning bindings in default defined, non-abstract imageable
    /// traversal. Boundables prune children; native instance proxies are omitted.
    /// Binding inputs inherit only within this root, including the root itself.
    /// Each returned query exposes its geometry, skeleton and dependency roots.
    pub fn skinning_queries(&self) -> Result<Vec<SkinningQuery<'a>>, SkelError> {
        let scene = self.scene();
        let mut pending = vec![self.path()];
        let mut result = Vec::new();
        while let Some(path) = pending.pop() {
            if scene.stage().resolve_specifier(path, scene.store()) != Some(Specifier::Def)
                || scene.stage().is_abstract(path, scene.store())
                || !scene.is_a(path, "Imageable")
            {
                continue;
            }
            if scene.is_a(path, "Boundable")
                && !scene.is_a(path, "Skeleton")
                && !scene.is_a(path, "SkelRoot")
            {
                if let Some(query) =
                    SkinningQuery::prepare_with_root(&scene, path, Some(self.path()), |skeleton| {
                        Skeleton::new(&scene, skeleton)
                            .expect("validated skeleton target")
                            .query()
                    })?
                {
                    result.push(query);
                }
            } else if !scene.stage().is_instance(path) {
                pending.extend(
                    scene
                        .stage()
                        .children_of(path)
                        .unwrap_or_default()
                        .iter()
                        .copied()
                        .rev(),
                );
            }
        }
        Ok(result)
    }
}

#[cfg(all(test, feature = "simd"))]
mod tests {
    use super::*;
    #[test]
    fn simd_matches_scalar_for_sizes_interpolations_and_projected_binds() {
        let level =
            fearless_simd::Level::try_detect().unwrap_or_else(fearless_simd::Level::baseline);
        let mut joints = [gf::IDENTITY; 3];
        joints[0][0][1] = 0.125;
        joints[1][3] = [0.3, -1.7, 2.9, 1.];
        joints[2][1][2] = -0.875;
        for count in [0, 1, 3, 7, 31] {
            let points = alloc::vec![[0.3, -1.7, 2.9]; count];
            for interpolation in [
                InfluenceInterpolation::Constant,
                InfluenceInterpolation::Vertex,
            ] {
                let blocks = if interpolation == InfluenceInterpolation::Constant {
                    1
                } else {
                    count
                };
                let indices: Vec<_> = (0..blocks * 4)
                    .map(|i| i32::try_from(i % 3).unwrap())
                    .collect();
                let weights: Vec<_> = (0..blocks * 4)
                    .map(|i| [0., 0.125, 0.375, 0.5][i % 4])
                    .collect();
                let influences = JointInfluences {
                    indices: &indices,
                    weights: &weights,
                    element_size: 4,
                    interpolation,
                };
                for project in [false, true] {
                    let mut bind = gf::IDENTITY;
                    bind[3][0] = -3.25;
                    if project {
                        bind[0][3] = 0.125;
                        bind[3][3] = 0.5;
                    }
                    influences.validate(count, joints.len()).unwrap();
                    let scalar = if project {
                        skin_points_validated::<true>(&bind, &joints, influences, &points)
                    } else {
                        skin_points_validated::<false>(&bind, &joints, influences, &points)
                    };
                    let simd = fearless_simd::dispatch!(level, simd => crate::skel::simd::skin_points(simd, &bind, &joints, influences, &points));
                    assert_eq!(scalar, simd);
                    let mut reused = points.clone();
                    fearless_simd::dispatch!(level, simd => crate::skel::simd::skin_points_in_place(simd, &bind, &joints, influences, &mut reused));
                    assert_eq!(scalar, reused);
                }
            }
        }
    }
}
