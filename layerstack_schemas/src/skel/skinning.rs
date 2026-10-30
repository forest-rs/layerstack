// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CPU skinning with explicit methods, joint order and geometry spaces.
use super::{
    BlendShapeQuery, SkelError, SkelProblem, SkelProblemReason, SkeletonQuery,
    inherited_target_with_root, invalid, read, tokens,
};
use crate::{
    PrimView, Scene, Time, gf,
    primvar::Primvar,
    usd_skel::{SkelRoot, Skeleton},
};
use alloc::{sync::Arc, vec, vec::Vec};
use hashbrown::HashMap;
use layerstack::{PathId, PropertyPath, Specifier, TokenId, stage::PrototypeId};

/// CPU deformation method authored by `primvars:skel:skinningMethod`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkinningMethod {
    /// Weighted transformed points, with unnormalized authored weights.
    #[default]
    ClassicLinear,
    /// Hemisphere-aligned normalized dual quaternions, with linear residual
    /// scale/shear. Joint matrices are affine, as required by `UsdSkel`.
    DualQuaternion,
}

/// Computes skeleton-space points with an explicit skinning method. DQS follows
/// `UsdSkelSkinPoints`: normalized rotation/translation plus unnormalized linear
/// residual scale. Singular joint factorization uses OpenUSD's zero-DQ fallback.
pub fn skin_points_with_method(
    method: SkinningMethod,
    geom_bind: &gf::Matrix4,
    joint_transforms: &[gf::Matrix4],
    influences: impl SkinningInfluences,
    points: &[[f32; 3]],
) -> Result<Vec<[f32; 3]>, SkelError> {
    if method == SkinningMethod::ClassicLinear {
        return skin_points(geom_bind, joint_transforms, influences, points);
    }
    let mut points = points.to_vec();
    skin_points_in_place_with_method(method, geom_bind, joint_transforms, influences, &mut points)?;
    Ok(points)
}
/// Deforms a reusable buffer with an explicit method. Influence validation
/// precedes mutation; errors leave points unchanged. DQS prepares a temporary
/// joint palette; use `SkelCache` to share retained palettes across geometry.
pub fn skin_points_in_place_with_method(
    method: SkinningMethod,
    geom_bind: &gf::Matrix4,
    joint_transforms: &[gf::Matrix4],
    influences: impl SkinningInfluences,
    points: &mut [[f32; 3]],
) -> Result<(), SkelError> {
    influences.with_separate(|influences| {
        if method == SkinningMethod::ClassicLinear {
            return skin_points_in_place(geom_bind, joint_transforms, influences, points);
        }
        influences.validate(points.len(), joint_transforms.len())?;
        super::dual_quaternion::Palette::points(joint_transforms)
            .view(None)
            .points(geom_bind, influences, points);
        Ok(())
    })
}

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
/// Borrowed influence blocks interleaved as `[index, weight]` pairs, the
/// form GPU skinning consumes ([`JointInfluenceArrays::interleaved`]). Every
/// skinning function takes them as it takes [`JointInfluences`]; each index
/// is the pair's first value truncated to an integer.
///
/// OpenUSD: the `TfSpan<const GfVec2f> influences` overloads of
/// `UsdSkelSkinPoints`, `UsdSkelSkinPointsLBS`, `UsdSkelSkinNormals`,
/// `UsdSkelSkinNormalsLBS`, `UsdSkelSkinFaceVaryingNormals`,
/// `UsdSkelSkinTransform` and `UsdSkelSkinTransformLBS`, which read each
/// index as `static_cast<int>(influence[0])` (`_InterleavedInfluencesFn`,
/// `pxr/usd/usdSkel/utils.cpp`).
///
/// [`JointInfluenceArrays::interleaved`]: super::JointInfluenceArrays::interleaved
#[derive(Clone, Copy, Debug)]
pub struct InterleavedInfluences<'a> {
    /// `[joint index, weight]` per influence.
    pub influences: &'a [[f32; 2]],
    /// Positive number of influences in each block.
    pub element_size: usize,
    /// Whether each point has its own block.
    pub interpolation: InfluenceInterpolation,
}

impl InterleavedInfluences<'_> {
    /// The influences as separate index and weight arrays, each index
    /// truncated toward zero as C++ `static_cast<int>` does.
    #[must_use]
    pub fn split(&self) -> super::JointInfluenceArrays {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "OpenUSD truncates the interleaved index to an int"
        )]
        let indices = self.influences.iter().map(|pair| pair[0] as i32).collect();
        let weights = self.influences.iter().map(|pair| pair[1]).collect();
        super::JointInfluenceArrays { indices, weights }
    }
}

/// Joint influences a skinning function reads: separate index and weight
/// arrays ([`JointInfluences`]) or interleaved `[index, weight]` pairs
/// ([`InterleavedInfluences`]).
pub trait SkinningInfluences: Copy {
    /// Calls `f` with the influences as separate arrays, splitting
    /// interleaved ones into scratch arrays first.
    fn with_separate<R>(self, f: impl FnOnce(JointInfluences<'_>) -> R) -> R;
}

impl SkinningInfluences for JointInfluences<'_> {
    fn with_separate<R>(self, f: impl FnOnce(JointInfluences<'_>) -> R) -> R {
        f(self)
    }
}

impl SkinningInfluences for InterleavedInfluences<'_> {
    fn with_separate<R>(self, f: impl FnOnce(JointInfluences<'_>) -> R) -> R {
        let arrays = self.split();
        f(JointInfluences {
            indices: &arrays.indices,
            weights: &arrays.weights,
            element_size: self.element_size,
            interpolation: self.interpolation,
        })
    }
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
    influences: impl SkinningInfluences,
    points: &[[f32; 3]],
) -> Result<Vec<[f32; 3]>, SkelError> {
    influences.with_separate(|influences| {
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
    })
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
    influences: impl SkinningInfluences,
    points: &mut [[f32; 3]],
) -> Result<(), SkelError> {
    influences.with_separate(|influences| {
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
    })
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
pub(super) fn transform_point(point: [f32; 3], matrix: &gf::Matrix4, project: bool) -> [f32; 3] {
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
pub(super) fn inherited_property(
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

/// A property providing one skinning input: on the geometry, or on the
/// ancestor whose binding it inherits.
///
/// OpenUSD: what `UsdSkelSkinningQuery::GetJointIndicesPrimvar` and its
/// siblings name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingProperty {
    /// The property at its stage path; inside a native instance, an
    /// instance proxy's path.
    pub path: PropertyPath,
    /// Inside a native instance, where the property's prim lies in the
    /// prototype every instance of it shares: equal for every instance, so
    /// an engine can share the influence buffers between them. C++ names
    /// the prototype's own property there (`/__Prototype_1/Body.…`).
    /// `None` outside instances, and for the instance root itself.
    pub prototype: Option<PrototypeLocation>,
}

/// A prim's place inside a prototype: the innermost native instance root
/// above it and the names leading from that root down to the prim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrototypeLocation {
    /// The prototype, valid for the stage snapshot the query was made on.
    pub prototype: PrototypeId,
    /// Prim names from the instance root down to the prim; never empty.
    pub relative_path: Vec<TokenId>,
}

/// The prototype location of an instance proxy prim, as
/// [`BindingProperty::prototype`] describes it.
fn prototype_location(scene: &Scene<'_>, prim: PathId) -> Option<PrototypeLocation> {
    if !scene.prim(prim)?.is_instance_proxy() {
        return None;
    }
    let paths = scene.store().paths();
    let mut relative_path = Vec::new();
    let mut path = prim;
    loop {
        relative_path.push(*paths.resolve(path).segments().last()?);
        let parent = scene.parent(path)?;
        if let Some(prototype) = scene.prim(parent)?.prototype() {
            relative_path.reverse();
            return Some(PrototypeLocation {
                prototype,
                relative_path,
            });
        }
        path = parent;
    }
}

/// A geometry binding snapshot. Definition and influence metadata are retained;
/// sampled values resolve at explicit times. Rebuild after scene edits.
/// Output points are in skeleton space, before the Skeleton's scene Xform.
#[derive(Clone, Debug)]
pub struct SkinningQuery<'a> {
    pub(super) scene: Scene<'a>,
    pub(super) skeleton: SkeletonQuery<'a>,
    pub(super) definition: Arc<SkinningDefinition>,
}
#[derive(Clone, Debug)]
pub(super) struct SkinningDefinition {
    pub(super) geometry: PathId,
    pub(super) skeleton: PathId,
    pub(super) indices: PathId,
    pub(super) weights: PathId,
    pub(super) geom_bind: Option<PathId>,
    pub(super) method: Option<PathId>,
    pub(super) joint_mapping: Option<Vec<Option<usize>>>,
    pub(super) element_size: usize,
    pub(super) interpolation: InfluenceInterpolation,
    pub(super) dependencies: Vec<PathId>,
    pub(super) blend_shapes: Option<BlendShapeQuery>,
}
impl SkinningDefinition {
    pub(super) fn query<'a>(
        self: &Arc<Self>,
        scene: &Scene<'a>,
        skeleton: SkeletonQuery<'a>,
    ) -> SkinningQuery<'a> {
        SkinningQuery {
            scene: *scene,
            skeleton,
            definition: Arc::clone(self),
        }
    }
}
pub(super) struct SkinningInputs {
    pub binding: super::SkinningBindingInputs,
    pub transforms: Vec<gf::Matrix4>,
}
impl SkinningInputs {
    pub(super) fn influences(&self) -> JointInfluences<'_> {
        self.binding.influences()
    }
}
impl<'a> SkinningQuery<'a> {
    /// Prepares inherited bindings and influence metadata. Unbound geometry or
    /// geometry without joint influences returns `None`; partial bindings error.
    /// Indexed primvars are flattened when evaluated, using their element size.
    pub fn new(scene: &Scene<'a>, geometry: PathId) -> Result<Option<Self>, SkelError> {
        Self::prepare(scene, geometry, |path| {
            Skeleton::get(scene, path)
                .expect("validated skeleton target")
                .query()
        })
    }
    pub(super) fn prepare(
        scene: &Scene<'a>,
        geometry: PathId,
        load: impl FnOnce(PathId) -> Result<SkeletonQuery<'a>, SkelError>,
    ) -> Result<Option<Self>, SkelError> {
        Self::prepare_with_root(scene, geometry, None, load, || {
            BlendShapeQuery::new(scene, geometry)
        })
    }
    fn prepare_with_root(
        scene: &Scene<'a>,
        geometry: PathId,
        root: Option<PathId>,
        load: impl FnOnce(PathId) -> Result<SkeletonQuery<'a>, SkelError>,
        shapes: impl FnOnce() -> Result<Option<BlendShapeQuery>, SkelError>,
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
        let blend_shapes = shapes()?;
        if let Some(shapes) = &blend_shapes {
            dependencies.extend(shapes.targets());
        }
        let store = scene.store();
        store.paths().sort_paths(&mut dependencies, store.tokens());
        Ok(Some(Self {
            scene: *scene,
            skeleton,
            definition: Arc::new(SkinningDefinition {
                geometry,
                skeleton: skeleton_path,
                indices,
                weights,
                geom_bind,
                method,
                joint_mapping,
                element_size,
                interpolation,
                dependencies,
                blend_shapes,
            }),
        }))
    }
    /// Geometry this binding deforms.
    #[must_use]
    pub fn geometry_path(&self) -> PathId {
        self.definition.geometry
    }
    /// Skeleton definition and animation query used by this binding.
    #[must_use]
    pub fn skeleton_query(&self) -> &SkeletonQuery<'a> {
        &self.skeleton
    }
    /// Prim inputs consulted by this snapshot, including external animation,
    /// in namespace order.
    /// Rebuild if their properties or binding ancestry change.
    #[must_use]
    pub fn dependencies(&self) -> &[PathId] {
        &self.definition.dependencies
    }
    /// The property named `name` of `prim`, with its prototype location
    /// when `in_prototype`.
    fn binding_property(
        &self,
        prim: PathId,
        name: &str,
        in_prototype: bool,
    ) -> Option<BindingProperty> {
        let path = PrimView::new(self.scene, prim).property_path(name)?;
        Some(BindingProperty {
            path,
            prototype: in_prototype
                .then(|| prototype_location(&self.scene, prim))
                .flatten(),
        })
    }
    /// The `primvars:skel:jointIndices` providing the influences: the
    /// geometry's, or the nearest ancestor's it inherits.
    ///
    /// OpenUSD: `UsdSkelSkinningQuery::GetJointIndicesPrimvar`.
    #[doc(alias = "GetJointIndicesPrimvar")]
    #[must_use]
    pub fn joint_indices_property(&self) -> BindingProperty {
        self.binding_property(self.definition.indices, "primvars:skel:jointIndices", true)
            .expect("a prepared binding names its interned influences")
    }
    /// The `primvars:skel:jointWeights` providing the influences, as
    /// [`Self::joint_indices_property`] finds it.
    ///
    /// OpenUSD: `UsdSkelSkinningQuery::GetJointWeightsPrimvar`.
    #[doc(alias = "GetJointWeightsPrimvar")]
    #[must_use]
    pub fn joint_weights_property(&self) -> BindingProperty {
        self.binding_property(self.definition.weights, "primvars:skel:jointWeights", true)
            .expect("a prepared binding names its interned influences")
    }
    /// The inherited `primvars:skel:geomBindTransform` with an authored
    /// value, if any (identity applies otherwise).
    ///
    /// OpenUSD: `UsdSkelSkinningQuery::GetGeomBindTransformAttr`.
    #[doc(alias = "GetGeomBindTransformAttr")]
    #[must_use]
    pub fn geom_bind_transform_property(&self) -> Option<BindingProperty> {
        self.binding_property(
            self.definition.geom_bind?,
            "primvars:skel:geomBindTransform",
            true,
        )
    }
    /// The inherited `primvars:skel:skinningMethod` with an authored value,
    /// if any (classic linear applies otherwise). As in C++, an instance
    /// proxy's method is named at the proxy, with no prototype location.
    ///
    /// OpenUSD: `UsdSkelSkinningQuery::GetSkinningMethodAttr`
    /// (`UsdSkel_CacheImpl` reads it from the proxy, `cacheImpl.cpp`).
    #[doc(alias = "GetSkinningMethodAttr")]
    #[must_use]
    pub fn skinning_method_property(&self) -> Option<BindingProperty> {
        self.binding_property(
            self.definition.method?,
            "primvars:skel:skinningMethod",
            false,
        )
    }
    /// The geometry's `skel:blendShapes`, when it binds blend shapes.
    ///
    /// OpenUSD: `UsdSkelSkinningQuery::GetBlendShapesAttr`.
    #[doc(alias = "GetBlendShapesAttr")]
    #[must_use]
    pub fn blend_shapes_property(&self) -> Option<BindingProperty> {
        self.definition.blend_shapes.as_ref()?;
        self.binding_property(self.definition.geometry, "skel:blendShapes", true)
    }
    /// The geometry's `skel:blendShapeTargets`, when it binds blend shapes.
    ///
    /// OpenUSD: `UsdSkelSkinningQuery::GetBlendShapeTargetsRel`.
    #[doc(alias = "GetBlendShapeTargetsRel")]
    #[must_use]
    pub fn blend_shape_targets_property(&self) -> Option<BindingProperty> {
        self.definition.blend_shapes.as_ref()?;
        self.binding_property(self.definition.geometry, "skel:blendShapeTargets", true)
    }
    /// Number of influences in each constant or vertex block.
    #[doc(alias = "UsdSkelSkinningQuery::GetNumInfluencesPerComponent")]
    #[doc(alias = "GetNumInfluencesPerComponent")]
    #[must_use]
    pub fn element_size(&self) -> usize {
        self.definition.element_size
    }
    /// Influence block interpolation.
    #[must_use]
    pub fn interpolation(&self) -> InfluenceInterpolation {
        self.definition.interpolation
    }
    /// Reads the inherited uniform skinning method, defaulting to classic linear.
    /// Unknown tokens return an explicit unsupported-method error.
    pub fn skinning_method(&self) -> Result<SkinningMethod, SkelError> {
        let method = self
            .definition
            .method
            .and_then(|p| {
                PrimView::new(self.scene, p)
                    .read_value("primvars:skel:skinningMethod", crate::value::read_token)
            })
            .unwrap_or("classicLinear");
        match method {
            "classicLinear" => Ok(SkinningMethod::ClassicLinear),
            "dualQuaternion" => Ok(SkinningMethod::DualQuaternion),
            _ => Err(SkelError::UnsupportedSkinningMethod {
                prim: self.definition.geometry,
            }),
        }
    }
    /// Skins caller-supplied geometry-local points with animated joint matrices.
    /// Blend-shape offsets are not applied by this method.
    pub fn skin_points(&self, points: &[[f32; 3]], time: Time) -> Result<Vec<[f32; 3]>, SkelError> {
        let inputs = self.inputs(time)?;
        skin_points_with_method(
            inputs.binding.method,
            &inputs.binding.bind,
            &inputs.transforms,
            inputs.influences(),
            points,
        )
    }
    pub(super) fn inputs(&self, time: Time) -> Result<SkinningInputs, SkelError> {
        let transforms = self.skeleton.skinning_transforms(time)?;
        let mut inputs = self.inputs_with_shared_palette(time, &transforms)?;
        if self.definition.joint_mapping.is_none() {
            inputs.transforms = transforms;
        }
        Ok(inputs)
    }
    // An identity joint order borrows the retained rig palette; only custom
    // orders need a per-binding matrix array. Snapshot queries install their
    // owned palette in `inputs` above.
    pub(super) fn inputs_with_shared_palette(
        &self,
        time: Time,
        transforms: &[gf::Matrix4],
    ) -> Result<SkinningInputs, SkelError> {
        let binding = self.binding_inputs(time)?;
        let ordered = self.definition.joint_mapping.as_ref().map(|mapping| {
            mapping
                .iter()
                .map(|i| i.map_or(gf::IDENTITY, |i| transforms[i]))
                .collect::<Vec<_>>()
        });
        Ok(SkinningInputs {
            binding,
            transforms: ordered.unwrap_or_default(),
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
        super::skin_normals_with_method(
            inputs.binding.method,
            &inputs.binding.bind,
            &inputs.transforms,
            inputs.influences(),
            normals,
        )
    }
    /// Reads sampled normals and skins them. Supports vertex/varying and mesh
    /// face-varying normals; constant normals require constant influences.
    /// Uniform normals are rejected. Blend-shape offsets are not applied.
    pub fn compute_skinned_normals(&self, time: Time) -> Result<Vec<[f32; 3]>, SkelError> {
        self.compute_normals(time, false)
    }
    /// Applies animated point-indexed normal offsets before skeletal normal
    /// skinning and normalization. Mesh face-varying normals expand each point's
    /// offset to its corners; other supported interpolation follows
    /// `compute_skinned_normals`. Constant normals reject active point-indexed
    /// shape normal offsets because they cannot represent per-point changes.
    pub fn compute_deformed_normals(&self, time: Time) -> Result<Vec<[f32; 3]>, SkelError> {
        self.compute_normals(time, true)
    }
    fn compute_normals(&self, time: Time, shapes: bool) -> Result<Vec<[f32; 3]>, SkelError> {
        let prim = PrimView::new(self.scene, self.definition.geometry);
        let mut normals = read(&prim, "normals", time, crate::value::read_float3_array)?
            .ok_or_else(|| invalid(self.definition.geometry, "normals"))?;
        let interpolation = prim
            .property_metadata("normals")
            .and_then(|m| m.interpolation())
            .unwrap_or("vertex");
        let inputs = self.inputs(time)?;
        if shapes && let Some(query) = &self.definition.blend_shapes {
            let contributions =
                query.compute_weights(&self.skeleton.blend_shape_weights(time, query.names())?)?;
            if interpolation == "constant" && query.has_normal_contributions(&contributions) {
                return Err(invalid(self.definition.geometry, "normals"));
            }
            if interpolation == "faceVarying" && self.scene.is_a(self.definition.geometry, "Mesh") {
                let points = read(&prim, "points", time, crate::value::read_float3_array)?
                    .ok_or_else(|| invalid(self.definition.geometry, "points"))?;
                let corners = read(
                    &prim,
                    "faceVertexIndices",
                    time,
                    crate::value::read_int_array,
                )?
                .ok_or_else(|| invalid(self.definition.geometry, "faceVertexIndices"))?;
                query.deform_corner_normals(
                    &contributions,
                    points.len(),
                    &corners,
                    &mut normals,
                )?;
            } else {
                query.deform_contributions(&contributions, &mut normals, true)?;
            }
        }
        match interpolation {
            "vertex" | "varying" => super::skin_normals_with_method(
                inputs.binding.method,
                &inputs.binding.bind,
                &inputs.transforms,
                inputs.influences(),
                &normals,
            ),
            "constant" if self.definition.interpolation == InfluenceInterpolation::Constant => {
                super::skin_normals_with_method(
                    inputs.binding.method,
                    &inputs.binding.bind,
                    &inputs.transforms,
                    inputs.influences(),
                    &normals,
                )
            }
            "faceVarying" if self.scene.is_a(self.definition.geometry, "Mesh") => {
                let points = read(&prim, "points", time, crate::value::read_float3_array)?
                    .ok_or_else(|| invalid(self.definition.geometry, "points"))?;
                let corners = read(
                    &prim,
                    "faceVertexIndices",
                    time,
                    crate::value::read_int_array,
                )?
                .ok_or_else(|| invalid(self.definition.geometry, "faceVertexIndices"))?;
                super::skin_face_varying_normals_with_method(
                    inputs.binding.method,
                    &inputs.binding.bind,
                    &inputs.transforms,
                    inputs.influences(),
                    points.len(),
                    &corners,
                    &normals,
                )
            }
            _ => Err(invalid(self.definition.geometry, "normals")),
        }
    }
    /// Computes the rigid binding's skeleton-space matrix at an explicit time.
    /// Requires constant influences; uses OpenUSD's float-frame rounding.
    pub fn compute_rigid_transform(&self, time: Time) -> Result<gf::Matrix4, SkelError> {
        let inputs = self.inputs(time)?;
        super::rigid_skinning_transform_with_method(
            inputs.binding.method,
            &inputs.binding.bind,
            &inputs.transforms,
            inputs.influences(),
        )
    }
    /// Reads the geometry's sampled `points`, then computes skeleton-space skinning.
    pub fn compute_skinned_points(&self, time: Time) -> Result<Vec<[f32; 3]>, SkelError> {
        let points = read(
            &PrimView::new(self.scene, self.definition.geometry),
            "points",
            time,
            crate::value::read_float3_array,
        )?
        .ok_or_else(|| invalid(self.definition.geometry, "points"))?;
        self.skin_points(&points, time)
    }
}
/// One skinnable prim bound to an active skeleton, as skel discovery finds
/// it: every such prim, with or without valid joint influences or blend
/// shapes, as C++ `UsdSkelBinding::GetSkinningTargets` lists it.
#[derive(Clone, Debug)]
pub struct SkelBinding<'a> {
    /// The bound geometry.
    pub geometry: PathId,
    /// The skeleton it binds through `skel:skeleton`.
    pub skeleton: PathId,
    /// Its joint influences, skinning it with the skeleton; `None` when it has
    /// none or they are malformed (see [`Self::problems`]). C++:
    /// `UsdSkelSkinningQuery::HasJointInfluences`.
    pub skinning: Option<SkinningQuery<'a>>,
    /// Its own blend shapes; `None` when it binds none or the binding is
    /// malformed. Weights come from the skeleton's animation, as
    /// [`super::BlendShapeCache`] reads them. C++:
    /// `UsdSkelSkinningQuery::HasBlendShapes`.
    pub blend_shapes: Option<BlendShapeQuery>,
    /// What was skipped or left out of this binding, including skipped
    /// inbetweens, in discovery order.
    pub problems: Vec<SkelProblem>,
}

/// Every binding skel discovery found under a `SkelRoot`, and the prims it
/// could not bind. Discovery itself does not fail: each problem stays with
/// the binding, or prim, it belongs to.
#[derive(Clone, Debug, Default)]
pub struct SkelBindings<'a> {
    /// Bound skinnable prims, in traversal order.
    pub bindings: Vec<SkelBinding<'a>>,
    /// Prims left unbound, such as one whose `skel:skeleton` targets
    /// something other than a skeleton.
    pub problems: Vec<SkelProblem>,
}

impl<'a> SkelBindings<'a> {
    /// The bindings' skinning queries: the bound prims with valid joint
    /// influences.
    pub fn skinning_queries(&self) -> impl Iterator<Item = &SkinningQuery<'a>> {
        self.bindings
            .iter()
            .filter_map(|binding| binding.skinning.as_ref())
    }

    /// Every problem: the unbound prims', then each binding's.
    pub fn problems(&self) -> impl Iterator<Item = &SkelProblem> {
        self.problems
            .iter()
            .chain(self.bindings.iter().flat_map(|binding| &binding.problems))
    }
}

impl<'a> SkelRoot<'a> {
    /// Discovers skel bindings in default defined, non-abstract imageable
    /// traversal. Boundables prune children; native instance proxies are
    /// omitted. Binding inputs inherit only within this root, including the
    /// root itself. Every skinnable prim bound to an active skeleton is a
    /// binding, as in C++: one with malformed influences is bound without
    /// them, and a malformed blend-shape binding or skeleton is reported on
    /// its binding rather than failing discovery.
    /// OpenUSD: `UsdSkelCache::Populate`, `ComputeSkelBindings`.
    pub fn bindings(&self) -> SkelBindings<'a> {
        self.discover(false)
    }
    /// [`Self::bindings`], inside native instances as well as ordinary
    /// geometry. Equivalent to applying `UsdTraverseInstanceProxies` to
    /// default traversal; geometry and relationship targets use each
    /// instance's composed namespace. AOUSD Core §11.3.3.
    pub fn bindings_with_instance_proxies(&self) -> SkelBindings<'a> {
        self.discover(true)
    }
    fn discover(&self, instance_proxies: bool) -> SkelBindings<'a> {
        let scene = self.scene();
        let root = self.path();
        let mut pending = vec![root];
        let mut found = SkelBindings::default();
        let mut skeletons: HashMap<PathId, Result<SkeletonQuery<'a>, SkelError>> = HashMap::new();
        while let Some(path) = pending.pop() {
            let Some(prim) = scene.prim(path) else {
                continue;
            };
            if prim.specifier() != Specifier::Def
                || prim.is_abstract()
                || !scene.is_a(path, "Imageable")
            {
                continue;
            }
            if !(scene.is_a(path, "Boundable")
                && !scene.is_a(path, "Skeleton")
                && !scene.is_a(path, "SkelRoot"))
            {
                if instance_proxies || !prim.is_instance() {
                    pending.extend(prim.child_paths().iter().rev().copied());
                }
                continue;
            }
            let skeleton = match inherited_target_with_root(
                &scene,
                path,
                "skel:skeleton",
                "Skeleton",
                Some(root),
            ) {
                Ok(Some(skeleton)) => skeleton,
                Ok(None) => continue,
                Err(error) => {
                    found.problems.push(SkelProblem::from_error(
                        &scene,
                        path,
                        error,
                        SkelProblemReason::SkeletonTarget,
                    ));
                    continue;
                }
            };
            if !scene
                .prim(skeleton)
                .is_some_and(layerstack::Prim::is_active)
            {
                continue;
            }
            let mut problems = Vec::new();
            let blend_shapes = match BlendShapeQuery::new(&scene, path) {
                Ok(shapes) => shapes,
                Err(error) => {
                    problems.push(SkelProblem::from_error(
                        &scene,
                        path,
                        error,
                        SkelProblemReason::BlendShapes,
                    ));
                    None
                }
            };
            if let Some(shapes) = &blend_shapes {
                problems.extend(shapes.problems().iter().cloned());
            }
            let query = skeletons
                .entry(skeleton)
                .or_insert_with(|| {
                    Skeleton::get(&scene, skeleton)
                        .expect("validated skeleton target")
                        .query()
                })
                .clone();
            let skinning = match query {
                Err(error) => {
                    problems.push(SkelProblem::from_error(
                        &scene,
                        skeleton,
                        error,
                        SkelProblemReason::Skeleton,
                    ));
                    None
                }
                Ok(query) => match SkinningQuery::prepare_with_root(
                    &scene,
                    path,
                    Some(root),
                    |_| Ok(query),
                    || Ok(blend_shapes.clone()),
                ) {
                    Ok(skinning) => skinning,
                    Err(error) => {
                        problems.push(SkelProblem::from_error(
                            &scene,
                            path,
                            error,
                            SkelProblemReason::JointInfluences,
                        ));
                        None
                    }
                },
            };
            found.bindings.push(SkelBinding {
                geometry: path,
                skeleton,
                skinning,
                blend_shapes,
                problems,
            });
        }
        found
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
